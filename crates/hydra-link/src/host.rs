//! The desktop end of the link. It runs its own tokio runtime on a thread named
//! `hydra-link`, so every public method is sync and callable from any thread.
//!
//! Each phone connection carries one bi-stream, opened by the phone: the phone writes
//! `Request` frames on its half and the host writes `ServerMsg` frames on the other. A
//! pairing phone's first frame is `Pair` (id 0); every connection then sends `Hello`
//! (id 0) before any other request.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::channel::mpsc as events;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::{Notify, oneshot};

use crate::close;
use crate::framing::{read_msg, write_msg};
use crate::pairing::{OFFER_LIFETIME, PairingOffer, secret_matches, word_pair};
use crate::protocol::{ALPN, ClientMsg, Outcome, PROTOCOL_VERSION, Request, ServerMsg};

/// How long a phone gets to open its stream and send each handshake frame.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a closing endpoint waits for its connections to wind down.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

pub struct HostConfig {
    pub secret_key: [u8; 32],
    /// The paired phone's EndpointId.
    pub paired: Option<String>,
    /// The window name sent in `Welcome`.
    pub window: String,
    /// False in tests: no relays and only direct addresses.
    pub relays: bool,
    #[doc(hidden)]
    pub offer_lifetime: Duration,
}

impl HostConfig {
    /// A config whose pairing offers last `OFFER_LIFETIME`.
    pub fn new(secret_key: [u8; 32], paired: Option<String>, window: String, relays: bool) -> Self {
        HostConfig {
            secret_key,
            paired,
            window,
            relays,
            offer_lifetime: OFFER_LIFETIME,
        }
    }
}

#[derive(Debug)]
pub enum HostEvent {
    Request {
        id: u64,
        msg: ClientMsg,
        reply: Replier,
    },
    Paired {
        phone: String,
        word: String,
    },
    Connected,
    Disconnected,
}

/// Answers one request on the connection it came in on. Dropping it unsent refuses the
/// request; answering after that connection has gone does nothing.
pub struct Replier {
    id: u64,
    out: Option<Arc<Outbox>>,
}

impl Replier {
    pub fn send(mut self, outcome: Outcome) {
        self.reply(outcome);
    }

    fn reply(&mut self, outcome: Outcome) {
        if let Some(out) = self.out.take() {
            out.push(ServerMsg::Reply {
                id: self.id,
                outcome,
            });
        }
    }
}

impl Drop for Replier {
    fn drop(&mut self) {
        self.reply(Outcome::Refused {
            reason: "hydraui closed the request".into(),
        });
    }
}

impl fmt::Debug for Replier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Replier").field("id", &self.id).finish()
    }
}

/// Most messages waiting to go to one phone; past this the oldest broadcast is dropped.
const OUTBOX_CAP: usize = 256;

/// One connection's outgoing queue, bounded at `OUTBOX_CAP`. When full it drops the
/// oldest message that is not a reply (a reply someone is waiting on is kept while
/// any broadcast can go instead).
#[derive(Default)]
struct Outbox {
    queue: Mutex<VecDeque<ServerMsg>>,
    ready: Notify,
}

impl Outbox {
    fn push(&self, msg: ServerMsg) {
        {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.len() >= OUTBOX_CAP {
                let victim = q
                    .iter()
                    .position(|m| !matches!(m, ServerMsg::Reply { .. }))
                    .unwrap_or(0);
                q.remove(victim);
            }
            q.push_back(msg);
        }
        self.ready.notify_one();
    }

    fn pop(&self) -> Option<ServerMsg> {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// The next message, waiting for one if the queue is empty.
    async fn next(&self) -> ServerMsg {
        loop {
            if let Some(msg) = self.pop() {
                return msg;
            }
            self.ready.notified().await;
        }
    }
}

/// The phone connection that has said Hello and been welcomed.
struct Current {
    serial: u64,
    phone: EndpointId,
    conn: Connection,
    out: Arc<Outbox>,
}

struct State {
    paired: Option<EndpointId>,
    offer: Option<(PairingOffer, Instant)>,
    current: Option<Current>,
    serial: u64,
}

struct Shared {
    state: Mutex<State>,
    events: events::UnboundedSender<HostEvent>,
    id: EndpointId,
    window: String,
    offer_lifetime: Duration,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, ev: HostEvent) {
        let _ = self.events.unbounded_send(ev);
    }
}

