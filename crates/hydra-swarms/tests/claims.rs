//! Claims through the service: machines taking keys, keeping them while their handle lives, and
//! being told when they stop being theirs.

mod common;

use std::time::Duration;

use common::*;
use futures::channel::oneshot;
use hydra_swarms::claims::{ContextSettings, Holder, HolderKind};
use hydra_swarms::{ClaimHandle, ClaimOutcome, Command, DirectPeers, LostReason, Role, SwarmView};

/// Fast enough to see a lapse: one renewal a second, two missed, and a second of slack.
const FAST: ContextSettings = ContextSettings {
    renew_secs: 1,
    lapse_after: 2,
    max_per_machine: 1,
    keep_done_secs: 3600,
};
const SLACK: Duration = Duration::from_secs(1);
/// Past the longest a claim in `FAST` can go unrenewed.
const PAST_LAPSE: Duration = Duration::from_secs(5);
/// How long a claim may take to settle when a member can't be reached or doesn't answer.
const WITHIN: Duration = Duration::from_secs(10);

fn holder(label: &str) -> Holder {
    Holder {
        kind: HolderKind::Session,
        label: label.to_string(),
    }
}

/// An owner `olive` with a swarm "Notes", `wren` joined to it with `role`, and the context
/// "inbox" with `settings` showing on both.
async fn claimers(role: Role, settings: ContextSettings) -> (Svc, Svc, String) {
    let book = DirectPeers::new();
    let mut olive = Svc::with_claim_slack("olive", &book, SLACK);
    let mut wren = Svc::with_claim_slack("wren", &book, SLACK);
    let notes = olive.create("Notes", &[]).await;
    let code = olive.invite(&notes.id, role).await;
    wren.join(code, "notes").await;
    let person = wren.person();
    olive
        .view("wren in the members", |v| {
            v.members.iter().any(|m| m.person == person)
        })
        .await;
    set_context(&olive, &notes.id, settings);
    context_is(&mut olive, &notes.id, settings).await;
    context_is(&mut wren, &notes.id, settings).await;
    (olive, wren, notes.id)
}

fn set_context(svc: &Svc, swarm: &str, settings: ContextSettings) {
    svc.send(Command::ClaimContextSet {
        swarm: swarm.to_string(),
        name: "inbox".to_string(),
        settings,
    });
}

async fn context_is(svc: &mut Svc, swarm: &str, settings: ContextSettings) {
    svc.view("the context's settings", |v| {
        v.id == swarm
            && v.claims
                .iter()
                .any(|c| c.name == "inbox" && c.settings == settings)
    })
    .await;
}

async fn ask_to_take(
    svc: &Svc,
    swarm: Option<&str>,
    context: &str,
    key: &str,
    label: &str,
) -> Result<ClaimOutcome, String> {
    let (reply, answer) = oneshot::channel();
    svc.send(Command::ClaimTake {
        swarm: swarm.map(str::to_string),
        context: context.to_string(),
        key: key.to_string(),
        holder: holder(label),
        retake: false,
        reply,
    });
    tokio::time::timeout(WAIT, answer)
        .await
        .expect("an answer in time")
        .expect("a reply")
}

async fn take(svc: &Svc, swarm: &str, key: &str) -> ClaimOutcome {
    ask_to_take(svc, Some(swarm), "inbox", key, "a session")
        .await
        .expect("no error")
}

async fn yours(svc: &Svc, swarm: &str, key: &str) -> ClaimHandle {
    match take(svc, swarm, key).await {
        ClaimOutcome::Yours(handle) => handle,
        other => panic!("expected the claim, got {other:?}"),
    }
}

async fn release(svc: &Svc, swarm: &str, key: &str, author: &str) -> Result<(), String> {
    let (reply, answer) = oneshot::channel();
    svc.send(Command::ClaimRelease {
        swarm: swarm.to_string(),
        context: "inbox".to_string(),
        key: key.to_string(),
        author: author.to_string(),
        reply,
    });
    answer.await.expect("a reply")
}

async fn reopen(svc: &Svc, swarm: &str, key: &str) -> Result<(), String> {
    let (reply, answer) = oneshot::channel();
    svc.send(Command::ClaimReopen {
        swarm: swarm.to_string(),
        context: "inbox".to_string(),
        key: key.to_string(),
        reply,
    });
    answer.await.expect("a reply")
}

