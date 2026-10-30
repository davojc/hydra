//! The swarms service end to end: two or three services, each with its own root, relays off,
//! finding each other through a shared book of loopback addresses.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use hydra_swarms::ids::PersonKey;
use hydra_swarms::{
    Attention, Command, DirectPeers, Identity, MachinesView, Role, ServiceConfig, ServiceEvent,
    SwarmView, SwarmsService,
};

const WAIT: Duration = Duration::from_secs(30);
/// How long to watch for something that mustn't happen.
const QUIET: Duration = Duration::from_secs(4);
/// How long a test machine loses the owners' answers to its relays: long enough for the owner
/// to gate it out and the connection to drop.
const LOST_FOR: Duration = Duration::from_secs(8);

type Keys = Arc<Mutex<HashMap<String, [u8; 32]>>>;
/// The personal key as the keyring holds it.
type Person = Arc<Mutex<[u8; 32]>>;

/// How many of these tests run at once. Each runs two to four services with their own iroh
/// endpoints, so on a machine with many cores, running them all together starves them and
/// their waits run out. Under `cargo nextest`, each test is its own process and the
/// `network` test group in `.config/nextest.toml` caps them instead.
const AT_ONCE: usize = 6;

static RUNNING: (Mutex<usize>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

/// A place among the tests running now, held until dropped.
struct Slot;

fn slot() -> Slot {
    let (count, freed) = &RUNNING;
    let mut n = count.lock().unwrap_or_else(|e| e.into_inner());
    while *n >= AT_ONCE {
        n = freed.wait(n).unwrap_or_else(|e| e.into_inner());
    }
    *n += 1;
    Slot
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (count, freed) = &RUNNING;
        *count.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        freed.notify_one();
    }
}

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("random bytes");
    b
}

/// One machine running the service.
struct Svc {
    tx: mpsc::UnboundedSender<Command>,
    rx: mpsc::UnboundedReceiver<ServiceEvent>,
    views: Vec<SwarmView>,
    toasts: Vec<String>,
    failures: Vec<String>,
    root: PathBuf,
    identity: Identity,
    keys: Keys,
    person: Person,
    machines: Option<MachinesView>,
    named: Vec<String>,
    book: DirectPeers,
    /// Where this machine's folders go.
    home: PathBuf,
    _dirs: Vec<tempfile::TempDir>,
}

fn launch(
    root: &Path,
    identity: Identity,
    keys: &Keys,
    person: &Person,
    book: &DirectPeers,
    lost_relays: Duration,
) -> (
    mpsc::UnboundedSender<Command>,
    mpsc::UnboundedReceiver<ServiceEvent>,
) {
    let held = Arc::clone(keys);
    let swarm_key = Box::new(move |id: &str| {
        let mut keys = held.lock().unwrap();
        Ok(*keys.entry(id.to_string()).or_insert_with(random32))
    });
    let stored = Arc::clone(person);
    let set_person = Box::new(move |key: [u8; 32]| {
        *stored.lock().unwrap() = key;
        Ok(())
    });
    let identity = Identity {
        person: *person.lock().unwrap(),
        ..identity
    };
    let cfg = ServiceConfig::new(root.to_path_buf(), identity, swarm_key, set_person, false)
        .direct_peers_for_tests(book)
        .lose_relay_answers_for_tests(lost_relays);
    SwarmsService::start(cfg).expect("start")
}

impl Svc {
    fn new(name: &str, book: &DirectPeers) -> Svc {
        Svc::losing_relays(name, book, Duration::ZERO)
    }

    /// A machine that loses the owners' answers to its relays for its first `lost`.
    fn losing_relays(name: &str, book: &DirectPeers, lost: Duration) -> Svc {
        let state = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let identity = Identity {
            person: PersonKey::generate().to_bytes(),
            name: name.to_string(),
            machine_name: format!("{name}-pc"),
        };
        let keys = Keys::default();
        let person = Person::new(Mutex::new(identity.person));
        let root = state.path().join("swarms");
        let (tx, rx) = launch(&root, identity.clone(), &keys, &person, book, lost);
        Svc {
            tx,
            rx,
            views: Vec::new(),
            toasts: Vec::new(),
            failures: Vec::new(),
            root,
            identity,
            keys,
            person,
            machines: None,
            named: Vec::new(),
            book: book.clone(),
            home: home.path().to_path_buf(),
            _dirs: vec![state, home],
        }
    }

    fn person(&self) -> String {
        PersonKey::from_bytes(*self.person.lock().unwrap())
            .id()
            .to_string()
    }

    fn folder(&self, name: &str) -> PathBuf {
        self.home.join(name)
    }

    fn send(&self, cmd: Command) {
        self.tx.unbounded_send(cmd).expect("the service is running");
    }

    fn take(&mut self, ev: ServiceEvent) {
        match ev {
            ServiceEvent::Swarms(v) => self.views = v,
            ServiceEvent::Toast { text, .. } => self.toasts.push(text),
            ServiceEvent::Failed(text) => self.failures.push(text),
            ServiceEvent::Machines(v) => self.machines = Some(v),
            ServiceEvent::Named { name } => self.named.push(name),
        }
    }

