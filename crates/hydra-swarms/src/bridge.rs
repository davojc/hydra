//! How every hydraui window reaches the swarms. Each window is its own process and only one runs
//! the service, so that one listens on loopback and writes where to find it in a file; the others
//! read the file, connect and pass their claims over. When the window running the swarms closes,
//! the handles it held for the others drop; the window that takes over writes the file again, and
//! each client takes up its live holders there, well inside the lapse time.
//!
//! The protocol is JSON lines. The file holds `{"port":…, "secret":"<32 hex>"}`, readable only by
//! this user; the first line a client sends is `Hello { secret }`. The right secret is answered
//! with `Welcome`, and a wrong one closes the connection without a reply.
//!
//! A holder cut off from a host has until its claim's lapse time, from when it was cut off, to be
//! taken up by another. After that it is told it lapsed, whether or not there is a host to ask.
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc as std_mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use futures::channel::oneshot;
use hydra_platform::fs::write_private;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use crate::claims::Holder;
use crate::ids::to_hex;
use crate::node::now_micros;
use crate::service::{
    ClaimHandle, ClaimOutcome, Command, ContextView, LostReason, NO_LONGER_YOURS, lock, random32,
};

/// How long a call waits for a connection.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// The most a call blocks, connection included.
const ANSWER_WAIT: Duration = Duration::from_secs(30);
/// The wait before connecting again, and between tries at taking a holder up again.
const RETRY: Duration = Duration::from_secs(2);
/// How long a client gives whatever has the port to say it is the host: a stale file can name a
/// port something else has taken since.
const WELCOME_WAIT: Duration = Duration::from_secs(5);
/// The longest between looks at a holder's deadline, so a clock that jumps is soon noticed.
const DEADLINE_LOOK: Duration = Duration::from_secs(1);
/// How long the host gives a new connection to say who it is.
const HELLO_WAIT: Duration = Duration::from_secs(5);
/// The longest line the host reads, since anything on this machine can connect.
const MAX_LINE: u64 = 1024 * 1024;

const NOT_RUNNING: &str = "The swarms aren't running on this machine right now.";
const NO_ANSWER: &str = "The swarms didn't answer in time.";
const WRONG_ANSWER: &str = "The swarms gave an answer that doesn't fit the question.";

/// What the bridge file holds.
#[derive(Serialize, Deserialize)]
struct BridgeFile {
    port: u16,
    secret: String,
}

/// The first line on every connection.
#[derive(Serialize, Deserialize)]
struct Hello {
    secret: String,
}

/// What a client sends after `Hello`.
#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    /// `holder_id` is the client's own, and stays the same when the holder is taken up again.
    Take {
        req: u64,
        holder_id: u64,
        swarm: Option<String>,
        context: String,
        key: String,
        holder: Holder,
        /// See `Command::ClaimTake`.
        retake: bool,
    },
    Release {
        req: u64,
        holder_id: u64,
    },
    Done {
        req: u64,
        holder_id: u64,
    },
    End {
        holder_id: u64,
    },
    List {
        req: u64,
        swarm: Option<String>,
        context: String,
    },
}

/// What the host sends.
#[derive(Serialize, Deserialize, Debug)]
pub enum HostMsg {
    /// The answer to a good `Hello`.
    Welcome,
    Reply {
        req: u64,
        answer: Answer,
    },
    Lost {
        holder_id: u64,
        reason: LostReason,
    },
}

/// The answer to a request, by what was asked.
#[derive(Serialize, Deserialize, Debug)]
pub enum Answer {
    Take(Result<TakeReply, String>),
    Finished(Result<(), String>),
    List(Result<ContextView, String>),
}

