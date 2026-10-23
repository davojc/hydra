//! The host and the client talking over real iroh endpoints, relays off, on loopback.

use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use hydra_link::client::{Client, LinkError};
use hydra_link::framing::{read_msg, write_msg};
use hydra_link::host::{Host, HostConfig, HostEvent};
use hydra_link::pairing::PairingOffer;
use hydra_link::{ALPN, ClientMsg, Outcome, Request, ServerMsg};
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayMode, SecretKey};

const WAIT: Duration = Duration::from_secs(10);

const DESKTOP: [u8; 32] = [1; 32];
const PHONE: [u8; 32] = [2; 32];
const STRANGER: [u8; 32] = [3; 32];

fn config() -> HostConfig {
    HostConfig::new(DESKTOP, None, "main".into(), false)
}

fn start(cfg: HostConfig) -> (Host, UnboundedReceiver<HostEvent>) {
    Host::start(cfg).expect("the host starts")
}

async fn next_event(events: &mut UnboundedReceiver<HostEvent>) -> HostEvent {
    tokio::time::timeout(WAIT, events.next())
        .await
        .expect("an event arrives in time")
        .expect("the event stream is open")
}

/// Skips events until one matches, failing if none does in time.
async fn wait_for(
    events: &mut UnboundedReceiver<HostEvent>,
    what: &str,
    pick: impl Fn(&HostEvent) -> bool,
) -> HostEvent {
    let found = tokio::time::timeout(WAIT, async {
        loop {
            let ev = events.next().await.expect("the event stream is open");
            if pick(&ev) {
                return ev;
            }
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("no {what} event in time"))
}

async fn timed<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, f)
        .await
        .expect("finished in time")
}

