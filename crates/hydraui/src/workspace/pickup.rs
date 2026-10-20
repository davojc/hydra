//! Auto-pickup (spec: docs/superpowers/specs/2026-10-20-hydraui-auto-pickup-design.md §5-§8): a
//! card moved into Pickup, or sent from its menu, is typed into a free Claude session of its
//! project. The rules are in `crate::pickup`; this is the wiring.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use gpui::prelude::*;
use gpui::{App, Context, SharedString, Window};
use gpui_component::WindowExt;
use gpui_component::notification::Notification;

use super::Workspace;
use crate::envs::PaneId;
use crate::jev::{JEV_URL, JevKey};
use crate::pickup::{
    Pick, Session, candidates, choose, focused_pane, idle_in_project, is_reserved, pick,
    still_in_pickup,
};
use crate::task_board_view::SendTarget;

/// Keys the "Sent to" toasts, so a newer one replaces the last.
struct SentCard;

/// The terminal rows Jev reads.
const JEV_ROWS: usize = 60;

impl Workspace {
    /// What hydraui knows about each terminal, for choosing where a card goes. `focused` is
    /// the terminal in front while the window is active.
    fn sessions(&self, focused: Option<PaneId>, cx: &App) -> Vec<Session> {
        let now = Instant::now();
        self.panes
            .iter()
            .map(|(id, view)| {
                let view = view.read(cx);
                Session {
                    pane: *id,
                    project_root: crate::tasks_project::git_root(view.cwd()),
                    cwd: view.cwd().clone(),
                    claude: view.claude(),
                    focused: focused == Some(*id),
                    reserved: self
                        .reserved
                        .get(id)
                        .is_some_and(|at| is_reserved(*at, now)),
                    draft: view.readiness().has_draft(),
                    finished: view.readiness().finished,
                }
            })
            .collect()
    }