    /// Takes in events until `check` gives a value or `WAIT` runs out.
    async fn until<T>(&mut self, what: &str, mut check: impl FnMut(&Svc) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(v) = check(self) {
                return v;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}; failures: {:?}, toasts: {:?}",
                self.failures,
                self.toasts
            );
            match tokio::time::timeout(left.min(Duration::from_millis(200)), self.rx.next()).await {
                Ok(Some(ev)) => self.take(ev),
                Ok(None) => panic!("the service stopped while waiting for {what}"),
                Err(_) => {}
            }
        }
    }

    /// Takes in whatever has arrived for `d`.
    async fn drain_for(&mut self, d: Duration) {
        let deadline = tokio::time::Instant::now() + d;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, self.rx.next()).await {
            self.take(ev);
        }
    }

    async fn view(&mut self, what: &str, check: impl Fn(&SwarmView) -> bool) -> SwarmView {
        self.until(what, |s| s.views.iter().find(|v| check(v)).cloned())
            .await
    }

    async fn failure(&mut self, text: &str) {
        self.until(&format!("the failure {text:?}"), |s| {
            s.failures.iter().any(|f| f == text).then_some(())
        })
        .await;
    }

    async fn toast(&mut self, text: &str) {
        self.until(&format!("the toast {text:?}"), |s| {
            s.toasts.iter().any(|t| t == text).then_some(())
        })
        .await;
    }

    /// Makes a swarm in a new folder holding `files`, and gives its view once it shows.
    async fn create(&mut self, name: &str, files: &[(&str, &str)]) -> SwarmView {
        let folder = self.folder(name);
        std::fs::create_dir_all(&folder).unwrap();
        for (p, text) in files {
            std::fs::write(folder.join(p), text).unwrap();
        }
        self.send(Command::Create {
            name: name.to_string(),
            folder: folder.clone(),
        });
        self.view(&format!("the swarm {name}"), |v| {
            v.name == name && v.folder == folder
        })
        .await
    }

    async fn invite(&mut self, swarm: &str, role: Role) -> String {
        let (reply, code) = oneshot::channel();
        self.send(Command::Invite {
            swarm: swarm.to_string(),
            role,
            reply,
        });
        code.await.expect("a reply").expect("an invite code")
    }

    /// Joins with `code` into a new folder named `folder`, and gives the swarm's view.
    async fn join(&mut self, code: String, folder: &str) -> SwarmView {
        let folder = self.folder(folder);
        self.send(Command::Join {
            code,
            folder: folder.clone(),
        });
        self.view("the joined swarm", |v| v.folder == folder).await
    }

    /// A code to link another machine to this one's person.
    async fn link_code(&mut self) -> String {
        let (reply, code) = oneshot::channel();
        self.send(Command::LinkCode { reply });
        code.await.expect("a reply").expect("a link code")
    }

    /// Links this machine with `code`, and waits until it lists `devices` machines of yours,
    /// with the one it linked through online.
    async fn link(&mut self, code: String, devices: usize) {
        self.send(Command::Link { code });
        self.until("the machines linked", |s| {
            let m = s.machines.as_ref()?;
            let online = m.devices.iter().filter(|d| d.online).count();
            (m.devices.len() == devices && online >= 2).then_some(())
        })
        .await;
    }

    /// This machine's device id, once the service has said it.
    async fn device(&mut self) -> String {
        self.until("this machine's device id", |s| {
            s.machines.as_ref().map(|m| m.this.clone())
        })
        .await
    }

    /// Waits for the offer of swarm `ns`, and gives it.
    async fn offer(&mut self, ns: &str) -> hydra_swarms::wire::Offer {
        self.until(&format!("the offer of {ns}"), |s| {
            s.machines
                .as_ref()?
                .offers
                .iter()
                .find(|o| o.ns == ns)
                .cloned()
        })
        .await
    }

    /// Takes the offered swarm `ns` into a new folder named `folder`, and gives its view.
    async fn accept(&mut self, ns: &str, folder: &str) -> SwarmView {
        let folder = self.folder(folder);
        self.send(Command::AcceptOffer {
            swarm: ns.to_string(),
            folder: folder.clone(),
        });
        self.view("the accepted swarm", |v| v.folder == folder)
            .await
    }

    /// Stops the service and waits until it has closed everything.
    async fn stop(&mut self) {
        self.send(Command::Stop);
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.next()).await {
                Ok(Some(ev)) => self.take(ev),
                Ok(None) => return,
                Err(_) => panic!("{}'s service didn't stop", self.identity.name),
            }
        }
    }

    /// Starts the service again on the same root, keys and person.
    fn restart(&mut self) {
        let (tx, rx) = launch(
            &self.root,
            self.identity.clone(),
            &self.keys,
            &self.person,
            &self.book,
            Duration::ZERO,
        );
        self.tx = tx;
        self.rx = rx;
        self.views.clear();
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Polls `check` until it holds or `WAIT` runs out.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn file_arrives(path: PathBuf, text: &str) {
    eventually(&format!("{} to hold {text:?}", path.display()), || {
        let got = read(&path);
        async move { got.as_deref() == Some(text) }
    })
    .await;
}

/// An owner `olive` with a swarm "Notes" holding a.md, and `wren` joined to it with `role`.
async fn pair(role: Role) -> (Svc, Svc, String) {
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut wren = Svc::new("wren", &book);
    let notes = olive.create("Notes", &[("a.md", "alpha")]).await;
    let code = olive.invite(&notes.id, role).await;
    let joined = wren.join(code, "notes").await;
    assert_eq!(joined.id, notes.id);
    file_arrives(wren.folder("notes").join("a.md"), "alpha").await;
    let person = wren.person();
    olive
        .view("wren in the members", |v| {
            v.members.iter().any(|m| m.person == person)
        })
        .await;
    (olive, wren, notes.id)
}

/// Wren edits a.md and deletes b.md, and both reach olive.
async fn wren_edits_and_deletes(olive: &Svc, wren: &Svc) {
    std::fs::write(olive.folder("Notes").join("b.md"), "bravo").unwrap();
    file_arrives(wren.folder("notes").join("b.md"), "bravo").await;
    std::fs::write(wren.folder("notes").join("a.md"), "alpha 2").unwrap();
    std::fs::remove_file(wren.folder("notes").join("b.md")).unwrap();
    file_arrives(olive.folder("Notes").join("a.md"), "alpha 2").await;
    let gone = olive.folder("Notes").join("b.md");
    eventually("b.md deleted on olive", || {
        let got = read(&gone);
        async move { got.is_none() }
    })
    .await;
}

/// Once wren is no longer a writer, olive's folder still has wren's edit and delete.
async fn wren_edits_and_deletes_stay(olive: &Svc) {
    tokio::time::sleep(QUIET).await;
    let notes = olive.folder("Notes");
    assert_eq!(read(&notes.join("a.md")).as_deref(), Some("alpha 2"));
    assert_eq!(read(&notes.join("b.md")), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_invite_join_and_a_file_syncs() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
    wren.toast("Joined Notes.").await;
    let v = wren
        .view("wren as a writer", |v| {
            v.id == id && v.my_role == Role::Writer
        })
        .await;
    assert_eq!(v.name, "Notes");
    assert_eq!(v.types, vec![".md"]);

    // Both ways, picked up by the folder watcher.
    std::fs::write(wren.folder("notes").join("b.md"), "bravo").unwrap();
    file_arrives(olive.folder("Notes").join("b.md"), "bravo").await;
    std::fs::write(olive.folder("Notes").join("c.md"), "charlie").unwrap();
    file_arrives(wren.folder("notes").join("c.md"), "charlie").await;

    let v = olive
        .view("both machines online", |v| {
            v.id == id && v.online == 2 && v.machines == 2
        })
        .await;
    let names: Vec<_> = v.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"olive") && names.contains(&"wren"));
    let mine = v
        .members
        .iter()
        .flat_map(|m| &m.machines)
        .find(|m| m.this_machine)
        .expect("this machine");
    assert_eq!(mine.name, "olive-pc");
    // The invite was spent.
    assert!(v.pending_invites.is_empty());
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_service_reopens_its_swarms_after_a_restart() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
    wren.stop().await;
    wren.restart();
    let v = wren.view("the swarm after a restart", |v| v.id == id).await;
    assert_eq!(v.name, "Notes");
    assert_eq!(v.folder, wren.folder("notes"));
    std::fs::write(olive.folder("Notes").join("d.md"), "delta").unwrap();
    file_arrives(wren.folder("notes").join("d.md"), "delta").await;
    std::fs::write(wren.folder("notes").join("e.md"), "echo").unwrap();
    file_arrives(olive.folder("Notes").join("e.md"), "echo").await;
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_member_stops_their_sync_and_tells_them() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
    wren_edits_and_deletes(&olive, &wren).await;
    olive.send(Command::Remove {
        swarm: id.clone(),
        person: wren.person(),
    });
    wren.toast("You were removed from Notes.").await;
    wren.view("the swarm marked removed", |v| v.id == id && v.removed)
        .await;
    olive
        .view("one member left", |v| v.id == id && v.members.len() == 1)
        .await;
    wren_edits_and_deletes_stay(&olive).await;

    std::fs::write(olive.folder("Notes").join("f.md"), "foxtrot").unwrap();
    std::fs::write(wren.folder("notes").join("g.md"), "golf").unwrap();
    tokio::time::sleep(QUIET).await;
    assert_eq!(read(&wren.folder("notes").join("f.md")), None);
    assert_eq!(read(&olive.folder("Notes").join("g.md")), None);
    // Their files are left as they are.
    assert_eq!(
        read(&wren.folder("notes").join("a.md")).as_deref(),
        Some("alpha 2")
    );
    // Still removed after a restart.
    wren.stop().await;
    wren.restart();
    wren.view("still removed", |v| {
        v.id == id && v.removed && v.name == "Notes"
    })
    .await;
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn demoting_a_writer_ignores_their_later_changes() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
    wren_edits_and_deletes(&olive, &wren).await;
    olive.send(Command::SetRole {
        swarm: id.clone(),
        person: wren.person(),
        role: Role::Reader,
    });
    wren.view("wren as a reader", |v| {
        v.id == id && v.my_role == Role::Reader
    })
    .await;
    std::fs::write(wren.folder("notes").join("h.md"), "hotel").unwrap();
    wren.toast(
        "Notes: your change isn't shared. You're a reader, so h.md stopped syncing on this machine.",
    )
    .await;
    wren.view("the held edit under Needs attention", |v| {
        v.id == id
            && v.attention
                .iter()
                .any(|a| matches!(a, Attention::ReaderEdit { path } if path.as_str() == "h.md"))
    })
    .await;
    wren_edits_and_deletes_stay(&olive).await;
    assert_eq!(read(&olive.folder("Notes").join("h.md")), None);
    // The owner's changes still reach the reader.
    std::fs::write(olive.folder("Notes").join("i.md"), "india").unwrap();
    file_arrives(wren.folder("notes").join("i.md"), "india").await;
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_owner_is_kept() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let notes = olive.create("Notes", &[]).await;
    let me = olive.person();
    let refused = "Notes needs at least one owner.";
    olive.send(Command::SetRole {
        swarm: notes.id.clone(),
        person: me.clone(),
        role: Role::Writer,
    });
    olive.failure(refused).await;
    olive.failures.clear();
    olive.send(Command::Remove {
        swarm: notes.id.clone(),
        person: me.clone(),
    });
    olive.failure(refused).await;
    olive.failures.clear();
    olive.send(Command::Leave {
        swarm: notes.id.clone(),
    });
    olive.failure(refused).await;
    olive.drain_for(Duration::from_millis(500)).await;
    let v = olive.view("Notes still there", |v| v.id == notes.id).await;
    assert_eq!(v.my_role, Role::Owner);
    assert!(!v.removed);
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_change_reaches_every_swarm() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut wren = Svc::new("wren", &book);
    let notes = olive.create("Notes", &[]).await;
    let plans = olive.create("Plans", &[]).await;
    let code = olive.invite(&notes.id, Role::Writer).await;
    wren.join(code, "notes").await;
    let code = olive.invite(&plans.id, Role::Reader).await;
    wren.join(code, "plans").await;
    let person = wren.person();
    for id in [&notes.id, &plans.id] {
        olive
            .view("wren in the members", |v| {
                v.id == *id && v.members.iter().any(|m| m.person == person)
            })
            .await;
        wren.view("wren's own roster", |v| {
            v.id == *id && v.members.iter().any(|m| m.person == person)
        })
        .await;
    }

    wren.send(Command::SetIdentity {
        name: "Wren W".into(),
        machine_name: "wren-laptop".into(),
    });
    for id in [&notes.id, &plans.id] {
        olive
            .view("wren's new names", |v| {
                v.id == *id
                    && v.members.iter().any(|m| {
                        m.person == person
                            && m.name == "Wren W"
                            && m.machines.iter().any(|x| x.name == "wren-laptop")
                    })
            })
            .await;
    }
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_keeps_the_files_and_stops_syncing() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
    wren_edits_and_deletes(&olive, &wren).await;
    wren.send(Command::Leave { swarm: id.clone() });
    wren.until("the swarm gone from the list", |s| {
        s.views.iter().all(|v| v.id != id).then_some(())
    })
    .await;
    olive
        .view("one member left", |v| v.id == id && v.members.len() == 1)
        .await;
    std::fs::write(olive.folder("Notes").join("j.md"), "juliet").unwrap();
    wren_edits_and_deletes_stay(&olive).await;
    assert_eq!(read(&wren.folder("notes").join("j.md")), None);
    assert_eq!(
        read(&wren.folder("notes").join("a.md")).as_deref(),
        Some("alpha 2")
    );
    // Gone for good: a restart doesn't bring it back.
    wren.stop().await;
    wren.restart();
    wren.drain_for(Duration::from_secs(2)).await;
    assert!(wren.views.iter().all(|v| v.id != id));
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_types_lets_py_files_sync() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut wren = Svc::new("wren", &book);
    let notes = olive
        .create("Notes", &[("a.md", "alpha"), ("tool.py", "print(1)")])
        .await;
    let code = olive.invite(&notes.id, Role::Writer).await;
    wren.join(code, "notes").await;
    file_arrives(wren.folder("notes").join("a.md"), "alpha").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(read(&wren.folder("notes").join("tool.py")), None);

    olive.send(Command::SetTypes {
        swarm: notes.id.clone(),
        types: vec![".md".into(), "PY".into()],
    });
    file_arrives(wren.folder("notes").join("tool.py"), "print(1)").await;
    wren.view("the new types", |v| {
        v.id == notes.id && v.types == vec![".md", ".py"]
    })
    .await;
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promoted_reader_is_handed_write_access() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Reader).await;
    olive.send(Command::SetRole {
        swarm: id.clone(),
        person: wren.person(),
        role: Role::Writer,
    });
    wren.view("wren as a writer", |v| {
        v.id == id && v.my_role == Role::Writer
    })
    .await;
    std::fs::write(wren.folder("notes").join("k.md"), "kilo").unwrap();
    file_arrives(olive.folder("Notes").join("k.md"), "kilo").await;
    assert!(
        !wren.toasts.iter().any(|t| t.contains("isn't shared")),
        "held as a reader edit: {:?}",
        wren.toasts
    );
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leave_whose_answer_is_lost_still_completes() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    // The owner writes wren's leaving, but wren doesn't hear so until the owner has gated wren
    // out and wren no longer counts it as connected, so only a relay that doesn't wait for a
    // connection can settle it.
    let mut wren = Svc::losing_relays("wren", &book, LOST_FOR);
    let notes = olive.create("Notes", &[("a.md", "alpha")]).await;
    let code = olive.invite(&notes.id, Role::Writer).await;
    wren.join(code, "notes").await;
    file_arrives(wren.folder("notes").join("a.md"), "alpha").await;
    let id = notes.id;

    wren.send(Command::Leave { swarm: id.clone() });
    olive
        .view("one member left", |v| v.id == id && v.members.len() == 1)
        .await;
    // Forgotten: its state goes, and a restart doesn't bring it back.
    let state = wren.root.join(&id);
    eventually("the left swarm's state to go", || {
        let gone = !state.exists();
        async move { gone }
    })
    .await;
    wren.stop().await;
    wren.restart();
    wren.drain_for(Duration::from_secs(2)).await;
    assert!(wren.views.iter().all(|v| v.id != id));
    assert_eq!(
        read(&wren.folder("notes").join("a.md")).as_deref(),
        Some("alpha")
    );
    olive.stop().await;
    wren.stop().await;
}

