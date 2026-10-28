//! One swarm on the network: an iroh endpoint and router running iroh-docs, iroh-blobs and
//! iroh-gossip, behind a gate that refuses machines outside the roster, and the swarm document's
//! files and signed change log.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::{Context, bail};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{
    AfterHandshakeOutcome, BeforeConnectOutcome, Connection, EndpointHooks, PortmapperConfig,
    WeakConnectionHandle, presets,
};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::{BlobsProtocol, Hash};
use iroh_docs::api::Doc;
use iroh_docs::api::protocol::{AddrInfoOptions, ShareMode};
use iroh_docs::engine::LiveEvent;
use iroh_docs::protocol::Docs;
use iroh_docs::store::Query;
use iroh_docs::{Author, AuthorId, Capability, CapabilityKind, Entry, NamespaceId};
use iroh_gossip::net::Gossip;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::decide::Remote;
use crate::ids::{PersonId, PersonKey};
use crate::invites::{Hydra, Invites};
use crate::paths::{
    FILES_PREFIX, LOG_PREFIX, MAX_FILE_BYTES, PARENTS_PREFIX, RelPath, file_key, log_key,
    parent_key, path_of_key, path_of_parent_key, safe_rel_path,
};
use crate::roster::{
    Change, Kept, KeptAuthor, KeptEntry, MAX_CHANGE_BYTES, Machine, Member, Role, Roster,
    SignedChange, Version,
};
use crate::wire::ALPN;

/// A parent record holds at most a few hundred hashes.
const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// What one author's file entry for a path was made from, so other machines can tell an edit
/// made on top of theirs from one made at the same time. `hash` is the entry's (None for a
/// delete), `parent` the version it was made from (None for a new file), and `ancestors`
/// `parent` and the versions before it, as far as the author knew them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ParentRecord {
    pub hash: Option<Hash>,
    pub parent: Option<Hash>,
    #[serde(default)]
    pub ancestors: Vec<Hash>,
}

/// Every current writer's entry for one path, and every former writer's as the roster keeps it,
/// and their parent records for it, by author.
#[derive(Default, Debug, Clone)]
pub struct FileHistory {
    pub entries: Vec<Remote>,
    pub records: Vec<(String, ParentRecord)>,
}

pub struct NodeConfig {
    /// `state_root/swarms/<ns>/`, holding the persistent `docs/` and `blobs/` stores.
    pub dir: PathBuf,
    /// This machine's key for this swarm: its iroh identity here.
    pub machine_key: [u8; 32],
    /// False in tests: no relays and only direct addresses.
    pub relays: bool,
    /// This machine's person.
    pub person: PersonKey,
    /// Whose genesis the roster folds from. None only for `create`, where it is the creator.
    pub anchor: Option<PersonId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeEvent {
    /// Any insert into the document, local or remote.
    Changed,
    ContentReady(Hash),
    PeerUp(String),
    PeerDown(String),
    SyncFinished,
    /// An owner told this machine it was removed from the swarm.
    Removed,
}

/// The ALPNs that carry the swarm document, its content and its gossip.
const GATED_ALPNS: [&[u8]; 3] = [iroh_docs::ALPN, iroh_blobs::ALPN, iroh_gossip::ALPN];

fn is_gated(alpn: &[u8]) -> bool {
    GATED_ALPNS.contains(&alpn)
}

/// Who may talk to this node on the gated ALPNs, and the open gated connections by peer, so that
/// taking a peer out of the gate can close them.
#[derive(Debug, Default)]
struct GateState {
    allowed: HashSet<EndpointId>,
    open: HashMap<EndpointId, Vec<WeakConnectionHandle>>,
}

type Gate = Arc<Mutex<GateState>>;

fn lock(gate: &Gate) -> std::sync::MutexGuard<'_, GateState> {
    gate.lock().unwrap_or_else(|e| e.into_inner())
}

