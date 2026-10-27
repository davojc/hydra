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
    Attention, Command, DirectPeers, Identity, Role, ServiceConfig, ServiceEvent, SwarmView,
    SwarmsService,
};

const WAIT: Duration = Duration::from_secs(30);
/// How long to watch for something that mustn't happen.
const QUIET: Duration = Duration::from_secs(4);
/// How long a test machine loses the owners' answers to its relays: long enough for the owner
/// to gate it out and the connection to drop.
const LOST_FOR: Duration = Duration::from_secs(8);

type Keys = Arc<Mutex<HashMap<String, [u8; 32]>>>;

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
    book: DirectPeers,
    /// Where this machine's folders go.
    home: PathBuf,
    _dirs: Vec<tempfile::TempDir>,
}

fn launch(
    root: &Path,
    identity: Identity,
    keys: &Keys,
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
    let cfg = ServiceConfig::new(root.to_path_buf(), identity, swarm_key, false)
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
        let root = state.path().join("swarms");
        let (tx, rx) = launch(&root, identity.clone(), &keys, book, lost);
        Svc {
            tx,
            rx,
            views: Vec::new(),
            toasts: Vec::new(),
            failures: Vec::new(),
            root,
            identity,
            keys,
            book: book.clone(),
            home: home.path().to_path_buf(),
            _dirs: vec![state, home],
        }
    }

    fn person(&self) -> String {
        PersonKey::from_bytes(self.identity.person).id().to_string()
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

    /// Stops the service and waits until it has closed everything.
    async fn stop(&mut self) {
        self.send(Command::Stop);
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.next()).await {
                Ok(Some(ev)) => self.take(ev),
                Ok(None) => return,
                Err(_) => panic!("the service didn't stop"),
            }
        }
    }

    /// Starts the service again on the same root, keys and person.
    fn restart(&mut self) {
        let (tx, rx) = launch(
            &self.root,
            self.identity.clone(),
            &self.keys,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_invite_join_and_a_file_syncs() {
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
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
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

    std::fs::write(olive.folder("Notes").join("f.md"), "foxtrot").unwrap();
    std::fs::write(wren.folder("notes").join("g.md"), "golf").unwrap();
    tokio::time::sleep(QUIET).await;
    assert_eq!(read(&wren.folder("notes").join("f.md")), None);
    assert_eq!(read(&olive.folder("Notes").join("g.md")), None);
    // Their files are left as they are.
    assert_eq!(
        read(&wren.folder("notes").join("a.md")).as_deref(),
        Some("alpha")
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
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
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
    tokio::time::sleep(QUIET).await;
    assert_eq!(read(&olive.folder("Notes").join("h.md")), None);
    // The owner's changes still reach the reader.
    std::fs::write(olive.folder("Notes").join("i.md"), "india").unwrap();
    file_arrives(wren.folder("notes").join("i.md"), "india").await;
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_owner_is_kept() {
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
    let (mut olive, mut wren, id) = pair(Role::Writer).await;
    wren.send(Command::Leave { swarm: id.clone() });
    wren.until("the swarm gone from the list", |s| {
        s.views.iter().all(|v| v.id != id).then_some(())
    })
    .await;
    olive
        .view("one member left", |v| v.id == id && v.members.len() == 1)
        .await;
    std::fs::write(olive.folder("Notes").join("j.md"), "juliet").unwrap();
    tokio::time::sleep(QUIET).await;
    assert_eq!(read(&wren.folder("notes").join("j.md")), None);
    assert_eq!(
        read(&wren.folder("notes").join("a.md")).as_deref(),
        Some("alpha")
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