// Plan B: linking your own machines.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_machine_takes_the_person_and_both_list_each_other() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive-new", &book);
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    assert_eq!(olive2.person(), olive.person());
    olive2
        .until("olive2 named olive", |s| {
            s.named.iter().any(|n| n == "olive").then_some(())
        })
        .await;
    olive
        .until("olive lists both, the code spent", |s| {
            let m = s.machines.as_ref()?;
            (m.devices.len() == 2 && m.links.is_empty()).then_some(())
        })
        .await;
    for svc in [&olive, &olive2] {
        let m = svc.machines.as_ref().unwrap();
        let here: Vec<&str> = m
            .devices
            .iter()
            .filter(|d| d.this_machine)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(here, vec![m.this.as_str()]);
    }
    let names: Vec<String> = olive
        .machines
        .as_ref()
        .unwrap()
        .devices
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert!(names.contains(&"olive-pc".to_string()));
    assert!(names.contains(&"olive-new-pc".to_string()));
    olive.stop().await;
    olive2.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_code_is_single_use() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let mut olive3 = Svc::new("olive3", &book);
    let code = olive.link_code().await;
    olive2.link(code.clone(), 2).await;
    olive3.send(Command::Link { code });
    olive3
        .failure("This link code has already been used.")
        .await;
    for s in [&mut olive, &mut olive2, &mut olive3] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_in_swarms_cannot_link_and_the_code_survives() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut wren = Svc::new("wren", &book);
    let mut fresh = Svc::new("fresh", &book);
    wren.create("Wren's", &[]).await;
    let code = olive.link_code().await;
    wren.send(Command::Link { code: code.clone() });
    wren.failure(
        "This machine is already in swarms as someone. Leave them before linking it to another \
         of your machines.",
    )
    .await;
    olive
        .until("the code still pending", |s| {
            (s.machines.as_ref()?.links.len() == 1).then_some(())
        })
        .await;
    fresh.link(code, 2).await;
    assert_eq!(fresh.person(), olive.person());
    for s in [&mut olive, &mut wren, &mut fresh] {
        s.stop().await;
    }
}