/// Endpoint hooks that keep machines outside the gate off docs, blobs and gossip in both
/// directions: a dial to one is never started, and a connection with one, either way, is closed
/// as soon as the handshake shows who it is. Other ALPNs (the hydra protocol) pass.
#[derive(Debug)]
struct GateHooks(Gate);

impl EndpointHooks for GateHooks {
    async fn before_connect<'a>(
        &'a self,
        remote: &'a EndpointAddr,
        alpn: &'a [u8],
    ) -> BeforeConnectOutcome {
        if is_gated(alpn) && !lock(&self.0).allowed.contains(&remote.id) {
            BeforeConnectOutcome::Reject
        } else {
            BeforeConnectOutcome::Accept
        }
    }

    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        if !is_gated(conn.alpn()) {
            return AfterHandshakeOutcome::accept();
        }
        let peer = conn.remote_id();
        let mut gate = lock(&self.0);
        if !gate.allowed.contains(&peer) {
            return AfterHandshakeOutcome::Reject {
                error_code: 1u32.into(),
                reason: b"not a member of this swarm".to_vec(),
            };
        }
        let open = gate.open.entry(peer).or_default();
        open.retain(|c| c.upgrade().is_some());
        open.push(conn.weak_handle());
        AfterHandshakeOutcome::accept()
    }
}

pub struct SwarmNode {
    pub(crate) core: Arc<Core>,
    router: Router,
}

/// What a node's methods and its hydra protocol handler share.
pub(crate) struct Core {
    pub(crate) endpoint: Endpoint,
    pub(crate) docs: Docs,
    pub(crate) blobs: FsStore,
    pub(crate) doc: Doc,
    pub(crate) author: AuthorId,
    pub(crate) anchor: PersonId,
    pub(crate) person: PersonKey,
    /// Set once this machine holds the write capability; a promotion can set it later.
    pub(crate) writable: AtomicBool,
    gate: Gate,
    pub(crate) addrs: MemoryLookup,
    pub(crate) events: broadcast::Sender<NodeEvent>,
    pub(crate) invites: Invites,
}

/// Where the hydra protocol handler finds the node, once there is one: the router starts before
/// the document is chosen.
pub(crate) type CoreSlot = Arc<OnceLock<Weak<Core>>>;

pub(crate) fn endpoint_builder(machine_key: &[u8; 32], relays: bool) -> iroh::endpoint::Builder {
    let builder = if relays {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
    };
    builder.secret_key(SecretKey::from_bytes(machine_key))
}

/// The endpoint, stores and router, before a document is chosen.
struct Parts {
    endpoint: Endpoint,
    router: Router,
    docs: Docs,
    blobs: FsStore,
    gate: Gate,
    addrs: MemoryLookup,
    slot: CoreSlot,
}

async fn start(cfg: &NodeConfig) -> anyhow::Result<Parts> {
    let docs_dir = cfg.dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("creating {}", docs_dir.display()))?;
    let blobs = FsStore::load(cfg.dir.join("blobs")).await?;
    let addrs = MemoryLookup::new();
    let gate: Gate = Arc::default();
    let bound = endpoint_builder(&cfg.machine_key, cfg.relays)
        .address_lookup(addrs.clone())
        .hooks(GateHooks(Arc::clone(&gate)))
        .bind()
        .await;
    let endpoint = match bound {
        Ok(ep) => ep,
        Err(e) => {
            let _ = blobs.shutdown().await;
            return Err(e.into());
        }
    };
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let spawned = Docs::persistent(docs_dir)
        .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
        .await;
    let docs = match spawned {
        Ok(docs) => docs,
        Err(e) => {
            gossip.shutdown().await.ok();
            endpoint.close().await;
            let _ = blobs.shutdown().await;
            return Err(e);
        }
    };
    let slot = CoreSlot::default();
    // The hydra ALPN is outside the gate, since invitees aren't members yet; each request checks
    // its own sender.
    let router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs, None))
        .accept(iroh_gossip::ALPN, gossip)
        .accept(iroh_docs::ALPN, docs.clone())
        .accept(ALPN, Hydra(Arc::clone(&slot)))
        .spawn();
    Ok(Parts {
        endpoint,
        router,
        docs,
        blobs,
        gate,
        addrs,
        slot,
    })
}

