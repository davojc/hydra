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
use hydra_link::{AlertKind, ClientMsg, Outcome, ServerMsg, SessionInfo};

use super::Workspace;
use crate::action_log;
use crate::activity::Activity;
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
/// the phone's connection) and then lets the link go for another window.
pub(super) struct PhoneLink {
    /// Owns the host: dropping it (cancelling the worker) stops the host.
    _worker: Task<()>,
    jobs: UnboundedSender<Job>,
    _events: Task<()>,
    _lock: owner::OwnerLock,
}

/// One thing for the worker to send, in order.
enum Job {
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
    /// Another window holds it.
    Elsewhere,
    Failed(String),
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
        remote_control: f.remote_control,
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

/// Takes the link for this window and starts the host, unless another window has it.
fn open(paths: &HydraPaths, paired: Option<String>, window: String) -> Opened {
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
async fn work(host: Host, mut jobs: UnboundedReceiver<Job>) {
    while let Some(job) = jobs.next().await {
        match job {
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
                // Still wanted, with the window's name as it is now.
                let Ok(Some((paths, paired, name))) = this.update(cx, |ws, _| {
                    ws.phone_wanted(seek).then(|| {
                        let name = ws
                            .layout
                            .as_deref()
                            .map_or_else(|| "untitled".into(), hydraui_sets::layout_name);
                        (ws.hydra.paths.clone(), ws.ui.phone.clone(), name)
                    })
                }) else {
                    return;
                };
                let opened = cx
                    .background_spawn(async move { open(&paths, paired, name) })
                    .await;
                if !matches!(opened, Opened::Elsewhere) {
                    let _ = this.update_in(cx, |ws, window, cx| {
                        ws.phone_opened(seek, opened, window, cx)
                    });
                    return;
                }
                cx.background_executor().timer(RETRY).await;
            }
        })
        .detach();
    }

    /// Stops the phone link here (or the wait for it), letting another window take it.
    #[expect(dead_code, reason = "the Phone page's switch (Task 9) calls it")]
    pub(super) fn phone_stop(&mut self) {
        self.phone_seek += 1;
        self.phone_seeking = false;
        if self.phone.take().is_some() {
            action_log::write("phone: this window let the phone link go");
        }
    }

    /// Whether the search for the link numbered `seek` should go on.
    fn phone_wanted(&self, seek: u64) -> bool {
        self.phone_seek == seek && self.ui.phone_access && self.phone.is_none()
    }

    fn phone_opened(
        &mut self,
        seek: u64,
        opened: Opened,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.phone_seek == seek {
            self.phone_seeking = false;
        }
        match opened {
            Opened::Link(lock, host, mut events) => {
                // Stopped meanwhile: dropping them lets the link go.
                if !self.phone_wanted(seek) {
                    return;
                }
                action_log::write(format!(
                    "phone: this window holds the phone link ({})",
                    host.endpoint_id()
                ));
                let (jobs, rx) = unbounded();
                let worker = cx.background_spawn(work(host, rx));
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
                    _worker: worker,
                    jobs,
                    _events: pump,
                    _lock: lock,
                });
                cx.notify();
            }
            Opened::Failed(why) => {
                action_log::write(format!("phone: {why}"));
                window.push_notification(Notification::warning(why).title("Phone"), cx);
            }
            Opened::Elsewhere => {}
        }
    }

    /// Queues `job` for the phone; nothing when the link isn't running here (a reply in it
    /// then refuses its request).
    fn phone_job(&self, job: Job) {
        if let Some(link) = &self.phone {
            let _ = link.jobs.unbounded_send(job);
        }
    }

    /// Sends `msg` to the phone, in order with everything else it is sent.
    pub(super) fn phone_send(&self, msg: ServerMsg) {
        self.phone_job(Job::Send(msg));
    }

    fn phone_event(&mut self, event: HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::Request { msg, reply, .. } => self.phone_request(msg, reply, window, cx),
            HostEvent::Paired { phone, word } => self.phone_paired(phone, word, window, cx),
            HostEvent::Connected => action_log::write("phone: connected"),
            HostEvent::Disconnected => action_log::write("phone: disconnected"),
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
