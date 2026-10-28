//! Every swarm on this machine, run on one tokio runtime: the registry, the commands hydraui
//! sends, the membership actions, and the views and notices it shows.
//!
//! The service's main task owns the list of swarms and takes commands. Each swarm has a task of
//! its own that owns its node and folder, so everything about one swarm (its commands, its
//! reconciles, its roster checks) happens one thing at a time, while swarms don't wait on each
//! other. Messages to other machines run beside a swarm's task, so a slow machine never holds
//! it up.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, bail};
use futures::StreamExt;
use futures::channel::{mpsc as fmpsc, oneshot};
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_docs::{Author, Capability, NamespaceId, NamespaceSecret};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::folder::{Attention, Folder, FolderEvent};
use crate::ids::{PersonId, PersonKey, to_hex};
use crate::invites::{Invite, InviteCode, NOT_THEIR_MACHINE, Refused, redeem, redeem_at};
use crate::mine::DEVICE_KEY_ID;
use crate::node::{NodeConfig, NodeEvent, SwarmNode, now_micros};
use crate::paths::{RelPath, safe_rel_path};
use crate::roster::{Change, Kept, Machine, Member, Role, Roster, SignedChange};
use crate::wire::{MineReply, Offer};

/// How often every swarm reconciles its folder, and retries what it couldn't send.
const PERIODIC: Duration = Duration::from_secs(60);
/// The quiet time after a document change before the folder is reconciled.
const SETTLE: Duration = Duration::from_millis(250);
/// The least time between two pushes of the swarm views.
const VIEWS_EVERY: Duration = Duration::from_millis(250);
/// How long after a member machine comes up or goes down it is synced with again.
const RESYNC_AFTER: Duration = Duration::from_secs(1);
/// How often a leaving machine asks the owners again until one confirms its leaving.
const LEAVE_RETRY: Duration = Duration::from_secs(5);
/// The first wait before another change no owner took is sent again; it doubles on each miss,
/// up to `PERIODIC`.
const RELAY_RETRY: Duration = Duration::from_secs(5);
/// How long a machine of yours waits for the roster to show another one joining through it.
const JOINING_WAIT: Duration = Duration::from_secs(20);
const REGISTRY_FILE: &str = "registry.json";
const SEEN_FILE: &str = "seen.json";
const OWED_FILE: &str = "owed.json";
const OUTBOX_FILE: &str = "outbox.json";

/// Who this machine's person is, and what they and this machine are called.
#[derive(Clone)]
pub struct Identity {
    pub person: [u8; 32],
    pub name: String,
    pub machine_name: String,
}

/// Gives a swarm's machine key for a key id, making it the first time.
pub type SwarmKeyFn = Box<dyn Fn(&str) -> Result<[u8; 32], String> + Send>;

/// Keeps a new personal key, in place of the one before.
pub type PersonKeyFn = Box<dyn Fn([u8; 32]) -> Result<(), String> + Send>;

mod linking;

pub struct ServiceConfig {
    /// `state_root/swarms`.
    pub root: PathBuf,
    pub identity: Identity,
    /// The keyring in hydraui, in memory in tests. A swarm made here asks for its namespace id;
    /// a swarm joined here asks for an id of its own, `join-<16 hex>`, made before the namespace
    /// is known (the joining machine's key is part of the request to join), and the registry
    /// remembers it.
    pub swarm_key: SwarmKeyFn,
    /// The keyring in hydraui, for a personal key this machine is given or makes.
    pub set_person: PersonKeyFn,
    pub relays: bool,
    direct_peers: Option<DirectPeers>,
    lose_relays: Duration,
}

impl ServiceConfig {
    pub fn new(
        root: PathBuf,
        identity: Identity,
        swarm_key: SwarmKeyFn,
        set_person: PersonKeyFn,
        relays: bool,
    ) -> Self {
        ServiceConfig {
            root,
            identity,
            swarm_key,
            set_person,
            relays,
            direct_peers: None,
            lose_relays: Duration::ZERO,
        }
    }

    /// Finds peers through `book` rather than address lookup, for tests with relays off: every
    /// node the service opens is added to it, and dials go to the addresses in it.
    /// Treats every owner's answer to a relay in the first `d` after starting as lost, though the
    /// owner acted on the relay, for tests.
    pub fn lose_relay_answers_for_tests(mut self, d: Duration) -> Self {
        self.lose_relays = d;
        self
    }

    pub fn direct_peers_for_tests(mut self, book: &DirectPeers) -> Self {
        self.direct_peers = Some(book.clone());
        self
    }
}

/// Where each test machine's nodes listen on loopback, shared by the services in a test.
#[derive(Clone, Default)]
pub struct DirectPeers(Arc<Mutex<HashMap<String, EndpointAddr>>>);

impl DirectPeers {
    pub fn new() -> DirectPeers {
        DirectPeers::default()
    }

    fn add(&self, addr: EndpointAddr) {
        lock(&self.0).insert(addr.id.to_string(), addr);
    }

    /// Where the node with endpoint id `id` listens, if it is in the book.
    pub fn get(&self, id: &str) -> Option<EndpointAddr> {
        lock(&self.0).get(id).cloned()
    }