    /// The sessions, each draft that may have been cleared settled from its screen first.
    fn fresh_sessions(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<Session> {
        let now = Instant::now();
        let live = &self.panes;
        self.reserved
            .retain(|id, at| live.contains_key(id) && is_reserved(*at, now));
        for view in self.panes.values() {
            view.update(cx, |v, _| v.check_draft());
        }
        let focused = focused_pane(
            window.is_window_active(),
            self.envs.active().and_then(|t| t.shown),
            |id| self.tasks_tabs.get(&id) == Some(&true),
        );
        self.sessions(focused, cx)
    }

    /// The card menu's Send to submenu (§8): `project`'s idle Claude sessions, as
    /// "<terminal> (<env>)".
    pub(super) fn send_targets(&self, project: &Path, cx: &App) -> Vec<SendTarget> {
        idle_in_project(&self.sessions(None, cx), project)
            .into_iter()
            .filter_map(|pane| {
                let view = self.panes.get(&pane)?.read(cx);
                let env = self.envs.env_of(pane).unwrap_or("?");
                Some(SendTarget {
                    pane,
                    label: format!("{} ({env})", view.label()),
                })
            })
            .collect()
    }

    /// The user moved card `text` into `project`'s Pickup (§4): it left Pickup first, so a
    /// past send of it no longer holds it, and it is offered.
    pub(super) fn card_arrived(
        &mut self,
        project: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.in_flight.arrived(&project, &text);
        self.offer_card(project, text, window, cx);
    }

    /// Sends card `text` of `project` to a free Claude session (§5), or says there is none
    /// (§7). For a card moved into Pickup and for the menu's Send to a free session. A card
    /// already being offered, or sent moments ago, is left alone.
    pub(super) fn offer_card(
        &mut self,
        project: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        let boards = &self.boards;
        self.in_flight.expire(now, |p, t| {
            boards
                .get(p)
                .is_some_and(|(b, _)| still_in_pickup(b.read(cx).board(), t))
        });
        if !self.in_flight.try_start(&project, &text, now) {
            return;
        }
        let key = match JevKey::app().get() {
            Ok(Some(key)) => Some(key),
            _ => None,
        };
        let sessions = self.fresh_sessions(window, cx);
        match pick(&sessions, &project, key.is_some()) {
            Pick::Certain(panes) => self.deliver(choose(&panes), &project, &text, window, cx),
            Pick::AskJev(panes) => {
                let Some(key) = key.filter(|_| !panes.is_empty()) else {
                    self.deliver(None, &project, &text, window, cx);
                    return;
                };
                // One call per session, in parallel, off the UI thread.
                let asks: Vec<_> = panes
                    .iter()
                    .filter_map(|pane| {
                        let state = self.panes.get(pane)?.read(cx).screen_text(JEV_ROWS);
                        let (pane, key) = (*pane, key.clone());
                        Some(cx.background_spawn(async move {
                            let free =
                                crate::jev::ask(JEV_URL, &key, &state).is_ok_and(|v| v.is_free());
                            (pane, free)
                        }))
                    })
                    .collect();
                cx.spawn_in(window, async move |this, cx| {
                    let answers = futures::future::join_all(asks).await;
                    let free: Vec<PaneId> = answers
                        .into_iter()
                        .filter_map(|(pane, free)| free.then_some(pane))
                        .collect();
                    let _ = this.update_in(cx, |ws, window, cx| {
                        ws.after_jev(project, text, free, window, cx)
                    });
                })
                .detach();
            }
        }
    }

    /// Jev found `free` free. Things may have changed during the calls: the facts are checked
    /// again, and nothing is sent when the card has left Pickup (§6) or the user sent it by
    /// hand meanwhile.
    fn after_jev(
        &mut self,
        project: PathBuf,
        text: String,
        mut free: Vec<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.in_flight.running(&project, &text) {
            return;
        }
        if !self.in_pickup(&project, &text, cx) {
            self.in_flight.finish(&project, &text);
            return;
        }
        let ok = candidates(&self.fresh_sessions(window, cx), &project);
        free.retain(|p| ok.contains(p));
        self.deliver(choose(&free), &project, &text, window, cx);
    }

    /// Ends an offer: sends the card to `pane` and holds it as sent, or, with no pane or a
    /// failed send, says no session is free and lets the card be offered again.
    fn deliver(
        &mut self,
        pane: Option<PaneId>,
        project: &Path,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sent = match pane {
            Some(pane) => self.send_card(pane, project, text, window, cx),
            None => {
                self.no_free_session(project, window, cx);
                false
            }
        };
        if sent {
            self.in_flight.sent(project, text, Instant::now());
        } else {
            self.in_flight.finish(project, text);
        }
    }

    /// Sends card `text` of `project` to terminal `pane`, the user's choice in Send to (§8):
    /// no draft, finished or Jev check, and sent even while an offer for the card runs. It is
    /// then held as sent, so a later offer leaves it alone.
    pub(super) fn send_card_to(
        &mut self,
        pane: PaneId,
        project: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.in_pickup(&project, &text, cx) && self.send_card(pane, &project, &text, window, cx)
        {
            self.in_flight.sent(&project, &text, Instant::now());
        }
    }

    /// Whether `text` is still a card in Pickup on `project`'s board.
    fn in_pickup(&self, project: &Path, text: &str, cx: &App) -> bool {
        self.boards
            .get(project)
            .is_some_and(|(board, _)| still_in_pickup(board.read(cx).board(), text))
    }

    /// Types the card's prompt into terminal `pane` (§6), reserves it, and says so in a toast
    /// that shows the terminal when clicked. When nothing could be typed (the terminal is
    /// gone or not running), says no session is free instead and returns `false`.
    fn send_card(
        &mut self,
        pane: PaneId,
        project: &Path,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let prompt = crate::pickup::prompt(text);
        let typed = self
            .panes
            .get(&pane)
            .cloned()
            .filter(|view| view.update(cx, |v, cx| v.type_prompt(&prompt, cx)));
        let Some(view) = typed else {
            self.no_free_session(project, window, cx);
            return false;
        };
        self.reserved.insert(pane, Instant::now());
        let place = format!(
            "Sent to {} in {}",
            view.read(cx).label(),
            self.envs.env_of(pane).unwrap_or("?")
        );
        let ws = cx.entity();
        let note = Notification::info(place)
            .title("Tasks")
            .id1::<SentCard>(SharedString::from(format!("sent-{pane}")))
            .on_click(move |_, window, cx| {
                ws.update(cx, |ws, cx| ws.show_terminal(pane, window, cx));
            });
        window.push_notification(note, cx);
        true
    }

    /// No session can take the card (§7): it stays in Pickup. The toast names the project when
    /// this window has terminals in more than one.
    fn no_free_session(&self, project: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let projects: HashSet<PathBuf> = self
            .panes
            .values()
            .map(|v| crate::tasks_project::project_root(v.read(cx).cwd()))
            .collect();
        let title = match project.file_name() {
            Some(name) if projects.len() > 1 => name.to_string_lossy().into_owned(),
            _ => "Tasks".to_string(),
        };
        window.push_notification(
            Notification::warning("No free Claude session for this card").title(title),
            cx,
        );
    }
}
