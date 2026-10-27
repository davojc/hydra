//! Swarm nodes talking over real iroh endpoints, relays off, on loopback.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use hydra_swarms::ids::{PersonId, PersonKey};
use hydra_swarms::node::{NodeConfig, SwarmNode};
use hydra_swarms::paths::{RelPath, safe_rel_path};
use hydra_swarms::roster::{Change, Machine, Member, Role, Roster, SignedChange};
use iroh_docs::{Author, Capability};

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

fn config(
    dir: &Path,
    machine_key: [u8; 32],
    person: PersonKey,
    anchor: Option<PersonId>,
) -> NodeConfig {
    NodeConfig {
        dir: dir.to_path_buf(),
        machine_key,
        relays: false,
        person,
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

struct Owner {
    node: SwarmNode,
    key: PersonKey,
    _dir: tempfile::TempDir,
}

async fn create_owner() -> Owner {
    let dir = tempfile::tempdir().unwrap();
    let key = PersonKey::generate();
    let me = Member {
        person: key.id(),
        name: "olive".into(),
        role: Role::Owner,
        machines: vec![Machine {
            endpoint: String::new(),
            name: "olive-pc".into(),
            author: String::new(),
        }],
    };
    let cfg = config(
        dir.path(),
        random32(),
        PersonKey::from_bytes(key.to_bytes()),
        None,
    );
    let node = SwarmNode::create(cfg, "Notes", me).await.expect("create");
    Owner {
        node,
        key,
        _dir: dir,
    }
}

struct Joiner {
    node: SwarmNode,
    key: PersonKey,
    _dir: tempfile::TempDir,
}

/// A machine whose person the owner adds with `role` (when given) before it joins with `cap`.
async fn join(owner: &Owner, name: &str, role: Option<Role>, cap: Capability) -> Joiner {
    let dir = tempfile::tempdir().unwrap();
    let key = PersonKey::generate();
    let machine_key = random32();
    let endpoint = iroh::SecretKey::from_bytes(&machine_key)
        .public()
        .to_string();
    let author = Author::from_bytes(&random32());
    if let Some(role) = role {
        let member = Member {
            person: key.id(),
            name: name.into(),
            role,
            machines: vec![Machine {
                endpoint,
                name: format!("{name}-pc"),
                author: author.id().to_string(),
            }],
        };
        let add = SignedChange::sign(
            &owner.node.id(),
            Change::AddMember { member },
            &owner.key,
            now(),
        );
        owner.node.write_change(&add).await.expect("add member");
    }
    let cfg = config(
        dir.path(),
        machine_key,
        PersonKey::from_bytes(key.to_bytes()),
        Some(owner.key.id()),
    );
    let node = SwarmNode::join(cfg, cap, author).await.expect("join");
    Joiner {
        node,
        key,
        _dir: dir,
    }
}

/// Lets `a` and `b` reach each other on loopback, opens both gates to them, and starts syncing.
async fn connect(a: &SwarmNode, b: &SwarmNode) {
    a.remember_addr(b.addr_for_tests());
    b.remember_addr(a.addr_for_tests());
    a.set_gate(&[b.endpoint_id()]);
    b.set_gate(&[a.endpoint_id()]);
    b.sync_with(&[a.endpoint_id()]).await.expect("sync");
}

async fn roster_with(node: &SwarmNode, members: usize) -> Roster {
    eventually("the roster", || async {
        node.roster()
            .await
            .unwrap()
            .filter(|r| r.members.len() == members)
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_created_swarm_has_its_creator_as_owner() {
    let o = create_owner().await;
    let r = o.node.roster().await.unwrap().expect("a roster");
    assert_eq!(r.name, "Notes");
    assert_eq!(r.role_of(&o.key.id()), Some(Role::Owner));
    let m = &r.members[0].machines[0];
    assert_eq!(m.endpoint, o.node.endpoint_id());
    assert_eq!(m.author, o.node.author());
    assert_eq!(m.name, "olive-pc");
    assert!(o.node.can_write());
    o.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_put_by_a_writer_reaches_another_writer() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap).await;
    connect(&o.node, &w.node).await;

    let r = roster_with(&w.node, 2).await;
    assert_eq!(r.role_of(&w.key.id()), Some(Role::Writer));
    let hash = w
        .node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();

    let r = o.node.roster().await.unwrap().unwrap();
    let got = eventually("x.md on the owner", || async {
        let files = o.node.files(&r).await.unwrap();
        let remote = files.get(&rel("x.md"))?;
        let bytes = o.node.read_blob(remote.hash?).await.unwrap()?;
        Some((remote.hash, remote.machine.clone(), bytes))
    })
    .await;
    assert_eq!(got.0, Some(hash));
    assert_eq!(got.1, "wren-pc");
    assert_eq!(&got.2[..], b"hello");
    o.node.shutdown().await;
    w.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_reaches_the_other_side_as_a_tombstone() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap).await;
    connect(&o.node, &w.node).await;
    let r = roster_with(&w.node, 2).await;

    o.node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();
    eventually("x.md on the writer", || async {
        w.node.files(&r).await.unwrap().get(&rel("x.md"))?.hash
    })
    .await;
    o.node.delete_file(&rel("x.md")).await.unwrap();
    let gone = eventually("the tombstone on the writer", || async {
        let files = w.node.files(&r).await.unwrap();
        let remote = files.get(&rel("x.md"))?;
        remote.hash.is_none().then(|| remote.author.clone())
    })
    .await;
    assert_eq!(gone, o.node.author());
    o.node.shutdown().await;
    w.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_cannot_write() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Reader).await.unwrap();
    assert!(matches!(cap, Capability::Read(_)));
    let r = join(&o, "rae", Some(Role::Reader), cap).await;
    assert!(!r.node.can_write());
    assert!(
        r.node
            .put_file(&rel("x.md"), Bytes::from_static(b"hi"))
            .await
            .is_err()
    );
    assert!(r.node.delete_file(&rel("x.md")).await.is_err());
    o.node.shutdown().await;
    r.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entries_from_an_author_who_is_not_a_writer_are_ignored() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap).await;
    connect(&o.node, &w.node).await;
    roster_with(&w.node, 2).await;

    w.node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();
    let r = o.node.roster().await.unwrap().unwrap();
    eventually("x.md on the owner", || async {
        o.node.files(&r).await.unwrap().get(&rel("x.md"))?.hash
    })
    .await;

    let demote = SignedChange::sign(
        &o.node.id(),
        Change::SetRole {
            person: w.key.id(),
            role: Role::Reader,
        },
        &o.key,
        now(),
    );
    o.node.write_change(&demote).await.unwrap();
    let r = o.node.roster().await.unwrap().unwrap();
    assert_eq!(r.role_of(&w.key.id()), Some(Role::Reader));
    assert!(!o.node.files(&r).await.unwrap().contains_key(&rel("x.md")));
    o.node.shutdown().await;
    w.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_outside_the_gate_cannot_sync() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap.clone()).await;
    connect(&o.node, &w.node).await;
    let r = roster_with(&w.node, 2).await;
    o.node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();

    // Holds the write capability, but nobody added it to the roster or a gate.
    let stranger = join(&o, "sly", None, cap).await;
    stranger.node.remember_addr(o.node.addr_for_tests());
    stranger.node.remember_addr(w.node.addr_for_tests());
    stranger
        .node
        .set_gate(&[o.node.endpoint_id(), w.node.endpoint_id()]);
    stranger
        .node
        .sync_with(&[o.node.endpoint_id(), w.node.endpoint_id()])
        .await
        .expect("sync");

    // The gated-in writer still gets it, so the swarm itself is syncing.
    eventually("x.md on the writer", || async {
        w.node.files(&r).await.unwrap().get(&rel("x.md"))?.hash
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        assert!(stranger.node.files(&r).await.unwrap().is_empty());
        assert!(stranger.node.roster().await.unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    o.node.shutdown().await;
    w.node.shutdown().await;
    stranger.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reopened_swarm_keeps_its_files_and_roster() {
    let dir = tempfile::tempdir().unwrap();
    let key = PersonKey::generate();
    let machine_key = random32();
    let me = Member {
        person: key.id(),
        name: "olive".into(),
        role: Role::Owner,
        machines: vec![Machine {
            endpoint: String::new(),
            name: "olive-pc".into(),
            author: String::new(),
        }],
    };
    let cfg = config(
        dir.path(),
        machine_key,
        PersonKey::from_bytes(key.to_bytes()),
        None,
    );
    let node = SwarmNode::create(cfg, "Notes", me).await.unwrap();
    let hash = node
        .put_file(&rel("x.md"), Bytes::from_static(b"kept"))
        .await
        .unwrap();
    let (ns, author, before) = (
        node.id(),
        node.author(),
        node.roster().await.unwrap().unwrap(),
    );
    node.shutdown().await;

    let cfg = config(
        dir.path(),
        machine_key,
        PersonKey::from_bytes(key.to_bytes()),
        None,
    );
    let node = SwarmNode::open(cfg, ns.parse().unwrap())
        .await
        .expect("open");
    assert_eq!(node.id(), ns);
    assert_eq!(node.author(), author);
    assert!(node.can_write());
    let r = node.roster().await.unwrap().expect("a roster");
    assert_eq!(r, before);
    let files = node.files(&r).await.unwrap();
    assert_eq!(files.get(&rel("x.md")).and_then(|f| f.hash), Some(hash));
    assert_eq!(&node.read_blob(hash).await.unwrap().unwrap()[..], b"kept");
    node.shutdown().await;
}