fn anchor_file(cfg: &NodeConfig) -> PathBuf {
    cfg.dir.join("anchor")
}

pub(crate) fn now_micros() -> u64 {
    u64::try_from(jiff::Timestamp::now().as_microsecond()).unwrap_or(0)
}

impl SwarmNode {
    /// A new swarm: makes the namespace and this machine's author, and writes the genesis signed
    /// by the config's person. `me` must be that person; its one machine entry is filled in with
    /// this node's endpoint and author, keeping the machine name given.
    pub async fn create(cfg: NodeConfig, name: &str, me: Member) -> anyhow::Result<SwarmNode> {
        if me.person != cfg.person.id() {
            bail!("the swarm's creator must be this machine's person");
        }
        let parts = start(&cfg).await?;
        let made = async {
            let doc = parts.docs.create().await?;
            let author = parts.docs.author_create().await?;
            parts.docs.author_set_default(author).await?;
            anyhow::Ok((doc, author))
        }
        .await;
        let (doc, author) = match made {
            Ok(v) => v,
            Err(e) => {
                stop(parts.router, parts.blobs).await;
                return Err(e);
            }
        };
        let node = SwarmNode::finish(cfg, parts, doc, author, creator).await?;
        let machine_name = me
            .machines
            .first()
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let owner = Member {
            role: Role::Owner,
            machines: vec![Machine {
                endpoint: node.endpoint_id(),
                name: machine_name,
                author: node.author(),
            }],
            ..me
        };
        let genesis = SignedChange::sign(
            &node.id(),
            Change::Genesis {
                name: name.to_string(),
                owner,
            },
            &node.core.person,
            now_micros(),
        );
        let written = async {
            node.write_change(&genesis).await?;
            node.core.refresh_gate_from_roster().await
        }
        .await;
        if let Err(e) = written {
            node.shutdown().await;
            return Err(e);
        }
        Ok(node)
    }

    /// Opens a swarm this machine already belongs to.
    pub async fn open(cfg: NodeConfig, ns: NamespaceId) -> anyhow::Result<SwarmNode> {
        let parts = start(&cfg).await?;
        let opened = async {
            let doc = parts
                .docs
                .open(ns)
                .await?
                .context("this machine doesn't hold that swarm")?;
            let author = parts.docs.author_default().await?;
            anyhow::Ok((doc, author))
        }
        .await;
        let (doc, author) = match opened {
            Ok(v) => v,
            Err(e) => {
                stop(parts.router, parts.blobs).await;
                return Err(e);
            }
        };
        SwarmNode::finish(cfg, parts, doc, author, saved_anchor).await
    }

    /// Joins with the capability an inviter handed over, writing as `author`, which the joiner
    /// made beforehand so the inviter could add it to the roster.
    ///
    /// The new node's gate is empty, since it has no roster yet, so it syncs with nobody until
    /// the gate is opened to the inviter. Joining with an invite code should go through
    /// [`SwarmNode::join_with_invite`], which does that and refreshes the gate from the roster
    /// once it arrives.
    pub async fn join(
        cfg: NodeConfig,
        cap: Capability,
        author: Author,
    ) -> anyhow::Result<SwarmNode> {
        given_anchor(&cfg)?;
        let parts = start(&cfg).await?;
        let id = author.id();
        let joined = async {
            parts.docs.author_import(author).await?;
            parts.docs.author_set_default(id).await?;
            parts.docs.import_namespace(cap).await
        }
        .await;
        let doc = match joined {
            Ok(doc) => doc,
            Err(e) => {
                stop(parts.router, parts.blobs).await;
                return Err(e);
            }
        };
        SwarmNode::finish(cfg, parts, doc, id, given_anchor).await
    }