pub struct Host {
    shared: Arc<Shared>,
    endpoint: Option<Endpoint>,
    stop: Option<oneshot::Sender<()>>,
    /// Resolves once the link thread has closed the endpoint (or ended without one).
    closed: Option<futures::channel::oneshot::Receiver<()>>,
}

impl Host {
    pub fn start(cfg: HostConfig) -> io::Result<(Host, events::UnboundedReceiver<HostEvent>)> {
        let paired = cfg
            .paired
            .as_deref()
            .map(EndpointId::from_str)
            .transpose()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let (events_tx, events_rx) = events::unbounded();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                paired,
                offer: None,
                current: None,
                serial: 0,
            }),
            events: events_tx,
            id: iroh::SecretKey::from_bytes(&cfg.secret_key).public(),
            window: cfg.window,
            offer_lifetime: cfg.offer_lifetime,
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<Endpoint>>();
        let (stop_tx, stop_rx) = oneshot::channel();
        let (closed_tx, closed_rx) = futures::channel::oneshot::channel::<()>();
        let run = Arc::clone(&shared);
        let (key, relays) = (cfg.secret_key, cfg.relays);
        // Detached: `stop` only signals it, and it finishes closing in the background.
        std::thread::Builder::new()
            .name("hydra-link".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("hydra-link")
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                rt.block_on(async move {
                    let bound = crate::endpoint_builder(&key, relays)
                        .alpns(vec![ALPN.to_vec()])
                        .bind()
                        .await;
                    let ep = match bound {
                        Ok(ep) => ep,
                        Err(e) => {
                            let _ = ready_tx.send(Err(io::Error::other(e.to_string())));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(ep.clone()));
                    tokio::select! {
                        () = accept_loop(ep.clone(), Arc::clone(&run)) => {}
                        _ = stop_rx => {}
                    }
                    if let Some(c) = run.lock().current.take() {
                        c.conn.close(close::BYE.into(), b"hydraui stopped");
                    }
                    let _ = tokio::time::timeout(CLOSE_GRACE, ep.close()).await;
                    // Only now may another endpoint bind this key.
                    let _ = closed_tx.send(());
                });
            })?;
        let endpoint = ready_rx
            .recv()
            .map_err(|_| io::Error::other("the link thread stopped while starting"))??;
        let host = Host {
            shared,
            endpoint: Some(endpoint),
            stop: Some(stop_tx),
            closed: Some(closed_rx),
        };
        Ok((host, events_rx))
    }

    pub fn endpoint_id(&self) -> String {
        self.shared.id.to_string()
    }

    /// This host's id with its bound sockets on loopback, for dialling it in tests.
    pub fn addr_for_tests(&self) -> EndpointAddr {
        let sockets = self.endpoint().bound_sockets();
        sockets
            .into_iter()
            .map(|s| {
                let ip = if s.is_ipv4() {
                    Ipv4Addr::LOCALHOST.into()
                } else {
                    Ipv6Addr::LOCALHOST.into()
                };
                SocketAddr::new(ip, s.port())
            })
            .fold(EndpointAddr::new(self.shared.id), |a, s| a.with_ip_addr(s))
    }

    /// A fresh offer that replaces any live one and expires after the offer lifetime.
    pub fn offer_pairing(&self) -> PairingOffer {
        let relay = self
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());
        let offer = PairingOffer::new(self.endpoint_id(), relay);
        let until = Instant::now() + self.shared.offer_lifetime;
        self.shared.lock().offer = Some((offer.clone(), until));
        offer
    }

    /// Forgets the paired phone, withdraws any live pairing offer and closes the phone's
    /// connection.
    pub fn unpair(&self) {
        let mut st = self.shared.lock();
        st.paired = None;
        st.offer = None;
        if let Some(c) = st.current.take() {
            c.conn.close(close::UNPAIRED.into(), b"unpaired");
        }
    }

    /// Sends `msg` to the connected phone; dropped when none is connected.
    pub fn broadcast(&self, msg: ServerMsg) {
        if let Some(c) = &self.shared.lock().current {
            c.out.push(msg);
        }
    }

    /// Tells the link thread to close the phone's connection and the endpoint and to end.
    /// Returns at once: the closing happens in the background. Dropping the host does
    /// the same.
    pub fn stop(self) {}

    /// Stops the host as `stop` does, returning at once with a future that resolves when
    /// the link thread has closed the phone's connection and the endpoint (after at most
    /// the close grace), so the same key can then be bound again without a clash.
    pub fn stop_and_wait_closed(mut self) -> impl Future<Output = ()> + Send + 'static {
        let closed = self.closed.take();
        drop(self);
        async move {
            if let Some(closed) = closed {
                // Cancelled means the link thread ended: the endpoint is gone either way.
                let _ = closed.await;
            }
        }
    }

    fn endpoint(&self) -> &Endpoint {
        self.endpoint
            .as_ref()
            .expect("the endpoint lives until drop")
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // The link thread holds its own endpoint handle and closes it inside its runtime.
        self.endpoint = None;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn accept_loop(ep: Endpoint, shared: Arc<Shared>) {
    while let Some(incoming) = ep.accept().await {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            if let Ok(conn) = incoming.await {
                serve(shared, conn).await;
            }
        });
    }
}