    fn all(&self) -> Vec<EndpointAddr> {
        lock(&self.0).values().cloned().collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmView {
    /// The namespace id.
    pub id: String,
    pub name: String,
    pub folder: PathBuf,
    pub my_role: Role,
    pub members: Vec<MemberView>,
    pub types: Vec<String>,
    pub pending_invites: Vec<Invite>,
    pub attention: Vec<Attention>,
    /// This machine and the member machines connected now.
    pub online: usize,
    pub machines: usize,
    pub syncing: bool,
    /// The machine that made the newest change applied here, and when (Unix seconds).
    pub last_change: Option<(String, u64)>,
    pub removed: bool,
    /// How many other machines of yours are in this swarm.
    pub my_other_machines: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberView {
    pub person: String,
    pub name: String,
    pub role: Role,
    /// An owner junior to this machine's person, whom they may demote or remove.
    pub junior: bool,
    pub machines: Vec<MachineView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineView {
    pub name: String,
    pub online: bool,
    /// When it was last connected, in Unix seconds, as this machine saw it.
    pub last_seen: Option<u64>,
    pub this_machine: bool,
}

/// Your machines, as this one sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachinesView {
    /// This machine's device id.
    pub this: String,
    pub devices: Vec<DeviceView>,
    /// The link codes this machine has made that can still be used.
    pub links: Vec<Invite<()>>,
    /// The swarms offered to this machine.
    pub offers: Vec<Offer>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    /// Heard from in the last minute (this machine always is).
    pub online: bool,
    pub this_machine: bool,
}

#[derive(Debug)]
pub enum Command {
    Create {
        name: String,
        folder: PathBuf,
    },
    Join {
        code: String,
        folder: PathBuf,
    },
    /// Replies with the code.
    Invite {
        swarm: String,
        role: Role,
        reply: oneshot::Sender<Result<String, String>>,
    },
    CancelInvite {
        swarm: String,
        id: String,
    },
    SetRole {
        swarm: String,
        person: String,
        role: Role,
    },
    Remove {
        swarm: String,
        person: String,
    },
    RenameSwarm {
        swarm: String,
        name: String,
    },
    SetTypes {
        swarm: String,
        types: Vec<String>,
    },
    /// Moves nothing: the swarm syncs into the new folder, which must be empty or new.
    SetFolder {
        swarm: String,
        folder: PathBuf,
    },
    /// Leaves the swarm, keeping its files; for a swarm this machine was removed from, forgets
    /// it.
    Leave {
        swarm: String,
    },
    /// Updates every swarm, through an owner where this person is a reader.
    SetIdentity {
        name: String,
        machine_name: String,
    },
    Resolve {
        swarm: String,
        action: Resolution,
    },
    /// Replies with a code another machine of yours can link with.
    LinkCode {
        reply: oneshot::Sender<Result<String, String>>,
    },
    CancelLink {
        id: String,
    },
    /// Links this machine to the one that made `code`, taking its person.
    Link {
        code: String,
    },
    /// Removes one of your machines, by device id.
    RemoveMachine {
        device: String,
    },
    /// Joins the swarm offered to this machine into `folder`, which must be empty or new.
    AcceptOffer {
        swarm: String,
        folder: PathBuf,
    },
    /// Says no to the swarm offered to this machine, for good.
    DeclineOffer {
        swarm: String,
    },
    Stop,
}

impl Command {
    fn swarm(&self) -> Option<&str> {
        match self {
            Command::Invite { swarm, .. }
            | Command::CancelInvite { swarm, .. }
            | Command::SetRole { swarm, .. }
            | Command::Remove { swarm, .. }
            | Command::RenameSwarm { swarm, .. }
            | Command::SetTypes { swarm, .. }
            | Command::SetFolder { swarm, .. }
            | Command::Leave { swarm }
            | Command::Resolve { swarm, .. } => Some(swarm),
            Command::Create { .. }
            | Command::Join { .. }
            | Command::SetIdentity { .. }
            | Command::LinkCode { .. }
            | Command::CancelLink { .. }
            | Command::Link { .. }
            | Command::RemoveMachine { .. }
            | Command::AcceptOffer { .. }
            | Command::DeclineOffer { .. }
            | Command::Stop => None,
        }
    }
}

/// How to settle an item under Needs attention. Paths are as the swarm spells them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    KeepTheirs { path: String, copy: String },
    KeepMine { path: String, copy: String },
    ShareAgain(String),
    DeleteHeld(String),
    UseSwarmVersion(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceEvent {
    /// Every swarm's view, sorted by name, whenever anything in them changes.
    Swarms(Vec<SwarmView>),
    /// `swarm` is the namespace id.
    Toast {
        swarm: String,
        text: String,
    },
    Failed(String),
    /// Your machines changed.
    Machines(MachinesView),
    /// Another machine of yours set your name; hydraui keeps it.
    Named {
        name: String,
    },
}

pub struct SwarmsService;

impl SwarmsService {
    /// Starts a tokio runtime on a thread named `hydra-swarms`, opens every swarm in
    /// `root/registry.json`, and returns the command sender and the event receiver. The event
    /// stream ends once `Stop` (or dropping the sender) has closed every swarm.
    #[allow(clippy::type_complexity)]
    pub fn start(
        cfg: ServiceConfig,
    ) -> io::Result<(
        fmpsc::UnboundedSender<Command>,
        fmpsc::UnboundedReceiver<ServiceEvent>,
    )> {
        let (cmd_tx, cmd_rx) = fmpsc::unbounded();
        let (ev_tx, ev_rx) = fmpsc::unbounded();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("hydra-swarms")
            .enable_all()
            .build()?;
        // The disk is touched only on the service's own thread, never the caller's.
        std::thread::Builder::new()
            .name("hydra-swarms".into())
            .spawn(move || {
                if let Err(e) = fs::create_dir_all(&cfg.root) {
                    let text = format!("Swarms can't start: {}: {e}", cfg.root.display());
                    let _ = ev_tx.unbounded_send(ServiceEvent::Failed(text));
                    return;
                }
                let (registry, damaged) = Registry::load(&cfg.root);
                let shared = Arc::new(Shared {
                    root: cfg.root,
                    relays: cfg.relays,
                    direct: cfg.direct_peers,
                    lose_relays_until: Instant::now() + cfg.lose_relays,
                    events: ev_tx,
                    registry,
                    views: Mutex::default(),
                    dirty: Notify::new(),
                    swarm_key: Mutex::new(cfg.swarm_key),
                });
                if let Some(text) = damaged {
                    shared.failed(text);
                }
                rt.block_on(run(shared, cfg.identity, cfg.set_person, cmd_rx));
            })?;
        Ok((cmd_tx, ev_rx))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn random32() -> anyhow::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("random bytes: {e}"))?;
    Ok(b)
}

fn now_secs() -> u64 {
    now_micros() / 1_000_000
}

/// `bytes` written to `path` through a temp file and a rename.
fn save_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// What `path` holds, or the default when it is missing or damaged.
fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// One swarm in `registry.json`.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Entry {
    ns: String,
    anchor: String,
    folder: PathBuf,
    #[serde(default)]
    removed: bool,
    /// The swarm's name as last seen, for showing it once removed.
    #[serde(default)]
    name: String,
    /// The id this machine's key for the swarm is kept under, when it isn't `ns`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    /// This machine left, and is still making sure the others hear about it.
    #[serde(default)]
    leaving: bool,
}

impl Entry {
    fn key_id(&self) -> &str {
        self.key.as_deref().unwrap_or(&self.ns)
    }
}

struct Registry {
    path: PathBuf,
    entries: Mutex<Vec<Entry>>,
}

impl Registry {
    /// The registry under `root`. A damaged one is moved aside, so nothing overwrites it, and
    /// the text says so.
    fn load(root: &Path) -> (Registry, Option<String>) {
        let path = root.join(REGISTRY_FILE);
        let (entries, damaged) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(entries) => (entries, None),
                Err(e) => {
                    let aside = path.with_extension("json.damaged");
                    let _ = fs::rename(&path, &aside);
                    let text = format!(
                        "The list of swarms was damaged ({e}), so it was set aside as {}.",
                        aside.display()
                    );
                    (Vec::new(), Some(text))
                }
            },
            Err(_) => (Vec::new(), None),
        };
        let registry = Registry {
            path,
            entries: Mutex::new(entries),
        };
        (registry, damaged)
    }

    fn entries(&self) -> Vec<Entry> {
        lock(&self.entries).clone()
    }

    fn contains(&self, ns: &str) -> bool {
        lock(&self.entries).iter().any(|e| e.ns == ns)
    }

    fn change(&self, f: impl FnOnce(&mut Vec<Entry>)) -> anyhow::Result<()> {
        let mut entries = lock(&self.entries);
        f(&mut entries);
        save_json(&self.path, &*entries)
    }

    fn add(&self, entry: Entry) -> anyhow::Result<()> {
        self.change(|all| {
            all.retain(|e| e.ns != entry.ns);
            all.push(entry);
        })
    }

    fn update(&self, ns: &str, f: impl FnOnce(&mut Entry)) -> anyhow::Result<()> {
        self.change(|all| {
            if let Some(e) = all.iter_mut().find(|e| e.ns == ns) {
                f(e);
            }
        })
    }

    fn remove(&self, ns: &str) -> anyhow::Result<()> {
        self.change(|all| all.retain(|e| e.ns != ns))
    }
}

/// What the service's tasks share.
struct Shared {
    root: PathBuf,
    relays: bool,
    direct: Option<DirectPeers>,
    /// Until when owners' answers to relays are treated as lost (tests only).
    lose_relays_until: Instant,
    events: fmpsc::UnboundedSender<ServiceEvent>,
    registry: Registry,
    views: Mutex<BTreeMap<String, SwarmView>>,
    /// Set when a view changes; the main task pushes them all, at most every `VIEWS_EVERY`.
    dirty: Notify,
    swarm_key: Mutex<SwarmKeyFn>,
}

impl Shared {
    fn emit(&self, ev: ServiceEvent) {
        // Nobody listening is fine.
        let _ = self.events.unbounded_send(ev);
    }

    fn failed(&self, text: impl Into<String>) {
        self.emit(ServiceEvent::Failed(text.into()));
    }

    fn toast(&self, ns: &str, text: String) {
        self.emit(ServiceEvent::Toast {
            swarm: ns.to_string(),
            text,
        });
    }

    fn set_view(&self, view: SwarmView) {
        let mut views = lock(&self.views);
        if views.get(&view.id) != Some(&view) {
            views.insert(view.id.clone(), view);
            self.dirty.notify_one();
        }
    }

    fn drop_view(&self, ns: &str) {
        if lock(&self.views).remove(ns).is_some() {
            self.dirty.notify_one();
        }
    }

    fn views(&self) -> Vec<SwarmView> {
        let mut all: Vec<SwarmView> = lock(&self.views).values().cloned().collect();
        all.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
        all
    }

    async fn machine_key(self: &Arc<Self>, id: &str) -> anyhow::Result<[u8; 32]> {
        let shared = Arc::clone(self);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || (lock(&shared.swarm_key))(&id))
            .await?
            .map_err(|e| anyhow::anyhow!("this machine's key for the swarm: {e}"))
    }

    /// Whether to treat an owner's answer to a relay as lost, for tests.
    fn lose_relay(&self) -> bool {
        Instant::now() < self.lose_relays_until
    }

    /// Lets `node` reach every machine in the test book; nothing without one.
    fn remember_peers(&self, node: &SwarmNode) {
        if let Some(book) = &self.direct {
            for addr in book.all() {
                node.remember_addr(addr);
            }
        }
    }

    /// Puts `node` in the test book.
    fn announce(&self, node: &SwarmNode) {
        if let Some(book) = &self.direct {
            book.add(node.addr_for_tests());
        }
    }

    fn node_config(&self, ns: &str, machine_key: [u8; 32], person: [u8; 32]) -> NodeConfig {
        NodeConfig {
            dir: self.root.join(ns),
            machine_key,
            relays: self.relays,
            person: PersonKey::from_bytes(person),
            anchor: None,
        }
    }
}

/// A swarm ready for its task.
struct Opened {
    entry: Entry,
    node: Option<SwarmNode>,
    folder: Option<Folder>,
}

struct Handle {
    tx: mpsc::UnboundedSender<Msg>,
    task: JoinHandle<()>,
}

enum Msg {
    Command(Command),
    Identity(Identity),
    /// Another machine of yours joining: this person's `AddMachine` for it, and where to answer.
    Joining(SignedChange, oneshot::Sender<MineReply>),
    Stop,
}

