//! Folders kept in sync through swarm nodes on loopback, relays off. Each side reconciles
//! explicitly rather than waiting on timers, except where the watcher itself is under test.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hydra_swarms::folder::{Attention, Folder, FolderEvent};
use hydra_swarms::ids::{PersonId, PersonKey};
use hydra_swarms::node::{NodeConfig, NodeEvent, SwarmNode};
use hydra_swarms::paths::{
    MAX_FILE_BYTES, RelPath, conflict_name, conflict_original, is_conflict_copy, numbered_copy,
    safe_rel_path,
};
use hydra_swarms::roster::{Change, Machine, Member, Role, Roster, SignedChange};
use iroh_blobs::Hash;
use iroh_docs::Author;
use jiff::Timestamp;
use jiff::tz::TimeZone;

const WAIT: Duration = Duration::from_secs(20);

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("random bytes");
    b
}

fn now() -> u64 {
    jiff::Timestamp::now().as_microsecond() as u64
}

fn rel(s: &str) -> RelPath {
    safe_rel_path(s).unwrap()
}

fn config(dir: &Path, person: &PersonKey, anchor: Option<PersonId>) -> NodeConfig {
    NodeConfig {
        dir: dir.to_path_buf(),
        machine_key: random32(),
        relays: false,
        person: PersonKey::from_bytes(person.to_bytes()),
        anchor,
    }
}

/// Polls `check` until it gives a value or `WAIT` runs out.
async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// One machine: its node, its person and its folder.
struct Side {
    node: SwarmNode,
    key: PersonKey,
    folder: Folder,
    root: PathBuf,
    _dirs: Vec<tempfile::TempDir>,
}

impl Side {
    async fn roster(&self) -> Roster {
        self.node.roster().await.unwrap().expect("a roster")
    }

    async fn reconcile(&mut self) -> Vec<FolderEvent> {
        let roster = self.roster().await;
        let me = roster
            .members
            .iter()
            .find(|m| m.person == self.key.id())
            .expect("a member")
            .clone();
        self.folder
            .reconcile(&self.node, &roster, &me)
            .await
            .expect("reconcile")
    }