/// What came of asking for a key. Times are Unix microseconds.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum TakeReply {
    Yours {
        holder_id: u64,
        swarm: String,
        /// How long the claim can go unrenewed before it lapses.
        lapse_micros: u64,
    },
    Held {
        person: String,
        machine: String,
        this_machine: bool,
        since: u64,
    },
    Done {
        person: String,
        at: u64,
    },
    Paused,
    AtLimit(i32),
    /// Only for a retake: the claim has ended, and this is why.
    Gone(LostReason),
}

/// The listening side, in the window that runs the swarms.
pub(crate) struct Host {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Host {
    /// Listens and writes `file`. Claims and lists go to `commands`.
    pub(crate) async fn start(
        file: PathBuf,
        commands: mpsc::UnboundedSender<Command>,
    ) -> anyhow::Result<Host> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("listening on this machine")?;
        let port = listener.local_addr()?.port();
        let secret = to_hex(&random32()?[..16]);
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        }
        let written = serde_json::to_string(&BridgeFile {
            port,
            secret: secret.clone(),
        })?;
        // Restricting a file means running a program on Windows.
        let writing = file.clone();
        tokio::task::spawn_blocking(move || save_private(&writing, &written)).await??;
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(listen(listener, file, secret, commands, stopped));
        Ok(Host { stop, task })
    }

    /// Closes every connection, which drops the handles they held, and removes the file.
    pub(crate) async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

async fn listen(
    listener: TcpListener,
    file: PathBuf,
    secret: String,
    commands: mpsc::UnboundedSender<Command>,
    mut stopped: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    connections.spawn(connection(stream, secret.clone(), commands.clone()));
                }
                // A connection that went before it was accepted, or a lack of sockets for a
                // while. Neither is worth stopping for, or spinning on.
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
    remove_if_ours(&file, &secret);
}

/// `text` written to `file` through a temp file and a rename, for this user's eyes only. The temp
/// is restricted while still empty, so the secret is never on disk with open access.
fn save_private(file: &Path, text: &str) -> anyhow::Result<()> {
    let tmp = file.with_extension("json.tmp");
    write_private(&tmp, "").with_context(|| format!("restricting {}", tmp.display()))?;
    let saved = fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, file));
    if saved.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    saved.with_context(|| format!("writing {}", file.display()))
}

/// Removes the file unless a newer host has written it since.
fn remove_if_ours(file: &Path, secret: &str) {
    let written: Option<BridgeFile> = fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if written.is_some_and(|w| w.secret == secret) {
        let _ = fs::remove_file(file);
    }
}

/// Doesn't stop at the first difference, so how long it takes says nothing about the secret.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |diff, (x, y)| diff | (x ^ y))
            == 0
}

/// A whole line, or None at the end of the stream or for one too long to be real.
async fn read_line(read: &mut AsyncBufReader<OwnedReadHalf>) -> Option<String> {
    let mut line = String::new();
    let mut limited = (&mut *read).take(MAX_LINE);
    limited.read_line(&mut line).await.ok()?;
    line.ends_with('\n').then_some(line)
}

/// One client, for as long as it stays connected. Everything it holds lives in this task's
/// `JoinSet`s, so when the task ends every handle drops and those holders lapse.
async fn connection(
    stream: tokio::net::TcpStream,
    secret: String,
    commands: mpsc::UnboundedSender<Command>,
) {
    let (read, write) = stream.into_split();
    let mut read = AsyncBufReader::new(read);
    let hello = tokio::time::timeout(HELLO_WAIT, read_line(&mut read)).await;
    let Ok(Some(line)) = hello else {
        return;
    };
    let Ok(hello) = serde_json::from_str::<Hello>(&line) else {
        return;
    };
    if !same_secret(&hello.secret, &secret) {
        return;
    }

    let (out, queued) = mpsc::unbounded_channel();
    let _ = out.send(HostMsg::Welcome);
    let (arrived, mut incoming) = mpsc::unbounded_channel();
    // A take can wait seconds on other machines, and a loss can come at any time, so reading and
    // writing each go on beside whatever is under way.
    let mut io = JoinSet::new();
    io.spawn(read_messages(read, arrived));
    io.spawn(write_messages(write, queued));
    let mut client = Client {
        out,
        commands,
        holders: HashMap::new(),
        holding: JoinSet::new(),
        takes: JoinSet::new(),
    };
    loop {
        tokio::select! {
            // What the client sent is dealt with before noticing it has gone.
            biased;
            Some(msg) = incoming.recv() => client.message(msg),
            Some(Ok((req, holder_id, outcome))) = client.takes.join_next(),
                if !client.takes.is_empty() => client.taken(req, holder_id, outcome),
            Some(Ok(holder_id)) = client.holding.join_next(), if !client.holding.is_empty() => {
                client.holders.remove(&holder_id);
            }
            _ = io.join_next() => return,
        }
    }
}

