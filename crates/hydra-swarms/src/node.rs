//! One swarm on the network: an iroh endpoint and router running iroh-docs, iroh-blobs and
//! iroh-gossip, behind a gate that refuses machines outside the roster, and the swarm document's
//! files and signed change log.
use std::collections::{BTreeMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, bail};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Accepting, Connection, PortmapperConfig, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
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
use tokio::sync::broadcast;

use crate::decide::Remote;
use crate::ids::{PersonId, PersonKey};
use crate::paths::{
    FILES_PREFIX, LOG_PREFIX, MAX_FILE_BYTES, RelPath, file_key, log_key, path_of_key,
};
use crate::roster::{Change, Machine, Member, Role, Roster, SignedChange};

/// A signed change is a few hundred bytes; anything far bigger in the log is not one.
const MAX_CHANGE_BYTES: u64 = 64 * 1024;

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
}

/// Endpoint ids allowed to connect on docs, blobs and gossip.
type GateSet = Arc<RwLock<HashSet<EndpointId>>>;

/// A protocol handler that refuses any connection from an endpoint outside the gate.
#[derive(Debug, Clone)]
struct Gated<P> {
    inner: P,
    gate: GateSet,
}

impl<P: ProtocolHandler> ProtocolHandler for Gated<P> {
    async fn on_accepting(&self, accepting: Accepting) -> Result<Connection, AcceptError> {
        let conn = self.inner.on_accepting(accepting).await?;
        let allowed = self
            .gate
            .read()
            .expect("the gate lock is never poisoned")
            .contains(&conn.remote_id());
        if !allowed {
            conn.close(1u32.into(), b"not a member of this swarm");
            return Err(AcceptError::from_err(std::io::Error::other(
                "not a member of this swarm",
            )));
        }
        Ok(conn)
    }

    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        self.inner.accept(conn).await
    }

    async fn shutdown(&self) {
        self.inner.shutdown().await
    }
}

fn gated<P>(inner: P, gate: &GateSet) -> Gated<P> {
    Gated {
        inner,
        gate: Arc::clone(gate),
    }
}

pub struct SwarmNode {
    endpoint: Endpoint,
    router: Router,
    blobs: FsStore,
    doc: Doc,
    author: AuthorId,
    anchor: PersonId,
    person: PersonKey,
    writable: bool,
    gate: GateSet,
    addrs: MemoryLookup,
    events: broadcast::Sender<NodeEvent>,
}

fn endpoint_builder(machine_key: &[u8; 32], relays: bool) -> iroh::endpoint::Builder {
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
    gate: GateSet,
    addrs: MemoryLookup,
}

async fn start(cfg: &NodeConfig) -> anyhow::Result<Parts> {
    let docs_dir = cfg.dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("creating {}", docs_dir.display()))?;
    let blobs = FsStore::load(cfg.dir.join("blobs")).await?;
    let addrs = MemoryLookup::new();
    let endpoint = endpoint_builder(&cfg.machine_key, cfg.relays)
        .address_lookup(addrs.clone())
        .bind()
        .await?;
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let docs = Docs::persistent(docs_dir)
        .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
        .await?;
    let gate: GateSet = Arc::default();
    let router = Router::builder(endpoint.clone())
        .accept(
            iroh_blobs::ALPN,
            gated(BlobsProtocol::new(&blobs, None), &gate),
        )
        .accept(iroh_gossip::ALPN, gated(gossip, &gate))
        .accept(iroh_docs::ALPN, gated(docs.clone(), &gate))
        .spawn();
    Ok(Parts {
        endpoint,
        router,
        docs,
        blobs,
        gate,
        addrs,
    })
}

fn anchor_file(cfg: &NodeConfig) -> PathBuf {
    cfg.dir.join("anchor")
}