    /// Reconciles until `done` holds, and gives every event seen on the way.
    async fn settle(&mut self, what: &str, done: impl Fn(&Side) -> bool) -> Vec<FolderEvent> {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut events = Vec::new();
        loop {
            events.extend(self.reconcile().await);
            if done(self) {
                return events;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn read(&self, p: &str) -> Option<String> {
        std::fs::read_to_string(self.path(p)).ok()
    }

    fn write(&self, p: &str, text: &str) {
        let path = self.path(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn remove(&self, p: &str) {
        std::fs::remove_file(self.path(p)).unwrap();
    }

    /// The swarm's latest word on `p`, as this machine sees it.
    async fn remote_hash(&self, p: &str) -> Option<Option<Hash>> {
        let roster = self.roster().await;
        let files = self.node.files(&roster).await.unwrap();
        files.get(&rel(p)).map(|r| r.hash)
    }

    async fn in_doc(&self, p: &str) -> bool {
        matches!(self.remote_hash(p).await, Some(Some(_)))
    }

    /// Waits until this machine has heard that `p` is now `hash` (None for a delete).
    async fn hears(&self, p: &str, hash: Option<Hash>) {
        eventually(&format!("{p} to be {hash:?}"), || async {
            (self.remote_hash(p).await == Some(hash)).then_some(())
        })
        .await;
    }

    async fn shutdown(self) {
        self.node.shutdown().await;
    }
}

fn hash(text: &str) -> Hash {
    Hash::new(text.as_bytes())
}

fn member(key: &PersonKey, name: &str, role: Role, endpoint: String, author: String) -> Member {
    Member {
        person: key.id(),
        name: name.into(),
        role,
        machines: vec![Machine {
            endpoint,
            name: format!("{name}-pc"),
            author,
        }],
    }
}

/// An owner whose folder already holds `files`, creating the swarm from it.
async fn owner_with(files: &[(&str, &str)]) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for (p, text) in files {
        let path = root.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let key = PersonKey::generate();
    let me = member(&key, "olive", Role::Owner, String::new(), String::new());
    let node = SwarmNode::create(config(dir.path(), &key, None), "Notes", me)
        .await
        .expect("create");
    let folder =
        Folder::new(root.path().to_path_buf(), dir.path().join("folder"), false).expect("folder");
    Side {
        node,
        key,
        folder,
        root: root.path().to_path_buf(),
        _dirs: vec![dir, root],
    }
}

async fn owner() -> Side {
    owner_with(&[]).await
}

/// A new person with `role`, added by `owner`, joining into a new folder and syncing with it.
async fn joiner(owner: &Side, name: &str, role: Role) -> Side {
    joiner_of(owner, owner.key.id(), name, role).await
}

/// As `joiner`, in the swarm whose genesis `anchor` signed.
async fn joiner_of(owner: &Side, anchor: PersonId, name: &str, role: Role) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let key = PersonKey::generate();
    let cfg = config(dir.path(), &key, Some(anchor));
    let endpoint = iroh::SecretKey::from_bytes(&cfg.machine_key)
        .public()
        .to_string();
    let author = Author::from_bytes(&random32());
    let add = SignedChange::sign(
        &owner.node.id(),
        Change::AddMember {
            member: member(&key, name, role, endpoint, author.id().to_string()),
        },
        &owner.key,
        now(),
    );
    owner.node.write_change(&add).await.expect("add member");
    let cap = owner.node.capability(role).await.unwrap();
    let node = SwarmNode::join(cfg, cap, author).await.expect("join");
    let root = dir.path().join("swarm folder");
    let folder = Folder::new(root.clone(), dir.path().join("folder"), true).expect("folder");
    node.remember_addr(owner.node.addr_for_tests());
    owner.node.remember_addr(node.addr_for_tests());
    let roster = owner.node.roster().await.unwrap().unwrap();
    owner.node.set_gate(&roster.endpoints());
    node.set_gate(&[owner.node.endpoint_id()]);
    node.sync_with(&[owner.node.endpoint_id()])
        .await
        .expect("sync");
    let side = Side {
        node,
        key,
        folder,
        root,
        _dirs: vec![dir],
    };
    eventually("the roster on the joiner", || async {
        side.node
            .roster()
            .await
            .unwrap()
            .filter(|r| r.role_of(&side.key.id()).is_some())
    })
    .await;
    side
}

/// An owner and a writer, both holding `p` as "one".
async fn synced_pair(p: &str) -> (Side, Side) {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    o.write(p, "one");
    o.reconcile().await;
    w.settle("the file on the writer", |s| {
        s.read(p).as_deref() == Some("one")
    })
    .await;
    (o, w)
}

fn set_mtime(path: &Path, at: Timestamp) {
    let when = SystemTime::UNIX_EPOCH + Duration::from_micros(at.as_microsecond() as u64);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Both sides change `p` from "one": the writer first (its file dated `wren_at`), then the owner,
/// which publishes. The writer then meets the owner's newer edit: the owner keeps the name and
/// the writer's edit becomes a conflict copy, which reaches the owner too. Gives the copy.
async fn make_conflict(o: &mut Side, w: &mut Side, p: &str, wren_at: Timestamp) -> RelPath {
    let copy = conflict_name(&rel(p), "wren-pc", wren_at, &TimeZone::system());
    conflict_named(o, w, p, wren_at, ("wren's", "olive's"), copy).await
}

/// As `make_conflict`, with the writer's and owner's contents given, and the copy's name
/// expected to be `copy`.
async fn conflict_named(
    o: &mut Side,
    w: &mut Side,
    p: &str,
    wren_at: Timestamp,
    (wren, olive): (&str, &str),
    copy: RelPath,
) -> RelPath {
    w.write(p, wren);
    set_mtime(&w.path(p), wren_at);
    o.write(p, olive);
    o.reconcile().await;
    w.hears(p, Some(hash(olive))).await;
    let events = w
        .settle("the conflict on the writer", |s| {
            s.read(copy.as_str()).as_deref() == Some(wren) && s.read(p).as_deref() == Some(olive)
        })
        .await;
    assert!(events.contains(&FolderEvent::NewConflict {
        path: rel(p),
        copy: copy.clone()
    }));
    o.settle("the conflict copy on the owner", |s| {
        s.read(copy.as_str()).as_deref() == Some(wren)
    })
    .await;
    copy
}

/// The conflict copies of `p` in a folder.
fn copies_of(side: &Side, p: &str) -> Vec<String> {
    let dir = side.path(p).parent().unwrap().to_path_buf();
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| conflict_original(&rel(n)).is_some_and(|o| o == rel(p)))
        .collect();
    out.sort();
    out
}

/// Lets two machines reach each other again after `cut_off`.
async fn reconnect(a: &Side, b: &Side) {
    a.node.set_gate(&[b.node.endpoint_id()]);
    b.node.set_gate(&[a.node.endpoint_id()]);
    b.node.sync_with(&[a.node.endpoint_id()]).await.unwrap();
}

fn cut_off(a: &Side, b: &Side) {
    a.node.set_gate(&[]);
    b.node.set_gate(&[]);
}

/// Waits until `side` holds `n` entries for `p`, each with its author's parent record.
async fn has_entries(side: &Side, p: &str, n: usize) {
    eventually(&format!("{n} entries for {p}"), || async {
        let roster = side.roster().await;
        let all = side.node.file_entries(&roster).await.unwrap();
        let h = all.get(&rel(p))?;
        let complete = h.entries.len() == n
            && h.entries.iter().all(|e| {
                h.records
                    .iter()
                    .any(|(a, r)| *a == e.author && r.hash == e.hash)
            });
        complete.then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_file_reaches_the_other_folder() {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    o.write("notes/a.md", "hello");
    let events = o.reconcile().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FolderEvent::LastChange { machine, .. } if machine == "olive-pc"))
    );
    let events = w
        .settle("a.md on the writer", |s| {
            s.read("notes/a.md").as_deref() == Some("hello")
        })
        .await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FolderEvent::LastChange { machine, .. } if machine == "olive-pc"))
    );
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_reaches_the_other_folder() {
    let (mut o, mut w) = synced_pair("x.md").await;
    w.write("x.md", "two");
    w.reconcile().await;
    o.settle("the edit on the owner", |s| {
        s.read("x.md").as_deref() == Some("two")
    })
    .await;
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_reaches_the_other_folder() {
    let (mut o, mut w) = synced_pair("x.md").await;
    o.remove("x.md");
    o.reconcile().await;
    assert_eq!(o.remote_hash("x.md").await, Some(None));
    w.settle("the delete on the writer", |s| s.read("x.md").is_none())
        .await;
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_of_other_types_stay_local() {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    o.write("a.py", "print()");
    o.write("x.md", "hello");
    o.reconcile().await;
    assert!(o.in_doc("x.md").await);
    assert_eq!(o.remote_hash("a.py").await, None);
    w.settle("x.md on the writer", |s| s.read("x.md").is_some())
        .await;
    assert!(w.read("a.py").is_none());
    assert_eq!(w.remote_hash("a.py").await, None);
    assert!(o.folder.attention().is_empty());
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversize_file_stays_local_and_is_flagged() {
    let mut o = owner().await;
    let big = vec![b'a'; MAX_FILE_BYTES as usize + 1];
    std::fs::write(o.path("big.md"), &big).unwrap();
    let events = o.reconcile().await;
    assert_eq!(o.remote_hash("big.md").await, None);
    let flagged = vec![Attention::TooBig {
        path: rel("big.md"),
    }];
    assert_eq!(o.folder.attention(), flagged);
    assert!(events.contains(&FolderEvent::Attention(flagged)));
    // Back under the cap, it syncs and the flag goes.
    o.write("big.md", "small now");
    o.reconcile().await;
    assert!(o.in_doc("big.md").await);
    assert!(o.folder.attention().is_empty());
    o.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conflict_keeps_both_and_both_sides_see_it() {
    let (mut o, mut w) = synced_pair("SKILL.md").await;
    let wren_at: Timestamp = "2026-10-27T13:05:00Z".parse().unwrap();
    let copy = make_conflict(&mut o, &mut w, "SKILL.md", wren_at).await;

    let when = wren_at
        .to_zoned(TimeZone::system())
        .strftime("%-d %b %H.%M")
        .to_string();
    assert_eq!(
        copy.as_str(),
        format!("SKILL (conflict, wren-pc, {when}).md")
    );
    assert!(is_conflict_copy(&copy));
    for side in [&o, &w] {
        assert_eq!(side.read("SKILL.md").as_deref(), Some("olive's"));
        assert_eq!(side.read(copy.as_str()).as_deref(), Some("wren's"));
        assert_eq!(
            side.folder.attention(),
            vec![Attention::Conflict {
                path: rel("SKILL.md"),
                copy: copy.clone()
            }]
        );
    }
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keep_mine_and_keep_theirs_resolve_everywhere() {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    o.write("x.md", "one");
    o.write("y.md", "one");
    o.reconcile().await;
    w.settle("both files on the writer", |s| {
        s.read("x.md").is_some() && s.read("y.md").is_some()
    })
    .await;
    let at: Timestamp = "2026-10-27T09:00:00Z".parse().unwrap();
    let x_copy = make_conflict(&mut o, &mut w, "x.md", at).await;
    let y_copy = make_conflict(&mut o, &mut w, "y.md", at).await;

    // The owner keeps its own version of x.md: the copy goes everywhere.
    o.folder
        .keep_theirs(&o.node, &rel("x.md"), &x_copy)
        .await
        .unwrap();
    assert!(o.read(x_copy.as_str()).is_none());
    w.settle("x's copy gone on the writer", |s| {
        s.read(x_copy.as_str()).is_none()
    })
    .await;
    assert_eq!(w.read("x.md").as_deref(), Some("olive's"));

    // The writer keeps its version of y.md, from the copy.
    w.folder
        .keep_mine(&w.node, &rel("y.md"), &y_copy)
        .await
        .unwrap();
    assert_eq!(w.read("y.md").as_deref(), Some("wren's"));
    assert!(w.read(y_copy.as_str()).is_none());
    o.settle("y resolved on the owner", |s| {
        s.read("y.md").as_deref() == Some("wren's") && s.read(y_copy.as_str()).is_none()
    })
    .await;

    w.reconcile().await;
    assert!(o.folder.attention().is_empty());
    assert!(w.folder.attention().is_empty());
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_after_a_remote_delete_is_held_locally() {
    let (mut o, mut w) = synced_pair("x.md").await;
    w.write("x.md", "kept");
    o.remove("x.md");
    o.reconcile().await;
    w.hears("x.md", None).await;
    let events = w.reconcile().await;
    let held = Attention::EditedAfterDelete { path: rel("x.md") };
    assert!(events.contains(&FolderEvent::Held(held.clone())));
    assert_eq!(w.folder.attention(), vec![held]);
    assert_eq!(w.read("x.md").as_deref(), Some("kept"));
    assert_eq!(w.remote_hash("x.md").await, Some(None));

    // Share it again: it comes back for everyone.
    w.folder.share_again(&w.node, &rel("x.md")).await.unwrap();
    assert!(w.folder.attention().is_empty());
    o.settle("x.md back on the owner", |s| {
        s.read("x.md").as_deref() == Some("kept")
    })
    .await;
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_held_removes_the_kept_edit() {
    let (mut o, mut w) = synced_pair("x.md").await;
    w.write("x.md", "kept");
    o.remove("x.md");
    o.reconcile().await;
    w.hears("x.md", None).await;
    w.reconcile().await;
    w.folder.delete_held(&rel("x.md")).await.unwrap();
    assert!(w.read("x.md").is_none());
    assert!(w.folder.attention().is_empty());
    w.reconcile().await;
    assert!(w.read("x.md").is_none());
    assert_eq!(w.remote_hash("x.md").await, Some(None));
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_local_delete_meeting_a_remote_edit_restores_it() {
    let (mut o, mut w) = synced_pair("x.md").await;
    w.remove("x.md");
    o.write("x.md", "two");
    o.reconcile().await;
    w.hears("x.md", Some(hash("two"))).await;
    let events = w
        .settle("x.md restored", |s| {
            s.read("x.md").as_deref() == Some("two")
        })
        .await;
    assert!(events.contains(&FolderEvent::Restored(rel("x.md"))));
    assert_eq!(o.remote_hash("x.md").await, Some(Some(hash("two"))));
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_readers_edit_is_held_and_use_swarm_version_resumes() {
    let mut o = owner().await;
    let mut r = joiner(&o, "rae", Role::Reader).await;
    o.write("x.md", "one");
    o.reconcile().await;
    r.settle("x.md on the reader", |s| s.read("x.md").is_some())
        .await;

    r.write("x.md", "mine");
    let events = r.reconcile().await;
    let held = Attention::ReaderEdit { path: rel("x.md") };
    assert!(events.contains(&FolderEvent::Held(held.clone())));
    assert_eq!(r.folder.attention(), vec![held]);

    // While held, the swarm's changes don't land here.
    o.write("x.md", "two");
    o.reconcile().await;
    r.hears("x.md", Some(hash("two"))).await;
    r.reconcile().await;
    assert_eq!(r.read("x.md").as_deref(), Some("mine"));
    assert_eq!(o.read("x.md").as_deref(), Some("two"));

    r.folder
        .use_swarm_version(&r.node, &rel("x.md"))
        .await
        .unwrap();
    assert_eq!(r.read("x.md").as_deref(), Some("two"));
    assert!(r.folder.attention().is_empty());

    // Syncing has resumed.
    o.write("x.md", "three");
    o.reconcile().await;
    r.settle("the next edit on the reader", |s| {
        s.read("x.md").as_deref() == Some("three")
    })
    .await;
    o.shutdown().await;
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_from_sync_are_not_republished() {
    let (mut o, mut w) = synced_pair("x.md").await;
    // One entry: the owner's.
    assert_eq!(w.node.file_entries_for_tests().await.unwrap(), 1);
    let events = w.reconcile().await;
    assert!(events.is_empty(), "{events:?}");
    o.reconcile().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(w.node.file_entries_for_tests().await.unwrap(), 1);
    assert_eq!(o.node.file_entries_for_tests().await.unwrap(), 1);
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_saves_publishes_only_the_last_content() {
    let mut o = owner().await;
    let mut events = o.node.events();
    // Let the genesis' own events go by.
    tokio::time::sleep(Duration::from_millis(500)).await;
    while events.try_recv().is_ok() {}

    let mut changes = o.folder.watch();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let p = o.path("x.md");
    std::fs::write(&p, "first").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&p, "").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&p, "last").unwrap();

    // Act on every signal, as the service does, for a few seconds, noting what the file held
    // when the watcher spoke.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut seen_at_signal = Vec::new();
    while let Ok(Some(())) = tokio::time::timeout_at(deadline, changes.recv()).await {
        seen_at_signal.push(o.read("x.md").unwrap_or_default());
        o.reconcile().await;
    }
    assert_eq!(seen_at_signal, vec!["last".to_string()]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut publishes = 0;
    while let Ok(e) = events.try_recv() {
        if e == NodeEvent::Changed {
            publishes += 1;
        }
    }
    // One publish: its parent record, then the file entry.
    assert_eq!(publishes, 2);
    assert_eq!(o.remote_hash("x.md").await, Some(Some(hash("last"))));
    o.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_file_never_publishes() {
    let (mut o, mut w) = synced_pair("x.md").await;
    o.write("x.md", "");
    o.write("new.md", "");
    o.reconcile().await;
    assert_eq!(o.remote_hash("x.md").await, Some(Some(hash("one"))));
    assert_eq!(o.remote_hash("new.md").await, None);
    w.reconcile().await;
    assert_eq!(w.read("x.md").as_deref(), Some("one"));
    o.shutdown().await;
    w.shutdown().await;
}

#[test]
fn joining_refuses_a_non_empty_folder() {
    let state = tempfile::tempdir().unwrap();
    let full = tempfile::tempdir().unwrap();
    std::fs::write(full.path().join("a.txt"), "x").unwrap();
    assert!(Folder::new(full.path().to_path_buf(), state.path().join("a"), true).is_err());
    // Creating from it is fine.
    assert!(Folder::new(full.path().to_path_buf(), state.path().join("b"), false).is_ok());
    let empty = tempfile::tempdir().unwrap();
    assert!(Folder::new(empty.path().to_path_buf(), state.path().join("c"), true).is_ok());
    let new = empty.path().join("new");
    assert!(Folder::new(new.clone(), state.path().join("d"), true).is_ok());
    assert!(new.is_dir());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creating_shares_the_files_already_there() {
    let mut o = owner_with(&[
        ("a.md", "alpha"),
        ("skills/b.md", "beta"),
        ("c.txt", "local"),
    ])
    .await;
    o.reconcile().await;
    assert_eq!(o.remote_hash("a.md").await, Some(Some(hash("alpha"))));
    assert_eq!(o.remote_hash("skills/b.md").await, Some(Some(hash("beta"))));
    assert_eq!(o.remote_hash("c.txt").await, None);
    let mut w = joiner(&o, "wren", Role::Writer).await;
    w.settle("the shared files on the joiner", |s| {
        s.read("a.md").as_deref() == Some("alpha")
            && s.read("skills/b.md").as_deref() == Some("beta")
    })
    .await;
    assert!(w.read("c.txt").is_none());
    o.shutdown().await;
    w.shutdown().await;
}

/// `link` in the owner's folder points at a folder outside it holding `secret.md`: nothing under
/// it is shared, and a swarm entry under it is never written there.
async fn links_are_not_followed(make_link: impl Fn(&Path, &Path)) {
    let mut o = owner().await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.md"), "secret").unwrap();
    make_link(outside.path(), &o.path("link"));
    assert!(o.path("link").join("secret.md").exists());
    o.write("x.md", "visible");
    o.reconcile().await;
    assert!(o.in_doc("x.md").await);
    assert_eq!(o.remote_hash("link/secret.md").await, None);

    o.node
        .put_file(&rel("link/evil.md"), Bytes::from_static(b"evil"))
        .await
        .unwrap();
    let events = o.reconcile().await;
    assert!(
        events.iter().any(|e| matches!(e, FolderEvent::Error(_))),
        "{events:?}"
    );
    assert!(!outside.path().join("evil.md").exists());
    o.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn symlinks_are_not_followed() {
    links_are_not_followed(|target, link| std::os::unix::fs::symlink(target, link).unwrap()).await;
    // A link to a single file isn't shared either.
    let mut o = owner().await;
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.md");
    std::fs::write(&secret, "secret").unwrap();
    std::os::unix::fs::symlink(&secret, o.path("file.md")).unwrap();
    o.reconcile().await;
    assert_eq!(o.remote_hash("file.md").await, None);
    o.shutdown().await;
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn junctions_are_not_followed() {
    links_are_not_followed(|target, link| junction::create(target, link).unwrap()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_taken_copy_name_gets_a_number() {
    let (mut o, mut w) = synced_pair("x.md").await;
    let at: Timestamp = "2026-10-27T13:05:00Z".parse().unwrap();
    let first = make_conflict(&mut o, &mut w, "x.md", at).await;
    // The same machine, the same minute, different content: the name is taken.
    let second = numbered_copy(&first, 2);
    assert!(second.as_str().ends_with(") 2.md"), "{second:?}");
    conflict_named(
        &mut o,
        &mut w,
        "x.md",
        at,
        ("wren again", "olive again"),
        second.clone(),
    )
    .await;
    for side in [&o, &w] {
        assert_eq!(side.read(first.as_str()).as_deref(), Some("wren's"));
        assert_eq!(side.read(second.as_str()).as_deref(), Some("wren again"));
    }
    w.reconcile().await;
    assert_eq!(
        w.folder.attention(),
        // In name order: " 2" sorts before ".md".
        vec![
            Attention::Conflict {
                path: rel("x.md"),
                copy: second
            },
            Attention::Conflict {
                path: rel("x.md"),
                copy: first
            },
        ]
    );
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publishes_keep_both() {
    let (mut o, mut w) = synced_pair("x.md").await;
    cut_off(&o, &w);
    o.write("x.md", "olive's");
    o.reconcile().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    w.write("x.md", "wren's");
    w.reconcile().await;
    // Each published without seeing the other's edit.
    assert_eq!(o.remote_hash("x.md").await, Some(Some(hash("olive's"))));
    assert_eq!(w.remote_hash("x.md").await, Some(Some(hash("wren's"))));
    reconnect(&o, &w).await;
    has_entries(&o, "x.md", 2).await;
    has_entries(&w, "x.md", 2).await;

    // The writer's is newer and keeps the name; the owner's becomes a copy named for it.
    let roster = o.roster().await;
    let at = o.node.file_entries(&roster).await.unwrap()[&rel("x.md")]
        .entries
        .iter()
        .find(|e| e.author == o.node.author())
        .unwrap()
        .at;
    let when = Timestamp::from_microsecond(at as i64).unwrap();
    let copy = conflict_name(&rel("x.md"), "olive-pc", when, &TimeZone::system());
    for side in [&mut o, &mut w] {
        side.settle("both versions kept", |s| {
            s.read("x.md").as_deref() == Some("wren's")
                && s.read(copy.as_str()).as_deref() == Some("olive's")
        })
        .await;
    }
    for side in [&mut o, &mut w] {
        side.reconcile().await;
        assert_eq!(copies_of(side, "x.md"), vec![copy.as_str().to_string()]);
        assert_eq!(
            side.folder.attention(),
            vec![Attention::Conflict {
                path: rel("x.md"),
                copy: copy.clone()
            }]
        );
    }

    // Resolving it sticks: the stale entry doesn't bring the copy back.
    o.folder
        .keep_theirs(&o.node, &rel("x.md"), &copy)
        .await
        .unwrap();
    w.settle("the copy gone on the writer", |s| {
        s.read(copy.as_str()).is_none()
    })
    .await;
    for side in [&mut o, &mut w] {
        side.reconcile().await;
        side.reconcile().await;
        assert!(copies_of(side, "x.md").is_empty());
        assert!(side.folder.attention().is_empty());
    }
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sequential_chain_is_not_a_conflict() {
    let mut a = owner().await;
    let mut b = joiner(&a, "bea", Role::Writer).await;
    let mut c = joiner(&a, "cy", Role::Writer).await;
    eventually("the full roster on bea's machine", || async {
        b.node
            .roster()
            .await
            .unwrap()
            .filter(|r| r.members.len() == 3)
    })
    .await;

    a.write("x.md", "a1");
    a.reconcile().await;
    b.settle("a1 on bea's", |s| s.read("x.md").as_deref() == Some("a1"))
        .await;
    b.write("x.md", "b2");
    b.reconcile().await;
    c.settle("b2 on cy's", |s| s.read("x.md").as_deref() == Some("b2"))
        .await;
    c.write("x.md", "c3");
    c.reconcile().await;
    for side in [&mut a, &mut b] {
        side.settle("c3 everywhere", |s| s.read("x.md").as_deref() == Some("c3"))
            .await;
    }
    for side in [&a, &b, &c] {
        has_entries(side, "x.md", 3).await;
    }
    for side in [&mut a, &mut b, &mut c] {
        let events = side.reconcile().await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FolderEvent::NewConflict { .. })),
            "{events:?}"
        );
        assert!(copies_of(side, "x.md").is_empty());
        assert!(side.folder.attention().is_empty());
        assert_eq!(side.read("x.md").as_deref(), Some("c3"));
    }
    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_readers_held_edit_publishes_after_promotion() {
    let mut o = owner().await;
    let mut r = joiner(&o, "rae", Role::Reader).await;
    o.write("x.md", "one");
    o.reconcile().await;
    r.settle("x.md on the reader", |s| s.read("x.md").is_some())
        .await;
    r.write("x.md", "mine");
    r.reconcile().await;
    assert_eq!(
        r.folder.attention(),
        vec![Attention::ReaderEdit { path: rel("x.md") }]
    );

    let promote = SignedChange::sign(
        &o.node.id(),
        Change::SetRole {
            person: r.key.id(),
            role: Role::Writer,
            kept: Default::default(),
        },
        &o.key,
        now(),
    );
    o.node.write_change(&promote).await.unwrap();
    o.node
        .send_promoted(&r.node.endpoint_id())
        .await
        .expect("promoted");
    assert!(r.node.can_write());

    // Once the reader's machine has the new roster, its held edit goes out.
    let deadline = tokio::time::Instant::now() + WAIT;
    while r.remote_hash("x.md").await != Some(Some(hash("mine"))) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the edit never went out"
        );
        r.reconcile().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(r.read("x.md").as_deref(), Some("mine"));
    assert!(r.folder.attention().is_empty());
    o.settle("the edit on the owner", |s| {
        s.read("x.md").as_deref() == Some("mine")
    })
    .await;
    o.shutdown().await;
    r.shutdown().await;
}

/// Makes `side`'s next pass save `text` to `p` once, after the scan and just before `p`'s
/// decision is carried out.
fn save_during_next_pass(side: &mut Side, p: &str, text: &'static str) {
    let target = side.path(p);
    let path = rel(p);
    let mut armed = true;
    side.folder.before_apply_for_tests(move |at| {
        if armed && *at == path {
            armed = false;
            std::fs::write(&target, text).unwrap();
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_save_during_a_pass_is_not_overwritten_by_an_incoming_edit() {
    let (mut o, mut w) = synced_pair("x.md").await;
    o.write("x.md", "two");
    o.reconcile().await;
    w.hears("x.md", Some(hash("two"))).await;
    save_during_next_pass(&mut w, "x.md", "saved meanwhile");
    w.reconcile().await;
    assert_eq!(w.read("x.md").as_deref(), Some("saved meanwhile"));
    // The next pass sees the save meet the incoming edit, and keeps both.
    w.settle("both versions kept", |s| copies_of(s, "x.md").len() == 1)
        .await;
    let copy = copies_of(&w, "x.md").remove(0);
    let mut kept = vec![w.read("x.md").unwrap(), w.read(&copy).unwrap()];
    kept.sort();
    assert_eq!(kept, vec!["saved meanwhile".to_string(), "two".to_string()]);
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_save_during_a_pass_is_not_removed_by_an_incoming_delete() {
    let (mut o, mut w) = synced_pair("x.md").await;
    o.remove("x.md");
    o.reconcile().await;
    w.hears("x.md", None).await;
    save_during_next_pass(&mut w, "x.md", "saved meanwhile");
    w.reconcile().await;
    assert_eq!(w.read("x.md").as_deref(), Some("saved meanwhile"));
    w.reconcile().await;
    assert_eq!(w.read("x.md").as_deref(), Some("saved meanwhile"));
    assert_eq!(
        w.folder.attention(),
        vec![Attention::EditedAfterDelete { path: rel("x.md") }]
    );
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_run_of_edits_over_another_authors_version_is_not_a_conflict() {
    let (mut o, mut w) = synced_pair("x.md").await;
    // Well past the recent window a parent record carries.
    for i in 0..210 {
        w.write("x.md", &format!("version {i}"));
        w.reconcile().await;
    }
    o.settle("the last version on the owner", |s| {
        s.read("x.md").as_deref() == Some("version 209")
    })
    .await;
    has_entries(&o, "x.md", 2).await;
    for side in [&mut o, &mut w] {
        let events = side.reconcile().await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FolderEvent::NewConflict { .. })),
            "{events:?}"
        );
        assert!(copies_of(side, "x.md").is_empty());
        assert!(side.folder.attention().is_empty());
    }
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_local_spelling_that_differs_by_case_publishes_under_the_swarms() {
    let (mut o, mut w) = synced_pair("Notes.md").await;
    // An editor saves the file back under another case.
    w.remove("Notes.md");
    w.write("notes.md", "two");
    w.reconcile().await;
    assert_eq!(w.remote_hash("Notes.md").await, Some(Some(hash("two"))));
    assert_eq!(w.remote_hash("notes.md").await, None);
    o.settle("the edit on the owner", |s| {
        s.read("Notes.md").as_deref() == Some("two")
    })
    .await;
    assert_eq!(o.remote_hash("notes.md").await, None);
    o.shutdown().await;
    w.shutdown().await;
}

/// `p` in any case, as Windows and macOS would find it.
fn read_any_case(side: &Side, p: &str) -> Option<String> {
    let dir = side.path(p).parent().unwrap().to_path_buf();
    let name = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .find(|n| n.eq_ignore_ascii_case(p))?;
    side.read(&name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_keys_differing_only_by_case_keep_both() {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    cut_off(&o, &w);
    o.write("Plan.md", "olive's");
    o.reconcile().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    w.write("plan.md", "wren's");
    w.reconcile().await;
    reconnect(&o, &w).await;
    for side in [&o, &w] {
        has_entries(side, "Plan.md", 1).await;
        has_entries(side, "plan.md", 1).await;
    }

    // The writer's key is newer and owns the file; the owner's content becomes its copy.
    let roster = o.roster().await;
    let at = o.node.file_entries(&roster).await.unwrap()[&rel("Plan.md")].entries[0].at;
    let when = Timestamp::from_microsecond(at as i64).unwrap();
    let copy = conflict_name(&rel("plan.md"), "olive-pc", when, &TimeZone::system());
    for side in [&mut o, &mut w] {
        side.settle("both versions kept", |s| {
            read_any_case(s, "plan.md").as_deref() == Some("wren's")
                && s.read(copy.as_str()).as_deref() == Some("olive's")
        })
        .await;
    }
    for side in [&mut o, &mut w] {
        side.reconcile().await;
        assert_eq!(copies_of(side, "plan.md"), vec![copy.as_str().to_string()]);
        assert_eq!(
            side.folder.attention(),
            vec![Attention::Conflict {
                path: rel("plan.md"),
                copy: copy.clone()
            }]
        );
    }
    o.shutdown().await;
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_cannot_resolve() {
    let mut o = owner().await;
    let mut r = joiner(&o, "rae", Role::Reader).await;
    o.write("x.md", "one");
    o.reconcile().await;
    r.settle("x.md on the reader", |s| s.read("x.md").is_some())
        .await;
    let copy = conflict_name(
        &rel("x.md"),
        "m",
        "2026-10-27T13:05:00Z".parse().unwrap(),
        &TimeZone::UTC,
    );
    let refused = r.folder.keep_theirs(&r.node, &rel("x.md"), &copy).await;
    assert_eq!(
        refused.unwrap_err().to_string(),
        "This machine can only read this swarm."
    );
    o.shutdown().await;
    r.shutdown().await;
}

/// How a writer stops being one.
#[derive(Clone, Copy)]
enum Retire {
    Leave,
    Remove,
    Demote,
}

/// The change that takes `w` off writing, signed by `w` for a leave and by `o` otherwise.
async fn retirement(how: Retire, o: &Side, w: &Side) -> (SignedChange, &'static str) {
    let ns = o.node.id();
    let person = w.key.id();
    let signer = match how {
        Retire::Leave => w,
        _ => o,
    };
    let kept = signer.node.kept(&person).await.unwrap();
    let (change, what) = match how {
        Retire::Leave => (Change::Leave { person, kept }, "leave"),
        Retire::Remove => (Change::Remove { person, kept }, "remove"),
        Retire::Demote => {
            let role = Role::Reader;
            (Change::SetRole { person, role, kept }, "demote")
        }
    };
    (SignedChange::sign(&ns, change, &signer.key, now()), what)
}

/// `o`'s roster with `w` put back as a writer, to see what `w` has written since.
async fn as_if_writer(o: &Side, w: &Side, mut member: Member) -> Roster {
    let mut roster = o.roster().await;
    member.role = Role::Writer;
    roster.members.retain(|m| m.person != w.key.id());
    roster.members.push(member);
    roster
}

/// A writer edits x.md and deletes y.md, which reach both remaining members, and then stops
/// being a writer. The remaining folders keep the edit and the delete, and nothing the writer
/// writes afterwards gets in.
async fn retiring_keeps_past_work(how: Retire) {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    let mut v = joiner(&o, "vic", Role::Writer).await;
    o.write("x.md", "one");
    o.write("y.md", "doomed");
    o.reconcile().await;
    for side in [&mut w, &mut v] {
        side.settle("both files", |s| {
            s.read("x.md").is_some() && s.read("y.md").is_some()
        })
        .await;
    }
    w.write("x.md", "two");
    w.remove("y.md");
    w.reconcile().await;
    for side in [&mut o, &mut v] {
        side.settle("the writer's edit and delete", |s| {
            s.read("x.md").as_deref() == Some("two") && s.read("y.md").is_none()
        })
        .await;
    }
    let wren = o
        .roster()
        .await
        .members
        .into_iter()
        .find(|m| m.person == w.key.id())
        .unwrap();

    let (change, what) = retirement(how, &o, &w).await;
    match how {
        Retire::Leave => w.node.write_change(&change).await.unwrap(),
        _ => o.node.write_change(&change).await.unwrap(),
    }
    for side in [&o, &v] {
        eventually(
            &format!("the {what} on every remaining machine"),
            || async {
                let r = side.roster().await;
                r.role_of(&w.key.id())
                    .is_none_or(|r| r < Role::Writer)
                    .then_some(())
            },
        )
        .await;
    }
    for side in [&mut o, &mut v] {
        for _ in 0..3 {
            side.reconcile().await;
        }
        assert_eq!(
            side.read("x.md").as_deref(),
            Some("two"),
            "after the {what}"
        );
        assert_eq!(side.read("y.md"), None, "after the {what}");
    }

    // The writer's machine still holds the write capability; what it writes now stays out.
    w.node
        .put_file(&rel("x.md"), Bytes::from_static(b"three"))
        .await
        .unwrap();
    w.node
        .put_file(&rel("z.md"), Bytes::from_static(b"new"))
        .await
        .unwrap();
    for side in [&o, &v] {
        eventually("the writer's later entries in the document", || async {
            let roster = as_if_writer(side, &w, wren.clone()).await;
            let files = side.node.files(&roster).await.unwrap();
            let x = files.get(&rel("x.md"))?.hash;
            let z = files.get(&rel("z.md"))?.hash;
            (x == Some(hash("three")) && z == Some(hash("new"))).then_some(())
        })
        .await;
    }
    for side in [&mut o, &mut v] {
        for _ in 0..3 {
            side.reconcile().await;
        }
        assert_eq!(
            side.read("x.md").as_deref(),
            Some("two"),
            "after the {what}"
        );
        assert_eq!(side.read("y.md"), None, "after the {what}");
        assert_eq!(side.read("z.md"), None, "after the {what}");
    }
    for side in [o, w, v] {
        side.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_who_leaves_keeps_their_past_edits_and_deletes() {
    retiring_keeps_past_work(Retire::Leave).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_writers_past_edits_and_deletes_stay() {
    retiring_keeps_past_work(Retire::Remove).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_demoted_writers_past_edits_and_deletes_stay() {
    retiring_keeps_past_work(Retire::Demote).await;
}

/// The time of `who`'s entry for `p`, as `side` holds it.
async fn entry_time(side: &Side, who: &Side, p: &str) -> Timestamp {
    let roster = side.roster().await;
    let author = who.node.author();
    let at = side.node.file_entries(&roster).await.unwrap()[&rel(p)]
        .entries
        .iter()
        .find(|e| e.author == author)
        .expect("their entry")
        .at;
    Timestamp::from_microsecond(at as i64).unwrap()
}

/// Reconciles until the folder holds `p` as `now` and `copy` as `kept`, and checks the conflict
/// is listed. Gives whether this machine raised it (rather than receiving the copy).
async fn kept_as_copy(side: &mut Side, p: &str, now: &str, copy: &RelPath, kept: &str) -> bool {
    let events = side
        .settle("the version kept as a copy", |s| {
            s.read(p).as_deref() == Some(now) && s.read(copy.as_str()).as_deref() == Some(kept)
        })
        .await;
    assert!(side.folder.attention().contains(&Attention::Conflict {
        path: rel(p),
        copy: copy.clone()
    }));
    events.contains(&FolderEvent::NewConflict {
        path: rel(p),
        copy: copy.clone(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_the_remover_had_not_received_is_kept_as_a_flagged_copy() {
    let mut o = owner().await;
    let mut w = joiner(&o, "wren", Role::Writer).await;
    let mut v = joiner(&o, "vic", Role::Writer).await;
    o.write("x.md", "one");
    o.reconcile().await;
    for side in [&mut w, &mut v] {
        side.settle("x.md", |s| s.read("x.md").as_deref() == Some("one"))
            .await;
    }
    // What the owner holds of wren's work before wren's edit reaches it.
    let kept = o.node.kept(&w.key.id()).await.unwrap();
    w.write("x.md", "two");
    w.reconcile().await;
    for side in [&mut o, &mut v] {
        side.settle("wren's edit", |s| s.read("x.md").as_deref() == Some("two"))
            .await;
    }
    let when = entry_time(&o, &w, "x.md").await;
    let remove = SignedChange::sign(
        &o.node.id(),
        Change::Remove {
            person: w.key.id(),
            kept,
        },
        &o.key,
        now(),
    );
    o.node.write_change(&remove).await.unwrap();
    let copy = conflict_name(&rel("x.md"), "wren-pc", when, &TimeZone::system());
    // The owner raises it; vic may raise it too or receive the owner's copy first.
    assert!(kept_as_copy(&mut o, "x.md", "one", &copy, "two").await);
    kept_as_copy(&mut v, "x.md", "one", &copy, "two").await;
    assert_eq!(copies_of(&v, "x.md"), vec![copy.as_str().to_string()]);
    for side in [o, w, v] {
        side.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unseen_members_edit_is_kept_as_a_flagged_copy() {
    let mut o = owner().await;
    let mut p = joiner(&o, "pat", Role::Owner).await;
    o.write("notes.md", "one");
    o.reconcile().await;
    p.settle("notes.md", |s| s.read("notes.md").as_deref() == Some("one"))
        .await;
    // Olive's view of pat before pat adds alice, as when olive removes pat without having
    // heard of alice.
    let kept = o.node.kept(&p.key.id()).await.unwrap();
    let mut a = joiner_of(&p, o.key.id(), "alice", Role::Writer).await;
    a.settle("notes.md", |s| s.read("notes.md").as_deref() == Some("one"))
        .await;
    a.write("notes.md", "alice's");
    a.reconcile().await;
    o.settle("alice's edit", |s| {
        s.read("notes.md").as_deref() == Some("alice's")
    })
    .await;
    let when = entry_time(&o, &a, "notes.md").await;
    let remove = SignedChange::sign(
        &o.node.id(),
        Change::Remove {
            person: p.key.id(),
            kept,
        },
        &o.key,
        now(),
    );
    o.node.write_change(&remove).await.unwrap();
    assert_eq!(o.roster().await.role_of(&a.key.id()), None);
    let copy = conflict_name(&rel("notes.md"), "alice-pc", when, &TimeZone::system());
    assert!(kept_as_copy(&mut o, "notes.md", "one", &copy, "alice's").await);
    for side in [o, p, a] {
        side.shutdown().await;
    }
}
