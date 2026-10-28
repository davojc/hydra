//! Swarm nodes talking over real iroh endpoints, relays off, on loopback.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use hydra_swarms::ids::{PersonId, PersonKey};
use hydra_swarms::invites::{INVITE_LIFETIME, InviteCode, Invites, redeem_at};
use hydra_swarms::node::{NodeConfig, NodeEvent, SwarmNode};
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
    dir: tempfile::TempDir,
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
    Owner { node, key, dir }
}

struct Joiner {
    node: SwarmNode,
    key: PersonKey,
    machine_key: [u8; 32],
    dir: tempfile::TempDir,
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
        machine_key,
        dir,
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
async fn a_demoted_writers_entries_count_only_as_the_demotion_kept_them() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap).await;
    connect(&o.node, &w.node).await;
    roster_with(&w.node, 2).await;

    let kept_hash = w
        .node
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
            kept: o.node.kept(&w.key.id()).await.unwrap(),
        },
        &o.key,
        now(),
    );
    o.node.write_change(&demote).await.unwrap();
    let r = o.node.roster().await.unwrap().unwrap();
    assert_eq!(r.role_of(&w.key.id()), Some(Role::Reader));
    let x = o.node.files(&r).await.unwrap()[&rel("x.md")].clone();
    assert_eq!(x.hash, Some(kept_hash));
    assert_eq!(x.machine, "wren-pc");

    // Still holding the write capability, the demoted writer's later entries stay out.
    w.node
        .put_file(&rel("x.md"), Bytes::from_static(b"later"))
        .await
        .unwrap();
    w.node
        .put_file(&rel("y.md"), Bytes::from_static(b"new"))
        .await
        .unwrap();
    eventually("the later entries on the owner", || async {
        (o.node.file_entries_for_tests().await.unwrap() == 2).then_some(())
    })
    .await;
    let files = o.node.files(&r).await.unwrap();
    assert_eq!(files[&rel("x.md")].hash, Some(kept_hash));
    assert!(!files.contains_key(&rel("y.md")));
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