/// Why a handshake stopped: the close code and reason to send.
type Refusal = (u32, &'static [u8]);
const REFUSED: Refusal = (close::REFUSED, b"refused");

async fn serve(shared: Arc<Shared>, conn: Connection) {
    match handshake(&shared, &conn).await {
        Ok((serial, out, recv)) => run(&shared, &conn, serial, out, recv).await,
        Err((code, reason)) => conn.close(code.into(), reason),
    }
}

async fn read_frame(recv: &mut RecvStream) -> Result<ClientMsg, Refusal> {
    match tokio::time::timeout(HANDSHAKE, read_msg::<_, Request>(recv)).await {
        Ok(Ok(Some(req))) if req.id == 0 => Ok(req.msg),
        _ => Err(REFUSED),
    }
}

async fn handshake(
    shared: &Shared,
    conn: &Connection,
) -> Result<(u64, Arc<Outbox>, RecvStream), Refusal> {
    // Who is calling is known from the TLS handshake: check it before reading anything.
    let phone = conn.remote_id();
    let (known, offered) = {
        let st = shared.lock();
        let offered = st
            .offer
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() < *until);
        (st.paired == Some(phone), offered)
    };
    if !known && !offered {
        return Err(REFUSED);
    }
    let (mut send, mut recv) = match tokio::time::timeout(HANDSHAKE, conn.accept_bi()).await {
        Ok(Ok(streams)) => streams,
        _ => return Err(REFUSED),
    };
    // A stranger may only Pair; the paired phone may Pair again (while an offer is
    // live) or go straight to Hello.
    let mut first = read_frame(&mut recv).await?;
    if let ClientMsg::Pair { secret } = &first {
        let word = pair(shared, phone, secret).ok_or(REFUSED)?;
        // hydraui hears of it even if the reply below can't be written.
        shared.emit(HostEvent::Paired {
            phone: phone.to_string(),
            word: word.clone(),
        });
        let reply = ServerMsg::Reply {
            id: 0,
            outcome: Outcome::Paired { word },
        };
        write_msg(&mut send, &reply).await.map_err(|_| REFUSED)?;
        first = read_frame(&mut recv).await?;
    } else if !known {
        return Err(REFUSED);
    }
    let ClientMsg::Hello { version } = first else {
        return Err(REFUSED);
    };
    if version < PROTOCOL_VERSION {
        let too_old = ServerMsg::TooOld {
            need: PROTOCOL_VERSION,
        };
        if write_msg(&mut send, &too_old).await.is_ok() && send.finish().is_ok() {
            // Let the frame land: the app hangs up once it has read it.
            let _ = tokio::time::timeout(CLOSE_GRACE, conn.closed()).await;
        }
        return Err((close::TOO_OLD, b"update the app"));
    }
    let (out, serial) = {
        let mut st = shared.lock();
        if st.paired != Some(phone) {
            return Err(REFUSED);
        }
        st.serial += 1;
        let serial = st.serial;
        let out = Arc::new(Outbox::default());
        // Queued under the lock, so no broadcast can overtake it.
        out.push(ServerMsg::Welcome {
            version: PROTOCOL_VERSION,
            window: shared.window.clone(),
        });
        tokio::spawn(write_loop(conn.clone(), send, Arc::clone(&out)));
        let old = st.current.replace(Current {
            serial,
            phone,
            conn: conn.clone(),
            out: out.clone(),
        });
        if let Some(old) = old {
            old.conn.close(close::REPLACED.into(), b"replaced");
        }
        (out, serial)
    };
    shared.emit(HostEvent::Connected);
    Ok((serial, out, recv))
}

