//! The phone end of the link. Async: the caller supplies the tokio runtime.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{
    ConnectError, ConnectingError, Connection, ConnectionError, RecvStream, SendStream,
};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::close;
use crate::framing::{read_msg, write_msg};
use crate::pairing::{PairingOffer, word_pair};
use crate::protocol::{ALPN, ClientMsg, Outcome, PROTOCOL_VERSION, Request, ServerMsg};

/// How long to wait for the desktop's close reason after a stream breaks.
const CLOSE_REASON_WAIT: Duration = Duration::from_secs(2);
/// How many unread events a slow `events()` receiver may fall behind by.
const EVENT_BACKLOG: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The desktop turned this phone away: not paired, or a wrong or expired secret.
    Refused,
    NotPaired,
    /// The app is older than the desktop accepts; it needs this protocol version.
    TooOld(u32),
    /// The connection ended.
    Closed,
    Io(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Refused => write!(f, "the desktop refused this phone"),
            LinkError::NotPaired => write!(f, "this phone is not paired"),
            LinkError::TooOld(need) => write!(f, "the app is too old: protocol {need} needed"),
            LinkError::Closed => write!(f, "the connection closed"),
            LinkError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LinkError {}

type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Outcome>>>>>;

pub struct Client {
    endpoint: Endpoint,
    conn: Connection,
    send: tokio::sync::Mutex<SendStream>,
    /// Waiting requests by id; `None` once the connection has ended.
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<ServerMsg>,
    reader: JoinHandle<()>,
}

impl Client {
    /// Pairs with the desktop in `offer` and returns the word both screens show.
    pub async fn pair(
        secret_key: [u8; 32],
        offer: &PairingOffer,
        relays: bool,
    ) -> Result<(Client, String), LinkError> {
        let mut addr = EndpointAddr::new(parse_id(&offer.endpoint)?);
        if let Some(relay) = &offer.relay {
            let url = RelayUrl::from_str(relay).map_err(|e| LinkError::Io(e.to_string()))?;
            addr = addr.with_relay_url(url);
        }
        open(secret_key, addr, relays, Some(&offer.secret)).await
    }

    /// Connects to the desktop this phone is paired with.
    pub async fn connect(
        secret_key: [u8; 32],
        desktop: &str,
        relays: bool,
    ) -> Result<Client, LinkError> {
        let addr = EndpointAddr::new(parse_id(desktop)?);
        Ok(open(secret_key, addr, relays, None).await?.0)
    }

    /// `pair` dialling `addr` directly, relays off.
    pub async fn pair_at(
        secret_key: [u8; 32],
        offer: &PairingOffer,
        addr: EndpointAddr,
    ) -> Result<(Client, String), LinkError> {
        open(secret_key, addr, false, Some(&offer.secret)).await
    }

    /// `connect` dialling `addr` directly, relays off.
    pub async fn connect_at(secret_key: [u8; 32], addr: EndpointAddr) -> Result<Client, LinkError> {
        Ok(open(secret_key, addr, false, None).await?.0)
    }

    pub async fn request(&self, msg: ClientMsg) -> Result<Outcome, LinkError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        match lock(&self.pending).as_mut() {
            Some(waiting) => waiting.insert(id, tx),
            None => return Err(LinkError::Closed),
        };
        let written = {
            let mut send = self.send.lock().await;
            write_msg(&mut *send, &Request { id, msg }).await
        };
        if written.is_err() {
            if let Some(waiting) = lock(&self.pending).as_mut() {
                waiting.remove(&id);
            }
            return Err(LinkError::Closed);
        }
        rx.await.map_err(|_| LinkError::Closed)
    }

    /// Everything the desktop sends that isn't a reply, from now on.
    pub fn events(&self) -> broadcast::Receiver<ServerMsg> {
        self.events.subscribe()
    }

    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
        self.conn.close(close::BYE.into(), b"bye");
        // Closing the endpoint sends the close out; it needs the runtime, if there is one.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let endpoint = self.endpoint.clone();
            rt.spawn(async move { endpoint.close().await });
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn parse_id(id: &str) -> Result<EndpointId, LinkError> {
    EndpointId::from_str(id).map_err(|e| LinkError::Io(format!("bad desktop id: {e}")))
}

async fn open(
    secret_key: [u8; 32],
    addr: EndpointAddr,
    relays: bool,
    secret: Option<&str>,
) -> Result<(Client, String), LinkError> {
    let endpoint = crate::endpoint_builder(&secret_key, relays)
        .bind()
        .await
        .map_err(|e| LinkError::Io(e.to_string()))?;
    match handshake(&endpoint, addr, secret).await {
        Ok((conn, send, recv, word)) => {
            let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
            let (events, _) = broadcast::channel(EVENT_BACKLOG);
            let reader = tokio::spawn(read_loop(recv, Arc::clone(&pending), events.clone()));
            let client = Client {
                endpoint,
                conn,
                send: tokio::sync::Mutex::new(send),
                pending,
                next_id: AtomicU64::new(1),
                events,
                reader,
            };
            Ok((client, word))
        }
        Err(e) => {
            endpoint.close().await;
            Err(e)
        }
    }
}

/// Connects, pairs when given a secret, and says Hello. Returns the pairing word, or an
/// empty string when not pairing.
async fn handshake(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    secret: Option<&str>,
) -> Result<(Connection, SendStream, RecvStream, String), LinkError> {
    let desktop = addr.id;
    let conn = endpoint.connect(addr, ALPN).await.map_err(connect_error)?;
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(streams) => streams,
        Err(e) => return Err(connection_error(&e)),
    };
    let mut word = String::new();
    if let Some(secret) = secret {
        let pair = ClientMsg::Pair {
            secret: secret.into(),
        };
        say(&conn, &mut send, pair).await?;
        match hear(&conn, &mut recv).await? {
            ServerMsg::Reply {
                id: 0,
                outcome: Outcome::Paired { word: theirs },
            } => {
                let ours = word_pair(secret, &desktop.to_string(), &endpoint.id().to_string());
                if theirs != ours {
                    return Err(LinkError::Io("the desktop derived a different word".into()));
                }
                word = ours;
            }
            ServerMsg::Reply {
                outcome: Outcome::Refused { .. },
                ..
            } => return Err(LinkError::Refused),
            other => return Err(unexpected(&other)),
        }
    }
    let hello = ClientMsg::Hello {
        version: PROTOCOL_VERSION,
    };
    say(&conn, &mut send, hello).await?;
    match hear(&conn, &mut recv).await? {
        ServerMsg::Welcome { .. } => Ok((conn, send, recv, word)),
        ServerMsg::TooOld { need } => {
            conn.close(close::BYE.into(), b"too old");
            Err(LinkError::TooOld(need))
        }
        other => Err(unexpected(&other)),
    }
}