    /// Settles the anchor, forwards the document's events, sets the gate from the roster and
    /// only then starts live sync, so peers stored from earlier syncs who are no longer members
    /// are never dialled.
    async fn finish(
        cfg: NodeConfig,
        parts: Parts,
        doc: Doc,
        author: AuthorId,
        anchor: fn(&NodeConfig) -> anyhow::Result<PersonId>,
    ) -> anyhow::Result<SwarmNode> {
        let ready = async {
            let anchor = anchor(&cfg)?;
            std::fs::write(anchor_file(&cfg), anchor.to_string())?;
            let writable = parts
                .docs
                .list()
                .await?
                .try_collect::<Vec<_>>()
                .await?
                .into_iter()
                .any(|(id, kind)| id == doc.id() && matches!(kind, CapabilityKind::Write));
            let stream = doc.subscribe().await?;
            anyhow::Ok((anchor, writable, stream))
        }
        .await;
        let (anchor, writable, mut stream) = match ready {
            Ok(v) => v,
            Err(e) => {
                stop(parts.router, parts.blobs).await;
                return Err(e);
            }
        };
        let (events, _) = broadcast::channel(256);
        let tx = events.clone();
        tokio::spawn(async move {
            while let Some(ev) = stream.next().await {
                let ev = match ev {
                    Ok(LiveEvent::InsertLocal { .. } | LiveEvent::InsertRemote { .. }) => {
                        NodeEvent::Changed
                    }
                    Ok(LiveEvent::ContentReady { hash }) => NodeEvent::ContentReady(hash),
                    Ok(LiveEvent::NeighborUp(p)) => NodeEvent::PeerUp(p.to_string()),
                    Ok(LiveEvent::NeighborDown(p)) => NodeEvent::PeerDown(p.to_string()),
                    Ok(LiveEvent::SyncFinished(_)) => NodeEvent::SyncFinished,
                    Ok(LiveEvent::PendingContentReady) => continue,
                    Err(_) => break,
                };
                // Nobody listening is fine.
                let _ = tx.send(ev);
            }
        });
        let core = Arc::new(Core {
            endpoint: parts.endpoint,
            docs: parts.docs,
            blobs: parts.blobs,
            doc,
            author,
            anchor,
            person: cfg.person,
            writable: AtomicBool::new(writable),
            gate: parts.gate,
            addrs: parts.addrs,
            events,
            invites: Invites::new(&cfg.dir),
        });
        let _ = parts.slot.set(Arc::downgrade(&core));
        let node = SwarmNode {
            core,
            router: parts.router,
        };
        let live = async {
            node.core.refresh_gate_from_roster().await?;
            node.core.doc.start_sync(vec![]).await
        }
        .await;
        if let Err(e) = live {
            node.shutdown().await;
            return Err(e);
        }
        Ok(node)
    }

    /// The namespace id.
    pub fn id(&self) -> String {
        self.core.id()
    }

    pub fn endpoint_id(&self) -> String {
        self.core.endpoint.id().to_string()
    }

    /// This machine's author id in the document.
    pub fn author(&self) -> String {
        self.core.author.to_string()
    }

    /// Whether this machine holds the write capability now.
    pub fn can_write(&self) -> bool {
        self.core.can_write()
    }

    pub fn events(&self) -> broadcast::Receiver<NodeEvent> {
        self.core.events.subscribe()
    }

    /// The capability to hand to someone joining with `role`: readers get only the read one.
    pub async fn capability(&self, role: Role) -> anyhow::Result<Capability> {
        self.core.capability(role).await
    }

    /// The roster folded from every signed change whose content is here, or None until the
    /// anchor's genesis has arrived.
    pub async fn roster(&self) -> anyhow::Result<Option<Roster>> {
        self.core.roster().await
    }

    /// Adds a signed change to the log.
    pub async fn write_change(&self, c: &SignedChange) -> anyhow::Result<()> {
        self.core.write_change(c).await
    }