/// Only asks; answers anything with Ok.
struct Asker;

impl hydra_swarms::device::DeviceHandler for Asker {
    fn answer(
        &self,
        _: String,
        _: hydra_swarms::wire::MineRequest,
    ) -> futures::future::BoxFuture<'static, hydra_swarms::wire::MineReply> {
        Box::pin(async { hydra_swarms::wire::MineReply::Ok })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strangers_get_nothing_from_a_device() {
    let _slot = slot();
    use hydra_swarms::mine::Mine;
    use hydra_swarms::wire::{MineReply, MineRequest};
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let device = olive.device().await;
    olive.drain_for(Duration::from_secs(1)).await;
    let before = olive.machines.clone();
    let stranger = hydra_swarms::device::DeviceNode::start(random32(), false, Arc::new(Asker))
        .await
        .unwrap();
    stranger.remember_addr(book.get(&device).expect("olive's device in the book"));
    let mallory = PersonKey::generate();
    let secret: String = mallory
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let asks = [
        MineRequest::Sync {
            mine: Mine {
                name: "Mallory".into(),
                name_at: u64::MAX,
                ..Mine::default()
            },
        },
        MineRequest::NewKey {
            secret,
            chain: Vec::new(),
        },
    ];
    for req in asks {
        let reply = stranger.ask(&device, &req).await.unwrap();
        let refused = MineReply::Refused {
            reason: "This isn't one of your machines.".into(),
        };
        assert_eq!(reply, refused);
    }
    olive.drain_for(Duration::from_secs(1)).await;
    assert_eq!(olive.machines, before);
    assert!(olive.named.is_empty());
    stranger.shutdown().await;
    olive.stop().await;
}

/// `who`'s machines in `svc`'s view of swarm `ns`, once `who` shows there with `count` of them.
async fn machines_of(svc: &mut Svc, ns: &str, who: &str, count: usize) -> SwarmView {
    let who = who.to_string();
    svc.view(&format!("{who} with {count} machines"), |v| {
        v.id == ns
            && v.members
                .iter()
                .filter(|m| m.person == who)
                .map(|m| m.machines.len())
                .collect::<Vec<_>>()
                == vec![count]
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offered_swarm_joins_as_the_same_person() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut wren = Svc::new("wren", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let notes = olive.create("Notes", &[("a.md", "alpha")]).await;
    let code = olive.invite(&notes.id, Role::Writer).await;
    wren.join(code, "notes").await;
    file_arrives(wren.folder("notes").join("a.md"), "alpha").await;
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    let offer = olive2.offer(&notes.id).await;
    assert_eq!((offer.name.as_str(), offer.role), ("Notes", Role::Owner));
    let here = olive2.accept(&notes.id, "notes2").await;
    assert_eq!(here.id, notes.id);
    file_arrives(olive2.folder("notes2").join("a.md"), "alpha").await;
    let person = olive.person();
    machines_of(&mut wren, &notes.id, &person, 2).await;
    let view = wren.views.iter().find(|v| v.id == notes.id).unwrap();
    assert_eq!(view.members.iter().filter(|m| m.name == "olive").count(), 1);
    // The new machine got write access along with the swarm.
    std::fs::write(olive2.folder("notes2").join("c.md"), "charlie").unwrap();
    file_arrives(wren.folder("notes").join("c.md"), "charlie").await;
    olive2
        .until("the offer gone once taken", |s| {
            s.machines.as_ref()?.offers.is_empty().then_some(())
        })
        .await;
    olive2
        .view("one other machine", |v| {
            v.id == notes.id && v.my_other_machines == 1
        })
        .await;
    for s in [&mut olive, &mut wren, &mut olive2] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_swarm_is_offered_to_your_other_machines() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    let plans = olive.create("Plans", &[]).await;
    olive2.toast("olive-pc offered you Plans.").await;
    let offer = olive2.offer(&plans.id).await;
    assert_eq!(offer.name, "Plans");
    olive.stop().await;
    olive2.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_joins_its_other_machine_through_an_owner() {
    let _slot = slot();
    let (mut olive, mut wren, id) = pair(Role::Reader).await;
    let mut wren2 = Svc::new("wren2", &wren.book.clone());
    let code = wren.link_code().await;
    wren2.link(code, 2).await;
    wren2.offer(&id).await;
    wren2.accept(&id, "notes2").await;
    file_arrives(wren2.folder("notes2").join("a.md"), "alpha").await;
    let person = wren.person();
    machines_of(&mut olive, &id, &person, 2).await;
    for s in [&mut olive, &mut wren, &mut wren2] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offer_is_taken_through_any_machine_of_yours_in_the_swarm() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let mut olive3 = Svc::new("olive3", &book);
    let notes = olive.create("Notes", &[("a.md", "alpha")]).await;
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    let code = olive.link_code().await;
    olive3.link(code, 3).await;
    olive2.offer(&notes.id).await;
    olive2.accept(&notes.id, "notes2").await;
    file_arrives(olive2.folder("notes2").join("a.md"), "alpha").await;
    // The offer olive3 holds came from olive, which has stopped; olive2 takes it instead.
    olive3.offer(&notes.id).await;
    olive.stop().await;
    olive3.accept(&notes.id, "notes3").await;
    file_arrives(olive3.folder("notes3").join("a.md"), "alpha").await;
    olive3.stop().await;

    // With none of them running, the offer stays.
    let mut olive4 = Svc::new("olive4", &book);
    let code = olive2.link_code().await;
    olive4.link(code, 4).await;
    olive4.offer(&notes.id).await;
    olive2.stop().await;
    olive4.send(Command::AcceptOffer {
        swarm: notes.id.clone(),
        folder: olive4.folder("notes4"),
    });
    olive4
        .failure("None of your machines in Notes is running. Try again when one is.")
        .await;
    assert!(
        olive4
            .machines
            .as_ref()
            .unwrap()
            .offers
            .iter()
            .any(|o| o.ns == notes.id)
    );
    olive4.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declined_offer_does_not_come_back() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let notes = olive.create("Notes", &[]).await;
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    olive2.offer(&notes.id).await;
    olive2.send(Command::DeclineOffer {
        swarm: notes.id.clone(),
    });
    olive2
        .until("the offer gone", |s| {
            s.machines.as_ref()?.offers.is_empty().then_some(())
        })
        .await;
    // A new swarm makes olive offer everything again; only the new one shows.
    let plans = olive.create("Plans", &[]).await;
    olive2.offer(&plans.id).await;
    olive2.drain_for(Duration::from_secs(1)).await;
    let offers = &olive2.machines.as_ref().unwrap().offers;
    assert_eq!(
        offers.iter().map(|o| o.ns.clone()).collect::<Vec<_>>(),
        vec![plans.id]
    );
    assert!(
        !olive2
            .toasts
            .iter()
            .any(|t| t.contains("offered you Notes"))
    );
    olive.stop().await;
    olive2.stop().await;
}

/// Olive and olive2 (linked, both in Notes, which holds a.md) and wren, a writer in it.
async fn olive_twice_and_wren() -> (Svc, Svc, Svc, String) {
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut wren = Svc::new("wren", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let notes = olive.create("Notes", &[("a.md", "alpha")]).await;
    let code = olive.invite(&notes.id, Role::Writer).await;
    wren.join(code, "notes").await;
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    olive2.offer(&notes.id).await;
    olive2.accept(&notes.id, "notes2").await;
    file_arrives(olive2.folder("notes2").join("a.md"), "alpha").await;
    let person = olive.person();
    machines_of(&mut wren, &notes.id, &person, 2).await;
    (olive, olive2, wren, notes.id)
}

/// `svc`'s view of swarm `ns` shows `person` named `name`, and still does after `QUIET`.
async fn named_and_stays(svc: &mut Svc, ns: &str, person: &str, name: &str) {
    let shows = |v: &SwarmView| {
        v.id == ns
            && v.members
                .iter()
                .any(|m| m.person == person && m.name == name)
    };
    svc.view(&format!("{person} named {name}"), shows).await;
    svc.drain_for(QUIET).await;
    let v = svc.views.iter().find(|v| v.id == ns).unwrap();
    assert!(shows(v), "{name} didn't stay: {:?}", v.members);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn your_name_changes_on_every_machine() {
    let _slot = slot();
    let (mut olive, mut olive2, mut wren, ns) = olive_twice_and_wren().await;
    olive.send(Command::SetIdentity {
        name: "Olive O".into(),
        machine_name: "olive-pc".into(),
    });
    olive2
        .until("olive2 named Olive O", |s| {
            s.named.iter().any(|n| n == "Olive O").then_some(())
        })
        .await;
    let person = olive.person();
    named_and_stays(&mut wren, &ns, &person, "Olive O").await;
    let v = wren.views.iter().find(|v| v.id == ns).unwrap();
    assert_eq!(v.members.iter().filter(|m| m.person == person).count(), 1);
    for s in [&mut olive, &mut olive2, &mut wren] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn names_set_apart_settle_on_the_newer() {
    let _slot = slot();
    let (mut olive, mut olive2, mut wren, ns) = olive_twice_and_wren().await;
    let person = olive.person();
    olive2.stop().await;
    olive.send(Command::SetIdentity {
        name: "A".into(),
        machine_name: "olive-pc".into(),
    });
    named_and_stays(&mut wren, &ns, &person, "A").await;
    olive.stop().await;
    olive2.restart();
    olive2.send(Command::SetIdentity {
        name: "B".into(),
        machine_name: "olive2-pc".into(),
    });
    named_and_stays(&mut wren, &ns, &person, "B").await;
    olive.restart();
    olive
        .until("olive named B", |s| {
            s.named.iter().any(|n| n == "B").then_some(())
        })
        .await;
    named_and_stays(&mut wren, &ns, &person, "B").await;
    named_and_stays(&mut olive, &ns, &person, "B").await;
    for s in [&mut olive, &mut olive2, &mut wren] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_on_one_machine_keeps_the_person_in() {
    let _slot = slot();
    let (mut olive, mut olive2, mut wren, ns) = olive_twice_and_wren().await;
    std::fs::write(olive2.folder("notes2").join("x.md"), "xray").unwrap();
    file_arrives(wren.folder("notes").join("x.md"), "xray").await;
    olive2.send(Command::Leave { swarm: ns.clone() });
    olive2
        .until("the swarm gone from olive2", |s| {
            s.views.iter().all(|v| v.id != ns).then_some(())
        })
        .await;
    let person = olive.person();
    machines_of(&mut wren, &ns, &person, 1).await;
    machines_of(&mut olive, &ns, &person, 1).await;
    std::fs::write(olive.folder("Notes").join("y.md"), "yankee").unwrap();
    file_arrives(wren.folder("notes").join("y.md"), "yankee").await;
    tokio::time::sleep(QUIET).await;
    // Olive2 keeps its files and gets nothing new; what it wrote before stays with the others.
    assert_eq!(
        read(&olive2.folder("notes2").join("a.md")).as_deref(),
        Some("alpha")
    );
    assert_eq!(read(&olive2.folder("notes2").join("y.md")), None);
    assert_eq!(
        read(&wren.folder("notes").join("x.md")).as_deref(),
        Some("xray")
    );
    assert_eq!(
        read(&olive.folder("Notes").join("x.md")).as_deref(),
        Some("xray")
    );
    let v = olive.views.iter().find(|v| v.id == ns).unwrap();
    assert_eq!(v.my_role, Role::Owner);
    // Olive offers its swarms again (a new one makes it); Notes isn't offered back to olive2.
    let other = olive.create("Other", &[]).await;
    olive2.offer(&other.id).await;
    olive2.drain_for(Duration::from_secs(1)).await;
    let offers = &olive2.machines.as_ref().unwrap().offers;
    assert!(offers.iter().all(|o| o.ns != ns), "{offers:?}");
    assert!(
        !olive2
            .toasts
            .iter()
            .any(|t| t.contains("offered you Notes"))
    );
    for s in [&mut olive, &mut olive2, &mut wren] {
        s.stop().await;
    }
}

/// Olive2 joined to Notes as another of olive's machines, through `olive`.
async fn add_to_notes(olive: &mut Svc, other: &mut Svc, ns: &str, devices: usize, folder: &str) {
    let code = olive.link_code().await;
    other.link(code, devices).await;
    other.offer(ns).await;
    other.accept(ns, folder).await;
    file_arrives(other.folder(folder).join("a.md"), "alpha").await;
}

/// `who`'s device id as `svc` lists it.
fn device_named(svc: &Svc, who: &str) -> String {
    svc.machines
        .as_ref()
        .unwrap()
        .devices
        .iter()
        .find(|d| d.name == format!("{who}-pc"))
        .map(|d| d.id.clone())
        .expect("that machine is listed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_machine_shuts_it_out_and_keeps_you_in() {
    let _slot = slot();
    let (mut olive, mut olive2, mut wren, ns) = olive_twice_and_wren().await;
    let mut olive3 = Svc::new("olive3", &olive.book.clone());
    add_to_notes(&mut olive, &mut olive3, &ns, 3, "notes3").await;
    let old = olive.person();
    machines_of(&mut wren, &ns, &old, 3).await;
    std::fs::write(olive3.folder("notes3").join("w.md"), "whiskey").unwrap();
    file_arrives(wren.folder("notes").join("w.md"), "whiskey").await;
    olive
        .until("olive3 listed", |s| {
            (s.machines.as_ref()?.devices.len() == 3).then_some(())
        })
        .await;
    let gone = device_named(&olive, "olive3");
    olive.send(Command::RemoveMachine { device: gone });
    olive
        .toast("olive3-pc was removed from your machines.")
        .await;
    let new = olive.person();
    assert_ne!(new, old);
    machines_of(&mut wren, &ns, &new, 2).await;
    let v = wren.views.iter().find(|v| v.id == ns).unwrap();
    assert!(v.members.iter().all(|m| m.person != old));
    olive2
        .until("olive2 holds the new key", |s| {
            (s.person() == new).then_some(())
        })
        .await;
    olive3.toast("This machine was removed from Notes.").await;
    std::fs::write(olive3.folder("notes3").join("z.md"), "zulu").unwrap();
    std::fs::write(olive.folder("Notes").join("y.md"), "yankee").unwrap();
    file_arrives(wren.folder("notes").join("y.md"), "yankee").await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(read(&wren.folder("notes").join("z.md")), None);
    assert_eq!(
        read(&wren.folder("notes").join("w.md")).as_deref(),
        Some("whiskey")
    );
    // Hydra no longer takes the old key: olive3 renaming itself changes nothing.
    olive3.send(Command::SetIdentity {
        name: "Mallory".into(),
        machine_name: "olive3-pc".into(),
    });
    named_and_stays(&mut wren, &ns, &new, "olive").await;
    for s in [&mut olive, &mut olive2, &mut olive3, &mut wren] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_that_was_away_picks_up_the_new_key() {
    let _slot = slot();
    let (mut olive, mut olive2, mut wren, ns) = olive_twice_and_wren().await;
    let mut olive3 = Svc::new("olive3", &olive.book.clone());
    add_to_notes(&mut olive, &mut olive3, &ns, 3, "notes3").await;
    // A swarm only olive2 of olive's machines is in.
    let plans = wren.create("Plans", &[]).await;
    let code = wren.invite(&plans.id, Role::Writer).await;
    olive2.join(code, "plans").await;
    let old = olive.person();
    machines_of(&mut wren, &plans.id, &old, 1).await;
    // Olive2 has seen itself in Plans before it stops.
    machines_of(&mut olive2, &plans.id, &old, 1).await;
    olive
        .until("olive3 listed", |s| {
            (s.machines.as_ref()?.devices.len() == 3).then_some(())
        })
        .await;
    olive2.stop().await;
    let gone = device_named(&olive, "olive3");
    olive.send(Command::RemoveMachine { device: gone });
    olive
        .toast("olive3-pc was removed from your machines.")
        .await;
    let new = olive.person();
    machines_of(&mut wren, &ns, &new, 2).await;
    olive2.restart();
    olive2
        .until("olive2 holds the new key", |s| {
            (s.person() == new).then_some(())
        })
        .await;
    olive2
        .view("olive2 still an owner of Notes", |v| {
            v.id == ns && v.my_role == Role::Owner
        })
        .await;
    machines_of(&mut wren, &plans.id, &new, 1).await;
    machines_of(&mut wren, &ns, &new, 2).await;
    for s in [&mut olive, &mut olive2, &mut olive3, &mut wren] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn you_cannot_remove_the_machine_you_are_on() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let me = olive.device().await;
    olive.send(Command::RemoveMachine { device: me });
    olive
        .failure("You can't remove the machine you're on.")
        .await;
    olive.send(Command::RemoveMachine {
        device: "nobody".into(),
    });
    olive.failure("That isn't one of your machines.").await;
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_machine_cannot_link_again() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    olive
        .until("olive2 listed", |s| {
            (s.machines.as_ref()?.devices.len() == 2).then_some(())
        })
        .await;
    let gone = device_named(&olive, "olive2");
    olive.send(Command::RemoveMachine { device: gone });
    olive
        .toast("olive2-pc was removed from your machines.")
        .await;
    // Its state wiped, but its device key kept: it is still the machine that was removed.
    olive2.stop().await;
    std::fs::remove_file(olive2.root.join("mine.json")).unwrap();
    olive2.restart();
    let code = olive.link_code().await;
    olive2.send(Command::Link { code });
    olive2
        .failure("This machine was removed from your machines.")
        .await;
    assert_ne!(olive2.person(), olive.person());
    olive
        .until("the code still pending", |s| {
            (s.machines.as_ref()?.links.len() == 1).then_some(())
        })
        .await;
    olive.stop().await;
    olive2.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_new_key_reaches_a_machine_that_was_away_through_any_other() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut a = Svc::new("a", &book);
    let mut b = Svc::new("b", &book);
    let mut c = Svc::new("c", &book);
    let mut d = Svc::new("d", &book);
    for (other, n) in [(&mut b, 2), (&mut c, 3), (&mut d, 4)] {
        let code = a.link_code().await;
        other.link(code, n).await;
    }
    for s in [&mut a, &mut b, &mut c] {
        let what = format!("all four listed on {}", s.identity.machine_name);
        s.until(&what, |s| {
            let n = s.machines.as_ref()?.devices.len();
            (n == 4).then_some(())
        })
        .await;
    }
    c.stop().await;
    let gone = device_named(&a, "d");
    a.send(Command::RemoveMachine { device: gone });
    a.toast("d-pc was removed from your machines.").await;
    let new = a.person();
    b.until("b holds the new key", |s| (s.person() == new).then_some(()))
        .await;
    // The machine that removed d is gone; c gets the key from b.
    a.stop().await;
    c.restart();
    c.until("c holds the new key", |s| (s.person() == new).then_some(()))
        .await;
    for s in [&mut b, &mut c, &mut d] {
        s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removal_that_cannot_be_saved_changes_nothing() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::new("olive", &book);
    let mut olive2 = Svc::new("olive2", &book);
    let code = olive.link_code().await;
    olive2.link(code, 2).await;
    olive
        .until("olive2 listed", |s| {
            (s.machines.as_ref()?.devices.len() == 2).then_some(())
        })
        .await;
    let before = olive.person();
    // mine.json can't be written: a folder stands where its temporary file goes.
    std::fs::create_dir_all(olive.root.join("mine.json.tmp")).unwrap();
    let gone = device_named(&olive, "olive2");
    olive.send(Command::RemoveMachine { device: gone });
    olive
        .until("the removal refused", |s| {
            s.failures
                .iter()
                .any(|f| f.starts_with("Couldn't remove olive2-pc:"))
                .then_some(())
        })
        .await;
    olive.drain_for(Duration::from_secs(1)).await;
    assert_eq!(olive.person(), before);
    assert_eq!(olive.machines.as_ref().unwrap().devices.len(), 2);
    assert_eq!(olive2.person(), before);
    olive.stop().await;
    olive2.stop().await;
}
