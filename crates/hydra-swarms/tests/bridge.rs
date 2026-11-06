//! The local bridge: windows that don't run the swarms taking claims through the one that does.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use futures::StreamExt;
use futures::channel::{mpsc as fmpsc, oneshot};
use hydra_swarms::bridge::{Answer, ClaimsClient, ClientMsg, HostMsg, TakeReply};
use hydra_swarms::claims::{ContextSettings, Holder, HolderKind};
use hydra_swarms::{ClaimOutcome, Command, ContextView, DirectPeers, LostReason, Role};

/// Fast enough to see a lapse: one renewal a second, two missed, and a second of slack.
const FAST: ContextSettings = ContextSettings {
    renew_secs: 1,
    lapse_after: 2,
    max_per_machine: 1,
    keep_done_secs: 3600,
};
const SLACK: Duration = Duration::from_secs(1);
/// Lapses in 11 s: long enough that a claim outlasts a change of host, even on a busy runner.
const OUTLASTS: ContextSettings = ContextSettings {
    renew_secs: 5,
    ..FAST
};
const NOT_RUNNING: &str = "The swarms aren't running on this machine right now.";

type Losses = Arc<Mutex<Vec<(u64, LostReason)>>>;

fn holder() -> Holder {
    Holder {
        kind: HolderKind::Session,
        label: "a session".to_string(),
    }
}

fn client_at(file: &Path) -> (Arc<ClaimsClient>, Losses) {
    let losses = Losses::default();
    let told = Arc::clone(&losses);
    let on_lost = Box::new(move |id, reason| told.lock().unwrap().push((id, reason)));
    let client = ClaimsClient::start(file.to_path_buf(), on_lost);
    (Arc::new(client), losses)
}

/// The client's calls block, so they run off the runtime's threads.
async fn off_runtime<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(call).await.unwrap()
}

async fn client_take(client: &Arc<ClaimsClient>, swarm: &str, key: &str) -> TakeReply {
    let (client, swarm, key) = (Arc::clone(client), swarm.to_string(), key.to_string());
    off_runtime(move || client.take(Some(&swarm), "inbox", &key, holder()))
        .await
        .expect("no error")
}

async fn client_list(client: &Arc<ClaimsClient>) -> ContextView {
    let client = Arc::clone(client);
    off_runtime(move || client.list(None, "inbox"))
        .await
        .expect("no error")
}

async fn take(svc: &Svc, swarm: &str, key: &str) -> ClaimOutcome {
    let (reply, answer) = oneshot::channel();
    svc.send(Command::ClaimTake {
        swarm: Some(swarm.to_string()),
        context: "inbox".to_string(),
        key: key.to_string(),
        holder: holder(),
        retake: false,
        reply,
    });
    answer.await.expect("a reply").expect("no error")
}