/// The author of the claim on `key`, once `svc` knows of it.
async fn author_of(svc: &mut Svc, swarm: &str, key: &str) -> String {
    let v = svc
        .view("the claim in the view", |v| {
            v.id == swarm && v.claims[0].live.iter().any(|c| c.key == key)
        })
        .await;
    let claim = v.claims[0].live.iter().find(|c| c.key == key);
    claim.expect("the claim").author.clone()
}

async fn next_loss(handle: &mut ClaimHandle) -> LostReason {
    tokio::time::timeout(WAIT, handle.lost())
        .await
        .expect("told in time")
        .expect("a reason")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_alone_claims() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    wren.stop().await;
    let asked = ask_to_take(&olive, None, "inbox", "k", "a session");
    let answer = tokio::time::timeout(WITHIN, asked).await;
    let outcome = answer.expect("within the time").expect("no error");
    assert!(matches!(outcome, ClaimOutcome::Yours(ref h) if h.swarm() == id));
    drop(outcome);
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_second_machine_is_told_who_holds_it() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    assert_eq!(
        (handle.swarm(), handle.context(), handle.key()),
        (id.as_str(), "inbox", "k")
    );
    match take(&wren, &id, "k").await {
        ClaimOutcome::Held {
            person,
            machine,
            this_machine,
            ..
        } => {
            assert_eq!(person, "olive");
            assert_eq!(machine, "olive-pc");
            assert!(!this_machine);
        }
        other => panic!("expected held, got {other:?}"),
    }
    drop(handle);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_machines_taking_at_once_settle_on_one() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let (a, b) = tokio::join!(take(&olive, &id, "k"), take(&wren, &id, "k"));
    let won = |o: &ClaimOutcome| matches!(o, ClaimOutcome::Yours(_));
    assert!(won(&a) != won(&b), "got {a:?} and {b:?}");
    drop((a, b));
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_sessions_on_one_machine_do_not_share_a_key() {
    let _slot = slot();
    let settings = ContextSettings {
        max_per_machine: -1,
        ..FAST
    };
    let (mut olive, mut wren, id) = claimers(Role::Writer, settings).await;
    let handle = yours(&olive, &id, "k").await;
    assert!(matches!(
        take(&olive, &id, "k").await,
        ClaimOutcome::Held {
            this_machine: true,
            ..
        }
    ));
    drop(handle);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_that_stops_renewing_lapses_and_another_claims() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    drop(handle);
    let deadline = tokio::time::Instant::now() + WAIT;
    let mine = loop {
        match take(&wren, &id, "k").await {
            ClaimOutcome::Yours(handle) => break handle,
            ClaimOutcome::Held { .. } => {}
            other => panic!("expected held or the claim, got {other:?}"),
        }
        assert!(tokio::time::Instant::now() < deadline, "it never lapsed");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    drop(mine);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_handle_keeps_its_claim_past_the_lapse_time() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    tokio::time::sleep(PAST_LAPSE).await;
    assert!(matches!(
        take(&wren, &id, "k").await,
        ClaimOutcome::Held { .. }
    ));
    drop(handle);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_woken_machine_is_told_it_lost() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let mut handle = yours(&olive, &id, "k").await;
    olive.send(Command::ClaimAgeForTests {
        swarm: id.clone(),
        context: "inbox".to_string(),
        key: "k".to_string(),
        by_secs: 60,
    });
    assert_eq!(next_loss(&mut handle).await, LostReason::Lapsed);
    assert_eq!(
        handle.done().await,
        Err("This claim is no longer yours.".to_string())
    );
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_stays_done() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    handle.done().await.expect("marked done");
    match take(&wren, &id, "k").await {
        ClaimOutcome::Done { person, .. } => assert_eq!(person, "olive"),
        other => panic!("expected done, got {other:?}"),
    }
    // A done key doesn't count, so the machine's one slot is free again.
    let next = yours(&olive, &id, "next").await;
    drop(next);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_reopens_a_done_key_and_it_can_be_claimed_again() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    yours(&olive, &id, "k").await.done().await.expect("done");
    match take(&wren, &id, "k").await {
        ClaimOutcome::Done { person, .. } => assert_eq!(person, "olive"),
        other => panic!("expected done, got {other:?}"),
    }
    reopen(&wren, &id, "k").await.expect("reopened");
    // Either machine may now take it.
    let mine = yours(&wren, &id, "k").await;
    mine.release().await.expect("released");
    olive
        .view("k no longer done", |v| {
            v.id == id && v.claims[0].done.iter().all(|d| d.key != "k")
        })
        .await;
    drop(yours(&olive, &id, "k").await);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_cannot_reopen() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Reader, FAST).await;
    yours(&olive, &id, "k").await.done().await.expect("done");
    assert_eq!(
        reopen(&wren, &id, "k").await,
        Err("Only writers can reopen a finished key in Notes.".to_string())
    );
    assert!(matches!(
        take(&olive, &id, "k").await,
        ClaimOutcome::Done { .. }
    ));
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn releasing_through_the_handle_frees_the_key_at_once() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    handle.release().await.expect("released");
    let mine = yours(&wren, &id, "k").await;
    drop(mine);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_limit_and_paused() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    assert!(matches!(
        take(&olive, &id, "other").await,
        ClaimOutcome::AtLimit(1)
    ));

    let paused = ContextSettings {
        max_per_machine: 0,
        ..FAST
    };
    set_context(&olive, &id, paused);
    context_is(&mut olive, &id, paused).await;
    context_is(&mut wren, &id, paused).await;
    assert!(matches!(
        take(&wren, &id, "fresh").await,
        ClaimOutcome::Paused
    ));
    // What was already held runs on.
    tokio::time::sleep(PAST_LAPSE).await;
    assert!(matches!(
        take(&wren, &id, "k").await,
        ClaimOutcome::Held { .. }
    ));
    drop(handle);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_releases_anyones_claim() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let mut handle = yours(&wren, &id, "k").await;
    let author = author_of(&mut olive, &id, "k").await;
    release(&olive, &id, "k", &author).await.expect("released");
    assert_eq!(
        next_loss(&mut handle).await,
        LostReason::Released {
            person: "olive".to_string()
        }
    );
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_cannot_release_someone_elses() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    let handle = yours(&olive, &id, "k").await;
    let author = author_of(&mut wren, &id, "k").await;
    assert_eq!(
        release(&wren, &id, "k", &author).await,
        Err("Only the machine's person or an owner can release this claim.".to_string())
    );
    drop(handle);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_cannot_claim() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Reader, FAST).await;
    let asked = ask_to_take(&wren, Some(&id), "inbox", "k", "a session").await;
    assert_eq!(
        asked.map(|_| ()),
        Err("Readers can't claim in Notes.".to_string())
    );
    olive.stop().await;
    wren.stop().await;
}