fn now_micros() -> u64 {
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
            &node.person,
            now_micros(),
        );
        let written = async {
            node.write_change(&genesis).await?;
            node.refresh_gate_from_roster().await
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
        let node = SwarmNode::finish(cfg, parts, doc, author, saved_anchor).await?;
        node.refresh_gate_from_roster().await?;
        Ok(node)
    }

    /// Joins with the capability an inviter handed over, writing as `author`, which the joiner
    /// made beforehand so the inviter could add it to the roster.
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

    /// Settles the anchor, starts live sync on the document and forwards its events.
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
            doc.start_sync(vec![]).await?;
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
        Ok(SwarmNode {
            endpoint: parts.endpoint,
            router: parts.router,
            blobs: parts.blobs,
            doc,
            author,
            anchor,
            person: cfg.person,
            writable,
            gate: parts.gate,
            addrs: parts.addrs,
            events,
        })
    }

    /// The namespace id.
    pub fn id(&self) -> String {
        self.doc.id().to_string()
    }

    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// This machine's author id in the document.
    pub fn author(&self) -> String {
        self.author.to_string()
    }

    /// Whether this machine holds the write capability.
    pub fn can_write(&self) -> bool {
        self.writable
    }

    pub fn events(&self) -> broadcast::Receiver<NodeEvent> {
        self.events.subscribe()
    }

    /// The capability to hand to someone joining with `role`: readers get only the read one.
    pub async fn capability(&self, role: Role) -> anyhow::Result<Capability> {
        if role == Role::Reader {
            return Ok(Capability::Read(self.doc.id()));
        }
        if !self.writable {
            bail!("this machine can't hand out write access it doesn't have");
        }
        let ticket = self
            .doc
            .share(ShareMode::Write, AddrInfoOptions::Id)
            .await?;
        Ok(ticket.capability)
    }

    /// The roster folded from every signed change whose content is here, or None until the
    /// anchor's genesis has arrived.
    pub async fn roster(&self) -> anyhow::Result<Option<Roster>> {
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
        Ok(Roster::fold(&self.id(), &self.anchor, &changes))
    }

    /// Adds a signed change to the log.
    pub async fn write_change(&self, c: &SignedChange) -> anyhow::Result<()> {
        self.ensure_writable()?;
        let bytes = serde_json::to_vec(c)?;
        self.doc.set_bytes(self.author, log_key(c), bytes).await?;
        Ok(())
    }

    /// Per file key, the newest entry (tombstones included) whose author is one of `roster`'s
    /// current writers or owners. Keys that aren't safe paths and entries over the size cap are
    /// left out.
    pub async fn files(&self, roster: &Roster) -> anyhow::Result<BTreeMap<RelPath, Remote>> {
        let query = Query::key_prefix(FILES_PREFIX).include_empty().build();
        let mut out: BTreeMap<RelPath, Remote> = BTreeMap::new();
        for e in self.entries(query).await? {
            let Some(path) = path_of_key(e.key()) else {
                continue;
            };
            if e.content_len() > MAX_FILE_BYTES {
                continue;
            }
            let author = e.author().to_string();
            let Some(member) = roster.member_by_author(&author) else {
                continue;
            };
            if member.role < Role::Writer {
                continue;
            }
            let newer = out
                .get(&path)
                .is_none_or(|r| (e.timestamp(), &author) > (r.at, &r.author));
            if !newer {
                continue;
            }
            let machine = member
                .machines
                .iter()
                .find(|m| m.author == author)
                .map(|m| m.name.clone())
                .unwrap_or_default();
            let hash = (e.content_hash() != Hash::EMPTY).then(|| e.content_hash());
            out.insert(
                path,
                Remote {
                    hash,
                    at: e.timestamp(),
                    author,
                    machine,
                },
            );
        }
        Ok(out)
    }

    /// Stores `bytes` and points `p`'s key at them.
    pub async fn put_file(&self, p: &RelPath, bytes: Bytes) -> anyhow::Result<Hash> {
        self.ensure_writable()?;
        let size = bytes.len() as u64;
        let tag = self.blobs.add_bytes(bytes).await?;
        self.doc
            .set_hash(self.author, file_key(p), tag.hash, size)
            .await?;
        Ok(tag.hash)
    }

    /// Writes a tombstone for `p`.
    pub async fn delete_file(&self, p: &RelPath) -> anyhow::Result<()> {
        self.ensure_writable()?;
        self.doc.del(self.author, file_key(p)).await?;
        Ok(())
    }

    /// The content of `h`, if it is all here.
    pub async fn read_blob(&self, h: Hash) -> anyhow::Result<Option<Bytes>> {
        if !self.blobs.has(h).await? {
            return Ok(None);
        }
        Ok(Some(self.blobs.get_bytes(h).await?))
    }

    /// Replaces who may connect on docs, blobs and gossip. Ids that don't parse are skipped.
    pub fn set_gate(&self, endpoints: &[String]) {
        let set = endpoints.iter().filter_map(|e| e.parse().ok()).collect();
        *self.gate.write().expect("the gate lock is never poisoned") = set;
    }

    /// Syncs the document with `endpoints`, and keeps them as live peers.
    pub async fn sync_with(&self, endpoints: &[String]) -> anyhow::Result<()> {
        let peers = endpoints
            .iter()
            .map(|e| e.parse::<EndpointId>().map(EndpointAddr::new))
            .collect::<Result<Vec<_>, _>>()
            .context("not an endpoint id")?;
        self.doc.start_sync(peers).await?;
        Ok(())
    }

    /// Where a peer can be reached when address lookup can't find it, as with relays off.
    pub fn remember_addr(&self, addr: EndpointAddr) {
        self.addrs.add_endpoint_info(addr);
    }

    /// This node's id with its bound sockets on loopback, for dialling it in tests.
    pub fn addr_for_tests(&self) -> EndpointAddr {
        self.endpoint
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
            .fold(EndpointAddr::new(self.endpoint.id()), |a, s| {
                a.with_ip_addr(s)
            })
    }

    pub async fn shutdown(self) {
        let _ = self.doc.close().await;
        stop(self.router, self.blobs).await;
    }

    async fn refresh_gate_from_roster(&self) -> anyhow::Result<()> {
        if let Some(r) = self.roster().await? {
            self.set_gate(&r.endpoints());
        }
        Ok(())
    }

    fn ensure_writable(&self) -> anyhow::Result<()> {
        if !self.writable {
            bail!("this machine can only read this swarm");
        }
        Ok(())
    }

    async fn entries(&self, query: Query) -> anyhow::Result<Vec<Entry>> {
        self.doc.get_many(query).await?.try_collect().await
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