/// A host with `PHONE` paired, and the client that paired.
async fn paired_host() -> (Host, UnboundedReceiver<HostEvent>, Client) {
    let (host, mut events) = start(config());
    let offer = host.offer_pairing();
    let (client, _) = timed(Client::pair_at(PHONE, &offer, host.addr_for_tests()))
        .await
        .expect("pairing works");
    wait_for(&mut events, "Connected", |e| {
        matches!(e, HostEvent::Connected)
    })
    .await;
    (host, events, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_with_the_right_secret_pairs() {
    let (host, mut events) = start(config());
    let offer = host.offer_pairing();
    assert_eq!(offer.endpoint, host.endpoint_id());
    let (client, word) = timed(Client::pair_at(PHONE, &offer, host.addr_for_tests()))
        .await
        .expect("pairing works");
    match next_event(&mut events).await {
        HostEvent::Paired {
            phone,
            word: host_word,
        } => {
            assert_eq!(phone, client.endpoint_id());
            assert_eq!(host_word, word);
        }
        other => panic!("expected Paired, got {other:?}"),
    }
    assert!(matches!(
        next_event(&mut events).await,
        HostEvent::Connected
    ));
    host.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_secret_is_refused_and_the_offer_survives() {
    let (host, mut events) = start(config());
    let offer = host.offer_pairing();
    let mut wrong = offer.clone();
    wrong.secret = "0".repeat(64);
    let got = timed(Client::pair_at(PHONE, &wrong, host.addr_for_tests())).await;
    assert!(
        matches!(got, Err(LinkError::Refused)),
        "got {:?}",
        got.err()
    );
    timed(Client::pair_at(PHONE, &offer, host.addr_for_tests()))
        .await
        .expect("the real offer still pairs");
    wait_for(&mut events, "Paired", |e| {
        matches!(e, HostEvent::Paired { .. })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_offer_is_refused() {
    let (host, _events) = start(HostConfig {
        offer_lifetime: Duration::from_millis(50),
        ..config()
    });
    let offer = host.offer_pairing();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let got = timed(Client::pair_at(PHONE, &offer, host.addr_for_tests())).await;
    assert!(
        matches!(got, Err(LinkError::Refused)),
        "got {:?}",
        got.err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stranger_is_refused_once_paired() {
    let (host, mut events, _client) = paired_host().await;
    let got = timed(Client::connect_at(STRANGER, host.addr_for_tests())).await;
    assert!(
        matches!(got, Err(LinkError::Refused)),
        "got {:?}",
        got.err()
    );
    // Nothing the stranger sent became a request.
    let quiet = tokio::time::timeout(Duration::from_millis(300), async {
        while let Some(ev) = events.next().await {
            assert!(
                !matches!(ev, HostEvent::Request { .. }),
                "a stranger's message reached hydraui"
            );
        }
    })
    .await;
    assert!(quiet.is_err(), "the event stream ended");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_get_their_replies() {
    let (host, mut events, client) = paired_host().await;
    let answer = Outcome::Snapshot {
        sessions: vec![],
        projects: vec!["E:/p".into()],
    };
    let (got, _) = tokio::join!(timed(client.request(ClientMsg::Snapshot)), async {
        match wait_for(&mut events, "Request", |e| {
            matches!(e, HostEvent::Request { .. })
        })
        .await
        {
            HostEvent::Request { msg, reply, .. } => {
                assert_eq!(msg, ClientMsg::Snapshot);
                reply.send(answer.clone());
            }
            _ => unreachable!(),
        }
    });
    assert_eq!(got.expect("a reply"), answer);
    host.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_replier_refuses() {
    let (_host, mut events, client) = paired_host().await;
    let (got, _) = tokio::join!(timed(client.request(ClientMsg::Snapshot)), async {
        let ev = wait_for(&mut events, "Request", |e| {
            matches!(e, HostEvent::Request { .. })
        })
        .await;
        drop(ev);
    });
    assert_eq!(
        got.expect("a reply"),
        Outcome::Refused {
            reason: "hydraui closed the request".into()
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_without_a_phone_is_dropped_and_later_messages_arrive() {
    let (host, mut events) = start(config());
    let offer = host.offer_pairing();
    let (first, _) = timed(Client::pair_at(PHONE, &offer, host.addr_for_tests()))
        .await
        .expect("pairing works");
    drop(first);
    wait_for(&mut events, "Disconnected", |e| {
        matches!(e, HostEvent::Disconnected)
    })
    .await;

    host.broadcast(ServerMsg::Notice {
        text: "lost".into(),
    });
    let client = timed(Client::connect_at(PHONE, host.addr_for_tests()))
        .await
        .expect("the paired phone connects");
    let mut seen = client.events();
    host.broadcast(ServerMsg::Notice {
        text: "seen".into(),
    });
    let first = timed(seen.recv()).await.expect("an event");
    assert_eq!(
        first,
        ServerMsg::Notice {
            text: "seen".into()
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_old_app_is_told_to_update() {
    let (host, _events, client) = paired_host().await;
    drop(client);
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&PHONE))
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("bind");
    let conn = timed(ep.connect(host.addr_for_tests(), ALPN))
        .await
        .expect("connect");
    let (mut send, mut recv) = conn.open_bi().await.expect("open a stream");
    write_msg(
        &mut send,
        &Request {
            id: 0,
            msg: ClientMsg::Hello { version: 0 },
        },
    )
    .await
    .expect("write hello");
    let got: Option<ServerMsg> = timed(read_msg(&mut recv)).await.expect("read");
    assert_eq!(got, Some(ServerMsg::TooOld { need: 1 }));
    conn.close(0u32.into(), b"bye");
    ep.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanished_phone_does_not_stop_the_host() {
    let (host, mut events, client) = paired_host().await;
    let asking = tokio::spawn(async move {
        let _ = client.request(ClientMsg::Snapshot).await;
    });
    let reply = match wait_for(&mut events, "Request", |e| {
        matches!(e, HostEvent::Request { .. })
    })
    .await
    {
        HostEvent::Request { reply, .. } => reply,
        _ => unreachable!(),
    };
    asking.abort();
    let _ = asking.await;
    wait_for(&mut events, "Disconnected", |e| {
        matches!(e, HostEvent::Disconnected)
    })
    .await;
    reply.send(Outcome::Ok);

    let client = timed(Client::connect_at(PHONE, host.addr_for_tests()))
        .await
        .expect("the phone reconnects");
    let (got, _) = tokio::join!(
        timed(client.request(ClientMsg::Seen { session: 1 })),
        async {
            match wait_for(&mut events, "Request", |e| {
                matches!(e, HostEvent::Request { .. })
            })
            .await
            {
                HostEvent::Request { reply, .. } => reply.send(Outcome::Ok),
                _ => unreachable!(),
            }
        }
    );
    assert_eq!(got.expect("a reply"), Outcome::Ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpair_closes_and_refuses() {
    let (host, _events, client) = paired_host().await;
    host.unpair();
    let got = timed(client.request(ClientMsg::Snapshot)).await;
    assert!(matches!(got, Err(LinkError::Closed)), "got {got:?}");
    let again = timed(Client::connect_at(PHONE, host.addr_for_tests())).await;
    assert!(
        matches!(again, Err(LinkError::Refused)),
        "got {:?}",
        again.err()
    );
}

#[test]
fn the_host_runs_without_a_tokio_runtime() {
    let (host, _events) = start(config());
    let offer: PairingOffer = host.offer_pairing();
    assert_eq!(offer.endpoint, host.endpoint_id());
    host.broadcast(ServerMsg::Notice { text: "x".into() });
    host.unpair();
    host.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_phone_can_pair_again() {
    let (host, mut events, first) = paired_host().await;
    let offer = host.offer_pairing();
    let (again, word) = timed(Client::pair_at(PHONE, &offer, host.addr_for_tests()))
        .await
        .expect("the paired phone pairs again");
    assert_eq!(
        word,
        hydra_link::pairing::word_pair(&offer.secret, &host.endpoint_id(), &again.endpoint_id())
    );
    match wait_for(&mut events, "Paired", |e| {
        matches!(e, HostEvent::Paired { .. })
    })
    .await
    {
        HostEvent::Paired { phone, word: w } => {
            assert_eq!(phone, again.endpoint_id());
            assert_eq!(w, word);
        }
        _ => unreachable!(),
    }
    drop(first);
}

/// A raw connection from `key` that sends one request frame; returns how the host closed it.
async fn raw_request(host: &Host, key: [u8; 32]) -> iroh::endpoint::ConnectionError {
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key))
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("bind");
    let conn = timed(ep.connect(host.addr_for_tests(), ALPN))
        .await
        .expect("connect");
    if let Ok((mut send, _recv)) = conn.open_bi().await {
        let _ = write_msg(
            &mut send,
            &Request {
                id: 1,
                msg: ClientMsg::Snapshot,
            },
        )
        .await;
    }
    let why = timed(conn.closed()).await;
    ep.close().await;
    why
}

async fn assert_no_request(events: &mut UnboundedReceiver<HostEvent>) {
    let quiet = tokio::time::timeout(Duration::from_millis(300), async {
        while let Some(ev) = events.next().await {
            assert!(
                !matches!(ev, HostEvent::Request { .. }),
                "a stranger's message reached hydraui"
            );
        }
    })
    .await;
    assert!(quiet.is_err(), "the event stream ended");
}

fn is_refused(e: &iroh::endpoint::ConnectionError) -> bool {
    matches!(e, iroh::endpoint::ConnectionError::ApplicationClosed(c) if c.error_code.into_inner() == 1)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strangers_raw_request_is_closed_unread() {
    let (host, mut events, _client) = paired_host().await;
    let why = raw_request(&host, STRANGER).await;
    assert!(is_refused(&why), "closed with {why:?}");
    assert_no_request(&mut events).await;

    host.offer_pairing();
    let why = raw_request(&host, STRANGER).await;
    assert!(is_refused(&why), "closed with {why:?}");
    assert_no_request(&mut events).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_since_the_handshake_reach_the_first_receiver() {
    let (host, mut events, client) = paired_host().await;
    host.broadcast(ServerMsg::Notice {
        text: "early".into(),
    });
    // The reply follows the notice on the same stream, so the notice has been read.
    let (got, _) = tokio::join!(timed(client.request(ClientMsg::Snapshot)), async {
        match wait_for(&mut events, "Request", |e| {
            matches!(e, HostEvent::Request { .. })
        })
        .await
        {
            HostEvent::Request { reply, .. } => reply.send(Outcome::Ok),
            _ => unreachable!(),
        }
    });
    assert_eq!(got.expect("a reply"), Outcome::Ok);
    let mut seen = client.events();
    assert_eq!(
        timed(seen.recv()).await.expect("an event"),
        ServerMsg::Notice {
            text: "early".into()
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_broadcasts_ends_with_the_newest() {
    let (host, _events, client) = paired_host().await;
    let mut seen = client.events();
    for i in 0..2000 {
        host.broadcast(ServerMsg::Notice {
            text: format!("n{i}"),
        });
    }
    host.broadcast(ServerMsg::Notice {
        text: "last".into(),
    });
    timed(async {
        loop {
            match seen.recv().await {
                Ok(ServerMsg::Notice { text }) if text == "last" => break,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("events ended: {e}"),
            }
        }
    })
    .await;
}

/// The test thread is busy inside `stop`, so on this single-threaded runtime the client
/// cannot answer the host's close; `stop` must not wait for it.
#[tokio::test]
async fn stop_does_not_wait_for_the_network() {
    let (host, _events, client) = paired_host().await;
    let began = std::time::Instant::now();
    host.stop();
    let took = began.elapsed();
    assert!(took < Duration::from_millis(500), "stop took {took:?}");
    drop(client);
}