/// When this machine's claim on `key` was made, as a second session on it is told.
async fn claimed_at(svc: &Svc, swarm: &str, key: &str) -> u64 {
    match take(svc, swarm, key).await {
        ClaimOutcome::Held {
            since,
            this_machine: true,
            ..
        } => since,
        other => panic!("expected held on this machine, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_takes_up_the_machines_own_claim() {
    let _slot = slot();
    // Slow enough that the claim outlasts the restart.
    let settings = ContextSettings {
        renew_secs: 30,
        ..FAST
    };
    let (mut olive, mut wren, id) = claimers(Role::Writer, settings).await;
    let first = yours(&wren, &id, "k").await;
    let since = claimed_at(&wren, &id, "k").await;
    wren.stop().await;
    drop(first);
    wren.restart();
    wren.view("the swarm after a restart", |v| v.id == id).await;
    let again = yours(&wren, &id, "k").await;
    assert_eq!(claimed_at(&wren, &id, "k").await, since);
    drop(again);
    olive.stop().await;
    wren.stop().await;
}

async fn retake(svc: &Svc, swarm: &str, key: &str) -> ClaimOutcome {
    let (reply, answer) = oneshot::channel();
    svc.send(Command::ClaimTake {
        swarm: Some(swarm.to_string()),
        context: "inbox".to_string(),
        key: key.to_string(),
        holder: holder("a session"),
        retake: true,
        reply,
    });
    answer.await.expect("a reply").expect("no error")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retake_only_ever_goes_on_with_the_claim_there_was() {
    let _slot = slot();
    let settings = ContextSettings {
        renew_secs: 30,
        max_per_machine: -1,
        ..FAST
    };
    let (mut olive, mut wren, id) = claimers(Role::Writer, settings).await;

    // Never claimed: told so, and nothing is written.
    let never = retake(&wren, &id, "never").await;
    assert!(
        matches!(never, ClaimOutcome::Gone(LostReason::Lapsed)),
        "got {never:?}"
    );
    let wrote = wren
        .view("the view after a retake", |v| v.id == id)
        .await
        .claims[0]
        .live
        .clone();
    assert!(wrote.is_empty(), "got {wrote:?}");

    // Still there: taken up, with the time it was claimed unchanged.
    let first = yours(&wren, &id, "k").await;
    let since = claimed_at(&wren, &id, "k").await;
    drop(first);
    let again = match retake(&wren, &id, "k").await {
        ClaimOutcome::Yours(handle) => handle,
        other => panic!("expected the claim, got {other:?}"),
    };
    assert_eq!(claimed_at(&wren, &id, "k").await, since);
    let author = author_of(&mut wren, &id, "k").await;
    drop(again);

    // Released meanwhile: the record is still there, and says who.
    release(&wren, &id, "k", &author).await.expect("released");
    let released = retake(&wren, &id, "k").await;
    assert!(
        matches!(
            &released,
            ClaimOutcome::Gone(LostReason::Released { person }) if person == "wren"
        ),
        "got {released:?}"
    );

    // A plain take after the same is a new claim, as ever.
    assert!(matches!(
        take(&wren, &id, "k").await,
        ClaimOutcome::Yours(_)
    ));
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_silent_member_does_not_stop_a_claim() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    wren.send(Command::ClaimsSilentForTests { swarm: id.clone() });
    let answer = tokio::time::timeout(WITHIN, take(&olive, &id, "k")).await;
    assert!(matches!(
        answer.expect("within the time"),
        ClaimOutcome::Yours(_)
    ));
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_context_can_be_claimed_in_without_naming_the_swarm_as_soon_as_it_is_made() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::with_claim_slack("olive", &book, SLACK);
    let notes = olive.create("Notes", &[]).await;
    // The swarm's task can also get round to reading the new context by itself, so one try could
    // pass by luck.
    for i in 0..8 {
        let context = format!("made-{i}");
        olive.send(Command::ClaimContextSet {
            swarm: notes.id.clone(),
            name: context.clone(),
            settings: FAST,
        });
        // These go through the task in turn, so by the second answer it has dealt with the
        // change and published what came of it. An empty key is refused without reading anything.
        for _ in 0..2 {
            release(&olive, &notes.id, "", "").await.unwrap_err();
        }
        let taken = ask_to_take(&olive, None, &context, "k", "a session").await;
        assert!(matches!(taken, Ok(ClaimOutcome::Yours(_))), "{taken:?}");
    }
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_context_that_cannot_be_read_keeps_its_view_and_the_others_carry_on() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::with_claim_slack("olive", &book, SLACK);
    let notes = olive.create("Notes", &[]).await;
    let id = notes.id.as_str();
    set_context(&olive, id, FAST);
    context_is(&mut olive, id, FAST).await;
    let _held = yours(&olive, id, "k").await;

    olive.send(Command::ClaimsUnreadableForTests {
        swarm: id.to_string(),
        context: "inbox".to_string(),
    });
    olive.send(Command::ClaimContextSet {
        swarm: id.to_string(),
        name: "later".to_string(),
        settings: FAST,
    });

    let (reply, answer) = oneshot::channel();
    olive.send(Command::ClaimsList {
        swarm: Some(id.to_string()),
        context: "later".to_string(),
        reply,
    });
    assert!(answer.await.unwrap().is_ok(), "the readable context shows");
    let (reply, answer) = oneshot::channel();
    olive.send(Command::ClaimsList {
        swarm: Some(id.to_string()),
        context: "inbox".to_string(),
        reply,
    });
    let inbox = answer.await.unwrap().expect("the old view of the context");
    assert!(inbox.live.iter().any(|c| c.key == "k"), "{inbox:?}");
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_view_shows_claims_and_recent_done() {
    let _slot = slot();
    let settings = ContextSettings {
        max_per_machine: -1,
        ..FAST
    };
    let (mut olive, mut wren, id) = claimers(Role::Writer, settings).await;
    let held = yours(&olive, &id, "held").await;
    yours(&olive, &id, "first").await.done().await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    yours(&olive, &id, "second").await.done().await.unwrap();

    let shown = |v: &SwarmView| {
        v.id == id
            && v.claims[0].live.iter().any(|c| c.key == "held")
            && v.claims[0].done.len() == 2
    };
    let mine = olive.view("olive's claims", shown).await;
    let theirs = wren.view("wren's claims", shown).await;

    for (view, is_mine) in [(&mine, true), (&theirs, false)] {
        let context = &view.claims[0];
        assert_eq!(context.name, "inbox");
        assert_eq!(context.settings, settings);
        let claim = &context.live[0];
        assert_eq!(claim.key, "held");
        assert_eq!(claim.person, "olive");
        assert_eq!(claim.machine, "olive-pc");
        assert_eq!(claim.holder, holder("a session"));
        assert_eq!(claim.mine, is_mine);
        let done: Vec<_> = context.done.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(done, ["second", "first"]);
        assert_eq!(context.done[0].person, "olive");
    }
    drop(held);
    olive.stop().await;
    wren.stop().await;
}

/// Whether `svc` shows swarm `id` as syncing at any point in the next `d`. Views that arrived
/// before now and haven't been taken in yet are looked at too.
async fn shows_syncing_during(svc: &mut Svc, id: &str, d: Duration) -> bool {
    let end = tokio::time::Instant::now() + d;
    svc.until("the time to pass", |s| {
        let view = s.views.iter().find(|v| v.id == id)?;
        if view.syncing {
            return Some(true);
        }
        (tokio::time::Instant::now() >= end).then_some(false)
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renewing_a_claim_does_not_show_as_syncing() {
    let _slot = slot();
    let (mut olive, mut wren, id) = claimers(Role::Writer, FAST).await;
    olive
        .view("the swarm quiet on olive", |v| v.id == id && !v.syncing)
        .await;
    wren.view("the swarm quiet on wren", |v| v.id == id && !v.syncing)
        .await;
    let handle = yours(&olive, &id, "k").await;
    let renewed = |v: &SwarmView| v.claims[0].live.first().map(|c| c.renewed_at);
    let first = wren
        .until("the claim in wren's view", |s| {
            s.views.iter().find(|v| v.id == id).and_then(renewed)
        })
        .await;

    // Writing a claim gives the folder nothing to do, on the machine renewing it or on any other.
    assert!(
        !shows_syncing_during(&mut olive, &id, PAST_LAPSE).await,
        "a renewal showed as syncing on the machine holding the claim"
    );
    assert!(
        !shows_syncing_during(&mut wren, &id, PAST_LAPSE).await,
        "a renewal showed as syncing on another machine"
    );
    // And the other machine did see the renewals, so that says something.
    let last = wren
        .until("the claim in wren's view", |s| {
            s.views.iter().find(|v| v.id == id).and_then(renewed)
        })
        .await;
    assert!(last > first, "the renewals never reached wren");
    drop(handle);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_claim_names_is_checked_on_the_way_in() {
    let _slot = slot();
    let book = DirectPeers::new();
    let mut olive = Svc::with_claim_slack("olive", &book, SLACK);
    let notes = olive.create("Notes", &[]).await;
    let id = notes.id.as_str();
    set_context(&olive, id, FAST);
    context_is(&mut olive, id, FAST).await;

    let long_key = "k".repeat(1025);
    for (swarm, context, key, text) in [
        (
            None,
            "inbox",
            long_key.as_str(),
            "A claim's key is 1 to 1024 bytes.",
        ),
        (Some(id), "inbox", "", "A claim's key is 1 to 1024 bytes."),
        (Some(id), "nope", "k", "Notes has no context called nope."),
        (
            None,
            "nope",
            "k",
            "No swarm here has a context called nope.",
        ),
        (
            Some(id),
            "Not A Context",
            "k",
            "A context's name uses a-z, 0-9, - and _.",
        ),
    ] {
        let asked = ask_to_take(&olive, swarm, context, key, "a session").await;
        assert_eq!(asked.map(|_| ()), Err(text.to_string()));
    }

    // Nothing was written for any of those, and a label too long for other machines to read is
    // cut down rather than refused.
    let (reply, answer) = oneshot::channel();
    olive.send(Command::ClaimsList {
        swarm: None,
        context: "inbox".to_string(),
        reply,
    });
    assert!(answer.await.unwrap().expect("the context").live.is_empty());
    let long_label = "x".repeat(70_000);
    let outcome = ask_to_take(&olive, None, "inbox", "k", &long_label).await;
    assert!(matches!(outcome, Ok(ClaimOutcome::Yours(_))));
    let (reply, answer) = oneshot::channel();
    olive.send(Command::ClaimsList {
        swarm: None,
        context: "inbox".to_string(),
        reply,
    });
    let list = answer.await.unwrap().expect("the context");
    assert_eq!(list.live[0].holder.label.chars().count(), 200);
    drop(outcome);
    olive.stop().await;
}