    /// Per file key, the newest entry (tombstones included) whose author is one of `roster`'s
    /// current writers or owners, or a former one's as the roster keeps it. Keys that aren't safe
    /// paths and entries over the size cap are left out.
    pub async fn files(&self, roster: &Roster) -> anyhow::Result<BTreeMap<RelPath, Remote>> {
        let mut out: BTreeMap<RelPath, Remote> = BTreeMap::new();
        for (path, remote) in self.file_versions(roster).await? {
            let newer = out
                .get(&path)
                .is_none_or(|r| (remote.at, &remote.author) > (r.at, &r.author));
            if newer {
                out.insert(path, remote);
            }
        }
        Ok(out)
    }

    /// Per file key, every entry from a current writer or owner, or a former one's as the roster
    /// keeps it (one per author, tombstones included, oldest first), and those authors' parent
    /// records for it.
    pub async fn file_entries(
        &self,
        roster: &Roster,
    ) -> anyhow::Result<BTreeMap<RelPath, FileHistory>> {
        let mut out: BTreeMap<RelPath, FileHistory> = BTreeMap::new();
        for (path, remote) in self.file_versions(roster).await? {
            out.entry(path).or_default().entries.push(remote);
        }
        for h in out.values_mut() {
            h.entries
                .sort_by(|a, b| (a.at, &a.author).cmp(&(b.at, &b.author)));
        }
        let query = Query::key_prefix(PARENTS_PREFIX).build();
        for e in self.core.entries(query).await? {
            let Some(path) = path_of_parent_key(e.key()) else {
                continue;
            };
            if e.content_len() > MAX_RECORD_BYTES {
                continue;
            }
            let author = e.author().to_string();
            if !roster.writes(&author) {
                continue;
            }
            if let Some(record) = self.read_record(e.content_hash()).await? {
                out.entry(path).or_default().records.push((author, record));
            }
        }
        for (author, retired) in roster.retired.iter().filter(|(a, _)| !roster.writes(a)) {
            for (path, v) in &retired.records {
                let (Some(path), Some(hash)) = (safe_rel_path(path), v.hash) else {
                    continue;
                };
                if let Some(record) = self.read_record(hash).await? {
                    let records = &mut out.entry(path).or_default().records;
                    records.push((author.clone(), record));
                }
            }
        }
        Ok(out)
    }