fn unexpected(msg: &ServerMsg) -> LinkError {
    LinkError::Io(format!("unexpected message from the desktop: {msg:?}"))
}

async fn say(conn: &Connection, send: &mut SendStream, msg: ClientMsg) -> Result<(), LinkError> {
    match write_msg(send, &Request { id: 0, msg }).await {
        Ok(()) => Ok(()),
        Err(_) => Err(why_closed(conn).await),
    }
}

async fn hear(conn: &Connection, recv: &mut RecvStream) -> Result<ServerMsg, LinkError> {
    match read_msg::<_, ServerMsg>(recv).await {
        Ok(Some(msg)) => Ok(msg),
        _ => Err(why_closed(conn).await),
    }
}

/// After a stream broke: what the desktop said when it closed the connection.
async fn why_closed(conn: &Connection) -> LinkError {
    let reason = match conn.close_reason() {
        Some(reason) => Some(reason),
        None => tokio::time::timeout(CLOSE_REASON_WAIT, conn.closed())
            .await
            .ok(),
    };
    match reason {
        Some(e) => connection_error(&e),
        None => LinkError::Closed,
    }
}

fn connection_error(e: &ConnectionError) -> LinkError {
    match e {
        ConnectionError::ApplicationClosed(close) => {
            match u32::try_from(close.error_code.into_inner()) {
                Ok(close::REFUSED) | Ok(close::UNPAIRED) => LinkError::Refused,
                _ => LinkError::Closed,
            }
        }
        _ => LinkError::Closed,
    }
}

fn connect_error(e: ConnectError) -> LinkError {
    match &e {
        ConnectError::Connection { source, .. }
        | ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => connection_error(source),
        _ => LinkError::Io(e.to_string()),
    }
}

/// Hands replies to their requests and everything else to `events`, until the
/// connection ends; then fails whatever is still waiting.
async fn read_loop(mut recv: RecvStream, pending: Pending, events: broadcast::Sender<ServerMsg>) {
    while let Ok(Some(msg)) = read_msg::<_, ServerMsg>(&mut recv).await {
        match msg {
            ServerMsg::Reply { id, outcome } => {
                let waiter = lock(&pending).as_mut().and_then(|w| w.remove(&id));
                if let Some(waiter) = waiter {
                    let _ = waiter.send(outcome);
                }
            }
            other => {
                let _ = events.send(other);
            }
        }
    }
    lock(&pending).take();
}