/// The service's main task: takes commands, starts each swarm's task, and pushes the views.
async fn run(
    shared: Arc<Shared>,
    mut identity: Identity,
    set_person: PersonKeyFn,
    mut commands: fmpsc::UnboundedReceiver<Command>,
) {
    let (up_tx, mut up) = mpsc::unbounded_channel();
    let machines = match start_machines(&shared, &identity, set_person, up_tx).await {
        Ok(m) => Some(m),
        Err(e) => {
            shared.failed(format!(
                "Your other machines can't be reached from here: {e:#}"
            ));
            None
        }
    };
    if let Some(m) = &machines {
        let name = m.name();
        if name != identity.name {
            identity.name = name.clone();
            shared.emit(ServiceEvent::Named { name });
        }
        m.publish();
        tokio::spawn(Arc::clone(m).sync_all());
    }
    let mut tick = tokio::time::interval_at(Instant::now() + PERIODIC, PERIODIC);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut swarms: HashMap<String, Handle> = HashMap::new();
    let mut opening: JoinSet<Option<Opened>> = JoinSet::new();
    for entry in shared.registry.entries() {
        opening.spawn(open_existing(Arc::clone(&shared), identity.clone(), entry));
    }
    // The first push says what there is, even when there is nothing.
    shared.dirty.notify_one();
    let mut last_push: Option<Instant> = None;
    let mut push_at: Option<Instant> = None;
    let mut offered: HashSet<String> = HashSet::new();
    loop {
        tokio::select! {
            cmd = commands.next() => match cmd {
                None | Some(Command::Stop) => break,
                Some(Command::Create { name, folder }) => {
                    opening.spawn(create(Arc::clone(&shared), identity.clone(), name, folder));
                }
                Some(Command::Join { code, folder }) => {
                    opening.spawn(join(Arc::clone(&shared), identity.clone(), code, folder));
                }
                Some(Command::SetIdentity { name, machine_name }) => {
                    if let Some(m) = &machines {
                        m.set_identity(&name, &machine_name);
                        tokio::spawn(Arc::clone(m).sync_all());
                    }
                    identity.name = name;
                    identity.machine_name = machine_name;
                    for h in swarms.values() {
                        let _ = h.tx.send(Msg::Identity(identity.clone()));
                    }
                }
                Some(Command::LinkCode { reply }) => {
                    let code = match &machines {
                        Some(m) => m.link_code(),
                        None => Err(NO_DEVICE.to_string()),
                    };
                    let _ = reply.send(code);
                }
                Some(Command::CancelLink { id }) => {
                    if let Some(m) = &machines {
                        m.cancel_link(&id);
                    }
                }
                Some(Command::Link { code }) => match &machines {
                    Some(m) => {
                        tokio::spawn(Arc::clone(m).link(code, identity.machine_name.clone()));
                    }
                    None => shared.failed(NO_DEVICE),
                },
                Some(Command::RemoveMachine { .. }) => shared.failed("Not yet."),
                Some(Command::AcceptOffer { swarm, folder }) => match &machines {
                    Some(m) => {
                        let accept = Arc::clone(m).accept(identity.clone(), swarm, folder);
                        opening.spawn(accept);
                    }
                    None => shared.failed(NO_DEVICE),
                },
                Some(Command::DeclineOffer { swarm }) => {
                    if let Some(m) = &machines {
                        m.decline(&swarm);
                    }
                }
                Some(cmd) => to_swarm(&shared, &mut swarms, cmd),
            },
            Some(up) = up.recv() => match up {
                linking::Up::Named(name) => {
                    identity.name = name.clone();
                    for h in swarms.values() {
                        let _ = h.tx.send(Msg::Identity(identity.clone()));
                    }
                    shared.emit(ServiceEvent::Named { name });
                }
                linking::Up::Person(person) => identity.person = person,
                linking::Up::Joining { ns, change, reply } => match swarms.get(&ns) {
                    Some(h) => {
                        if let Err(mpsc::error::SendError(Msg::Joining(_, reply))) =
                            h.tx.send(Msg::Joining(*change, reply))
                        {
                            let _ = reply.send(linking::not_here());
                        }
                    }
                    None => {
                        let _ = reply.send(linking::not_here());
                    }
                },
            },
            _ = tick.tick() => {
                if let Some(m) = &machines {
                    tokio::spawn(Arc::clone(m).sync_all());
                }
            }
            Some(done) = opening.join_next(), if !opening.is_empty() => {
                if let Ok(Some(opened)) = done {
                    let ns = opened.entry.ns.clone();
                    let (tx, rx) = mpsc::unbounded_channel();
                    let worker = Worker::new(Arc::clone(&shared), identity.clone(), opened);
                    let task = tokio::spawn(worker.run(rx));
                    swarms.insert(ns, Handle { tx, task });
                }
            }
            () = shared.dirty.notified() => {
                if push_at.is_none() {
                    let earliest = last_push.map_or_else(Instant::now, |t| t + VIEWS_EVERY);
                    push_at = Some(earliest.max(Instant::now()));
                }
            }
            () = sleep_until(push_at) => {
                push_at = None;
                last_push = Some(Instant::now());
                let views = shared.views();
                // A swarm showing for the first time is offered to your other machines; its
                // view is what the offer is made from.
                let shown: HashSet<String> = views.iter().map(|v| v.id.clone()).collect();
                if let Some(m) = &machines
                    && !shown.is_subset(&offered)
                {
                    tokio::spawn(Arc::clone(m).sync_all());
                }
                offered = shown;
                shared.emit(ServiceEvent::Swarms(views));
            }
        }
    }
    // Swarms still being made or joined are let finish, then closed with the rest.
    while let Some(done) = opening.join_next().await {
        if let Ok(Some(Opened {
            node: Some(node), ..
        })) = done
        {
            node.shutdown().await;
        }
    }
    for h in swarms.values() {
        let _ = h.tx.send(Msg::Stop);
    }
    for (_, h) in swarms {
        let _ = h.task.await;
    }
    if let Some(m) = machines {
        m.shutdown().await;
    }
}

/// Why nothing to do with your other machines works here.
const NO_DEVICE: &str = "Your other machines can't be reached from here.";

async fn start_machines(
    shared: &Arc<Shared>,
    identity: &Identity,
    set_person: PersonKeyFn,
    up: mpsc::UnboundedSender<linking::Up>,
) -> anyhow::Result<Arc<linking::Machines>> {
    let device_key = shared.machine_key(DEVICE_KEY_ID).await?;
    linking::Machines::start(Arc::clone(shared), device_key, identity, set_person, up).await
}

/// Passes a command to its swarm's task.
fn to_swarm(shared: &Shared, swarms: &mut HashMap<String, Handle>, cmd: Command) {
    let ns = cmd.swarm().unwrap_or_default().to_string();
    let cmd = match swarms.get(&ns) {
        Some(h) => match h.tx.send(Msg::Command(cmd)) {
            Ok(()) => return,
            Err(mpsc::error::SendError(Msg::Command(cmd))) => {
                swarms.remove(&ns);
                cmd
            }
            Err(_) => return,
        },
        None => cmd,
    };
    let text = "That swarm isn't on this machine.".to_string();
    if let Command::Invite { reply, .. } = cmd {
        let _ = reply.send(Err(text.clone()));
    }
    shared.failed(text);
}

/// Waits until `at`, or forever without one.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Opens a swarm from the registry.
async fn open_existing(shared: Arc<Shared>, identity: Identity, entry: Entry) -> Option<Opened> {
    if entry.removed {
        return Some(Opened {
            entry,
            node: None,
            folder: None,
        });
    }
    let opened = async {
        let ns: NamespaceId = entry
            .ns
            .parse()
            .map_err(|_| anyhow::anyhow!("its id isn't valid"))?;
        let anchor: PersonId = entry.anchor.parse().map_err(anyhow::Error::msg)?;
        let machine_key = shared.machine_key(entry.key_id()).await?;
        let mut cfg = shared.node_config(&entry.ns, machine_key, identity.person);
        cfg.anchor = Some(anchor);
        let node = SwarmNode::open(cfg, ns).await?;
        shared.announce(&node);
        anyhow::Ok(node)
    }
    .await;
    let label = if entry.name.is_empty() {
        &entry.ns
    } else {
        &entry.name
    };
    let node = match opened {
        Ok(node) => node,
        Err(e) => {
            shared.failed(format!("{label} couldn't be opened: {e:#}"));
            return None;
        }
    };
    let folder = if entry.leaving {
        None
    } else if !entry.folder.is_dir() {
        // Made again empty, it would look as if every file had been deleted here.
        shared.failed(format!(
            "{label}: the folder {} is missing, so it isn't syncing.",
            entry.folder.display()
        ));
        None
    } else {
        let state = shared.root.join(&entry.ns).join("folder");
        match Folder::new(entry.folder.clone(), state, false) {
            Ok(f) => Some(f),
            Err(e) => {
                shared.failed(format!("{label}: {e:#}"));
                None
            }
        }
    };
    Some(Opened {
        entry,
        node: Some(node),
        folder,
    })
}