/// Checks a `Pair` against the live offer. On a match the offer is used up, the phone
/// becomes the paired one (closing any other phone's connection) and the word is returned.
fn pair(shared: &Shared, phone: EndpointId, secret: &str) -> Option<String> {
    let mut st = shared.lock();
    let (offer, until) = st.offer.as_ref()?;
    if Instant::now() >= *until || !secret_matches(&offer.secret, secret) {
        return None;
    }
    let word = word_pair(secret, &shared.id.to_string(), &phone.to_string());
    st.offer = None;
    st.paired = Some(phone);
    if st.current.as_ref().is_some_and(|c| c.phone != phone)
        && let Some(old) = st.current.take()
    {
        old.conn
            .close(close::UNPAIRED.into(), b"another phone paired");
    }
    Some(word)
}

/// Turns the phone's requests into events until the connection ends.
async fn run(
    shared: &Shared,
    conn: &Connection,
    serial: u64,
    out: Arc<Outbox>,
    mut recv: RecvStream,
) {
    while let Ok(Some(Request { id, msg })) = read_msg::<_, Request>(&mut recv).await {
        match msg {
            ClientMsg::Hello { .. } | ClientMsg::Pair { .. } => {
                out.push(ServerMsg::Reply {
                    id,
                    outcome: Outcome::Refused {
                        reason: "already connected".into(),
                    },
                });
            }
            msg => shared.emit(HostEvent::Request {
                id,
                msg,
                reply: Replier {
                    id,
                    out: Some(Arc::clone(&out)),
                },
            }),
        }
    }
    conn.close(close::BYE.into(), b"bye");
    // A connection that was replaced says nothing: its phone is still connected.
    let gone = {
        let mut st = shared.lock();
        match &st.current {
            Some(c) if c.serial == serial => {
                st.current = None;
                true
            }
            Some(_) => false,
            None => true,
        }
    };
    if gone {
        shared.emit(HostEvent::Disconnected);
    }
}

/// Writes queued messages to the phone until the connection ends.
async fn write_loop(conn: Connection, mut send: SendStream, out: Arc<Outbox>) {
    loop {
        tokio::select! {
            msg = out.next() => {
                if write_msg(&mut send, &msg).await.is_err() {
                    // A phone that can't be written to would stay connected but deaf: hang
                    // up, so it reconnects.
                    conn.close(close::BYE.into(), b"write failed");
                    break;
                }
            }
            _ = conn.closed() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(i: usize) -> ServerMsg {
        ServerMsg::Notice {
            text: format!("n{i}"),
        }
    }

    fn drain(out: &Outbox) -> Vec<ServerMsg> {
        std::iter::from_fn(|| out.pop()).collect()
    }

    #[test]
    fn a_full_outbox_drops_the_oldest() {
        let out = Outbox::default();
        for i in 0..OUTBOX_CAP + 50 {
            out.push(notice(i));
        }
        let got = drain(&out);
        assert_eq!(got.len(), OUTBOX_CAP);
        assert_eq!(got[0], notice(50));
        assert_eq!(got[OUTBOX_CAP - 1], notice(OUTBOX_CAP + 49));
    }

    #[test]
    fn a_full_outbox_keeps_replies_over_broadcasts() {
        let out = Outbox::default();
        let reply = ServerMsg::Reply {
            id: 7,
            outcome: Outcome::Ok,
        };
        out.push(reply.clone());
        for i in 0..OUTBOX_CAP + 10 {
            out.push(notice(i));
        }
        let got = drain(&out);
        assert_eq!(got.len(), OUTBOX_CAP);
        assert_eq!(got[0], reply);
        assert_eq!(got[OUTBOX_CAP - 1], notice(OUTBOX_CAP + 9));
    }
}
