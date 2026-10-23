//! The phone link (spec: docs/superpowers/specs/2026-10-23-hydra-mobile-design.md §4): the
//! window that holds it runs the host, answers the phone's requests and tells it what changes.
//!
//! Everything the phone is sent goes through one background worker, in order, so a session's
//! states never arrive swapped and no file is read on the UI thread.
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{App, Context, Entity, Task, Window};
use gpui_component::WindowExt;
use gpui_component::notification::Notification;
use hydra_core::paths::HydraPaths;
use hydra_link::host::{Host, HostConfig, HostEvent, Replier};
use hydra_link::pairing::PairingOffer;
use hydra_link::{AlertKind, ClientMsg, Outcome, ServerMsg, SessionInfo};

use super::Workspace;
use crate::action_log;
use crate::activity::Activity;
use crate::config::{self, UiConfig};
use crate::envs::PaneId;
use crate::link::boards::{self, PhoneChange};
use crate::link::{RcAction, owner, store, wire};
use crate::mcp::{Alert, SessionState};
use crate::task_board::TaskBoard;
use crate::tasks::{Board, Column};
use crate::tasks_project;

/// While another window holds the link, this one tries to take it this often.
const RETRY: Duration = Duration::from_secs(10);

/// What this window runs while it holds the phone link. Dropping it stops the host (closing
/// the phone's connection) and lets the link go for another window only once the host's
/// endpoint has closed: the worker owns both, and ends by stopping the host, waiting off the
/// UI thread for its endpoint to close, then dropping the lock.
pub(super) struct PhoneLink {
    worker: Option<Task<()>>,
    jobs: UnboundedSender<Job>,
    _events: Task<()>,
}

impl Drop for PhoneLink {
    fn drop(&mut self) {
        // The worker finishes on its own (not cancelled), so it can stop the host first.
        let _ = self.jobs.unbounded_send(Job::Stop);
        if let Some(worker) = self.worker.take() {
            worker.detach();
        }
    }
}

/// One thing for the worker to do, in order.
enum Job {
    /// Stop the host, then let the link go; the last job.
    Stop,
    /// A fresh pairing offer, sent back.
    Offer(futures::channel::oneshot::Sender<PairingOffer>),
    /// Forget the paired phone and hang up on it.
    Unpair,
    Send(ServerMsg),
    /// A terminal as it is now; its project and card are read off the UI thread.
    Session(PaneFacts),
    Snapshot {
        panes: Vec<PaneFacts>,
        /// The projects of the boards this window has loaded.
        boards: Vec<PathBuf>,
        reply: Replier,
    },
    Board {
        known: Vec<PathBuf>,
        project: String,
        reply: Replier,
    },
}

/// What the UI thread knows about a terminal, for the phone.
#[derive(Debug, Clone)]
struct PaneFacts {
    id: PaneId,
    env: String,
    tint: u32,
    label: String,
    cwd: PathBuf,
    claude: Option<SessionState>,
    activity: Activity,
    finished: bool,
    remote_control: bool,
    /// The name its Claude session takes cards under.
    taker: Option<String>,
    /// Unix seconds.
    changed_at: i64,
}

/// What came of trying to take the link.
enum Opened {
    Link(owner::OwnerLock, Host, UnboundedReceiver<HostEvent>),
    /// Phone access is off in config.toml: nothing was started.
    Off,
    /// Another window holds it.
    Elsewhere,
    Failed(String),
}

/// The phone settings as config.toml has them. hydraui windows don't watch the file, so
/// another window may have changed them since this one read them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Saved {
    access: bool,
    phone: Option<String>,
}

impl Saved {
    fn of(cfg: UiConfig) -> Self {
        Saved {
            access: cfg.phone_access,
            phone: cfg.phone,
        }
    }

    /// Takes these settings into `ui`, over what this window remembered.
    fn adopt(self, ui: &mut UiConfig) {
        ui.phone_access = self.access;
        ui.phone = self.phone;
    }
}

/// What taking the link does, decided by the settings on disk alone.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// Phone access is off: start nothing.
    Off,
    /// Start the host, letting in only `paired`.
    Start { paired: Option<String> },
}

fn plan(saved: &Saved) -> Plan {
    if saved.access {
        Plan::Start {
            paired: saved.phone.clone(),
        }
    } else {
        Plan::Off
    }
}

/// "#rrggbb" for an 0xRRGGBB tint.
fn hex(tint: u32) -> String {
    format!("#{:06x}", tint & 0xff_ffff)
}