    async fn read_record(&self, hash: Hash) -> anyhow::Result<Option<ParentRecord>> {
        let Some(bytes) = self.read_blob(hash).await? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Every file entry from a current writer or owner, and every former one's the roster
    /// keeps, with its path.
    async fn file_versions(&self, roster: &Roster) -> anyhow::Result<Vec<(RelPath, Remote)>> {
        let query = Query::key_prefix(FILES_PREFIX).include_empty().build();
        let mut out = Vec::new();
        for e in self.core.entries(query).await? {
            let Some(path) = path_of_key(e.key()) else {
                continue;
            };
            if e.content_len() > MAX_FILE_BYTES {
                continue;
            }
            let author = e.author().to_string();
            let Some(member) = roster
                .member_by_author(&author)
                .filter(|m| m.role >= Role::Writer)
            else {
                continue;
            };
            let machine = member
                .machines
                .iter()
                .find(|m| m.author == author)
                .map(|m| m.name.clone())
                .unwrap_or_default();
            let hash = (e.content_hash() != Hash::EMPTY).then(|| e.content_hash());
            out.push((
                path,
                Remote {
                    hash,
                    at: e.timestamp(),
                    author,
                    machine,
                },
            ));
        }
        for (author, retired) in roster.retired.iter().filter(|(a, _)| !roster.writes(a)) {
            for (path, v) in &retired.files {
                let Some(path) = safe_rel_path(path) else {
                    continue;
                };
                out.push((
                    path,
                    Remote {
                        hash: v.hash.filter(|h| *h != Hash::EMPTY),
                        at: v.at,
                        author: author.clone(),
                        machine: retired.machine.clone(),
                    },
                ));
            }
        }
        Ok(out)
    }

    /// What a change taking `person` out of a role keeps of their work, as this machine holds
    /// it: their changes in the log and, while they are a writer, their machines' file entries
    /// and parent records.
    pub async fn kept(&self, person: &PersonId) -> anyhow::Result<Kept> {
        self.core.kept(person).await
    }

    /// What the machine whose author id is `author` wrote, as a change taking that one machine
    /// out lists it: its file entries and parent records, and no changes.
    pub async fn kept_machine(&self, author: &str) -> anyhow::Result<Kept> {
        let authors = self.core.kept_authors(&[author.to_string()]).await?;
        Ok(Kept {
            changes: Vec::new(),
            authors,
        })
    }

    /// Records what this machine's next entry for `p` is made from. Written before the entry
    /// itself.
    pub async fn put_parent(&self, p: &RelPath, record: &ParentRecord) -> anyhow::Result<()> {
        let core = &self.core;
        core.ensure_writable()?;
        let bytes = serde_json::to_vec(record)?;
        core.doc
            .set_bytes(core.author, parent_key(p), bytes)
            .await?;
        Ok(())
    }

    /// Stores `bytes` and points `p`'s key at them.
    pub async fn put_file(&self, p: &RelPath, bytes: Bytes) -> anyhow::Result<Hash> {
        let core = &self.core;
        core.ensure_writable()?;
        let size = bytes.len() as u64;
        let tag = core.blobs.add_bytes(bytes).await?;
        core.doc
            .set_hash(core.author, file_key(p), tag.hash, size)
            .await?;
        Ok(tag.hash)
    }

    /// Writes a tombstone for `p`.
    pub async fn delete_file(&self, p: &RelPath) -> anyhow::Result<()> {
        let core = &self.core;
        core.ensure_writable()?;
        core.doc.del(core.author, file_key(p)).await?;
        Ok(())
    }

    /// The content of `h`, if it is all here.
    pub async fn read_blob(&self, h: Hash) -> anyhow::Result<Option<Bytes>> {
        self.core.read_blob(h).await
    }

    /// Replaces who this node talks to on docs, blobs and gossip; ids that don't parse are
    /// skipped. From the moment this returns, nobody outside `endpoints` is dialled or accepted
    /// on them, and every open docs, blobs or gossip connection with someone no longer in the
    /// gate is closed, so nothing more reaches them through this node.
    pub fn set_gate(&self, endpoints: &[String]) {
        self.core.set_gate(endpoints);
    }

    /// Syncs the document with `endpoints`, and keeps them as live peers.
    pub async fn sync_with(&self, endpoints: &[String]) -> anyhow::Result<()> {
        let peers = endpoints
            .iter()
            .map(|e| e.parse::<EndpointId>().map(EndpointAddr::new))
            .collect::<Result<Vec<_>, _>>()
            .context("not an endpoint id")?;
        self.core.doc.start_sync(peers).await?;
        Ok(())
    }

    /// Where a peer can be reached when address lookup can't find it, as with relays off.
    pub fn remember_addr(&self, addr: EndpointAddr) {
        self.core.addrs.add_endpoint_info(addr);
    }

    /// This node's id with its bound sockets on loopback, for dialling it in tests.
    pub fn addr_for_tests(&self) -> EndpointAddr {
        let endpoint = &self.core.endpoint;
        endpoint
            .bound_sockets()
            .into_iter()
            .map(|s| {
                let ip = if s.is_ipv4() {
                    Ipv4Addr::LOCALHOST.into()
                } else {
                    Ipv6Addr::LOCALHOST.into()
                };
                SocketAddr::new(ip, s.port())
            })
            .fold(EndpointAddr::new(endpoint.id()), |a, s| a.with_ip_addr(s))
    }

    /// How many entries the change log holds, for tests.
    pub async fn log_len_for_tests(&self) -> anyhow::Result<usize> {
        let query = Query::key_prefix(LOG_PREFIX).build();
        Ok(self.core.entries(query).await?.len())
    }

    /// How many file entries the document holds, every author's and tombstones included, for
    /// tests.
    pub async fn file_entries_for_tests(&self) -> anyhow::Result<usize> {
        let query = Query::key_prefix(FILES_PREFIX).include_empty().build();
        Ok(self.core.entries(query).await?.len())
    }

    pub async fn shutdown(self) {
        let _ = self.core.doc.close().await;
        stop(self.router, self.core.blobs.clone()).await;
    }
}

impl Core {
    pub(crate) fn id(&self) -> String {
        self.doc.id().to_string()
    }