/// Makes a swarm from `folder`, sharing the allowed files already in it.
async fn create(
    shared: Arc<Shared>,
    identity: Identity,
    name: String,
    folder: PathBuf,
) -> Option<Opened> {
    match try_create(&shared, identity, name.trim(), folder).await {
        Ok(opened) => Some(opened),
        Err(e) => {
            shared.failed(format!("{} couldn't be made: {e:#}", name.trim()));
            None
        }
    }
}

async fn try_create(
    shared: &Arc<Shared>,
    identity: Identity,
    name: &str,
    folder: PathBuf,
) -> anyhow::Result<Opened> {
    if name.is_empty() {
        bail!("a swarm needs a name");
    }
    // The namespace is made here rather than by the node, so its key can be kept under its id.
    let secret = NamespaceSecret::from_bytes(&random32()?);
    let ns = secret.id().to_string();
    let dir = shared.root.join(&ns);
    let machine_key = shared.machine_key(&ns).await?;
    let made = async {
        let synced = Folder::new(folder.clone(), dir.join("folder"), false)?;
        let person = PersonKey::from_bytes(identity.person);
        let anchor = person.id();
        let mut cfg = shared.node_config(&ns, machine_key, identity.person);
        cfg.anchor = Some(anchor);
        let author = Author::from_bytes(&random32()?);
        let node = SwarmNode::join(cfg, Capability::Write(secret), author).await?;
        let owner = Member {
            person: anchor,
            name: identity.name.clone(),
            role: Role::Owner,
            machines: vec![Machine {
                endpoint: node.endpoint_id(),
                name: identity.machine_name.clone(),
                author: node.author(),
            }],
        };
        let genesis = SignedChange::sign(
            &ns,
            Change::Genesis {
                name: name.to_string(),
                owner,
            },
            &person,
            now_micros(),
        );
        let entry = Entry {
            ns: ns.clone(),
            anchor: anchor.to_string(),
            folder,
            removed: false,
            name: name.to_string(),
            key: None,
            leaving: false,
        };
        let written = async {
            node.write_change(&genesis).await?;
            node.core.refresh_gate_from_roster().await?;
            shared.registry.add(entry.clone())
        }
        .await;
        if let Err(e) = written {
            node.shutdown().await;
            return Err(e);
        }
        shared.announce(&node);
        anyhow::Ok(Opened {
            entry,
            node: Some(node),
            folder: Some(synced),
        })
    }
    .await;
    if made.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    made
}

/// Joins the swarm `code` invites to, into `folder`, which must be empty or new.
async fn join(
    shared: Arc<Shared>,
    identity: Identity,
    code: String,
    folder: PathBuf,
) -> Option<Opened> {
    match try_join(&shared, identity, &code, folder).await {
        Ok(opened) => {
            shared.toast(&opened.entry.ns, format!("Joined {}.", opened.entry.name));
            Some(opened)
        }
        Err(e) => {
            shared.failed(format!("{e:#}"));
            None
        }
    }
}

async fn try_join(
    shared: &Arc<Shared>,
    identity: Identity,
    code: &str,
    folder: PathBuf,
) -> anyhow::Result<Opened> {
    let code = InviteCode::decode(code).context("That isn't a swarm invite code.")?;
    // Checked before the invite is spent.
    if fs::read_dir(&folder).is_ok_and(|mut d| d.next().is_some()) {
        bail!("A swarm can only be joined into an empty or new folder.");
    }
    let key_id = format!("join-{}", to_hex(&random32()?[..8]));
    let machine_key = shared.machine_key(&key_id).await?;
    let person = PersonKey::from_bytes(identity.person);
    let author = Author::from_bytes(&random32()?);
    let me = Member {
        person: person.id(),
        name: identity.name.clone(),
        role: Role::Reader,
        machines: vec![Machine {
            endpoint: SecretKey::from_bytes(&machine_key).public().to_string(),
            name: identity.machine_name.clone(),
            author: author.id().to_string(),
        }],
    };
    let host = host_addr(shared, &code)?;
    let (cap, anchor, name) = if shared.direct.is_some() {
        redeem_at(&code, me, &person, machine_key, host.clone()).await?
    } else {
        redeem(&code, me, &person, machine_key, shared.relays).await?
    };
    let ns = cap.id().to_string();
    if shared.registry.contains(&ns) {
        bail!("You're already in {name}.");
    }
    let joining = Joining {
        anchor,
        name,
        folder,
        key_id,
        machine_key,
        cap,
        author,
        host,
    };
    open_joined(shared, &identity, joining).await
}

/// A swarm this machine has been let into, before it opens.
struct Joining {
    anchor: PersonId,
    name: String,
    folder: PathBuf,
    key_id: String,
    machine_key: [u8; 32],
    cap: Capability,
    author: Author,
    /// The member machine that let it in, which it syncs with first.
    host: EndpointAddr,
}

/// Opens a swarm this machine was let into and syncs with the machine that let it in; with
/// nothing left behind if that fails.
async fn open_joined(
    shared: &Arc<Shared>,
    identity: &Identity,
    j: Joining,
) -> anyhow::Result<Opened> {
    let ns = j.cap.id().to_string();
    let host = j.host.id.to_string();
    let dir = shared.root.join(&ns);
    let joined = async {
        let synced = Folder::new(j.folder.clone(), dir.join("folder"), true)?;
        let mut cfg = shared.node_config(&ns, j.machine_key, identity.person);
        cfg.anchor = Some(j.anchor);
        let node = SwarmNode::join(cfg, j.cap, j.author).await?;
        // The gate starts with the inviter alone; the swarm's task opens it to the roster once
        // the roster shows this machine.
        let entry = Entry {
            ns: ns.clone(),
            anchor: j.anchor.to_string(),
            folder: j.folder,
            removed: false,
            name: j.name,
            key: Some(j.key_id),
            leaving: false,
        };
        let started = async {
            shared.remember_peers(&node);
            node.remember_addr(j.host);
            node.set_gate(std::slice::from_ref(&host));
            node.sync_with(std::slice::from_ref(&host)).await?;
            shared.registry.add(entry.clone())
        }
        .await;
        if let Err(e) = started {
            node.shutdown().await;
            return Err(e);
        }
        shared.announce(&node);
        anyhow::Ok(Opened {
            entry,
            node: Some(node),
            folder: Some(synced),
        })
    }
    .await;
    if joined.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    joined
}

/// Where the inviting machine is: from the test book when there is one, else its id and relay.
fn host_addr(shared: &Shared, code: &InviteCode) -> anyhow::Result<EndpointAddr> {
    if let Some(book) = &shared.direct {
        return book
            .get(&code.host)
            .context("the inviting machine isn't in the test book");
    }
    let id: EndpointId = code.host.parse().context("the invite's host isn't valid")?;
    let mut addr = EndpointAddr::new(id);
    if let Some(relay) = &code.relay {
        let url: RelayUrl = relay.parse().context("the invite's relay isn't valid")?;
        addr = addr.with_relay_url(url);
    }
    Ok(addr)
}

/// What this machine remembers about who it has seen, kept in `seen.json`.
#[derive(Serialize, Deserialize, Default)]
struct SeenFile {
    /// Endpoint to the Unix seconds it was last connected.
    #[serde(default)]
    machines: BTreeMap<String, u64>,
    #[serde(default)]
    last_change: Option<(String, u64)>,
    /// Whether a roster has shown this machine, so a removal synced just before a restart is
    /// still seen as one after it.
    #[serde(default)]
    me: bool,
}

/// A message to another machine finished.
enum Sent {
    Promoted { endpoint: String, ok: bool },
    Relayed { sig: String, leave: bool, ok: bool },
    Removed,
}

/// One swarm's task: its node, its folder and everything this machine knows of it.
struct Worker {
    shared: Arc<Shared>,
    ns: String,
    dir: PathBuf,
    identity: Identity,
    person: PersonKey,
    name: String,
    folder_path: PathBuf,
    node: Option<Arc<SwarmNode>>,
    my_endpoint: String,
    events: Option<broadcast::Receiver<NodeEvent>>,
    folder: Option<Folder>,
    watch: Option<mpsc::Receiver<()>>,
    roster: Option<Roster>,
    /// Whether any roster has shown this machine since the swarm opened.
    seen_me: bool,
    /// Endpoints already asked to sync.
    peers: HashSet<String>,
    online: HashSet<String>,
    /// Machines to sync with again shortly: ones that just came up, since changes made while
    /// the gossip link was forming reach nobody, and ones that went down, in case they came
    /// straight back (a restart leaves the other side holding the old connection until it
    /// times out).
    resync: HashSet<String>,
    seen: SeenFile,
    /// Machines owed the write capability after their person was promoted.
    owed: Vec<String>,
    /// This person's own changes waiting for an owner to write them.
    outbox: Vec<SignedChange>,
    /// Promotions and relays under way, by endpoint or signature.
    in_flight: HashSet<String>,
    sends: JoinSet<Sent>,
    pass_at: Option<Instant>,
    syncing: bool,
    /// The folder errors last reported, so a lasting one isn't repeated every pass.
    errors: HashSet<String>,
    removed: bool,
    leaving: bool,
    /// Set by a handler; carried out once it returns, when no one holds the node.
    removed_now: bool,
    leave_done: bool,
    /// When the changes no owner took yet are next sent to every owner machine, connected or
    /// not.
    relay_retry_at: Option<Instant>,
    /// The wait before that, for changes other than leaving.
    relay_backoff: Duration,
    finished: bool,
    /// Whether the names in the roster have been checked against this machine's, which may have
    /// changed while the service wasn't running.
    names_checked: bool,
}