async fn read_messages(
    mut read: AsyncBufReader<OwnedReadHalf>,
    arrived: mpsc::UnboundedSender<ClientMsg>,
) {
    while let Some(line) = read_line(&mut read).await {
        let Ok(msg) = serde_json::from_str(&line) else {
            return;
        };
        if arrived.send(msg).is_err() {
            return;
        }
    }
}

async fn write_messages(mut write: OwnedWriteHalf, mut queued: mpsc::UnboundedReceiver<HostMsg>) {
    while let Some(msg) = queued.recv().await {
        let Ok(mut line) = serde_json::to_string(&msg) else {
            return;
        };
        line.push('\n');
        if write.write_all(line.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// A request to give a claim up, or to mark it done.
struct Finish {
    req: u64,
    done: bool,
}

/// The host's side of one connected client.
struct Client {
    out: mpsc::UnboundedSender<HostMsg>,
    commands: mpsc::UnboundedSender<Command>,
    /// Each live holder's task. Dropping its sender ends the holder.
    holders: HashMap<u64, mpsc::UnboundedSender<Finish>>,
    holding: JoinSet<u64>,
    takes: JoinSet<(u64, u64, Result<ClaimOutcome, String>)>,
}

impl Client {
    fn message(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Take {
                req,
                holder_id,
                swarm,
                context,
                key,
                holder,
                retake,
            } => {
                let (reply, answer) = oneshot::channel();
                let cmd = Command::ClaimTake {
                    swarm,
                    context,
                    key,
                    holder,
                    retake,
                    reply,
                };
                // With the service gone this fails, and the dropped `reply` is the answer.
                let _ = self.commands.send(cmd);
                self.takes.spawn(async move {
                    let outcome = answer
                        .await
                        .unwrap_or_else(|_| Err(NOT_RUNNING.to_string()));
                    (req, holder_id, outcome)
                });
            }
            ClientMsg::Release { req, holder_id } => self.finish(req, holder_id, false),
            ClientMsg::Done { req, holder_id } => self.finish(req, holder_id, true),
            ClientMsg::End { holder_id } => {
                self.holders.remove(&holder_id);
            }
            ClientMsg::List {
                req,
                swarm,
                context,
            } => {
                let (reply, answer) = oneshot::channel();
                let cmd = Command::ClaimsList {
                    swarm,
                    context,
                    reply,
                };
                let _ = self.commands.send(cmd);
                let out = self.out.clone();
                tokio::spawn(async move {
                    let view = answer
                        .await
                        .unwrap_or_else(|_| Err(NOT_RUNNING.to_string()));
                    let answer = Answer::List(view);
                    let _ = out.send(HostMsg::Reply { req, answer });
                });
            }
        }
    }

    fn finish(&mut self, req: u64, holder_id: u64, done: bool) {
        let asked = self.holders.get(&holder_id);
        if asked.is_none_or(|holder| holder.send(Finish { req, done }).is_err()) {
            let answer = Answer::Finished(Err(NO_LONGER_YOURS.to_string()));
            let _ = self.out.send(HostMsg::Reply { req, answer });
        }
    }

    fn taken(&mut self, req: u64, holder_id: u64, outcome: Result<ClaimOutcome, String>) {
        let reply = outcome.map(|outcome| match outcome {
            ClaimOutcome::Yours(handle) => {
                let swarm = handle.swarm().to_string();
                let lapse_micros = handle.lapse_micros();
                let (finishes, finishing) = mpsc::unbounded_channel();
                // In place before the client hears, so its next request finds it.
                self.holders.insert(holder_id, finishes);
                let out = self.out.clone();
                self.holding.spawn(hold(holder_id, handle, finishing, out));
                TakeReply::Yours {
                    holder_id,
                    swarm,
                    lapse_micros,
                }
            }
            ClaimOutcome::Held {
                person,
                machine,
                this_machine,
                since,
            } => TakeReply::Held {
                person,
                machine,
                this_machine,
                since,
            },
            ClaimOutcome::Done { person, at } => TakeReply::Done { person, at },
            ClaimOutcome::Paused => TakeReply::Paused,
            ClaimOutcome::AtLimit(limit) => TakeReply::AtLimit(limit),
            ClaimOutcome::Gone(reason) => TakeReply::Gone(reason),
        });
        let answer = Answer::Take(reply);
        let _ = self.out.send(HostMsg::Reply { req, answer });
    }
}

/// Keeps a handle for its holder until the claim is lost or finished, or the holder ends.
async fn hold(
    holder_id: u64,
    mut handle: ClaimHandle,
    mut finishing: mpsc::UnboundedReceiver<Finish>,
    out: mpsc::UnboundedSender<HostMsg>,
) -> u64 {
    tokio::select! {
        lost = handle.lost() => {
            // None: the swarm closed, so nothing will renew this claim any more.
            let reason = lost.unwrap_or(LostReason::Lapsed);
            let _ = out.send(HostMsg::Lost { holder_id, reason });
        }
        finish = finishing.recv() => {
            if let Some(Finish { req, done }) = finish {
                let finished = if done {
                    handle.done().await
                } else {
                    handle.release().await
                };
                let answer = Answer::Finished(finished);
                let _ = out.send(HostMsg::Reply { req, answer });
            }
        }
    }
    holder_id
}

/// A window's way to the swarms: blocking, with no runtime, and safe to call from any thread,
/// `on_lost` included. Dropping it ends every holder.
pub struct ClaimsClient {
    inner: Arc<Inner>,
}

/// The connection went before the answer came.
struct Gone;

struct Inner {
    state: Mutex<State>,
    /// Woken whenever a call or a retake might stop waiting.
    changed: Condvar,
    /// `on_lost` runs on a thread of its own, so it can call the client and hold nothing up.
    lost: std_mpsc::Sender<(u64, LostReason)>,
}

#[derive(Default)]
struct State {
    conn: Option<Conn>,
    waiting: HashMap<u64, Waiter>,
    live: HashMap<u64, Live>,
    next_req: u64,
    next_holder: u64,
    connections: u64,
    closed: bool,
}

struct Conn {
    id: u64,
    /// Writing happens on its own thread, so nothing that sends can block on the socket.
    out: std_mpsc::Sender<ClientMsg>,
    stream: TcpStream,
}

/// A holder this client has, with what it takes to claim for it again.
struct Live {
    swarm: String,
    context: String,
    key: String,
    holder: Holder,
    lapse_micros: u64,
    /// When it lost a host's handle (Unix microseconds, as a machine asleep doesn't count
    /// `Instant`s reliably). Kept across connections until a host takes it up, so one that keeps
    /// restarting can't put its deadline off.
    detached_at: Option<u64>,
}

impl Live {
    /// When it stops being this client's, unless a host takes it up first.
    fn deadline(&self) -> Option<u64> {
        let detached_at = self.detached_at?;
        Some(detached_at.saturating_add(self.lapse_micros))
    }
}

/// A request waiting for its answer.
struct Waiter {
    asked: Asked,
    reply: std_mpsc::SyncSender<Result<Answer, Gone>>,
}

enum Asked {
    Take {
        context: String,
        key: String,
        holder: Holder,
    },
    Retake(u64),
    Finish(u64),
    List,
}

impl ClaimsClient {
    pub fn start(file: PathBuf, on_lost: Box<dyn Fn(u64, LostReason) + Send>) -> ClaimsClient {
        let (lost, losses) = std_mpsc::channel();
        background(move || {
            for (holder_id, reason) in losses {
                on_lost(holder_id, reason);
            }
        });
        let inner = Arc::new(Inner {
            state: Mutex::default(),
            changed: Condvar::new(),
            lost,
        });
        let supervising = Arc::clone(&inner);
        background(move || supervising.supervise(&file));
        let watching = Arc::clone(&inner);
        background(move || watching.watch_deadlines());
        ClaimsClient { inner }
    }

    /// Blocks up to 30 s. Err when the swarms aren't reachable.
    pub fn take(
        &self,
        swarm: Option<&str>,
        context: &str,
        key: &str,
        holder: Holder,
    ) -> Result<TakeReply, String> {
        let asked = Asked::Take {
            context: context.to_string(),
            key: key.to_string(),
            holder: holder.clone(),
        };
        let holder_id = {
            let mut state = lock(&self.inner.state);
            state.next_holder += 1;
            state.next_holder
        };
        let msg = |req| ClientMsg::Take {
            req,
            holder_id,
            swarm: swarm.map(str::to_string),
            context: context.to_string(),
            key: key.to_string(),
            holder,
            retake: false,
        };
        match self.inner.ask(asked, msg)? {
            Answer::Take(reply) => reply,
            _ => Err(WRONG_ANSWER.to_string()),
        }
    }

    pub fn release(&self, holder_id: u64) -> Result<(), String> {
        let msg = |req| ClientMsg::Release { req, holder_id };
        self.finish(holder_id, msg)
    }

    pub fn done(&self, holder_id: u64) -> Result<(), String> {
        let msg = |req| ClientMsg::Done { req, holder_id };
        self.finish(holder_id, msg)
    }

    fn finish(&self, holder_id: u64, msg: impl FnOnce(u64) -> ClientMsg) -> Result<(), String> {
        match self.inner.ask(Asked::Finish(holder_id), msg)? {
            Answer::Finished(finished) => finished,
            _ => Err(WRONG_ANSWER.to_string()),
        }
    }

    /// Ends the holder: renewals stop, the claim lapses. Never blocks.
    pub fn end(&self, holder_id: u64) {
        let mut state = lock(&self.inner.state);
        state.live.remove(&holder_id);
        if let Some(conn) = &state.conn {
            // With the connection going, the host drops the handle anyway.
            let _ = conn.out.send(ClientMsg::End { holder_id });
        }
        self.inner.changed.notify_all();
    }

    pub fn list(&self, swarm: Option<&str>, context: &str) -> Result<ContextView, String> {
        let msg = |req| ClientMsg::List {
            req,
            swarm: swarm.map(str::to_string),
            context: context.to_string(),
        };
        match self.inner.ask(Asked::List, msg)? {
            Answer::List(view) => view,
            _ => Err(WRONG_ANSWER.to_string()),
        }
    }
}

impl Drop for ClaimsClient {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state);
        state.closed = true;
        if let Some(conn) = &state.conn {
            let _ = conn.stream.shutdown(Shutdown::Both);
        }
        self.inner.changed.notify_all();
    }
}