/// Puts `name` on `from`, then checks for 5 s that `to` never hears of it.
async fn never_arrives(from: &SwarmNode, to: &SwarmNode, roster: &Roster, name: &str) {
    from.put_file(&rel(name), Bytes::from_static(b"after"))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let files = to.files(roster).await.unwrap();
        assert!(
            !files.contains_key(&rel(name)),
            "{name} reached a machine outside the gate"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_dropped_from_the_gate_while_connected_gets_nothing_more() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap).await;
    connect(&o.node, &w.node).await;
    let r = roster_with(&w.node, 2).await;
    o.node
        .put_file(&rel("x.md"), Bytes::from_static(b"before"))
        .await
        .unwrap();
    eventually("x.md on the writer", || async {
        w.node.files(&r).await.unwrap().get(&rel("x.md"))?.hash
    })
    .await;

    // The writer stays connected and keeps the owner in its own gate; only the owner drops it.
    o.node.set_gate(&[]);
    never_arrives(&o.node, &w.node, &r, "y.md").await;
    o.node.shutdown().await;
    w.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reopened_member_never_dials_a_removed_machine() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap.clone()).await;
    let x = join(&o, "xan", Some(Role::Writer), cap).await;
    connect(&o.node, &w.node).await;
    roster_with(&w.node, 3).await;
    // The writer dials xan itself, so iroh-docs keeps xan as a peer of this document.
    w.node.remember_addr(x.node.addr_for_tests());
    x.node.remember_addr(w.node.addr_for_tests());
    w.node
        .set_gate(&[o.node.endpoint_id(), x.node.endpoint_id()]);
    x.node.set_gate(&[w.node.endpoint_id()]);
    w.node.sync_with(&[x.node.endpoint_id()]).await.unwrap();
    let r = roster_with(&x.node, 3).await;

    let remove = SignedChange::sign(
        &o.node.id(),
        Change::Remove {
            person: x.key.id(),
            kept: o.node.kept(&x.key.id()).await.unwrap(),
        },
        &o.key,
        now(),
    );
    o.node.write_change(&remove).await.unwrap();
    roster_with(&w.node, 2).await;
    let ns = w.node.id();
    let Joiner {
        node,
        key,
        machine_key,
        dir,
    } = w;
    node.shutdown().await;

    let cfg = config(
        dir.path(),
        machine_key,
        PersonKey::from_bytes(key.to_bytes()),
        None,
    );
    let w = SwarmNode::open(cfg, ns.parse().unwrap())
        .await
        .expect("open");
    // xan is still reachable and would still take the writer's entries.
    w.remember_addr(o.node.addr_for_tests());
    w.remember_addr(x.node.addr_for_tests());
    x.node.remember_addr(w.addr_for_tests());
    x.node.set_gate(&[w.endpoint_id()]);
    w.put_file(&rel("z.md"), Bytes::from_static(b"after"))
        .await
        .unwrap();
    // Syncing with the current members: iroh-docs adds the peers it synced with before.
    w.sync_with(&[o.node.endpoint_id()]).await.unwrap();
    never_arrives(&w, &x.node, &r, "z2.md").await;
    assert!(!x.node.files(&r).await.unwrap().contains_key(&rel("z.md")));
    o.node.shutdown().await;
    w.shutdown().await;
    x.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_never_dials_a_machine_outside_its_gate() {
    let o = create_owner().await;
    let cap = o.node.capability(Role::Writer).await.unwrap();
    let w = join(&o, "wren", Some(Role::Writer), cap.clone()).await;
    connect(&o.node, &w.node).await;
    let r = roster_with(&w.node, 2).await;
    // Holds the write capability and would take the owner's entries, but isn't in its gate.
    let stranger = join(&o, "sly", None, cap).await;
    stranger.node.set_gate(&[o.node.endpoint_id()]);
    o.node.remember_addr(stranger.node.addr_for_tests());
    o.node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();
    o.node
        .sync_with(&[stranger.node.endpoint_id()])
        .await
        .unwrap();
    never_arrives(&o.node, &stranger.node, &r, "y.md").await;
    assert!(stranger.node.files(&r).await.unwrap().is_empty());
    o.node.shutdown().await;
    w.node.shutdown().await;
    stranger.node.shutdown().await;
}

/// The member a joiner sends: its role is the invite's, and on joining through
/// `join_with_invite` its machine's endpoint and author are filled in.
fn joining_member(key: &PersonKey, name: &str) -> Member {
    Member {
        person: key.id(),
        name: name.into(),
        role: Role::Reader,
        machines: vec![Machine {
            endpoint: String::new(),
            name: format!("{name}-pc"),
            author: Author::from_bytes(&random32()).id().to_string(),
        }],
    }
}

/// A new person's machine joining through `code`, dialling `host` directly.
async fn join_by_code(host: &SwarmNode, code: &InviteCode, name: &str) -> Joiner {
    let dir = tempfile::tempdir().unwrap();
    let key = PersonKey::generate();
    let machine_key = random32();
    let cfg = config(
        dir.path(),
        machine_key,
        PersonKey::from_bytes(key.to_bytes()),
        None,
    );
    let me = joining_member(&key, name);
    let node = SwarmNode::join_with_invite_at(cfg, code, me, host.addr_for_tests())
        .await
        .expect("join with an invite");
    host.remember_addr(node.addr_for_tests());
    Joiner {
        node,
        key,
        machine_key,
        dir,
    }
}

async fn invite_and_join(o: &Owner, name: &str, role: Role) -> Joiner {
    let (_, code) = o.node.invite(role, &o.key).await.expect("invite");
    join_by_code(&o.node, &code, name).await
}

