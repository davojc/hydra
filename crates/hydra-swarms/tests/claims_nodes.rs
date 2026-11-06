//! Claim records and the ask step, over real iroh endpoints with relays off, on loopback.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use hydra_swarms::claims::{
    ClaimRecord, ContextSettings, DoneRecord, Holder, HolderKind, KeyState, People, Records,
    ReleaseRecord, Take, decide_take, settle,
};
use hydra_swarms::ids::{PersonId, PersonKey};
use hydra_swarms::node::{NodeConfig, NodeEvent, SwarmNode};
use hydra_swarms::roster::{Change, Machine, Member, Role, Roster, SignedChange};
use hydra_swarms::wire::ClaimAnswer;
use iroh_blobs::Hash;
use iroh_docs::{Author, Capability};

const WAIT: Duration = Duration::from_secs(20);
const ASK: Duration = Duration::from_secs(5);
const SLACK: Duration = Duration::from_secs(30);
const SEC: u64 = 1_000_000;
const CONTEXT: &str = "inbox";

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("random bytes");
    b
}

fn now() -> u64 {
    jiff::Timestamp::now().as_microsecond() as u64
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

struct Peer {
    node: SwarmNode,
    _dir: tempfile::TempDir,
}

/// An owner `a` and a machine `b` it added with `role`, synced, with the roster both now hold.
async fn pair(role: Role) -> (Peer, Peer, Roster) {
    let a_dir = tempfile::tempdir().unwrap();
    let a_key = PersonKey::generate();
    let me = Member {
        person: a_key.id(),
        name: "olive".into(),
        role: Role::Owner,
        machines: vec![Machine {
            endpoint: String::new(),
            name: "olive-pc".into(),
            author: String::new(),
        }],
    };
    let cfg = config(
        a_dir.path(),
        random32(),
        PersonKey::from_bytes(a_key.to_bytes()),
        None,
    );
    let a = SwarmNode::create(cfg, "Notes", me).await.expect("create");

    let cap: Capability = a.capability(role).await.unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let b_key = PersonKey::generate();
    let machine_key = random32();
    let author = Author::from_bytes(&random32());
    let member = Member {
        person: b_key.id(),
        name: "wren".into(),
        role,
        machines: vec![Machine {
            endpoint: iroh::SecretKey::from_bytes(&machine_key)
                .public()
                .to_string(),
            name: "wren-pc".into(),
            author: author.id().to_string(),
        }],
    };
    let add = SignedChange::sign(&a.id(), Change::AddMember { member }, &a_key, now());
    a.write_change(&add).await.expect("add member");
    let cfg = config(
        b_dir.path(),
        machine_key,
        PersonKey::from_bytes(b_key.to_bytes()),
        Some(a_key.id()),
    );
    let b = SwarmNode::join(cfg, cap, author).await.expect("join");

    a.remember_addr(b.addr_for_tests());
    b.remember_addr(a.addr_for_tests());
    a.set_gate(&[b.endpoint_id()]);
    b.set_gate(&[a.endpoint_id()]);
    b.sync_with(&[a.endpoint_id()]).await.expect("sync");
    let roster = eventually("the roster", || async {
        b.roster().await.unwrap().filter(|r| r.members.len() == 2)
    })
    .await;
    (
        Peer {
            node: a,
            _dir: a_dir,
        },
        Peer {
            node: b,
            _dir: b_dir,
        },
        roster,
    )
}

async fn hold_back_sync(a: &SwarmNode, b: &SwarmNode) {
    a.leave_sync_for_tests().await.unwrap();
    b.leave_sync_for_tests().await.unwrap();
}

async fn sync_again(a: &SwarmNode, b: &SwarmNode) {
    a.sync_with(&[b.endpoint_id()]).await.unwrap();
    b.sync_with(&[a.endpoint_id()]).await.unwrap();
}

fn claim_at(key: &str, claimed_at: u64) -> ClaimRecord {
    ClaimRecord {
        context: CONTEXT.into(),
        key: key.into(),
        claimed_at,
        renewed_at: now(),
        limit: 1,
        holder: Holder {
            kind: HolderKind::Session,
            label: "session".into(),
        },
    }
}

fn done_at(key: &str, at: u64) -> DoneRecord {
    DoneRecord {
        context: CONTEXT.into(),
        key: key.into(),
        at,
    }
}

async fn records(node: &SwarmNode) -> Records {
    node.claim_records(CONTEXT).await.unwrap()
}

async fn settled(node: &SwarmNode, roster: &Roster) -> BTreeMap<String, KeyState> {
    settle(
        &records(node).await,
        &People::from_roster(roster),
        &ContextSettings::default(),
        now(),
        SLACK,
    )
}

fn holder_of(states: &BTreeMap<String, KeyState>, key: &str) -> Option<String> {
    match states.get(key) {
        Some(KeyState::Held { author, .. }) => Some(author.clone()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_claims_at_once_settle_on_one_with_sync_held_back() {
    let (a, b, roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let (claim_a, claim_b) = (claim_at("k", 200), claim_at("k", 100));

    let (wrote_a, wrote_b) =
        tokio::join!(a.node.write_claim(&claim_a), b.node.write_claim(&claim_b));
    wrote_a.unwrap();
    wrote_b.unwrap();
    let to_b = [b.node.endpoint_id()];
    let to_a = [a.node.endpoint_id()];
    let (from_b, from_a) = tokio::join!(
        a.node.ask_claim(&to_b, &claim_a, ASK),
        b.node.ask_claim(&to_a, &claim_b, ASK)
    );
    assert_eq!((from_b.len(), from_a.len()), (1, 1));
    a.node.take_answers(from_b, &roster);
    b.node.take_answers(from_a, &roster);

    let winner = Some(b.node.author());
    assert_eq!(holder_of(&settled(&a.node, &roster).await, "k"), winner);
    assert_eq!(holder_of(&settled(&b.node, &roster).await, "k"), winner);
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_that_arrives_first_stops_the_other_claiming() {
    let (a, b, roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let mut heard_on_b = b.node.events();

    let claim = claim_at("k", now());
    a.node.write_claim(&claim).await.unwrap();
    let answers = a.node.ask_claim(&[b.node.endpoint_id()], &claim, ASK).await;
    assert_eq!(answers.len(), 1);

    let take = decide_take(
        &settled(&b.node, &roster).await,
        "k",
        &b.node.author(),
        &ContextSettings::default(),
    );
    assert!(
        matches!(&take, Take::Held { author, .. } if *author == a.node.author()),
        "{take:?}"
    );
    tokio::time::timeout(WAIT, async {
        while heard_on_b.recv().await.unwrap() != NodeEvent::ClaimHeard {}
    })
    .await
    .expect("asking tells the node it heard a claim");
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_whose_content_comes_late_is_told_as_a_claim_change() {
    let (a, b, _roster) = pair(Role::Writer).await;
    let mut events = b.node.events();
    let claim = claim_at("k", now());
    let blob = Hash::new(serde_json::to_vec(&claim).unwrap());
    a.node.write_claim(&claim).await.unwrap();

    // The entry comes first, its content after. A read made on the first event can miss the
    // content, so there has to be a second event, and nothing to wake the folder for it.
    let told = async {
        let mut changes = 0;
        loop {
            match events.recv().await.unwrap() {
                NodeEvent::ClaimsChanged => changes += 1,
                NodeEvent::ContentReady(hash) => {
                    assert_ne!(hash, blob, "a claim's content looked like a file's")
                }
                _ => continue,
            }
            if changes >= 2 {
                break;
            }
        }
    };
    tokio::time::timeout(WAIT, told)
        .await
        .expect("told of the entry and then of its content");
    // Only what the events prompted has been read, and the claim is in it.
    let seen = records(&b.node).await;
    assert!(seen.claims.iter().any(|(_, c)| *c == claim), "{seen:?}");
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_silent_member_is_passed_over() {
    let (a, b, _roster) = pair(Role::Writer).await;
    b.node.silent_claims_for_tests();

    let started = Instant::now();
    let answers = a
        .node
        .ask_claim(
            &[b.node.endpoint_id()],
            &claim_at("k", now()),
            Duration::from_millis(500),
        )
        .await;
    assert!(answers.is_empty());
    assert!(started.elapsed() < Duration::from_secs(2));
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tombstone_clears_a_heard_claim() {
    let (a, b, roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let claim = claim_at("k", now());
    a.node.write_claim(&claim).await.unwrap();
    a.node.ask_claim(&[b.node.endpoint_id()], &claim, ASK).await;
    assert_eq!(
        holder_of(&settled(&b.node, &roster).await, "k"),
        Some(a.node.author())
    );

    sync_again(&a.node, &b.node).await;
    a.node.delete_claim(CONTEXT, "k").await.unwrap();

    // Long before the claim could lapse, which takes minutes.
    eventually("the key to come free on b", || async {
        (!settled(&b.node, &roster).await.contains_key("k")).then_some(())
    })
    .await;
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_made_after_a_release_outlives_the_older_tombstone() {
    let (a, b, roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    a.node.write_claim(&claim_at("k", now())).await.unwrap();
    a.node.delete_claim(CONTEXT, "k").await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    // Claimed again, and heard by b before the document says anything but the tombstone.
    let again = claim_at("k", now());
    a.node.ask_claim(&[b.node.endpoint_id()], &again, ASK).await;

    a.node.write_done(&done_at("marker", now())).await.unwrap();
    sync_again(&a.node, &b.node).await;
    eventually("what a wrote to reach b", || async {
        settled(&b.node, &roster)
            .await
            .contains_key("marker")
            .then_some(())
    })
    .await;

    assert_eq!(
        holder_of(&settled(&b.node, &roster).await, "k"),
        Some(a.node.author())
    );
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_counts_once_for_a_key_with_its_newest_claim() {
    let (a, b, _roster) = pair(Role::Writer).await;
    let t = now();
    let older = claim_at("k", t - 10 * SEC);
    a.node.write_claim(&older).await.unwrap();
    eventually("the claim on b", || async {
        (!records(&b.node).await.claims.is_empty()).then_some(())
    })
    .await;

    // Heard is newer than the document: heard is the one that counts.
    let newer = claim_at("k", t - 5 * SEC);
    a.node.ask_claim(&[b.node.endpoint_id()], &newer, ASK).await;
    let seen = records(&b.node).await.claims;
    assert_eq!(seen, vec![(a.node.author(), newer)]);

    // The document catches up and passes it.
    let newest = claim_at("k", t);
    a.node.write_claim(&newest).await.unwrap();
    let seen = eventually("the newest claim on b", || async {
        let claims = records(&b.node).await.claims;
        (claims.iter().any(|(_, c)| c.claimed_at == t)).then_some(claims)
    })
    .await;
    assert_eq!(seen, vec![(a.node.author(), newest)]);
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_carries_a_done_the_asker_has_not_synced() {
    let (a, b, roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let at = now();
    b.node.write_done(&done_at("k", at)).await.unwrap();

    let answers = a
        .node
        .ask_claim(&[b.node.endpoint_id()], &claim_at("k", now()), ASK)
        .await;
    a.node.take_answers(answers, &roster);

    assert_eq!(
        settled(&a.node, &roster).await.get("k"),
        Some(&KeyState::Done {
            author: b.node.author(),
            at
        })
    );
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_only_says_what_is_held_for_that_key() {
    let (a, b, _roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let mine = claim_at("k", now());
    b.node.write_claim(&mine).await.unwrap();
    b.node.write_claim(&claim_at("other", now())).await.unwrap();

    let asked = claim_at("k", now());
    let answers = a.node.ask_claim(&[b.node.endpoint_id()], &asked, ASK).await;

    let mut got = answers[0].claims.clone();
    got.sort_by_key(|(author, _)| *author != b.node.author());
    assert_eq!(got, vec![(b.node.author(), mine), (a.node.author(), asked)]);
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_readers_question_is_turned_away() {
    let (a, reader, _roster) = pair(Role::Reader).await;
    let mut heard = a.node.events();

    let answers = reader
        .node
        .ask_claim(&[a.node.endpoint_id()], &claim_at("k", now()), ASK)
        .await;

    assert!(answers.is_empty());
    assert!(records(&a.node).await.claims.is_empty());
    while let Ok(ev) = heard.try_recv() {
        assert_ne!(ev, NodeEvent::ClaimHeard);
    }
    a.node.shutdown().await;
    reader.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_from_anyone_but_a_writer_are_not_kept() {
    let (a, b, roster) = pair(Role::Writer).await;
    let answer = ClaimAnswer {
        claims: vec![("stranger".into(), claim_at("k", now()))],
        dones: vec![("stranger".into(), done_at("j", now()))],
    };
    a.node.take_answers(vec![answer], &roster);
    let held = records(&a.node).await;
    assert!(held.claims.is_empty());
    assert!(held.dones.is_empty());
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heard_claim_is_forgotten_after_an_hour() {
    let (a, b, _roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let t = now();
    let recent = ClaimRecord {
        renewed_at: t - 3590 * SEC,
        ..claim_at("recent", t - 3590 * SEC)
    };
    let old = ClaimRecord {
        renewed_at: t - 3610 * SEC,
        ..claim_at("old", t - 3610 * SEC)
    };
    let to_b = [b.node.endpoint_id()];
    a.node.ask_claim(&to_b, &recent, ASK).await;
    a.node.ask_claim(&to_b, &old, ASK).await;

    let seen = records(&b.node).await.claims;
    assert_eq!(seen, vec![(a.node.author(), recent)]);
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_over_the_limit_is_skipped() {
    let (a, b, roster) = pair(Role::Writer).await;
    hold_back_sync(&a.node, &b.node).await;
    let mut big = claim_at("k", now());
    big.holder.label = "x".repeat(65 * 1024);

    a.node.write_claim(&big).await.unwrap();
    assert!(records(&a.node).await.claims.is_empty());

    a.node.ask_claim(&[b.node.endpoint_id()], &big, ASK).await;
    assert!(records(&b.node).await.claims.is_empty());

    let answer = ClaimAnswer {
        claims: vec![(b.node.author(), big)],
        dones: vec![],
    };
    a.node.take_answers(vec![answer], &roster);
    assert!(records(&a.node).await.claims.is_empty());
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pruning_takes_this_machines_old_entries_and_leaves_the_rest() {
    let (a, b, _roster) = pair(Role::Writer).await;
    let settings = ContextSettings::default();
    let t = now();
    let long_ago = t - settings.keep_done_secs * SEC - SEC;
    let release = |key: &str, at: u64| ReleaseRecord {
        context: CONTEXT.into(),
        key: key.into(),
        target: "someone".into(),
        claimed_at: 1,
        at,
    };
    // Lapsed by the rule settle uses: 3 missed renewals of 60 s, plus slack.
    let lapsed = ClaimRecord {
        renewed_at: t - 211 * SEC,
        ..claim_at("lapsed", t - 211 * SEC)
    };
    let live = ClaimRecord {
        renewed_at: t - 209 * SEC,
        ..claim_at("live", t - 209 * SEC)
    };
    let theirs = ClaimRecord {
        renewed_at: t - 211 * SEC,
        ..claim_at("theirs", t - 211 * SEC)
    };
    a.node.write_claim(&lapsed).await.unwrap();
    a.node.write_claim(&live).await.unwrap();
    a.node.write_done(&done_at("old", long_ago)).await.unwrap();
    a.node.write_done(&done_at("new", t)).await.unwrap();
    a.node
        .write_release(&release("old", long_ago))
        .await
        .unwrap();
    a.node.write_release(&release("new", t)).await.unwrap();
    b.node.write_claim(&theirs).await.unwrap();
    eventually("b's claim on a", || async {
        (records(&a.node).await.claims.len() == 3).then_some(())
    })
    .await;

    a.node
        .prune_claims(CONTEXT, &settings, t, SLACK)
        .await
        .unwrap();

    let held = records(&a.node).await;
    let mut claims: Vec<&str> = held.claims.iter().map(|(_, c)| c.key.as_str()).collect();
    claims.sort();
    assert_eq!(claims, ["live", "theirs"]);
    let dones: Vec<&str> = held.dones.iter().map(|(_, d)| d.key.as_str()).collect();
    assert_eq!(dones, ["new"]);
    let releases: Vec<&str> = held.releases.iter().map(|(_, r)| r.key.as_str()).collect();
    assert_eq!(releases, ["new"]);
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claims_stay_out_of_the_folder_entries() {
    let (a, b, roster) = pair(Role::Writer).await;
    let before = a.node.file_entries_for_tests().await.unwrap();
    assert!(a.node.files(&roster).await.unwrap().is_empty());

    let claim = claim_at("k", now());
    a.node.write_claim(&claim).await.unwrap();
    a.node.write_done(&done_at("j", now())).await.unwrap();

    assert_eq!(a.node.file_entries_for_tests().await.unwrap(), before);
    assert!(a.node.files(&roster).await.unwrap().is_empty());
    assert!(a.node.file_entries(&roster).await.unwrap().is_empty());
    a.node.shutdown().await;
    b.node.shutdown().await;
}