fn background(work: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .name("hydra-claims".into())
        .spawn(work)
        .expect("starting a thread for claims");
}

/// The host named in `file`, connected and having welcomed the secret, with what reads from it.
fn connect(file: &Path) -> Option<(TcpStream, BufReader<TcpStream>)> {
    let bytes = fs::read(file).ok()?;
    let BridgeFile { port, secret } = serde_json::from_slice(&bytes).ok()?;
    let host = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&host, RETRY).ok()?;
    let _ = stream.set_nodelay(true);
    write_line(&mut stream, &Hello { secret }).ok()?;
    // On the clone that reads: on Windows a clone doesn't share its timeouts with the original.
    let reading = stream.try_clone().ok()?;
    reading.set_read_timeout(Some(WELCOME_WAIT)).ok()?;
    let mut reading = BufReader::new(reading);
    let mut line = String::new();
    reading.read_line(&mut line).ok()?;
    let HostMsg::Welcome = serde_json::from_str(&line).ok()? else {
        return None;
    };
    reading.get_ref().set_read_timeout(None).ok()?;
    Some((stream, reading))
}

fn write_line<T: Serialize>(to: &mut TcpStream, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    to.write_all(line.as_bytes())
}

fn write_queued(mut stream: TcpStream, queue: std_mpsc::Receiver<ClientMsg>) {
    for msg in queue {
        if write_line(&mut stream, &msg).is_err() {
            // The reading side then sees the end, and everything waiting is told.
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn supervise(self: Arc<Self>, file: &Path) {
        loop {
            if let Some((stream, reading)) = connect(file) {
                self.serve(stream, reading);
            }
            if !self.pause(RETRY, None) {
                return;
            }
        }
    }

    /// Runs one connection until it ends.
    fn serve(self: &Arc<Self>, stream: TcpStream, reading: BufReader<TcpStream>) {
        let Ok(writing) = stream.try_clone() else {
            return;
        };
        let (out, queue) = std_mpsc::channel();
        background(move || write_queued(writing, queue));
        let (conn_id, holders): (u64, Vec<u64>) = {
            let mut state = self.state();
            if state.closed {
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
            state.connections += 1;
            let id = state.connections;
            state.conn = Some(Conn { id, out, stream });
            self.changed.notify_all();
            (id, state.live.keys().copied().collect())
        };
        // Each on its own, so one the host is slow over doesn't use up another's lapse time.
        for holder_id in holders {
            let inner = Arc::clone(self);
            background(move || inner.retake(conn_id, holder_id));
        }
        for line in reading.lines() {
            let msg = line.ok().and_then(|l| serde_json::from_str(&l).ok());
            let Some(msg) = msg else {
                break;
            };
            self.received(msg);
        }
        self.disconnected();
    }

    fn disconnected(&self) {
        let mut state = self.state();
        if let Some(conn) = state.conn.take() {
            let _ = conn.stream.shutdown(Shutdown::Both);
        }
        let now = now_micros();
        for live in state.live.values_mut() {
            live.detached_at.get_or_insert(now);
        }
        for (_, waiter) in state.waiting.drain() {
            let _ = waiter.reply.try_send(Err(Gone));
        }
        self.changed.notify_all();
    }

    fn received(&self, msg: HostMsg) {
        match msg {
            HostMsg::Welcome => {}
            HostMsg::Lost { holder_id, reason } => self.lose(holder_id, reason),
            HostMsg::Reply { req, answer } => self.replied(req, answer),
        }
    }

    /// Forgets the holder and tells `on_lost`, unless it has ended already.
    fn lose(&self, holder_id: u64, reason: LostReason) {
        if self.state().live.remove(&holder_id).is_some() {
            self.changed.notify_all();
            let _ = self.lost.send((holder_id, reason));
        }
    }

    /// Tells each holder whose deadline has passed that it lapsed, and forgets it so no later host
    /// can bring it back. This goes on with or without a connection.
    fn watch_deadlines(&self) {
        let mut state = self.state();
        while !state.closed {
            let now = now_micros();
            let due: Vec<u64> = state
                .live
                .iter()
                .filter(|(_, live)| live.deadline().is_some_and(|at| at <= now))
                .map(|(id, _)| *id)
                .collect();
            for holder_id in due {
                state.live.remove(&holder_id);
                let _ = self.lost.send((holder_id, LostReason::Lapsed));
                self.changed.notify_all();
            }
            let next = state.live.values().filter_map(Live::deadline).min();
            state = match next {
                Some(at) => {
                    let left = Duration::from_micros(at.saturating_sub(now));
                    self.changed
                        .wait_timeout(state, left.min(DEADLINE_LOOK))
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                // Woken when a connection goes.
                None => self.changed.wait(state).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }

    /// Holders change here, in the order the host's messages came, so a loss straight after an
    /// answer always finds what that answer did.
    fn replied(&self, req: u64, answer: Answer) {
        let mut state = self.state();
        let taken = match &answer {
            Answer::Take(Ok(TakeReply::Yours {
                holder_id,
                swarm,
                lapse_micros,
            })) => Some((*holder_id, swarm, *lapse_micros)),
            _ => None,
        };
        let Some(waiter) = state.waiting.remove(&req) else {
            // Whoever asked gave up waiting, so nobody would ever end this claim.
            if let (Some((holder_id, ..)), Some(conn)) = (taken, &state.conn) {
                let _ = conn.out.send(ClientMsg::End { holder_id });
            }
            return;
        };
        match waiter.asked {
            Asked::Take {
                context,
                key,
                holder,
            } => {
                if let Some((holder_id, swarm, lapse_micros)) = taken {
                    let live = Live {
                        swarm: swarm.clone(),
                        context,
                        key,
                        holder,
                        lapse_micros,
                        detached_at: None,
                    };
                    state.live.insert(holder_id, live);
                }
            }
            Asked::Retake(holder_id) => {
                if let Some((_, _, lapse_micros)) = taken {
                    match state.live.get_mut(&holder_id) {
                        Some(live) => {
                            live.lapse_micros = lapse_micros;
                            live.detached_at = None;
                        }
                        // Ended, or out of time, while the host was answering.
                        None => {
                            if let Some(conn) = &state.conn {
                                let _ = conn.out.send(ClientMsg::End { holder_id });
                            }
                        }
                    }
                }
            }
            // Whatever the answer, the host has given up the handle.
            Asked::Finish(holder_id) => {
                state.live.remove(&holder_id);
            }
            Asked::List => {}
        }
        self.changed.notify_all();
        let _ = waiter.reply.try_send(Ok(answer));
    }

    /// Sends a request once connected, and blocks for its answer.
    fn ask(&self, asked: Asked, msg: impl FnOnce(u64) -> ClientMsg) -> Result<Answer, String> {
        let started = Instant::now();
        let connect_by = started + CONNECT_WAIT;
        let mut state = self.state();
        loop {
            let ready = match asked {
                // A holder a new host hasn't taken up yet has nothing there to finish.
                Asked::Finish(holder_id) => match state.live.get(&holder_id) {
                    Some(live) => live.detached_at.is_none(),
                    None => return Err(NO_LONGER_YOURS.to_string()),
                },
                _ => true,
            };
            if ready && state.conn.is_some() {
                break;
            }
            let left = connect_by.saturating_duration_since(Instant::now());
            if left.is_zero() || state.closed {
                return Err(NOT_RUNNING.to_string());
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let (req, answer) = Inner::send(&mut state, asked, msg);
        drop(state);
        let left = (started + ANSWER_WAIT).saturating_duration_since(Instant::now());
        match self.wait_answer(req, &answer, left) {
            Some(Ok(answer)) => Ok(answer),
            Some(Err(Gone)) => Err(NOT_RUNNING.to_string()),
            None => Err(NO_ANSWER.to_string()),
        }
    }

    /// Queues a request on the connection there must be.
    fn send(
        state: &mut State,
        asked: Asked,
        msg: impl FnOnce(u64) -> ClientMsg,
    ) -> (u64, std_mpsc::Receiver<Result<Answer, Gone>>) {
        state.next_req += 1;
        let req = state.next_req;
        let (reply, answer) = std_mpsc::sync_channel(1);
        state.waiting.insert(req, Waiter { asked, reply });
        if let Some(conn) = &state.conn {
            // If the connection has failed, it is about to tell everyone waiting.
            let _ = conn.out.send(msg(req));
        }
        (req, answer)
    }

    /// The answer to `req`, or None after `left`, by when no answer is wanted any more.
    fn wait_answer(
        &self,
        req: u64,
        answer: &std_mpsc::Receiver<Result<Answer, Gone>>,
        left: Duration,
    ) -> Option<Result<Answer, Gone>> {
        if let Ok(answer) = answer.recv_timeout(left) {
            return Some(answer);
        }
        if self.state().waiting.remove(&req).is_some() {
            return None;
        }
        // It arrived just as the time ran out, and is on its way.
        answer.recv().ok()
    }

    /// Takes a holder up on a host that has just come. A host just started can't answer until its
    /// swarm opens and has had a first pass, so errors mean that. The host before may also not
    /// have dropped its handle yet, which shows as held by this machine. Both are tried again
    /// until the holder's deadline, when `watch_deadlines` forgets it and this ends. Anything else
    /// is a real answer.
    fn retake(&self, conn_id: u64, holder_id: u64) {
        loop {
            let Some(reply) = self.ask_retake(conn_id, holder_id) else {
                return;
            };
            match reply {
                Ok(TakeReply::Yours { .. }) => return,
                Ok(TakeReply::Held {
                    this_machine: true, ..
                })
                | Err(_) => {}
                Ok(TakeReply::Held {
                    person, machine, ..
                }) => {
                    self.lose(holder_id, LostReason::Beaten { person, machine });
                    return;
                }
                Ok(TakeReply::Done { person, .. }) => {
                    self.lose(holder_id, LostReason::Done { person });
                    return;
                }
                Ok(TakeReply::Gone(reason)) => {
                    self.lose(holder_id, reason);
                    return;
                }
                Ok(TakeReply::Paused | TakeReply::AtLimit(_)) => {
                    self.lose(holder_id, LostReason::Lapsed);
                    return;
                }
            }
            if !self.pause(RETRY, Some(conn_id)) {
                return;
            }
        }
    }

    /// One try at taking a holder up. None when the holder has gone or the connection has: the
    /// next connection starts afresh.
    fn ask_retake(&self, conn_id: u64, holder_id: u64) -> Option<Result<TakeReply, String>> {
        let mut state = self.state();
        if state.conn.as_ref()?.id != conn_id {
            return None;
        }
        let live = state.live.get(&holder_id)?;
        let swarm = Some(live.swarm.clone());
        let (context, key, holder) = (live.context.clone(), live.key.clone(), live.holder.clone());
        let msg = |req| ClientMsg::Take {
            req,
            holder_id,
            swarm,
            context,
            key,
            holder,
            retake: true,
        };
        let (req, answer) = Inner::send(&mut state, Asked::Retake(holder_id), msg);
        drop(state);
        match self.wait_answer(req, &answer, ANSWER_WAIT) {
            Some(Ok(Answer::Take(reply))) => Some(reply),
            Some(Ok(_)) => Some(Err(WRONG_ANSWER.to_string())),
            Some(Err(Gone)) => None,
            None => Some(Err(NO_ANSWER.to_string())),
        }
    }

    /// Waits for `d`. False if the client closed, or the connection `conn_id` has gone.
    fn pause(&self, d: Duration, conn_id: Option<u64>) -> bool {
        let until = Instant::now() + d;
        let mut state = self.state();
        loop {
            let on_it = conn_id.is_none_or(|id| state.conn.as_ref().is_some_and(|c| c.id == id));
            if state.closed || !on_it {
                return false;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