/// Redeems `code` at `host` as a new person, and gives the refusal.
async fn refusal(host: &SwarmNode, code: &InviteCode) -> String {
    let key = PersonKey::generate();
    let me = joining_member(&key, "sid");
    let err = redeem_at(code, me, &key, random32(), host.addr_for_tests())
        .await
        .expect_err("refused");
    err.to_string()
}

fn now_secs() -> u64 {
    jiff::Timestamp::now().as_second() as u64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_lets_a_reader_join_with_read_only_access() {
    let o = create_owner().await;
    o.node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();
    let r = invite_and_join(&o, "rae", Role::Reader).await;
    assert!(!r.node.can_write());
    let roster = roster_with(&r.node, 2).await;
    assert_eq!(roster.role_of(&r.key.id()), Some(Role::Reader));
    let m = roster.member_by_endpoint(&r.node.endpoint_id()).unwrap();
    assert_eq!(m.machines[0].author, r.node.author());
    assert_eq!(m.machines[0].name, "rae-pc");
    let got = eventually("x.md on the reader", || async {
        let files = r.node.files(&roster).await.unwrap();
        r.node
            .read_blob(files.get(&rel("x.md"))?.hash?)
            .await
            .unwrap()
    })
    .await;
    assert_eq!(&got[..], b"hello");
    o.node.shutdown().await;
    r.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_lets_a_writer_join_and_write() {
    let o = create_owner().await;
    let w = invite_and_join(&o, "wren", Role::Writer).await;
    assert!(w.node.can_write());
    roster_with(&w.node, 2).await;
    let hash = w
        .node
        .put_file(&rel("x.md"), Bytes::from_static(b"hello"))
        .await
        .unwrap();
    let r = o.node.roster().await.unwrap().unwrap();
    assert_eq!(r.role_of(&w.key.id()), Some(Role::Writer));
    let got = eventually("x.md on the owner", || async {
        o.node.files(&r).await.unwrap().get(&rel("x.md"))?.hash
    })
    .await;
    assert_eq!(got, hash);
    o.node.shutdown().await;
    w.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_works_once() {
    let o = create_owner().await;
    let (invite, code) = o.node.invite(Role::Reader, &o.key).await.unwrap();
    assert_eq!(o.node.pending_invites(), vec![invite]);
    let r = join_by_code(&o.node, &code, "rae").await;
    assert!(o.node.pending_invites().is_empty());
    assert_eq!(
        refusal(&o.node, &code).await,
        "This invite has already been used."
    );
    assert_eq!(o.node.roster().await.unwrap().unwrap().members.len(), 2);
    o.node.shutdown().await;
    r.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_invite_is_refused() {
    let o = create_owner().await;
    let long_ago = now_secs() - INVITE_LIFETIME - 60;
    let (_, token) = Invites::new(o.dir.path())
        .create(Role::Reader, long_ago)
        .unwrap();
    let code = InviteCode {
        host: o.node.endpoint_id(),
        relay: None,
        token,
    };
    assert_eq!(refusal(&o.node, &code).await, "This invite has expired.");
    assert_eq!(o.node.roster().await.unwrap().unwrap().members.len(), 1);
    o.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_invite_is_refused() {
    let o = create_owner().await;
    let (invite, code) = o.node.invite(Role::Writer, &o.key).await.unwrap();
    o.node.cancel_invite(&invite.id).unwrap();
    assert!(o.node.pending_invites().is_empty());
    assert_eq!(refusal(&o.node, &code).await, "This invite was cancelled.");
    o.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_cannot_make_invites() {
    let o = create_owner().await;
    let w = invite_and_join(&o, "wren", Role::Writer).await;
    roster_with(&w.node, 2).await;
    assert!(w.node.invite(Role::Reader, &w.key).await.is_err());
    assert!(w.node.pending_invites().is_empty());
    o.node.shutdown().await;
    w.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_from_someone_no_longer_an_owner_is_refused() {
    let o = create_owner().await;
    let b = invite_and_join(&o, "bea", Role::Owner).await;
    roster_with(&b.node, 2).await;
    let (_, code) = b.node.invite(Role::Reader, &b.key).await.unwrap();
    let demote = SignedChange::sign(
        &o.node.id(),
        Change::SetRole {
            person: b.key.id(),
            role: Role::Writer,
            kept: o.node.kept(&b.key.id()).await.unwrap(),
        },
        &o.key,
        now(),
    );
    o.node.write_change(&demote).await.unwrap();
    eventually("the demotion on bea's machine", || async {
        let r = b.node.roster().await.unwrap()?;
        (r.role_of(&b.key.id()) == Some(Role::Writer)).then_some(())
    })
    .await;
    assert_eq!(
        refusal(&b.node, &code).await,
        "Whoever invited you is no longer an owner of this swarm."
    );
    o.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_readers_rename_goes_through_an_owner() {
    let o = create_owner().await;
    let w = invite_and_join(&o, "wren", Role::Writer).await;
    let r = invite_and_join(&o, "rae", Role::Reader).await;
    roster_with(&r.node, 3).await;
    let rename = SignedChange::sign(
        &o.node.id(),
        Change::Rename {
            person: r.key.id(),
            name: "Rae B".into(),
        },
        &r.key,
        now(),
    );
    r.node
        .relay(&o.node.endpoint_id(), rename)
        .await
        .expect("relayed");
    for node in [&o.node, &w.node, &r.node] {
        eventually("the new name everywhere", || async {
            let roster = node.roster().await.unwrap()?;
            let m = roster.members.iter().find(|m| m.person == r.key.id())?;
            (m.name == "Rae B").then_some(())
        })
        .await;
    }
    o.node.shutdown().await;
    w.node.shutdown().await;
    r.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_for_someone_else_is_refused() {
    let o = create_owner().await;
    let r = invite_and_join(&o, "rae", Role::Reader).await;
    roster_with(&r.node, 2).await;
    let ns = o.node.id();
    let others = [
        Change::Rename {
            person: o.key.id(),
            name: "Mallory".into(),
        },
        Change::Leave {
            person: o.key.id(),
            kept: Default::default(),
        },
        Change::SetRole {
            person: r.key.id(),
            role: Role::Owner,
            kept: Default::default(),
        },
    ];
    for change in others {
        let c = SignedChange::sign(&ns, change, &r.key, now());
        assert!(r.node.relay(&o.node.endpoint_id(), c).await.is_err());
    }
    let roster = o.node.roster().await.unwrap().unwrap();
    assert_eq!(roster.role_of(&r.key.id()), Some(Role::Reader));
    assert_eq!(roster.role_of(&o.key.id()), Some(Role::Owner));
    assert_eq!(roster.members[0].name, "olive");
    o.node.shutdown().await;
    r.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promoted_reader_can_write() {
    let o = create_owner().await;
    let r = invite_and_join(&o, "rae", Role::Reader).await;
    roster_with(&r.node, 2).await;
    assert!(!r.node.can_write());
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
    let hash = r
        .node
        .put_file(&rel("x.md"), Bytes::from_static(b"mine"))
        .await
        .unwrap();
    let roster = o.node.roster().await.unwrap().unwrap();
    let got = eventually("x.md on the owner", || async {
        o.node.files(&roster).await.unwrap().get(&rel("x.md"))?.hash
    })
    .await;
    assert_eq!(got, hash);
    o.node.shutdown().await;
    r.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_an_owner_can_tell_a_machine_it_was_removed() {
    let o = create_owner().await;
    let r = invite_and_join(&o, "rae", Role::Reader).await;
    roster_with(&r.node, 2).await;
    let mut events = r.node.events();
    let ns = o.node.id();
    let removal = SignedChange::sign(
        &ns,
        Change::Remove {
            person: r.key.id(),
            kept: o.node.kept(&r.key.id()).await.unwrap(),
        },
        &o.key,
        now(),
    );
    assert!(
        r.node
            .send_removed(&o.node.endpoint_id(), &removal)
            .await
            .is_err()
    );
    // A notice whose change doesn't remove this machine's person is only a claim, and refused.
    let rename = SignedChange::sign(
        &ns,
        Change::RenameSwarm {
            name: "Elsewhere".into(),
        },
        &o.key,
        now(),
    );
    assert!(
        o.node
            .send_removed(&r.node.endpoint_id(), &rename)
            .await
            .is_err()
    );
    // The removal itself is proof, even before it reaches this machine's log.
    o.node
        .send_removed(&r.node.endpoint_id(), &removal)
        .await
        .expect("told");
    let got = tokio::time::timeout(WAIT, async {
        while events.recv().await.unwrap() != NodeEvent::Removed {}
    })
    .await;
    assert!(got.is_ok(), "no removal event");
    o.node.shutdown().await;
    r.node.shutdown().await;
}

#[test]
fn invite_codes_round_trip_and_tolerate_whitespace() {
    let code = InviteCode {
        host: iroh::SecretKey::from_bytes(&random32())
            .public()
            .to_string(),
        relay: Some("https://relay.example/".into()),
        token: "ab".repeat(32),
    };
    let text = code.encode();
    assert!(text.starts_with("hydra-swarm:"));
    assert_eq!(InviteCode::decode(&text), Some(code.clone()));
    assert_eq!(
        InviteCode::decode(&format!("  \n{text}\t \r\n")),
        Some(code)
    );
    assert_eq!(InviteCode::decode("hydra-swarm:not base64!"), None);
    assert_eq!(InviteCode::decode(""), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_from_outside_the_swarm_writes_nothing() {
    let o = create_owner().await;
    let r = invite_and_join(&o, "rae", Role::Reader).await;
    roster_with(&r.node, 2).await;
    let cap = o.node.capability(Role::Reader).await.unwrap();
    let stranger = join(&o, "sly", None, cap).await;
    stranger.node.remember_addr(o.node.addr_for_tests());
    let ns = o.node.id();
    let before = o.node.log_len_for_tests().await.unwrap();

    // A stranger renaming itself, and a member's machine passing on someone else's own change.
    let own = |key: &PersonKey| {
        SignedChange::sign(
            &ns,
            Change::Rename {
                person: key.id(),
                name: "Sly".into(),
            },
            key,
            now(),
        )
    };
    for _ in 0..3 {
        let c = own(&stranger.key);
        assert!(stranger.node.relay(&o.node.endpoint_id(), c).await.is_err());
    }
    let c = own(&stranger.key);
    assert!(r.node.relay(&o.node.endpoint_id(), c).await.is_err());
    assert_eq!(o.node.log_len_for_tests().await.unwrap(), before);
    o.node.shutdown().await;
    r.node.shutdown().await;
    stranger.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_join_with_an_overlong_name_leaves_the_invite_unspent() {
    let o = create_owner().await;
    let (invite, code) = o.node.invite(Role::Reader, &o.key).await.unwrap();
    let key = PersonKey::generate();
    let me = joining_member(&key, &"n".repeat(201));
    let err = redeem_at(&code, me, &key, random32(), o.node.addr_for_tests())
        .await
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "Your name and your machine's name can't be blank or too long."
    );
    assert_eq!(o.node.pending_invites(), vec![invite]);
    o.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_join_reusing_a_members_author_leaves_the_invite_unspent() {
    let o = create_owner().await;
    let (invite, code) = o.node.invite(Role::Writer, &o.key).await.unwrap();
    let key = PersonKey::generate();
    let mut me = joining_member(&key, "mallory");
    me.machines[0].author = o.node.author();
    let err = redeem_at(&code, me, &key, random32(), o.node.addr_for_tests())
        .await
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "The joining machine's ids already belong to someone in this swarm."
    );
    assert_eq!(o.node.pending_invites(), vec![invite]);
    o.node.shutdown().await;
}
