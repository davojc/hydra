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
use crate::action_log;
use crate::envs::PaneId;
use crate::jev::{JEV_URL, JevKey, Verdict};
use crate::pickup::{
    Pick, Session, candidates, choose, describe, focused_pane, idle_in_project, is_reserved,
    jev_failure, no_free_text, pick, still_in_pickup,
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

    /// Terminal `pane` as the log names it: "<terminal> (<env>) #<pane>".
    pub(super) fn pane_name(&self, pane: PaneId, cx: &App) -> String {
        let label = self
            .panes
            .get(&pane)
            .map_or_else(|| "?".to_string(), |v| v.read(cx).label());
        format!(
            "{label} ({}) #{pane}",
            self.envs.env_of(pane).unwrap_or("?")
        )
    }

    /// `panes` by name for the log, comma-separated.
    fn pane_names(&self, panes: &[PaneId], cx: &App) -> String {
        let names: Vec<String> = panes.iter().map(|p| self.pane_name(*p, cx)).collect();
        if names.is_empty() {
            "none".into()
        } else {
            names.join(", ")
        }
    }

    /// Logs every terminal and why it can or can't take a card for `project`.
    fn log_sessions(&self, sessions: &[Session], project: &Path, cx: &App) {
        let mut text = format!("pickup: {} terminal(s) in this window", sessions.len());
        for s in sessions {
            text.push_str(&format!(
                "\n{}: {} [cwd {}]",
                self.pane_name(s.pane, cx),
                describe(s, project),
                s.cwd.display()
            ));
        }
        action_log::write(text);
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
        action_log::write(format!(
            "pickup: card moved into Pickup in {}: {:?}",
            project.display(),
            text
        ));
        self.in_flight.arrived(&project, &text);
        self.offer_card(project, text, false, window, cx);
    }

    /// Sends card `text` of `project` to a free Claude session (§5), or says there is none
    /// (§7). For a card moved into Pickup and for the menu's Send to a free session
    /// (`by_hand`). A card already being offered, or sent moments ago, is left alone; when the
    /// user asked by hand, a toast says so.
    pub(super) fn offer_card(
        &mut self,
        project: PathBuf,
        text: String,
        by_hand: bool,
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
        if by_hand {
            action_log::write(format!(
                "pickup: Send to a free session, in {}: {:?}",
                project.display(),
                text
            ));
        }
        if !self.in_flight.try_start(&project, &text, now) {
            action_log::write("pickup: already on its way (being offered, or sent < 30 s ago)");
            if by_hand {
                window.push_notification(
                    Notification::info("This card is already on its way").title("Tasks"),
                    cx,
                );
            }
            return;
        }
        let key = match JevKey::app().get() {
            Ok(Some(key)) => Some(key),
            Ok(None) => {
                action_log::write("pickup: no Jev key set");
                None
            }
            Err(e) => {
                action_log::write(format!("pickup: can't read the Jev key: {e}"));
                None
            }
        };
        let sessions = self.fresh_sessions(window, cx);
        self.log_sessions(&sessions, &project, cx);
        match pick(&sessions, &project, key.is_some()) {
            Pick::Certain(panes) => {
                action_log::write(format!(
                    "pickup: clearly free: {}",
                    self.pane_names(&panes, cx)
                ));
                self.deliver(choose(&panes), &project, &text, None, window, cx)
            }
            Pick::AskJev(panes) => {
                let Some(key) = key.filter(|_| !panes.is_empty()) else {
                    action_log::write("pickup: none clearly free, and none for Jev to judge");
                    self.deliver(None, &project, &text, None, window, cx);
                    return;
                };
                action_log::write(format!(
                    "pickup: none clearly free; asking Jev about {}",
                    self.pane_names(&panes, cx)
                ));
                // One call per session, in parallel, off the UI thread.
                let asks: Vec<_> = panes
                    .iter()
                    .filter_map(|pane| {
                        let state = self.panes.get(pane)?.read(cx).screen_text(JEV_ROWS);
                        let name = self.pane_name(*pane, cx);
                        let (pane, key) = (*pane, key.clone());
                        Some(cx.background_spawn(async move {
                            // The error is a short reason without the key (`jev::ask`).
                            let verdict =
                                crate::jev::ask(JEV_URL, &key, &state).map_err(|e| e.to_string());
                            log_jev(&name, &state, &verdict);
                            (pane, verdict.map(|v| v.is_free()))
                        }))
                    })
                    .collect();
                cx.spawn_in(window, async move |this, cx| {
                    let answers: Vec<(PaneId, Result<bool, String>)> =
                        futures::future::join_all(asks).await;
                    let results: Vec<Result<bool, String>> =
                        answers.iter().map(|(_, r)| r.clone()).collect();
                    let failure = jev_failure(&results);
                    let free: Vec<PaneId> = answers
                        .into_iter()
                        .filter_map(|(pane, free)| (free == Ok(true)).then_some(pane))
                        .collect();
                    let _ = this.update_in(cx, |ws, window, cx| {
                        ws.after_jev(project, text, free, failure, window, cx)
                    });
                })
                .detach();
            }
        }
    }

    /// Jev found `free` free; `failure` is why, when every call failed. Things may have
    /// changed during the calls: the facts are checked again, and nothing is sent when the card
    /// has left Pickup (§6) or the user sent it by hand meanwhile.
    fn after_jev(
        &mut self,
        project: PathBuf,
        text: String,
        mut free: Vec<PaneId>,
        failure: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.in_flight.running(&project, &text) {
            action_log::write("pickup: after Jev, the card had already been sent by hand");
            return;
        }
        if !self.in_pickup(&project, &text, cx) {
            action_log::write("pickup: after Jev, the card had left Pickup: not sent");
            self.in_flight.finish(&project, &text);
            return;
        }
        let ok = candidates(&self.fresh_sessions(window, cx), &project);
        let before = free.len();
        free.retain(|p| ok.contains(p));
        if free.len() < before {
            action_log::write(format!(
                "pickup: {} that Jev found free changed meanwhile (busy, typed in or in front)",
                before - free.len()
            ));
        }
        action_log::write(format!(
            "pickup: Jev found free: {}",
            self.pane_names(&free, cx)
        ));
        let pane = choose(&free);
        self.deliver(pane, &project, &text, failure.as_deref(), window, cx);
    }

    /// Ends an offer: sends the card to `pane` and holds it as sent, or, with no pane or a
    /// failed send, says no session is free (and why Jev couldn't help, `jev_failure`) and
    /// lets the card be offered again.
    fn deliver(
        &mut self,
        pane: Option<PaneId>,
        project: &Path,
        text: &str,
        jev_failure: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sent = match pane {
            Some(pane) => self.send_card(pane, project, text, window, cx),
            None => {
                action_log::write(match jev_failure {
                    Some(why) => format!("pickup: no free session (every Jev call failed: {why})"),
                    None => "pickup: no free session".to_string(),
                });
                self.no_free_session(project, jev_failure, window, cx);
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
        action_log::write(format!(
            "pickup: Send to {} by hand: {:?}",
            self.pane_name(pane, cx),
            text
        ));
        if !self.in_pickup(&project, &text, cx) {
            action_log::write("pickup: the card is no longer in Pickup: not sent");
            return;
        }
        if self.send_card(pane, &project, &text, window, cx) {
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
            action_log::write(format!(
                "pickup: couldn't type into {} (gone or not running)",
                self.pane_name(pane, cx)
            ));
            self.no_free_session(project, None, window, cx);
            return false;
        };
        action_log::write(format!(
            "pickup: typed into {}: {prompt}",
            self.pane_name(pane, cx)
        ));
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
    /// this window has terminals in more than one, and says why when every Jev call failed.
    fn no_free_session(
        &self,
        project: &Path,
        jev_failure: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            Notification::warning(no_free_text(jev_failure)).title(title),
            cx,
        );
    }
}

/// Logs Jev's answer about terminal `name`, with the screen it was sent.
fn log_jev(name: &str, state: &str, verdict: &Result<Verdict, String>) {
    let said = match verdict {
        Ok(v) => format!(
            "waiting {:.2}, finished {:.2}, draft {:.2}: {}",
            v.waiting,
            v.finished,
            v.draft,
            if v.is_free() { "free" } else { "not free" }
        ),
        Err(e) => format!("failed: {e}"),
    };
    action_log::write(format!(
        "jev: {name}: {said}\nscreen sent ({} lines):\n{state}",
        state.lines().count()
    ));
}