    pub(crate) fn can_write(&self) -> bool {
        self.writable.load(Ordering::Acquire)
    }

    pub(crate) async fn capability(&self, role: Role) -> anyhow::Result<Capability> {
        if role == Role::Reader {
            return Ok(Capability::Read(self.doc.id()));
        }
        if !self.can_write() {
            bail!("this machine can't hand out write access it doesn't have");
        }
        let ticket = self
            .doc
            .share(ShareMode::Write, AddrInfoOptions::Id)
            .await?;
        Ok(ticket.capability)
    }

    pub(crate) async fn roster(&self) -> anyhow::Result<Option<Roster>> {
        self.roster_with(None).await
    }

    /// The roster as it would be with `extra` in the log too.
    pub(crate) async fn roster_with(
        &self,
        extra: Option<&SignedChange>,
    ) -> anyhow::Result<Option<Roster>> {
        let mut changes = self.changes().await?;
        changes.extend(extra.cloned());
        Ok(Roster::fold(&self.id(), &self.anchor, &changes))
    }

    /// Every change in the log whose content is here.
    async fn changes(&self) -> anyhow::Result<Vec<SignedChange>> {
        let entries = self.entries(Query::key_prefix(LOG_PREFIX).build()).await?;
        let mut changes = Vec::new();
        for e in entries {
            if e.content_len() == 0 || e.content_len() > MAX_CHANGE_BYTES {
                continue;
            }
            let Some(bytes) = self.read_blob(e.content_hash()).await? else {
                continue;
            };
            if let Ok(c) = serde_json::from_slice::<SignedChange>(&bytes) {
                changes.push(c);
            }
        }
        Ok(changes)
    }

    pub(crate) async fn kept(&self, person: &PersonId) -> anyhow::Result<Kept> {
        let ns = self.id();
        let changes = self.changes().await?;
        let mut kept = Kept {
            changes: changes
                .iter()
                .filter(|c| c.by == *person && c.verify(&ns))
                .map(|c| c.sig.clone())
                .collect(),
            authors: Vec::new(),
        };
        let roster = Roster::fold(&ns, &self.anchor, &changes);
        let Some(member) = roster
            .as_ref()
            .and_then(|r| r.members.iter().find(|m| m.person == *person))
            .filter(|m| m.role >= Role::Writer)
        else {
            return Ok(kept);
        };
        let authors: Vec<String> = member.machines.iter().map(|m| m.author.clone()).collect();
        kept.authors = self.kept_authors(&authors).await?;
        Ok(kept)
    }

    /// The file entries and parent records each of `authors` wrote.
    pub(crate) async fn kept_authors(&self, authors: &[String]) -> anyhow::Result<Vec<KeptAuthor>> {
        let mut authors: BTreeMap<String, KeptAuthor> = authors
            .iter()
            .map(|a| {
                let author = KeptAuthor {
                    author: a.clone(),
                    files: Vec::new(),
                    records: Vec::new(),
                };
                (a.clone(), author)
            })
            .collect();
        let files = Query::key_prefix(FILES_PREFIX).include_empty().build();
        for e in self.entries(files).await? {
            let (Some(path), Some(author)) = (
                path_of_key(e.key()),
                authors.get_mut(&e.author().to_string()),
            ) else {
                continue;
            };
            if e.content_len() <= MAX_FILE_BYTES {
                let hash = (e.content_hash() != Hash::EMPTY).then(|| e.content_hash());
                author.files.push(kept_entry(&path, e.timestamp(), hash));
            }
        }
        for e in self
            .entries(Query::key_prefix(PARENTS_PREFIX).build())
            .await?
        {
            let (Some(path), Some(author)) = (
                path_of_parent_key(e.key()),
                authors.get_mut(&e.author().to_string()),
            ) else {
                continue;
            };
            if e.content_len() <= MAX_RECORD_BYTES {
                let hash = Some(e.content_hash());
                author.records.push(kept_entry(&path, e.timestamp(), hash));
            }
        }
        Ok(authors.into_values().collect())
    }