/// The card `taker` is working on: the first in InProgress taken under that name.
fn card_of(board: &Board, taker: &str) -> Option<String> {
    board
        .cards(Column::InProgress)
        .iter()
        .find(|c| c.taker.as_deref() == Some(taker))
        .map(|c| c.text.clone())
}

/// Unix seconds `ago` before `now`.
fn unix_secs(now: SystemTime, ago: Duration) -> i64 {
    now.checked_sub(ago)
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

fn alert_kind(alert: Alert) -> AlertKind {
    match alert {
        Alert::NeedsYou => AlertKind::NeedsYou,
        Alert::Finished => AlertKind::Finished,
    }
}

/// Terminal `f` as the phone shows it, in `project`, whose board (when read) gives its card.
fn session_info(f: &PaneFacts, project: &Path, board: Option<&Board>) -> SessionInfo {
    let (kind, status) = wire::status(f.claude, f.activity, f.finished);
    SessionInfo {
        id: f.id,
        env: f.env.clone(),
        env_color: hex(f.tint),
        label: f.label.clone(),
        cwd: f.cwd.to_string_lossy().into_owned(),
        project: Some(project.to_string_lossy().into_owned()),
        kind,
        status,
        card: f
            .taker
            .as_deref()
            .and_then(|t| board.and_then(|b| card_of(b, t))),
        // Only a Claude session has Remote Control; a plain shell never reports it.
        remote_control: f.remote_control && kind == hydra_link::Kind::Claude,
        changed_at: f.changed_at,
    }
}

/// The terminals as the phone shows them, each project's board read once (only for a
/// terminal whose Claude takes cards). Reads files: never on the UI thread.
fn resolve(panes: &[PaneFacts]) -> Vec<SessionInfo> {
    let mut boards: HashMap<PathBuf, Option<Board>> = HashMap::new();
    panes
        .iter()
        .map(|f| {
            let project = tasks_project::project_root(&f.cwd);
            let board = match f.taker {
                Some(_) => boards
                    .entry(project.clone())
                    .or_insert_with(|| tasks_project::load(&project).ok().map(|(b, _)| b))
                    .as_ref(),
                None => None,
            };
            session_info(f, &project, board)
        })
        .collect()
}

/// The projects hydraui knows: those of its terminals and of the boards it has loaded,
/// sorted. Reads files: never on the UI thread.
fn projects(panes: &[PaneFacts], boards: &[PathBuf]) -> Vec<String> {
    panes
        .iter()
        .map(|f| tasks_project::project_root(&f.cwd))
        .chain(boards.iter().cloned())
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Reads the phone settings from config.toml and, when phone access is on there, takes the
/// link for this window and starts the host for the phone paired there, unless another
/// window has it. Reads files: never on the UI thread.
fn open(paths: &HydraPaths, window: String) -> (Saved, Opened) {
    let saved = Saved::of(config::load(paths).0);
    let opened = match plan(&saved) {
        Plan::Off => Opened::Off,
        Plan::Start { paired } => start(paths, paired, window),
    };
    (saved, opened)
}

/// Takes the link for this window and starts the host, unless another window has it.
fn start(paths: &HydraPaths, paired: Option<String>, window: String) -> Opened {
    let Some(lock) = owner::try_own(paths) else {
        return Opened::Elsewhere;
    };
    let key = match store::LinkKey::new().get_or_create() {
        Ok(key) => key,
        Err(e) => return Opened::Failed(format!("Couldn't read the phone link's key: {e}")),
    };
    match Host::start(HostConfig::new(key, paired, window, true)) {
        Ok((host, events)) => Opened::Link(lock, host, events),
        Err(e) => Opened::Failed(format!("Couldn't start the phone link: {e}")),
    }
}

/// Sends the jobs to the phone in order until the link stops.
async fn work(host: Host, lock: owner::OwnerLock, mut jobs: UnboundedReceiver<Job>) {
    while let Some(job) = jobs.next().await {
        match job {
            Job::Stop => break,
            Job::Offer(tx) => {
                let _ = tx.send(host.offer_pairing());
            }
            Job::Unpair => host.unpair(),
            Job::Send(msg) => host.broadcast(msg),
            Job::Session(f) => {
                for s in resolve(std::slice::from_ref(&f)) {
                    host.broadcast(ServerMsg::Session(s));
                }
            }
            Job::Snapshot {
                panes,
                boards,
                reply,
            } => reply.send(Outcome::Snapshot {
                sessions: resolve(&panes),
                projects: projects(&panes, &boards),
            }),
            Job::Board {
                known,
                project,
                reply,
            } => reply.send(boards::board(&known, &project).map_or_else(|o| o, Outcome::Board)),
        }
    }
    release(host, lock).await;
}

/// Stops `host`, then lets the link go once its endpoint has closed, so the next owner (another
/// window, or this one after an off-and-on) never binds the same key while it still runs.
async fn release(host: Host, lock: owner::OwnerLock) {
    host.stop_and_wait_closed().await;
    drop(lock);
}

impl Workspace {
    /// Runs the phone link in this window when phone access is on and no other window has
    /// it; while another has it, tries again every 10 s, so this one takes over when that one
    /// closes. Does nothing while the link runs here or is being sought.
    pub(super) fn phone_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ui.phone_access || self.phone.is_some() || self.phone_seeking {
            return;
        }
        self.phone_seeking = true;
        let seek = self.phone_seek;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                // Still wanted, with the window's name as it is now. Whether phone access is on
                // and which phone is paired are read from disk by `open`: another window may
                // have changed them since this one started.
                let Ok(Some((paths, name))) = this.update(cx, |ws, _| {
                    ws.phone_wanted(seek).then(|| {
                        let name = ws
                            .layout
                            .as_deref()
                            .map_or_else(|| "untitled".into(), hydraui_sets::layout_name);
                        (ws.hydra.paths.clone(), name)
                    })
                }) else {
                    return;
                };
                let (saved, opened) = cx.background_spawn(async move { open(&paths, name) }).await;
                let again = this.update_in(cx, |ws, window, cx| {
                    ws.phone_opened(seek, saved, opened, window, cx)
                });
                if !matches!(again, Ok(true)) {
                    return;
                }
                cx.background_executor().timer(RETRY).await;
            }
        })
        .detach();
    }

    /// Stops the phone link here (or the wait for it), letting another window take it.
    pub(super) fn phone_stop(&mut self) {
        self.phone_seek += 1;
        self.phone_seeking = false;
        self.phone_connected = false;
        self.phone_offer = None;
        self.phone_word = None;
        if self.phone.take().is_some() {
            action_log::write("phone: this window let the phone link go");
        }
    }

    /// Whether the search for the link numbered `seek` should go on.
    fn phone_wanted(&self, seek: u64) -> bool {
        self.phone_seek == seek && self.ui.phone_access && self.phone.is_none()
    }

    /// Takes in what `open` found; true while another window holds the link and this one
    /// should try again.
    fn phone_opened(
        &mut self,
        seek: u64,
        saved: Saved,
        opened: Opened,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.phone_seek == seek {
            // The settings on disk win over what this window remembered; the Phone page
            // shows them.
            saved.adopt(&mut self.ui);
            cx.notify();
        }
        let again = matches!(opened, Opened::Elsewhere) && self.phone_wanted(seek);
        if self.phone_seek == seek && !again {
            self.phone_seeking = false;
        }
        match opened {
            Opened::Link(lock, host, mut events) => {
                // Stopped meanwhile: let the link go, once its endpoint has closed.
                if !self.phone_wanted(seek) {
                    cx.background_spawn(release(host, lock)).detach();
                    return false;
                }
                action_log::write(format!(
                    "phone: this window holds the phone link ({})",
                    host.endpoint_id()
                ));
                let (jobs, rx) = unbounded();
                let worker = cx.background_spawn(work(host, lock, rx));
                let pump = cx.spawn_in(window, async move |this, cx| {
                    while let Some(event) = events.next().await {
                        if this
                            .update_in(cx, |ws, window, cx| ws.phone_event(event, window, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                self.phone = Some(PhoneLink {
                    worker: Some(worker),
                    jobs,
                    _events: pump,
                });
                cx.notify();
            }
            Opened::Failed(why) => {
                action_log::write(format!("phone: {why}"));
                window.push_notification(Notification::warning(why).title("Phone"), cx);
            }
            Opened::Elsewhere | Opened::Off => {}
        }
        again
    }

    /// Queues `job` for the phone; nothing when the link isn't running here (a reply in it
    /// then refuses its request).
    fn phone_job(&self, job: Job) {
        if let Some(link) = &self.phone {
            let _ = link.jobs.unbounded_send(job);
        }
    }

    /// Asks the host for a fresh pairing offer, sent back on `tx`.
    pub(super) fn phone_offer_job(&self, tx: futures::channel::oneshot::Sender<PairingOffer>) {
        self.phone_job(Job::Offer(tx));
    }

    /// Has the host forget the paired phone and hang up on it.
    pub(super) fn phone_unpair_job(&self) {
        self.phone_job(Job::Unpair);
    }

    /// Sends `msg` to the phone, in order with everything else it is sent.
    pub(super) fn phone_send(&self, msg: ServerMsg) {
        self.phone_job(Job::Send(msg));
    }

    fn phone_event(&mut self, event: HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::Request { msg, reply, .. } => self.phone_request(msg, reply, window, cx),
            HostEvent::Paired { phone, word } => self.phone_paired(phone, word, window, cx),
            HostEvent::Connected => {
                action_log::write("phone: connected");
                self.phone_connected = true;
                cx.notify();
            }
            HostEvent::Disconnected => {
                action_log::write("phone: disconnected");
                self.phone_connected = false;
                cx.notify();
            }
        }
    }

    /// A phone paired: it is the one allowed from now on, and the word both screens show is
    /// kept for the Phone page.
    fn phone_paired(
        &mut self,
        phone: String,
        word: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        action_log::write(format!("phone: paired ({word})"));
        self.ui.phone = Some(phone.clone());
        self.phone_word = Some(word);
        self.phone_offer = None;
        let paths = self.hydra.paths.clone();
        let saved = cx.background_spawn(async move { store::set_paired(&paths, Some(&phone)) });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = saved.await {
                action_log::write(format!("phone: couldn't save the paired phone: {e}"));
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::warning(format!("Couldn't save the paired phone: {e}"))
                            .title("Phone"),
                        cx,
                    )
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn phone_request(
        &mut self,
        msg: ClientMsg,
        reply: Replier,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match msg {
            ClientMsg::Snapshot => {
                let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
                ids.sort_unstable();
                let panes = ids
                    .into_iter()
                    .filter_map(|id| self.phone_facts(id, cx))
                    .collect();
                let boards = self.boards.keys().cloned().collect();
                self.phone_job(Job::Snapshot {
                    panes,
                    boards,
                    reply,
                });
            }
            ClientMsg::Board { project } => self.phone_job(Job::Board {
                known: self.phone_known(cx),
                project,
                reply,
            }),
            ClientMsg::AddCard { project, text, to } => {
                self.phone_change(project, PhoneChange::Add { text, to }, reply, window, cx)
            }
            ClientMsg::MoveCard { project, card, to } => {
                self.phone_change(project, PhoneChange::Move { card, to }, reply, window, cx)
            }
            ClientMsg::Seen { session } => {
                if !self.panes.contains_key(&session) {
                    reply.send(Outcome::Missing);
                    return;
                }
                self.phone_finished.remove(&session);
                self.phone_broadcast_session(session, cx);
                reply.send(Outcome::Ok);
            }
            ClientMsg::RemoteControl { session } => self.phone_remote_control(session, reply, cx),
            // The host answers these itself.
            ClientMsg::Hello { .. } | ClientMsg::Pair { .. } => reply.send(Outcome::Refused {
                reason: "Not expected now".into(),
            }),
        }
    }

    /// The phone asks for Remote Control in terminal `session`: typed as `/remote-control` only
    /// into an idle Claude with nothing in its input.
    fn phone_remote_control(&mut self, session: PaneId, reply: Replier, cx: &mut Context<Self>) {
        let Some(view) = self.panes.get(&session).cloned() else {
            reply.send(Outcome::Missing);
            return;
        };
        let typed = view.update(cx, |v, cx| {
            let action =
                crate::link::remote_control_reply(v.claude(), v.check_draft(), v.remote_control());
            match action {
                RcAction::AlreadyOn => Ok(()),
                RcAction::Type if v.type_prompt("/remote-control", cx) => {
                    v.set_remote_control(true);
                    Ok(())
                }
                RcAction::Type => Err("This terminal isn't running.".to_string()),
                RcAction::Refuse(reason) => Err(reason.to_string()),
            }
        });
        match typed {
            Ok(()) => {
                self.phone_broadcast_session(session, cx);
                reply.send(Outcome::Ok);
            }
            Err(reason) => reply.send(Outcome::Refused { reason }),
        }
    }

    /// The folders the phone may name a project by: the terminals' folders and the loaded
    /// boards' projects (`boards::known_root` normalises them).
    fn phone_known(&self, cx: &App) -> Vec<PathBuf> {
        self.panes
            .values()
            .map(|v| v.read(cx).cwd().clone())
            .chain(self.boards.keys().cloned())
            .collect()
    }

    /// Applies the phone's board change off the UI thread; the phone then gets the board, and
    /// a card it put into Pickup is offered to a free session.
    fn phone_change(
        &mut self,
        project: String,
        change: PhoneChange,
        reply: Replier,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known = self.phone_known(cx);
        let arrived = boards::arrived_in_pickup(&change);
        let applied = cx.background_spawn(async move { boards::apply(&known, &project, change) });
        cx.spawn_in(window, async move |this, cx| {
            let info = match applied.await {
                Ok(info) => info,
                Err(outcome) => {
                    reply.send(outcome);
                    return;
                }
            };
            reply.send(Outcome::Ok);
            let _ = this.update_in(cx, |ws, window, cx| {
                let root = PathBuf::from(&info.project);
                ws.phone_send(ServerMsg::Board(info));
                if let Some(text) = arrived {
                    ws.phone_card_arrived(root, text, window, cx);
                }
            });
        })
        .detach();
    }

    /// Terminal `id` as the UI thread knows it; `None` when it's gone.
    fn phone_facts(&self, id: PaneId, cx: &App) -> Option<PaneFacts> {
        let view = self.panes.get(&id)?.read(cx);
        let now = Instant::now();
        let (activity, since) = self
            .activity
            .get(&id)
            .map_or((view.activity(now), now), |a| (a.state, a.since));
        let env = view.env().to_string();
        let tint = self
            .tints
            .get(&env)
            .copied()
            .unwrap_or_else(|| crate::palette::env_tint(&env, None));
        Some(PaneFacts {
            id,
            env,
            tint,
            label: view.label(),
            cwd: view.cwd().clone(),
            claude: view.claude(),
            activity,
            finished: self.phone_finished.contains(&id),
            remote_control: view.remote_control(),
            taker: view.taker_name(),
            changed_at: unix_secs(SystemTime::now(), since.elapsed()),
        })
    }

    /// Tells the phone how terminal `id` is now.
    pub(super) fn phone_broadcast_session(&self, id: PaneId, cx: &App) {
        if self.phone.is_none() {
            return;
        }
        if let Some(facts) = self.phone_facts(id, cx) {
            self.phone_job(Job::Session(facts));
        }
    }

    /// A Claude hook in terminal `id` reported, raising `alert` (if any); `changed` when its
    /// state isn't what it was. It shows as Finished until the phone has seen it or it changes
    /// again, and the alert goes to the phone whether or not the terminal is in sight on the
    /// desktop (phone spec §4).
    pub(super) fn phone_session_changed(
        &mut self,
        id: PaneId,
        alert: Option<Alert>,
        changed: bool,
        cx: &App,
    ) {
        if alert == Some(Alert::Finished) {
            self.phone_finished.insert(id);
        } else if changed {
            self.phone_finished.remove(&id);
        }
        self.phone_broadcast_session(id, cx);
        if let Some(alert) = alert {
            self.phone_send(ServerMsg::Alert {
                session: id,
                kind: alert_kind(alert),
            });
        }
    }

    /// Terminal `id` closed.
    pub(super) fn phone_pane_gone(&mut self, id: PaneId) {
        self.phone_finished.remove(&id);
        self.phone_send(ServerMsg::SessionGone { id });
    }

    /// A loaded board reloaded or was changed here: the phone gets it as it is now.
    pub(super) fn phone_board_changed(&self, board: &Entity<TaskBoard>, cx: &App) {
        if self.phone.is_none() {
            return;
        }
        let board = board.read(cx);
        self.phone_send(ServerMsg::Board(wire::board_info(
            board.project(),
            board.board(),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydra_link::{Kind, Status};

    use crate::tasks::Card;

    fn facts() -> PaneFacts {
        PaneFacts {
            id: 7,
            env: "work".into(),
            tint: 0x0a_0b0c,
            label: "api".into(),
            cwd: PathBuf::from("/w/api/src"),
            claude: Some(SessionState::Idle),
            activity: Activity::Idle,
            finished: true,
            remote_control: true,
            taker: Some("api-1".into()),
            changed_at: 1_700_000_000,
        }
    }

    fn card(text: &str, taker: Option<&str>) -> Card {
        Card {
            text: text.into(),
            taker: taker.map(Into::into),
            commit: None,
        }
    }

    #[test]
    fn a_session_shows_its_card_env_colour_and_state() {
        let mut board = Board::default();
        board.columns[Column::Pickup as usize].push(card("waiting", Some("api-1")));
        board.columns[Column::InProgress as usize].push(card("other's", Some("web-2")));
        board.columns[Column::InProgress as usize].push(card("mine", Some("api-1")));
        let s = session_info(&facts(), Path::new("/w/api"), Some(&board));
        assert_eq!(s.id, 7);
        assert_eq!(s.env_color, "#0a0b0c");
        assert_eq!(s.card.as_deref(), Some("mine"));
        assert_eq!((s.kind, s.status), (Kind::Claude, Status::Finished));
        assert_eq!(s.project.as_deref(), Some("/w/api"));
        assert_eq!(s.changed_at, 1_700_000_000);
        assert!(s.remote_control);
        // A plain shell never reports Remote Control, whatever its environment says.
        let shell = PaneFacts {
            claude: None,
            ..facts()
        };
        let s = session_info(&shell, Path::new("/w/api"), None);
        assert_eq!(s.kind, Kind::Shell);
        assert!(!s.remote_control);
        // No board read, or no name to take cards under: no card.
        assert_eq!(session_info(&facts(), Path::new("/w/api"), None).card, None);
        let nameless = PaneFacts {
            taker: None,
            ..facts()
        };
        assert_eq!(
            session_info(&nameless, Path::new("/w/api"), Some(&board)).card,
            None
        );
    }

    fn saved(access: bool, phone: Option<&str>) -> Saved {
        Saved {
            access,
            phone: phone.map(Into::into),
        }
    }

    #[test]
    fn with_phone_access_off_on_disk_nothing_starts() {
        assert_eq!(plan(&saved(false, None)), Plan::Off);
        assert_eq!(plan(&saved(false, Some("phone-a"))), Plan::Off);
        assert_eq!(
            plan(&saved(true, Some("phone-a"))),
            Plan::Start {
                paired: Some("phone-a".into())
            }
        );
        assert_eq!(plan(&saved(true, None)), Plan::Start { paired: None });
    }

    #[test]
    fn the_settings_on_disk_win_over_what_the_window_remembered() {
        // This window started with phone-a paired and access on; another window has since
        // unpaired it and paired phone-b, or turned access off.
        let remembered = UiConfig {
            phone_access: true,
            phone: Some("phone-a".into()),
            ..UiConfig::default()
        };
        let mut ui = remembered.clone();
        saved(true, Some("phone-b")).adopt(&mut ui);
        assert_eq!(ui.phone.as_deref(), Some("phone-b"));
        assert!(ui.phone_access);
        let mut ui = remembered.clone();
        saved(true, None).adopt(&mut ui);
        assert_eq!(ui.phone, None, "an unpaired phone is forgotten");
        let mut ui = remembered;
        saved(false, Some("phone-a")).adopt(&mut ui);
        assert!(!ui.phone_access);
    }

    #[test]
    fn open_reads_the_settings_from_disk_and_starts_nothing_while_off() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        store::set_paired(&paths, Some("phone-b")).unwrap();
        store::set_enabled(&paths, false).unwrap();
        let (got, opened) = open(&paths, "main".into());
        assert_eq!(got, saved(false, Some("phone-b")));
        assert!(matches!(opened, Opened::Off));
        assert!(
            !owner::held_elsewhere(&paths),
            "the link wasn't taken while access is off"
        );
    }

    #[test]
    fn only_an_in_progress_card_is_a_sessions_card() {
        let mut board = Board::default();
        board.columns[Column::Verify as usize].push(card("done-ish", Some("a")));
        assert_eq!(card_of(&board, "a"), None);
    }

    #[test]
    fn times_are_unix_seconds() {
        let now = UNIX_EPOCH + Duration::from_secs(1000);
        assert_eq!(unix_secs(now, Duration::from_secs(30)), 970);
        assert_eq!(unix_secs(now, Duration::from_secs(5000)), 0);
    }

    #[test]
    fn projects_are_listed_once_each() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir_all(a.join(".git")).unwrap();
        std::fs::write(a.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let pane = |cwd: PathBuf| PaneFacts { cwd, ..facts() };
        let panes = [pane(a.join("src")), pane(a.clone())];
        let got = projects(&panes, &[a.clone(), d.path().join("b")]);
        let want: Vec<String> = [a, d.path().join("b")]
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, want);
    }
}