/// Asks `svc` for `key` until the claim is its own.
async fn take_once_free(svc: &Svc, swarm: &str, key: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match take(svc, swarm, key).await {
            ClaimOutcome::Yours(handle) => {
                // Kept renewing until the machine stops, whatever the caller does next.
                std::mem::forget(handle);
                return;
            }
            ClaimOutcome::Held { .. } => {}
            other => panic!("expected held or the claim, got {other:?}"),
        }
        assert!(tokio::time::Instant::now() < deadline, "it never lapsed");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn release(svc: &Svc, swarm: &str, key: &str, author: &str) {
    let (reply, answer) = oneshot::channel();
    svc.send(Command::ClaimRelease {
        swarm: swarm.to_string(),
        context: "inbox".to_string(),
        key: key.to_string(),
        author: author.to_string(),
        reply,
    });
    answer.await.expect("a reply").expect("released");
}

async fn set_context(svc: &mut Svc, swarm: &str, settings: ContextSettings) {
    svc.send(Command::ClaimContextSet {
        swarm: swarm.to_string(),
        name: "inbox".to_string(),
        settings,
    });
    svc.view("the context's settings", |v| {
        v.id == swarm
            && v.claims
                .iter()
                .any(|c| c.name == "inbox" && c.settings == settings)
    })
    .await;
}

/// `olive` alone, running the swarms with a bridge at the file given back, with a context "inbox".
async fn one_machine(settings: ContextSettings) -> (Svc, String, tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    let book = DirectPeers::new();
    let mut olive = Svc::with_bridge("olive", &book, SLACK, file.clone());
    let notes = olive.create("Notes", &[]).await;
    set_context(&mut olive, &notes.id, settings).await;
    (olive, notes.id, dir, file)
}

/// As `one_machine`, with `wren` joined as a writer and seeing the context too.
async fn two_machines(settings: ContextSettings) -> (Svc, Svc, String, tempfile::TempDir, PathBuf) {
    two_machines_as(Role::Writer, settings).await
}

/// As `two_machines`, with `wren` joined with `role`.
async fn two_machines_as(
    role: Role,
    settings: ContextSettings,
) -> (Svc, Svc, String, tempfile::TempDir, PathBuf) {
    let (mut olive, id, dir, file) = one_machine(settings).await;
    let mut wren = Svc::with_claim_slack("wren", &olive.book.clone(), SLACK);
    let code = olive.invite(&id, role).await;
    wren.join(code, "notes").await;
    let person = wren.person();
    olive
        .view("wren in the members", |v| {
            v.members.iter().any(|m| m.person == person)
        })
        .await;
    wren.view("the context's settings", |v| {
        v.claims.iter().any(|c| c.settings == settings)
    })
    .await;
    (olive, wren, id, dir, file)
}

fn yours(reply: TakeReply) -> (u64, String) {
    match reply {
        TakeReply::Yours {
            holder_id, swarm, ..
        } => (holder_id, swarm),
        other => panic!("expected the claim, got {other:?}"),
    }
}

async fn told_of_loss(losses: &Losses) -> (u64, LostReason) {
    let what = "the holder to be told it lost";
    eventually(what, || {
        let told = !losses.lock().unwrap().is_empty();
        async move { told }
    })
    .await;
    losses.lock().unwrap()[0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_claims_and_is_told_it_lost() {
    let _slot = slot();
    let (mut olive, id, _dir, file) = one_machine(FAST).await;
    let (client, losses) = client_at(&file);
    let (holder_id, swarm) = yours(client_take(&client, &id, "k").await);
    assert_eq!(swarm, id);
    let view = client_list(&client).await;
    assert_eq!(view.live.len(), 1);
    release(&olive, &id, "k", &view.live[0].author).await;
    let reason = LostReason::Released {
        person: "olive".to_string(),
    };
    assert_eq!(told_of_loss(&losses).await, (holder_id, reason));
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_finishes_what_it_holds() {
    let _slot = slot();
    let settings = ContextSettings {
        max_per_machine: -1,
        ..FAST
    };
    let (mut olive, id, _dir, file) = one_machine(settings).await;
    let (client, losses) = client_at(&file);
    let (first, _) = yours(client_take(&client, &id, "first").await);
    let (second, _) = yours(client_take(&client, &id, "second").await);
    assert_ne!(first, second);
    // Held by this machine, even though through this same client.
    assert!(matches!(
        client_take(&client, &id, "first").await,
        TakeReply::Held {
            this_machine: true,
            ..
        }
    ));

    let finishing = Arc::clone(&client);
    let finished = off_runtime(move || {
        (
            finishing.release(first),
            finishing.done(second),
            finishing.release(first),
        )
    })
    .await;
    assert_eq!(finished.0, Ok(()));
    assert_eq!(finished.1, Ok(()));
    assert_eq!(
        finished.2,
        Err("This claim is no longer yours.".to_string())
    );

    let (again, _) = yours(client_take(&client, &id, "first").await);
    assert_ne!(again, first);
    assert!(matches!(
        client_take(&client, &id, "second").await,
        TakeReply::Done { .. }
    ));
    assert!(losses.lock().unwrap().is_empty());
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_secret_is_refused() {
    let _slot = slot();
    let (mut olive, id, _dir, file) = one_machine(FAST).await;
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let secret = written["secret"].as_str().unwrap();
    assert_eq!(secret.len(), 32);
    assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));
    let port = written["port"].as_u64().unwrap() as u16;

    let got = off_runtime(move || {
        let mut raw = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        raw.set_read_timeout(Some(WAIT)).unwrap();
        let wrong = "0".repeat(32);
        let list = ClientMsg::List {
            req: 1,
            swarm: None,
            context: "inbox".to_string(),
        };
        let list = serde_json::to_string(&list).unwrap();
        // The host may have closed by the time the second line goes, which is fine.
        let lines = format!("{{\"secret\":\"{wrong}\"}}\n{list}\n");
        let _ = raw.write_all(lines.as_bytes());
        let mut got = Vec::new();
        // Closed by the host either way; a timeout here would be the failure.
        let read = raw.read_to_end(&mut got);
        let closed =
            read.is_ok() || read.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset);
        (closed, got)
    })
    .await;
    assert!(got.0, "the connection wasn't closed");
    assert!(got.1.is_empty(), "got a reply: {:?}", got.1);

    // A client with the secret still gets in.
    let (client, _) = client_at(&file);
    yours(client_take(&client, &id, "k").await);
    olive.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_client_stops_renewing() {
    let _slot = slot();
    let (mut olive, mut wren, id, _dir, file) = two_machines(FAST).await;
    let (client, _) = client_at(&file);
    yours(client_take(&client, &id, "k").await);
    let held = take(&wren, &id, "k").await;
    assert!(matches!(held, ClaimOutcome::Held { .. }), "got {held:?}");
    drop(client);
    take_once_free(&wren, &id, "k").await;
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ended_holder_stops_renewing() {
    let _slot = slot();
    let (mut olive, mut wren, id, _dir, file) = two_machines(FAST).await;
    let (client, losses) = client_at(&file);
    let (holder_id, _) = yours(client_take(&client, &id, "k").await);
    client.end(holder_id);
    take_once_free(&wren, &id, "k").await;
    assert!(losses.lock().unwrap().is_empty());
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn holders_survive_the_host_changing() {
    let _slot = slot();
    let (mut olive, mut wren, id, _dir, file) = two_machines(OUTLASTS).await;
    let (client, losses) = client_at(&file);
    let (holder_id, _) = yours(client_take(&client, &id, "k").await);
    let since = match take(&wren, &id, "k").await {
        ClaimOutcome::Held { since, .. } => since,
        other => panic!("expected held, got {other:?}"),
    };

    olive.stop().await;
    assert!(!file.exists(), "the host left its file behind");
    olive.restart();
    // Past the longest the claim could have gone unrenewed had the new host not taken it up.
    tokio::time::sleep(Duration::from_secs(14)).await;
    match take(&wren, &id, "k").await {
        ClaimOutcome::Held { since: now, .. } => assert_eq!(now, since),
        other => panic!("expected held, got {other:?}"),
    }
    assert!(losses.lock().unwrap().is_empty());

    // The holder works on the new host as it did on the old.
    let finishing = Arc::clone(&client);
    off_runtime(move || finishing.release(holder_id))
        .await
        .expect("released");
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_host_says_who_took_the_key_meanwhile() {
    let _slot = slot();
    let (mut olive, mut wren, id, _dir, file) = two_machines_as(Role::Owner, OUTLASTS).await;
    let (client, losses) = client_at(&file);
    let (holder_id, _) = yours(client_take(&client, &id, "k").await);
    let author = client_list(&client).await.live[0].author.clone();
    olive.stop().await;
    // Within the holder's deadline, an owner frees the key and takes it.
    release(&wren, &id, "k", &author).await;
    take_once_free(&wren, &id, "k").await;
    olive.restart();
    // A new host that has heard already says it was beaten. One that hears only after taking the
    // holder up says it was released.
    let (lost, reason) = told_of_loss(&losses).await;
    assert_eq!(lost, holder_id);
    let by_wren = match &reason {
        LostReason::Beaten { person, machine } => person == "wren" && machine == "wren-pc",
        LostReason::Released { person } => person == "wren",
        _ => false,
    };
    assert!(by_wren, "got {reason:?}");
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_with_no_host_gets_a_plain_error() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _) = client_at(&dir.path().join("nothing-here.json"));
    let started = Instant::now();
    let answers = off_runtime(move || {
        (
            client.take(None, "inbox", "k", holder()).map(|_| ()),
            client.list(None, "inbox").map(|_| ()),
        )
    })
    .await;
    let plain = Err(NOT_RUNNING.to_string());
    assert_eq!(answers.0, plain);
    assert_eq!(answers.1, plain);
    // Each gives a connection 5 s to come.
    assert!(started.elapsed() >= Duration::from_secs(9));
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_do_not_keep_a_service_alive() {
    let _slot = slot();
    let (mut olive, id, _dir, file) = one_machine(FAST).await;
    let (client, _) = client_at(&file);
    yours(client_take(&client, &id, "k").await);
    // hydraui going away drops its sender rather than saying stop.
    olive.tx = fmpsc::unbounded().0;
    tokio::time::timeout(WAIT, async { while olive.rx.next().await.is_some() {} })
        .await
        .expect("the service stopped");
    assert!(!file.exists(), "the host left its file behind");
}

/// A host that gives the first take the claim and then closes the connection. After that, each
/// connection answers takes with `script` in turn, repeating the last answer; it gives how many
/// takes it has seen since the close.
fn scripted_host(
    file: &Path,
    script: Vec<Result<TakeReply, String>>,
    lapse_micros: u64,
) -> Arc<Mutex<usize>> {
    let listener = listener_for(file);
    let seen = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&seen);
    std::thread::spawn(move || {
        serve_first_take(&listener, lapse_micros);

        let (again, _) = listener.accept().unwrap();
        let mut lines = greeted(&again);
        let mut tries = 0;
        while let Some((req, _)) = next_take(&mut lines) {
            *counted.lock().unwrap() += 1;
            answer(&again, req, script[tries.min(script.len() - 1)].clone());
            tries += 1;
        }
    });
    seen
}

/// The next take's request and holder, or None once the client has gone.
fn next_take(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Option<(u64, u64)> {
    for line in lines {
        if let ClientMsg::Take { req, holder_id, .. } = serde_json::from_str(&line.ok()?).ok()? {
            return Some((req, holder_id));
        }
    }
    None
}

fn answer(mut to: &TcpStream, req: u64, reply: Result<TakeReply, String>) {
    let msg = HostMsg::Reply {
        req,
        answer: Answer::Take(reply),
    };
    // The client may have gone, which is fine.
    let _ = writeln!(to, "{}", serde_json::to_string(&msg).unwrap());
}

/// A client whose holder went through `scripted_host`'s first connection.
fn holding_through_script(
    file: &Path,
    script: Vec<Result<TakeReply, String>>,
    lapse_micros: u64,
) -> (Arc<ClaimsClient>, Losses, Arc<Mutex<usize>>, u64) {
    let seen = scripted_host(file, script, lapse_micros);
    let (client, losses) = client_at(file);
    let reply = client.take(None, "inbox", "k", holder()).expect("no error");
    (client, losses, seen, yours(reply).0)
}

/// A lapse time long enough that no test here waits it out, in microseconds.
const LONG: u64 = 600_000_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retake_that_keeps_failing_gives_up_as_lapsed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    let script = vec![Err("That swarm isn't on this machine.".to_string())];
    let (_client, losses, seen, id) =
        off_runtime(move || holding_through_script(&file, script, 6_000_000)).await;
    let started = Instant::now();
    assert_eq!(told_of_loss(&losses).await, (id, LostReason::Lapsed));
    // Not at the first error, but after it had tried until its time ran out.
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(*seen.lock().unwrap() >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retake_held_by_this_machine_is_not_a_loss() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    let held = TakeReply::Held {
        person: "olive".to_string(),
        machine: "olive-pc".to_string(),
        this_machine: true,
        since: 1,
    };
    let last_step = TakeReply::Yours {
        holder_id: 0,
        swarm: "the swarm".to_string(),
        lapse_micros: LONG,
    };
    let script = vec![Ok(held), Ok(last_step)];
    let (client, losses, seen, id) =
        off_runtime(move || holding_through_script(&file, script, LONG)).await;
    eventually("the second try", || {
        let tries = *seen.lock().unwrap();
        async move { tries >= 2 }
    })
    .await;
    assert!(losses.lock().unwrap().is_empty());
    // Taken up again, it goes on being held rather than still being retaken.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(*seen.lock().unwrap(), 2);
    client.end(id);
    assert!(losses.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retake_that_finds_an_answer_says_why_at_once() {
    let beaten = TakeReply::Held {
        person: "wren".to_string(),
        machine: "wren-pc".to_string(),
        this_machine: false,
        since: 1,
    };
    let done = TakeReply::Done {
        person: "wren".to_string(),
        at: 1,
    };
    let wren = "wren".to_string();
    let cases = [
        (
            beaten,
            LostReason::Beaten {
                person: wren.clone(),
                machine: "wren-pc".to_string(),
            },
        ),
        (done, LostReason::Done { person: wren }),
        (
            TakeReply::Gone(LostReason::Released {
                person: "wren".to_string(),
            }),
            LostReason::Released {
                person: "wren".to_string(),
            },
        ),
        (TakeReply::Paused, LostReason::Lapsed),
        (TakeReply::AtLimit(1), LostReason::Lapsed),
    ];
    for (reply, reason) in cases {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bridge.json");
        let (_client, losses, _, id) =
            off_runtime(move || holding_through_script(&file, vec![Ok(reply)], LONG)).await;
        assert_eq!(told_of_loss(&losses).await, (id, reason));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retake_of_a_key_released_meanwhile_says_so_and_claims_nothing() {
    let _slot = slot();
    let (mut olive, mut wren, id, _dir, file) = two_machines_as(Role::Owner, OUTLASTS).await;
    let (client, losses) = client_at(&file);
    let (holder_id, _) = yours(client_take(&client, &id, "k").await);
    let author = client_list(&client).await.live[0].author.clone();
    olive.stop().await;
    release(&wren, &id, "k", &author).await;
    olive.restart();
    let reason = LostReason::Released {
        person: "wren".to_string(),
    };
    assert_eq!(told_of_loss(&losses).await, (holder_id, reason));
    assert!(client_list(&client).await.live.is_empty());
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_freed_after_the_lapse_is_not_claimed_afresh_by_a_new_host() {
    let _slot = slot();
    let (mut olive, mut wren, id, _dir, file) = two_machines(FAST).await;
    let (client, losses) = client_at(&file);
    yours(client_take(&client, &id, "k").await);
    olive.stop().await;
    // Another machine takes it once it has lapsed, and gives it up again.
    let deadline = tokio::time::Instant::now() + WAIT;
    let handle = loop {
        match take(&wren, &id, "k").await {
            ClaimOutcome::Yours(handle) => break handle,
            ClaimOutcome::Held { .. } => {}
            other => panic!("expected held or the claim, got {other:?}"),
        }
        assert!(tokio::time::Instant::now() < deadline, "it never lapsed");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    handle.release().await.expect("released");
    olive.restart();
    // Time for the client to reconnect and try, and for what it wrote to show.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(client_list(&client).await.live.is_empty());
    assert_eq!(losses.lock().unwrap().len(), 1);
    olive.stop().await;
    wren.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_with_no_host_is_told_after_the_lapse_time() {
    let _slot = slot();
    let (mut olive, id, _dir, file) = one_machine(FAST).await;
    let (client, losses) = client_at(&file);
    let (holder_id, _) = yours(client_take(&client, &id, "k").await);
    olive.stop().await;
    let stopped = Instant::now();
    assert_eq!(told_of_loss(&losses).await, (holder_id, LostReason::Lapsed));
    // Two renewals and the slack, less a little for the time stopping took.
    assert!(stopped.elapsed() >= Duration::from_secs(1));
    assert!(stopped.elapsed() < Duration::from_secs(15));

    // A later host can't bring it back.
    olive.restart();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client_list(&client).await.live.is_empty());
    assert_eq!(losses.lock().unwrap().len(), 1);
    olive.stop().await;
}

/// A host that gives the first take the claim and closes, then on each new connection answers
/// one take with an error and closes again, as one that keeps restarting would. It gives how
/// many connections it has seen since the first.
fn flapping_host(file: &Path, lapse_micros: u64) -> Arc<Mutex<usize>> {
    let listener = listener_for(file);
    let seen = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&seen);
    std::thread::spawn(move || {
        serve_first_take(&listener, lapse_micros);
        loop {
            let (again, _) = listener.accept().unwrap();
            *counted.lock().unwrap() += 1;
            let mut lines = greeted(&again);
            if let Some((req, _)) = next_take(&mut lines) {
                answer(&again, req, Err("Not open yet.".to_string()));
            }
            let _ = again.shutdown(std::net::Shutdown::Both);
        }
    });
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_that_keeps_restarting_does_not_put_the_deadline_off() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    let seen = flapping_host(&file, 7_000_000);
    let (client, losses) = client_at(&file);
    let (id, _) = off_runtime({
        let client = Arc::clone(&client);
        move || yours(client.take(None, "inbox", "k", holder()).expect("no error"))
    })
    .await;
    let started = Instant::now();
    assert_eq!(told_of_loss(&losses).await, (id, LostReason::Lapsed));
    assert!(started.elapsed() >= Duration::from_secs(6));
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(*seen.lock().unwrap() >= 2, "the host never came back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listener_that_says_nothing_does_not_stall_the_client() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    // Whatever has the port now accepts and never answers.
    let silent = listener_for(&file);
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        while let Ok((stream, _)) = silent.accept() {
            kept.push(stream);
        }
    });
    let (client, _) = client_at(&file);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The file then names a real host.
    let host = listener_for(&file);
    std::thread::spawn(move || serve_first_take(&host, 600_000_000));
    let started = Instant::now();
    let got = off_runtime(move || {
        loop {
            match client.take(None, "inbox", "k", holder()) {
                Ok(reply) => return Some(reply),
                Err(_) if started.elapsed() < Duration::from_secs(25) => {}
                Err(_) => return None,
            }
        }
    })
    .await;
    assert!(got.is_some(), "the client never reached the real host");
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bridge_file_is_private_and_clients_still_read_it() {
    let _slot = slot();
    let (mut olive, id, _dir, file) = one_machine(FAST).await;
    assert_only_this_user_can_read(&file);
    assert!(
        !file.with_extension("json.tmp").exists(),
        "the temp file stayed behind"
    );
    let (client, _) = client_at(&file);
    yours(client_take(&client, &id, "k").await);
    olive.stop().await;
}

#[cfg(unix)]
fn assert_only_this_user_can_read(file: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(windows)]
fn assert_only_this_user_can_read(file: &Path) {
    let shown = std::process::Command::new("icacls")
        .arg(file)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&shown.stdout).to_string();
    // Inherited entries are the ones that let in more than its user.
    assert!(!shown.contains("(I)"), "still inherits: {shown}");
    assert_eq!(
        shown.matches(":(").count(),
        1,
        "more than one grant: {shown}"
    );
}

/// A listener on loopback, with `file` naming it.
fn listener_for(file: &Path) -> TcpListener {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let secret = "ab".repeat(16);
    let contents = serde_json::json!({ "port": port, "secret": secret });
    std::fs::write(file, contents.to_string()).unwrap();
    listener
}

/// Lines from a new connection, past its `Hello`, which it has welcomed.
fn greeted(mut stream: &TcpStream) -> impl Iterator<Item = std::io::Result<String>> + use<> {
    let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
    lines.next();
    let welcome = serde_json::to_string(&HostMsg::Welcome).unwrap();
    let _ = writeln!(stream, "{welcome}");
    lines
}

/// Takes one connection, gives its first take the claim, and closes it.
fn serve_first_take(listener: &TcpListener, lapse_micros: u64) {
    let (first, _) = listener.accept().unwrap();
    let mut lines = greeted(&first);
    let (req, holder_id) = next_take(&mut lines).expect("the first take");
    let reply = Ok(TakeReply::Yours {
        holder_id,
        swarm: "the swarm".to_string(),
        lapse_micros,
    });
    answer(&first, req, reply);
    drop(lines);
    first.shutdown(std::net::Shutdown::Both).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_connection_stays_up_past_the_wait_for_welcome() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    let listener = listener_for(&file);
    let connections = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&connections);
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            *counted.lock().unwrap() += 1;
            kept.push((greeted(&stream), stream));
        }
    });
    let (_client, _) = client_at(&file);
    // Nothing to read for longer than a client waits to be welcomed.
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(*connections.lock().unwrap(), 1);
}