    pub(crate) async fn write_change(&self, c: &SignedChange) -> anyhow::Result<()> {
        self.ensure_writable()?;
        let bytes = serde_json::to_vec(c)?;
        self.doc.set_bytes(self.author, log_key(c), bytes).await?;
        Ok(())
    }

    pub(crate) async fn read_blob(&self, h: Hash) -> anyhow::Result<Option<Bytes>> {
        if !self.blobs.has(h).await? {
            return Ok(None);
        }
        Ok(Some(self.blobs.get_bytes(h).await?))
    }

    pub(crate) fn set_gate(&self, endpoints: &[String]) {
        let allowed: HashSet<EndpointId> =
            endpoints.iter().filter_map(|e| e.parse().ok()).collect();
        let dropped: Vec<WeakConnectionHandle> = {
            let mut gate = lock(&self.gate);
            gate.allowed = allowed;
            let GateState { allowed, open } = &mut *gate;
            let gone: Vec<EndpointId> = open
                .keys()
                .filter(|id| !allowed.contains(*id))
                .copied()
                .collect();
            gone.iter()
                .filter_map(|id| open.remove(id))
                .flatten()
                .collect()
        };
        for c in dropped {
            if let Some(conn) = c.upgrade() {
                conn.close(1u32.into(), b"not a member of this swarm");
            }
        }
    }

    pub(crate) async fn refresh_gate_from_roster(&self) -> anyhow::Result<()> {
        if let Some(r) = self.roster().await? {
            self.set_gate(&r.endpoints());
        }
        Ok(())
    }

    fn ensure_writable(&self) -> anyhow::Result<()> {
        if !self.can_write() {
            bail!("this machine can only read this swarm");
        }
        Ok(())
    }

    async fn entries(&self, query: Query) -> anyhow::Result<Vec<Entry>> {
        self.doc.get_many(query).await?.try_collect().await
    }
}

fn kept_entry(path: &RelPath, at: u64, hash: Option<Hash>) -> KeptEntry {
    KeptEntry {
        path: path.as_str().to_string(),
        version: Version { at, hash },
    }
}

/// A new swarm's anchor is its creator.
fn creator(cfg: &NodeConfig) -> anyhow::Result<PersonId> {
    let me = cfg.person.id();
    match cfg.anchor {
        Some(a) if a != me => bail!("a new swarm's anchor is its creator"),
        _ => Ok(me),
    }
}

/// A joiner learns the anchor from whoever invited it.
fn given_anchor(cfg: &NodeConfig) -> anyhow::Result<PersonId> {
    cfg.anchor.context("joining a swarm needs its anchor")
}

/// The anchor saved beside the stores, else the config's.
fn saved_anchor(cfg: &NodeConfig) -> anyhow::Result<PersonId> {
    match std::fs::read_to_string(anchor_file(cfg)) {
        Ok(s) => s
            .trim()
            .parse()
            .map_err(|e: String| anyhow::anyhow!("the saved anchor is damaged: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => given_anchor(cfg),
        Err(e) => Err(e.into()),
    }
}

/// Shuts the router (and with it docs, gossip and the endpoint) and then the blob store.
async fn stop(router: Router, blobs: FsStore) {
    let _ = router.shutdown().await;
    let _ = blobs.shutdown().await;
}
