//! What the tests of the swarms service share: machines running the service, each with its own
//! root, relays off, finding each other through a shared book of loopback addresses.
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use hydra_swarms::ids::PersonKey;
use hydra_swarms::{
    Command, DirectPeers, Identity, MachinesView, Role, ServiceConfig, ServiceEvent, SwarmView,
    SwarmsService,
};

pub const WAIT: Duration = Duration::from_secs(30);
/// The service's own allowance for clock differences, for tests that don't care about it.
const DEFAULT_CLAIM_SLACK: Duration = Duration::from_secs(30);

pub type Keys = Arc<Mutex<HashMap<String, [u8; 32]>>>;
/// The personal key as the keyring holds it.
pub type Person = Arc<Mutex<[u8; 32]>>;

/// How many of these tests run at once. Each runs two to four services with their own iroh
/// endpoints, so on a machine with many cores, running them all together starves them and
/// their waits run out. Under `cargo nextest`, each test is its own process and the
/// `network` test group in `.config/nextest.toml` caps them instead.
const AT_ONCE: usize = 6;

static RUNNING: (Mutex<usize>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

/// A place among the tests running now, held until dropped.
pub struct Slot;

pub fn slot() -> Slot {
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

pub fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("random bytes");
    b
}

/// One machine running the service.
pub struct Svc {
    pub tx: mpsc::UnboundedSender<Command>,
    pub rx: mpsc::UnboundedReceiver<ServiceEvent>,
    pub views: Vec<SwarmView>,
    pub toasts: Vec<String>,
    pub failures: Vec<String>,
    pub root: PathBuf,
    pub identity: Identity,
    pub keys: Keys,
    pub person: Person,
    pub machines: Option<MachinesView>,
    pub named: Vec<String>,
    pub book: DirectPeers,
    pub claim_slack: Duration,
    /// Where this machine's folders go.
    pub home: PathBuf,
    pub _dirs: Vec<tempfile::TempDir>,
}

pub fn launch(
    root: &Path,
    identity: Identity,
    keys: &Keys,
    person: &Person,
    book: &DirectPeers,
    lost_relays: Duration,
    claim_slack: Duration,
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
        .lose_relay_answers_for_tests(lost_relays)
        .claim_slack_for_tests(claim_slack);
    SwarmsService::start(cfg).expect("start")
}

impl Svc {
    pub fn new(name: &str, book: &DirectPeers) -> Svc {
        Svc::losing_relays(name, book, Duration::ZERO)
    }

    /// A machine that loses the owners' answers to its relays for its first `lost`.
    pub fn losing_relays(name: &str, book: &DirectPeers, lost: Duration) -> Svc {
        Svc::start(name, book, lost, DEFAULT_CLAIM_SLACK)
    }

    /// A machine that allows `slack` for clock differences before a claim lapses.
    pub fn with_claim_slack(name: &str, book: &DirectPeers, slack: Duration) -> Svc {
        Svc::start(name, book, Duration::ZERO, slack)
    }

    fn start(name: &str, book: &DirectPeers, lost: Duration, claim_slack: Duration) -> Svc {
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
        let (tx, rx) = launch(
            &root,
            identity.clone(),
            &keys,
            &person,
            book,
            lost,
            claim_slack,
        );
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
            claim_slack,
            home: home.path().to_path_buf(),
            _dirs: vec![state, home],
        }
    }

    pub fn person(&self) -> String {
        PersonKey::from_bytes(*self.person.lock().unwrap())
            .id()
            .to_string()
    }

    pub fn folder(&self, name: &str) -> PathBuf {
        self.home.join(name)
    }

    pub fn send(&self, cmd: Command) {
        self.tx.unbounded_send(cmd).expect("the service is running");
    }

    pub fn take(&mut self, ev: ServiceEvent) {
        match ev {
            ServiceEvent::Swarms(v) => self.views = v,
            ServiceEvent::Toast { text, .. } => self.toasts.push(text),
            ServiceEvent::Failed(text) => self.failures.push(text),
            ServiceEvent::Machines(v) => self.machines = Some(v),
            ServiceEvent::Named { name } => self.named.push(name),
        }
    }

    /// Takes in events until `check` gives a value or `WAIT` runs out.
    pub async fn until<T>(&mut self, what: &str, mut check: impl FnMut(&Svc) -> Option<T>) -> T {
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
    pub async fn drain_for(&mut self, d: Duration) {
        let deadline = tokio::time::Instant::now() + d;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, self.rx.next()).await {
            self.take(ev);
        }
    }

    pub async fn view(&mut self, what: &str, check: impl Fn(&SwarmView) -> bool) -> SwarmView {
        self.until(what, |s| s.views.iter().find(|v| check(v)).cloned())
            .await
    }

    pub async fn failure(&mut self, text: &str) {
        self.until(&format!("the failure {text:?}"), |s| {
            s.failures.iter().any(|f| f == text).then_some(())
        })
        .await;
    }

    pub async fn toast(&mut self, text: &str) {
        self.until(&format!("the toast {text:?}"), |s| {
            s.toasts.iter().any(|t| t == text).then_some(())
        })
        .await;
    }

    /// Makes a swarm in a new folder holding `files`, and gives its view once it shows.
    pub async fn create(&mut self, name: &str, files: &[(&str, &str)]) -> SwarmView {
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

    pub async fn invite(&mut self, swarm: &str, role: Role) -> String {
        let (reply, code) = oneshot::channel();
        self.send(Command::Invite {
            swarm: swarm.to_string(),
            role,
            reply,
        });
        code.await.expect("a reply").expect("an invite code")
    }

    /// Joins with `code` into a new folder named `folder`, and gives the swarm's view.
    pub async fn join(&mut self, code: String, folder: &str) -> SwarmView {
        let folder = self.folder(folder);
        self.send(Command::Join {
            code,
            folder: folder.clone(),
        });
        self.view("the joined swarm", |v| v.folder == folder).await
    }

    /// A code to link another machine to this one's person.
    pub async fn link_code(&mut self) -> String {
        let (reply, code) = oneshot::channel();
        self.send(Command::LinkCode { reply });
        code.await.expect("a reply").expect("a link code")
    }

    /// Links this machine with `code`, and waits until it lists `devices` machines of yours,
    /// with the one it linked through online.
    pub async fn link(&mut self, code: String, devices: usize) {
        self.send(Command::Link { code });
        self.until("the machines linked", |s| {
            let m = s.machines.as_ref()?;
            let online = m.devices.iter().filter(|d| d.online).count();
            (m.devices.len() == devices && online >= 2).then_some(())
        })
        .await;
    }

    /// This machine's device id, once the service has said it.
    pub async fn device(&mut self) -> String {
        self.until("this machine's device id", |s| {
            s.machines.as_ref().map(|m| m.this.clone())
        })
        .await
    }

    /// Waits for the offer of swarm `ns`, and gives it.
    pub async fn offer(&mut self, ns: &str) -> hydra_swarms::wire::Offer {
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
    pub async fn accept(&mut self, ns: &str, folder: &str) -> SwarmView {
        let folder = self.folder(folder);
        self.send(Command::AcceptOffer {
            swarm: ns.to_string(),
            folder: folder.clone(),
        });
        self.view("the accepted swarm", |v| v.folder == folder)
            .await
    }

    /// Stops the service and waits until it has closed everything.
    pub async fn stop(&mut self) {
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
    pub fn restart(&mut self) {
        let (tx, rx) = launch(
            &self.root,
            self.identity.clone(),
            &self.keys,
            &self.person,
            &self.book,
            Duration::ZERO,
            self.claim_slack,
        );
        self.tx = tx;
        self.rx = rx;
        self.views.clear();
    }
}

pub fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Polls `check` until it holds or `WAIT` runs out.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
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

pub async fn file_arrives(path: PathBuf, text: &str) {
    eventually(&format!("{} to hold {text:?}", path.display()), || {
        let got = read(&path);
        async move { got.as_deref() == Some(text) }
    })
    .await;
}