impl Worker {
    fn new(shared: Arc<Shared>, identity: Identity, opened: Opened) -> Worker {
        let Opened {
            entry,
            node,
            folder,
        } = opened;
        let dir = shared.root.join(&entry.ns);
        let node = node.map(Arc::new);
        let my_endpoint = node.as_ref().map(|n| n.endpoint_id()).unwrap_or_default();
        let seen: SeenFile = load_json(&dir.join(SEEN_FILE));
        Worker {
            seen_me: seen.me,
            seen,
            owed: load_json(&dir.join(OWED_FILE)),
            outbox: load_json(&dir.join(OUTBOX_FILE)),
            ns: entry.ns,
            dir,
            person: PersonKey::from_bytes(identity.person),
            identity,
            name: entry.name,
            folder_path: entry.folder,
            events: node.as_ref().map(|n| n.events()),
            watch: folder.as_ref().map(Folder::watch),
            node,
            my_endpoint,
            folder,
            roster: None,
            peers: HashSet::new(),
            online: HashSet::new(),
            resync: HashSet::new(),
            in_flight: HashSet::new(),
            sends: JoinSet::new(),
            pass_at: Some(Instant::now()),
            syncing: false,
            errors: HashSet::new(),
            removed: entry.removed,
            leaving: entry.leaving,
            removed_now: false,
            leave_done: false,
            relay_retry_at: None,
            relay_backoff: RELAY_RETRY,
            finished: false,
            names_checked: false,
            shared,
        }
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        self.publish_view();
        let mut tick = tokio::time::interval_at(Instant::now() + PERIODIC, PERIODIC);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    None | Some(Msg::Stop) => break,
                    Some(Msg::Command(cmd)) => self.command(cmd).await,
                    Some(Msg::Identity(id)) => self.set_identity(id).await,
                    Some(Msg::Joining(change, reply)) => self.joining(change, reply).await,
                },
                ev = next_event(&mut self.events) => self.node_event(ev),
                Some(()) = next_signal(&mut self.watch) => self.soon(Duration::ZERO),
                _ = tick.tick() => self.periodic().await,
                () = sleep_until(self.pass_at) => self.pass().await,
                () = sleep_until(self.relay_retry_at) => {
                    self.relay_retry_at = None;
                    self.deliver(true);
                }
                Some(done) = self.sends.join_next(), if !self.sends.is_empty() => {
                    if let Ok(sent) = done {
                        self.sent(sent);
                    }
                }
            }
            self.settle().await;
            if self.finished {
                return;
            }
        }
        self.close().await;
    }

    /// Carries out what a handler decided once nothing holds the node.
    async fn settle(&mut self) {
        if self.removed_now {
            self.removed_now = false;
            self.mark_removed().await;
        }
        if self.leave_done && self.leaving {
            self.leave_done = false;
            self.finish_leaving().await;
        }
        self.publish_view();
    }

    /// Stops talking to other machines and closes the node.
    async fn close(&mut self) {
        self.sends.shutdown().await;
        self.events = None;
        self.watch = None;
        self.folder = None;
        if let Some(node) = self.node.take() {
            match Arc::try_unwrap(node) {
                Ok(node) => node.shutdown().await,
                // Never expected: every other holder is a send, all stopped above.
                Err(node) => drop(node),
            }
        }
    }

    fn soon(&mut self, after: Duration) {
        let at = Instant::now() + after;
        self.pass_at = Some(self.pass_at.map_or(at, |t| t.min(at)));
    }

    fn node_event(&mut self, ev: Option<Result<NodeEvent, broadcast::error::RecvError>>) {
        let Some(ev) = ev else {
            self.events = None;
            return;
        };
        match ev {
            Ok(NodeEvent::Changed | NodeEvent::ContentReady(_))
            | Err(broadcast::error::RecvError::Lagged(_)) => {
                self.syncing = true;
                self.soon(SETTLE);
            }
            Ok(NodeEvent::SyncFinished) => self.soon(SETTLE),
            Ok(NodeEvent::PeerUp(e)) => {
                self.online.insert(e.clone());
                self.resync.insert(e);
                self.soon(RESYNC_AFTER);
                self.deliver(false);
            }
            Ok(NodeEvent::PeerDown(e)) => {
                self.online.remove(&e);
                self.seen.machines.insert(e.clone(), now_secs());
                self.save_seen();
                self.resync.insert(e);
                self.soon(RESYNC_AFTER);
            }
            Ok(NodeEvent::Removed) => {
                if !self.leaving {
                    self.removed_now = true;
                }
            }
            Err(broadcast::error::RecvError::Closed) => self.events = None,
        }
    }

    async fn periodic(&mut self) {
        self.soon(Duration::ZERO);
        let Some(node) = self.node.clone() else {
            return;
        };
        // Every member machine is synced with again: those not connected in case they are
        // back, and those connected for any change whose broadcast didn't reach this machine.
        if let Some(r) = &self.roster {
            let others: Vec<String> = r
                .endpoints()
                .into_iter()
                .filter(|e| *e != self.my_endpoint)
                .collect();
            if !others.is_empty() {
                self.shared.remember_peers(&node);
                let _ = node.sync_with(&others).await;
            }
        }
        self.deliver(true);
    }

    /// Reads the roster, keeps the gate and peers in step with it, and reconciles the folder.
    async fn pass(&mut self) {
        self.pass_at = None;
        let Some(node) = self.node.clone() else {
            return;
        };
        match node.roster().await {
            Ok(Some(r)) => self.roster_read(&node, r).await,
            Ok(None) => {}
            Err(e) => self.report(format!("{}: {e:#}", self.name)),
        }
        self.sync_again(&node).await;
        if self.seen_me && !self.names_checked && !self.removed_now {
            self.names_checked = true;
            self.set_identity(self.identity.clone()).await;
        }
        if self.removed_now || self.removed || self.leaving || !self.seen_me {
            self.syncing = false;
            return;
        }
        let (Some(folder), Some(roster)) = (self.folder.as_mut(), self.roster.as_ref()) else {
            self.syncing = false;
            return;
        };
        let me = roster
            .members
            .iter()
            .find(|m| m.person == self.person.id())
            .cloned();
        let Some(me) = me else {
            self.syncing = false;
            return;
        };
        match folder.reconcile(&node, roster, &me).await {
            Ok(events) => self.folder_events(&node, events).await,
            Err(e) => {
                let text = format!("{}: {e:#}", self.name);
                if self.errors.insert(text.clone()) {
                    self.shared.failed(text);
                }
            }
        }
        self.syncing = false;
    }

    /// Ruling 9: the gate follows the roster, every new member machine is synced with, and a
    /// roster that has taken this machine's person out means it was removed.
    async fn roster_read(&mut self, node: &SwarmNode, r: Roster) {
        let me = self.person.id();
        let me_in = r.member_by_endpoint(&self.my_endpoint).is_some();
        if me_in {
            if !self.seen_me {
                self.seen_me = true;
                self.seen.me = true;
                self.save_seen();
            }
            // A joiner keeps its inviter-only gate until the roster shows it.
            node.set_gate(&r.endpoints());
        } else if self.seen_me
            && !self.leaving
            && r.removed.contains(&me)
            && r.role_of(&me).is_none()
        {
            self.removed_now = true;
        }
        if r.name != self.name && !r.name.is_empty() {
            self.name = r.name.clone();
            let name = r.name.clone();
            if let Err(e) = self.shared.registry.update(&self.ns, |e| e.name = name) {
                self.report(format!("{}: {e:#}", self.name));
            }
        }
        let new: Vec<String> = r
            .endpoints()
            .into_iter()
            .filter(|e| *e != self.my_endpoint && !self.peers.contains(e))
            .collect();
        if !new.is_empty() && !self.removed_now {
            self.shared.remember_peers(node);
            match node.sync_with(&new).await {
                Ok(()) => self.peers.extend(new),
                Err(e) => self.report(format!("{}: {e:#}", self.name)),
            }
        }
        // Promotions owed to machines whose person is no longer a writer are dropped.
        let before = self.owed.len();
        self.owed.retain(|e| {
            r.member_by_endpoint(e)
                .is_some_and(|m| m.role >= Role::Writer)
        });
        if self.owed.len() != before {
            self.save_owed();
        }
        self.roster = Some(r);
        self.deliver(false);
    }

    /// Syncs again with the member machines that just came up or went down.
    async fn sync_again(&mut self, node: &SwarmNode) {
        let Some(r) = &self.roster else {
            return;
        };
        let again: Vec<String> = self
            .resync
            .drain()
            .filter(|e| r.member_by_endpoint(e).is_some())
            .collect();
        if !again.is_empty() {
            self.shared.remember_peers(node);
            let _ = node.sync_with(&again).await;
        }
    }

    async fn folder_events(&mut self, node: &SwarmNode, events: Vec<FolderEvent>) {
        let mut files = None;
        let mut errors = HashSet::new();
        for ev in events {
            let text = match ev {
                FolderEvent::NewConflict { path, copy } => {
                    let machine = match copy_machine(&copy) {
                        Some(m) if m != self.my_machine_name() => m,
                        _ => self.remote_machine(node, &mut files, &path).await,
                    };
                    format!(
                        "{}: a conflict. You and {machine} both changed {}. Both copies are kept.",
                        self.name,
                        path.as_str()
                    )
                }
                FolderEvent::Held(Attention::ReaderEdit { path }) => format!(
                    "{}: your change isn't shared. You're a reader, so {} stopped syncing on this machine.",
                    self.name,
                    path.as_str()
                ),
                FolderEvent::Held(Attention::EditedAfterDelete { path }) => {
                    let machine = self.remote_machine(node, &mut files, &path).await;
                    format!(
                        "{}: {machine} deleted {}, but your edit is kept on this machine.",
                        self.name,
                        path.as_str()
                    )
                }
                FolderEvent::Held(Attention::NoLongerShared { path }) => format!(
                    "{}: {} is no longer in the swarm, as whoever wrote it was taken out. It stays on this machine only.",
                    self.name,
                    path.as_str()
                ),
                FolderEvent::Restored(path) => {
                    let machine = self.remote_machine(node, &mut files, &path).await;
                    format!(
                        "{}: {} was changed on {machine} after you deleted it, so it's back.",
                        self.name,
                        path.as_str()
                    )
                }
                FolderEvent::LastChange { machine, at } => {
                    self.seen.last_change = Some((machine, at / 1_000_000));
                    self.save_seen();
                    continue;
                }
                FolderEvent::Error(text) => {
                    errors.insert(format!("{}: {text}", self.name));
                    continue;
                }
                FolderEvent::Held(_) | FolderEvent::Attention(_) => continue,
            };
            self.shared.toast(&self.ns, text);
        }
        for text in errors.difference(&self.errors) {
            self.shared.failed(text.clone());
        }
        self.errors = errors;
    }

    /// The machine whose entry for `path` is the swarm's newest.
    async fn remote_machine(
        &self,
        node: &SwarmNode,
        files: &mut Option<BTreeMap<RelPath, crate::decide::Remote>>,
        path: &RelPath,
    ) -> String {
        if files.is_none()
            && let Some(r) = &self.roster
        {
            *files = node.files(r).await.ok();
        }
        files
            .as_ref()
            .and_then(|f| f.get(path))
            .map(|r| r.machine.clone())
            .unwrap_or_else(|| "another machine".to_string())
    }

    fn my_machine_name(&self) -> String {
        self.roster
            .as_ref()
            .and_then(|r| r.member_by_endpoint(&self.my_endpoint))
            .and_then(|m| m.machines.iter().find(|x| x.endpoint == self.my_endpoint))
            .map_or_else(|| self.identity.machine_name.clone(), |x| x.name.clone())
    }

    /// Reports a failure that isn't a command's.
    fn report(&self, text: String) {
        self.shared.failed(text);
    }

    async fn command(&mut self, cmd: Command) {
        let done = match cmd {
            Command::Invite { role, reply, .. } => {
                let code = self.invite(role).await;
                let _ = reply.send(code.clone());
                code.map(|_| ())
            }
            Command::CancelInvite { id, .. } => self.cancel_invite(&id),
            Command::SetRole { person, role, .. } => self.set_role(&person, role).await,
            Command::Remove { person, .. } => self.remove(&person).await,
            Command::RenameSwarm { name, .. } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    Err("A swarm needs a name.".to_string())
                } else {
                    let only = format!("Only owners can rename {}.", self.name);
                    self.owner_change(Change::RenameSwarm { name }, only).await
                }
            }
            Command::SetTypes { types, .. } => {
                let only = format!("Only owners can choose which files {} syncs.", self.name);
                self.owner_change(Change::SetTypes { types }, only).await
            }
            Command::SetFolder { folder, .. } => self.set_folder(folder),
            Command::Leave { .. } => self.leave().await,
            Command::Resolve { action, .. } => self.resolve(action).await,
            Command::Create { .. }
            | Command::Join { .. }
            | Command::SetIdentity { .. }
            | Command::LinkCode { .. }
            | Command::CancelLink { .. }
            | Command::Link { .. }
            | Command::RemoveMachine { .. }
            | Command::AcceptOffer { .. }
            | Command::DeclineOffer { .. }
            | Command::Stop => Ok(()),
        };
        if let Err(text) = done {
            self.shared.failed(text);
        }
    }

    /// The node, unless this machine is out of the swarm.
    fn live_node(&self) -> Result<Arc<SwarmNode>, String> {
        if self.removed {
            return Err(format!("You were removed from {}.", self.name));
        }
        if self.leaving {
            return Err(format!("You've left {}.", self.name));
        }
        self.node
            .clone()
            .ok_or_else(|| format!("{} isn't open.", self.name))
    }

    async fn fresh_roster(&self, node: &SwarmNode) -> Result<Roster, String> {
        node.roster()
            .await
            .map_err(|e| format!("{}: {e:#}", self.name))?
            .ok_or_else(|| format!("{}'s members haven't arrived yet.", self.name))
    }

    /// Whether this machine's person may demote or remove `p`: anyone but an owner who isn't
    /// junior to them.
    fn may_lower(&self, r: &Roster, p: &PersonId) -> bool {
        let me = self.person.id();
        r.role_of(p) != Some(Role::Owner) || *p == me || r.outranks(&me, p)
    }

    fn senior(&self, m: &Member) -> String {
        format!(
            "{} became an owner of {} before you, so only an owner senior to them can demote or remove them.",
            m.name, self.name
        )
    }

    fn needs_an_owner(&self) -> String {
        format!("{} needs at least one owner.", self.name)
    }

    fn is_last_owner(r: &Roster, p: &PersonId) -> bool {
        r.role_of(p) == Some(Role::Owner)
            && r.members.iter().filter(|m| m.role == Role::Owner).count() == 1
    }

    async fn write(&mut self, node: &SwarmNode, change: Change) -> Result<SignedChange, String> {
        let signed = SignedChange::sign(&self.ns, change, &self.person, now_micros());
        node.write_change(&signed)
            .await
            .map_err(|e| format!("{}: {e:#}", self.name))?;
        self.soon(Duration::ZERO);
        Ok(signed)
    }

    /// What a change taking `person` out of a role keeps of their work.
    async fn kept(&self, node: &SwarmNode, person: &PersonId) -> Result<Kept, String> {
        node.kept(person)
            .await
            .map_err(|e| format!("{}: {e:#}", self.name))
    }

    async fn invite(&mut self, role: Role) -> Result<String, String> {
        let node = self.live_node()?;
        let (_, code) = node
            .invite(role, &self.person)
            .await
            .map_err(|e| format!("{e:#}"))?;
        Ok(code.encode())
    }

    fn cancel_invite(&mut self, id: &str) -> Result<(), String> {
        let node = self.live_node()?;
        node.cancel_invite(id).map_err(|e| format!("{e:#}"))
    }

    /// The roster, once this machine's person is an owner in it.
    async fn as_owner(&self, node: &SwarmNode, only: String) -> Result<Roster, String> {
        let r = self.fresh_roster(node).await?;
        if r.role_of(&self.person.id()) != Some(Role::Owner) {
            return Err(only);
        }
        Ok(r)
    }

    fn member(&self, r: &Roster, person: &str) -> Result<(PersonId, Member), String> {
        let id: PersonId = person.parse()?;
        let m = r
            .members
            .iter()
            .find(|m| m.person == id)
            .ok_or_else(|| format!("They aren't a member of {}.", self.name))?;
        Ok((id, m.clone()))
    }

    async fn set_role(&mut self, person: &str, role: Role) -> Result<(), String> {
        let node = self.live_node()?;
        let only = format!("Only owners can change roles in {}.", self.name);
        let r = self.as_owner(&node, only).await?;
        let (id, m) = self.member(&r, person)?;
        if role != Role::Owner && Self::is_last_owner(&r, &id) {
            return Err(self.needs_an_owner());
        }
        if m.role == role {
            return Ok(());
        }
        if role < m.role && !self.may_lower(&r, &id) {
            return Err(self.senior(&m));
        }
        let kept = if role < m.role {
            self.kept(&node, &id).await?
        } else {
            Kept::default()
        };
        let change = Change::SetRole {
            person: id,
            role,
            kept,
        };
        self.write(&node, change).await?;
        // Ruling 8: a reader made a writer or owner is handed the write capability.
        if m.role == Role::Reader && role >= Role::Writer {
            for x in &m.machines {
                if !self.owed.contains(&x.endpoint) {
                    self.owed.push(x.endpoint.clone());
                }
            }
            self.save_owed();
            // Sent once the roster read in the next pass shows the new role.
        }
        Ok(())
    }

    async fn remove(&mut self, person: &str) -> Result<(), String> {
        let node = self.live_node()?;
        let only = format!("Only owners can remove people from {}.", self.name);
        let r = self.as_owner(&node, only).await?;
        let (id, m) = self.member(&r, person)?;
        if Self::is_last_owner(&r, &id) {
            return Err(self.needs_an_owner());
        }
        if id == self.person.id() {
            drop(node);
            return self.leave().await;
        }
        if !self.may_lower(&r, &id) {
            return Err(self.senior(&m));
        }
        let kept = self.kept(&node, &id).await?;
        let removal = self
            .write(&node, Change::Remove { person: id, kept })
            .await?;
        let removal = Arc::new(removal);
        self.shared.remember_peers(&node);
        for x in m.machines {
            if self.online.contains(&x.endpoint) {
                let (node, removal) = (Arc::clone(&node), Arc::clone(&removal));
                self.sends.spawn(async move {
                    let _ = node.send_removed(&x.endpoint, &removal).await;
                    Sent::Removed
                });
            }
        }
        Ok(())
    }

    async fn owner_change(&mut self, change: Change, only: String) -> Result<(), String> {
        let node = self.live_node()?;
        self.as_owner(&node, only).await?;
        self.write(&node, change).await.map(drop)
    }

    /// Syncs into a new, empty folder from scratch; the old one is left as it is.
    fn set_folder(&mut self, folder: PathBuf) -> Result<(), String> {
        self.live_node()?;
        if fs::read_dir(&folder).is_ok_and(|mut d| d.next().is_some()) {
            return Err(format!(
                "{} can only move to an empty or new folder.",
                self.name
            ));
        }
        let state = self.dir.join("folder");
        // What the old folder last synced as means nothing for the new one.
        self.watch = None;
        self.folder = None;
        match fs::remove_file(state.join("index.json")) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", self.name)),
        }
        let new = Folder::new(folder.clone(), state, true).map_err(|e| format!("{e:#}"))?;
        self.watch = Some(new.watch());
        self.folder = Some(new);
        self.folder_path = folder.clone();
        self.shared
            .registry
            .update(&self.ns, |e| e.folder = folder)
            .map_err(|e| format!("{}: {e:#}", self.name))?;
        self.soon(Duration::ZERO);
        Ok(())
    }

    /// Writes this person's leaving where this machine can, and stops syncing the folder. The
    /// leaving also goes to an online owner, who writes it for a reader, and whose answer shows
    /// the swarm has heard; the node stays up, out of the list, until then.
    async fn leave(&mut self) -> Result<(), String> {
        if self.removed {
            self.forget().await;
            return Ok(());
        }
        let node = self.live_node()?;
        let r = self.fresh_roster(&node).await?;
        let me = self.person.id();
        let others = r
            .person_machines(&me)
            .iter()
            .any(|x| x.endpoint != self.my_endpoint);
        // With other machines of yours in the swarm, only this one leaves.
        let change = if others {
            let kept = if r.role_of(&me) >= Some(Role::Writer) {
                node.kept_machine(&node.author())
                    .await
                    .map_err(|e| format!("{}: {e:#}", self.name))?
            } else {
                Kept::default()
            };
            Change::RemoveMachine {
                person: me,
                endpoint: self.my_endpoint.clone(),
                kept,
            }
        } else {
            if Self::is_last_owner(&r, &me) {
                return Err(self.needs_an_owner());
            }
            let kept = self.kept(&node, &me).await?;
            Change::Leave { person: me, kept }
        };
        let signed = SignedChange::sign(&self.ns, change, &self.person, now_micros());
        if node.can_write() {
            node.write_change(&signed)
                .await
                .map_err(|e| format!("{}: {e:#}", self.name))?;
        }
        self.outbox.push(signed);
        self.save_outbox();
        self.leaving = true;
        self.watch = None;
        self.folder = None;
        if let Err(e) = self.shared.registry.update(&self.ns, |e| e.leaving = true) {
            self.report(format!("{}: {e:#}", self.name));
        }
        self.shared.drop_view(&self.ns);
        self.deliver(true);
        Ok(())
    }

    async fn finish_leaving(&mut self) {
        self.close().await;
        self.forget().await;
    }

    /// Takes the swarm off this machine, keeping its folder.
    async fn forget(&mut self) {
        self.close().await;
        if let Err(e) = self.shared.registry.remove(&self.ns) {
            self.report(format!("{}: {e:#}", self.name));
        }
        self.shared.drop_view(&self.ns);
        let _ = fs::remove_dir_all(&self.dir);
        self.finished = true;
    }

    async fn mark_removed(&mut self) {
        if self.removed {
            return;
        }
        self.removed = true;
        self.shared
            .toast(&self.ns, format!("You were removed from {}.", self.name));
        let name = self.name.clone();
        if let Err(e) = self.shared.registry.update(&self.ns, |e| {
            e.removed = true;
            e.name = name;
        }) {
            self.report(format!("{}: {e:#}", self.name));
        }
        self.online.clear();
        self.close().await;
    }

    async fn resolve(&mut self, action: Resolution) -> Result<(), String> {
        let node = self.live_node()?;
        let name = self.name.clone();
        let folder = self
            .folder
            .as_mut()
            .ok_or_else(|| format!("{name} isn't syncing a folder here."))?;
        let rel = |s: &str| safe_rel_path(s).ok_or_else(|| format!("{s} isn't a path in {name}."));
        let done = match action {
            Resolution::KeepTheirs { path, copy } => {
                folder.keep_theirs(&node, &rel(&path)?, &rel(&copy)?).await
            }
            Resolution::KeepMine { path, copy } => {
                folder.keep_mine(&node, &rel(&path)?, &rel(&copy)?).await
            }
            Resolution::ShareAgain(p) => folder.share_again(&node, &rel(&p)?).await,
            Resolution::DeleteHeld(p) => folder.delete_held(&rel(&p)?).await,
            Resolution::UseSwarmVersion(p) => folder.use_swarm_version(&node, &rel(&p)?).await,
        };
        self.soon(Duration::ZERO);
        done.map_err(|e| format!("{e:#}"))
    }

    /// Writes this person's new name and machine name, or queues them for an owner.
    async fn set_identity(&mut self, id: Identity) {
        self.identity = id;
        let Ok(node) = self.live_node() else {
            return;
        };
        let Some(r) = self.roster.clone() else {
            return;
        };
        let me = self.person.id();
        let Some(member) = r.members.iter().find(|m| m.person == me) else {
            return;
        };
        let mut changes = Vec::new();
        if member.name != self.identity.name {
            changes.push(Change::Rename {
                person: me,
                name: self.identity.name.clone(),
            });
        }
        let machine = member
            .machines
            .iter()
            .find(|x| x.endpoint == self.my_endpoint);
        if machine.is_some_and(|x| x.name != self.identity.machine_name) {
            changes.push(Change::RenameMachine {
                person: me,
                endpoint: self.my_endpoint.clone(),
                name: self.identity.machine_name.clone(),
            });
        }
        for change in changes {
            if node.can_write() {
                if let Err(e) = self.write(&node, change).await {
                    self.report(e);
                }
            } else if !self.outbox.iter().any(|c| c.change == change) {
                let signed = SignedChange::sign(&self.ns, change, &self.person, now_micros());
                self.outbox.push(signed);
            }
        }
        self.save_outbox();
        self.deliver(true);
    }

    /// Another machine of yours is joining: writes this person's `AddMachine` for it (or has an
    /// owner write it), then answers once the roster shows that machine and the gate lets it in,
    /// with write access when this person has it.
    async fn joining(&mut self, change: SignedChange, reply: oneshot::Sender<MineReply>) {
        let refuse = |reason: String| MineReply::Refused { reason };
        let (Ok(node), Change::AddMachine { machine, .. }) = (self.live_node(), &change.change)
        else {
            let _ = reply.send(linking::not_here());
            return;
        };
        let endpoint = machine.endpoint.clone();
        if node.can_write() {
            if let Err(e) = node.write_change(&change).await {
                let _ = reply.send(refuse(format!("{}: {e:#}", self.name)));
                return;
            }
        } else if !self.outbox.iter().any(|c| c.sig == change.sig) {
            self.outbox.push(change);
            self.save_outbox();
            self.deliver(true);
        }
        let me = self.person.id();
        let host = self.my_endpoint.clone();
        let name = self.name.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + JOINING_WAIT;
            while Instant::now() < deadline {
                if let Ok(Some(r)) = node.roster().await
                    && r.member_by_endpoint(&endpoint).is_some()
                {
                    node.set_gate(&r.endpoints());
                    let write_secret = match r.role_of(&me) {
                        Some(role) if role >= Role::Writer => {
                            match node.capability(Role::Writer).await {
                                Ok(Capability::Write(secret)) => Some(to_hex(&secret.to_bytes())),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    let _ = reply.send(MineReply::Joined { write_secret, host });
                    return;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let _ = reply.send(refuse(format!(
                "Your machine in {name} couldn't add this one yet. Try again in a moment."
            )));
        });
    }

    /// Sends what is owed to connected machines: the write capability to promoted ones, and this
    /// person's queued changes to an owner.
    ///
    /// Relays go over the hydra protocol, which no gate closes, so with `all_owners` they also go
    /// to owner machines that aren't connected: an owner that has this machine out (as once it
    /// has its leaving) is never connected again, but still answers.
    fn deliver(&mut self, all_owners: bool) {
        let Some(node) = self.node.clone() else {
            return;
        };
        let Some(r) = &self.roster else {
            return;
        };
        self.shared.remember_peers(&node);
        for endpoint in self.owed.clone() {
            let promoted = r
                .member_by_endpoint(&endpoint)
                .is_some_and(|m| m.role >= Role::Writer);
            if !promoted || !self.online.contains(&endpoint) || self.in_flight.contains(&endpoint) {
                continue;
            }
            self.in_flight.insert(endpoint.clone());
            let node = Arc::clone(&node);
            self.sends.spawn(async move {
                let ok = node.send_promoted(&endpoint).await.is_ok();
                Sent::Promoted { endpoint, ok }
            });
        }
        let owners: Vec<String> = r
            .members
            .iter()
            .filter(|m| m.role == Role::Owner)
            .flat_map(|m| m.machines.iter().map(|x| x.endpoint.clone()))
            .filter(|e| *e != self.my_endpoint && (all_owners || self.online.contains(e)))
            .collect();
        if owners.is_empty() {
            return;
        }
        for change in self.outbox.clone() {
            if !self.in_flight.insert(change.sig.clone()) {
                continue;
            }
            let node = Arc::clone(&node);
            let owners = owners.clone();
            let shared = Arc::clone(&self.shared);
            let me = self.my_endpoint.clone();
            self.sends.spawn(async move {
                let sig = change.sig.clone();
                let leave = match &change.change {
                    Change::Leave { .. } => true,
                    Change::RemoveMachine { endpoint, .. } => *endpoint == me,
                    _ => false,
                };
                let mut ok = false;
                for owner in owners {
                    let answer = node.relay(&owner, change.clone()).await;
                    if shared.lose_relay() {
                        continue;
                    }
                    match answer {
                        Ok(()) => ok = true,
                        // The owner has this person out already, so there's nothing to pass on.
                        Err(e)
                            if e.downcast_ref::<Refused>()
                                .is_some_and(|r| r.0 == NOT_THEIR_MACHINE) =>
                        {
                            ok = true
                        }
                        Err(_) => continue,
                    }
                    break;
                }
                Sent::Relayed { sig, leave, ok }
            });
        }
    }

    fn sent(&mut self, sent: Sent) {
        match sent {
            Sent::Promoted { endpoint, ok } => {
                self.in_flight.remove(&endpoint);
                if ok {
                    self.owed.retain(|e| *e != endpoint);
                    self.save_owed();
                }
            }
            Sent::Relayed { sig, leave, ok } => {
                self.in_flight.remove(&sig);
                if ok {
                    self.outbox.retain(|c| c.sig != sig);
                    self.save_outbox();
                    self.relay_backoff = RELAY_RETRY;
                    if leave {
                        self.leave_done = true;
                    }
                } else if leave && self.leaving {
                    self.relay_retry_at = Some(Instant::now() + LEAVE_RETRY);
                } else if self.relay_retry_at.is_none() {
                    self.relay_retry_at = Some(Instant::now() + self.relay_backoff);
                    self.relay_backoff = (self.relay_backoff * 2).min(PERIODIC);
                }
            }
            Sent::Removed => {}
        }
    }

    fn save_seen(&self) {
        if let Err(e) = save_json(&self.dir.join(SEEN_FILE), &self.seen) {
            self.report(format!("{}: {e:#}", self.name));
        }
    }

    fn save_owed(&self) {
        if let Err(e) = save_json(&self.dir.join(OWED_FILE), &self.owed) {
            self.report(format!("{}: {e:#}", self.name));
        }
    }

    fn save_outbox(&self) {
        if let Err(e) = save_json(&self.dir.join(OUTBOX_FILE), &self.outbox) {
            self.report(format!("{}: {e:#}", self.name));
        }
    }

    fn publish_view(&self) {
        if self.leaving || self.finished {
            return;
        }
        self.shared.set_view(self.view());
    }

    fn view(&self) -> SwarmView {
        let me = self.person.id();
        let roster = self.roster.as_ref();
        let machine_view = |x: &Machine| {
            let this_machine = x.endpoint == self.my_endpoint;
            MachineView {
                name: x.name.clone(),
                online: this_machine || self.online.contains(&x.endpoint),
                last_seen: self.seen.machines.get(&x.endpoint).copied(),
                this_machine,
            }
        };
        let members: Vec<MemberView> = roster
            .map(|r| {
                r.members
                    .iter()
                    .map(|m| MemberView {
                        person: m.person.to_string(),
                        name: m.name.clone(),
                        role: m.role,
                        junior: r.outranks(&me, &m.person),
                        machines: m.machines.iter().map(machine_view).collect(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let others = members
            .iter()
            .flat_map(|m| &m.machines)
            .filter(|x| !x.this_machine);
        let machines = 1 + others.clone().count();
        let online = 1 + others.filter(|x| x.online).count();
        let my_role = roster.and_then(|r| r.role_of(&me)).unwrap_or(Role::Reader);
        let pending_invites = match &self.node {
            Some(node) if my_role == Role::Owner && !self.removed => node.pending_invites(),
            _ => Vec::new(),
        };
        SwarmView {
            id: self.ns.clone(),
            name: self.name.clone(),
            folder: self.folder_path.clone(),
            my_role,
            members,
            types: roster.map(|r| r.types.clone()).unwrap_or_default(),
            pending_invites,
            attention: self
                .folder
                .as_ref()
                .map(Folder::attention)
                .unwrap_or_default(),
            online,
            machines,
            syncing: self.syncing,
            last_change: self.seen.last_change.clone(),
            removed: self.removed,
            my_other_machines: roster
                .map(|r| {
                    r.person_machines(&me)
                        .iter()
                        .filter(|x| x.endpoint != self.my_endpoint)
                        .count()
                })
                .unwrap_or(0),
        }
    }
}

async fn next_event(
    rx: &mut Option<broadcast::Receiver<NodeEvent>>,
) -> Option<Result<NodeEvent, broadcast::error::RecvError>> {
    match rx {
        Some(rx) => Some(rx.recv().await),
        None => std::future::pending().await,
    }
}

async fn next_signal(rx: &mut Option<mpsc::Receiver<()>>) -> Option<()> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The machine named in a conflict copy's name: `<stem> (conflict, <machine>, <time>)<ext>`.
fn copy_machine(copy: &RelPath) -> Option<String> {
    let name = copy.as_str().rsplit('/').next()?;
    let (_, rest) = name.rsplit_once(" (conflict, ")?;
    let (machine, _) = rest.rsplit_once(", ")?;
    Some(machine.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_names_its_machine() {
        let copy = safe_rel_path("a/SKILL (conflict, Davo's desktop, 27 Oct 13.05).md").unwrap();
        assert_eq!(copy_machine(&copy).as_deref(), Some("Davo's desktop"));
        let numbered = safe_rel_path("x (conflict, a, b, 1 Jan 00.00) 2.md").unwrap();
        assert_eq!(copy_machine(&numbered).as_deref(), Some("a, b"));
        assert_eq!(copy_machine(&safe_rel_path("plain.md").unwrap()), None);
    }

    #[test]
    fn a_registry_without_the_newer_fields_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let old = r#"[{"ns":"n","anchor":"a","folder":"f"}]"#;
        fs::write(dir.path().join(REGISTRY_FILE), old).unwrap();
        let (r, damaged) = Registry::load(dir.path());
        assert!(damaged.is_none());
        let e = &r.entries()[0];
        assert!(!e.removed && !e.leaving);
        assert_eq!(e.key_id(), "n");
    }

    #[test]
    fn a_damaged_registry_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(REGISTRY_FILE), "not json").unwrap();
        let (r, damaged) = Registry::load(dir.path());
        assert!(damaged.is_some());
        assert!(r.entries().is_empty());
        assert!(dir.path().join("registry.json.damaged").exists());
    }
}
