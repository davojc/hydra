//! The tab strip's busy indicators: each terminal's activity (working, waiting, idle), checked
//! every second, for any program.
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Hsla, SharedString, Window, div, px};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, WindowExt};

use super::Workspace;
use crate::activity::{Activity, busiest};
use crate::envs::PaneId;
use crate::mcp::{Alert, SessionState, alert_for};

/// Keys the Claude alerts' toasts, one per terminal, so a newer one replaces the last.
struct ClaudeAlert;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneActivity {
    pub state: Activity,
    /// `state` is what a Claude session's hooks said, not a guess from output.
    pub claude: bool,
    /// When `state` last changed.
    pub since: Instant,
}

/// "12s", "3m", "1h".
fn ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// The tab's tooltip, worked out when it's shown so the time is current.
pub(super) fn tooltip_text(a: PaneActivity) -> String {
    let for_ = ago(a.since.elapsed());
    match (a.claude, a.state) {
        (true, Activity::Working) => format!("Claude is working ({for_})"),
        (true, Activity::Waiting) => format!("Claude needs you ({for_})"),
        (true, Activity::Idle) => format!("Claude finished ({for_} ago)"),
        (false, Activity::Working) => format!("Working ({for_})"),
        (false, _) => format!("Quiet for {for_}, maybe waiting for input"),
    }
}

impl Workspace {
    /// Recomputes every terminal's activity. Redraws when one changed, and while any is
    /// working (once a second, for the blinking dot: no per-frame animation).
    pub(super) fn tick_activity(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut changed = false;
        let mut working = false;
        for (id, view) in &self.panes {
            let view = view.read(cx);
            let (state, claude) = (view.activity(now), view.claude().is_some());
            working |= state == Activity::Working;
            let entry = self.activity.entry(*id).or_insert(PaneActivity {
                state: Activity::Idle,
                claude,
                since: now,
            });
            if entry.state != state || entry.claude != claude {
                *entry = PaneActivity {
                    state,
                    claude,
                    since: now,
                };
                changed = true;
            }
        }
        let live = &self.panes;
        self.activity.retain(|id: &PaneId, _| live.contains_key(id));
        if working {
            self.blink = !self.blink;
        }
        if changed || working {
            cx.notify();
        }
    }

    /// A Claude hook in the terminal holding `token` said its session is in `state`. When it
    /// starts asking for you, or finishes, out of sight, you are told (see `alert`).
    pub(super) fn session_state(
        &mut self,
        token: &str,
        state: SessionState,
        resumed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self
            .panes
            .iter()
            .find(|(_, v)| v.read(cx).mcp_token() == Some(token))
            .map(|(id, v)| (*id, v.clone()));
        let Some((id, view)) = found else {
            crate::action_log::write(format!("hook: {state:?} from a token no terminal holds"));
            return;
        };
        let before = view.read(cx).claude();
        crate::action_log::write(format!(
            "hook: {}: {before:?} -> {state:?}{}",
            self.pane_name(id, cx),
            if resumed { " (resumed session)" } else { "" }
        ));
        if state == SessionState::Ended {
            // Claude ended without reporting: its stage fails (orchestrator spec §3.4).
            self.stage_session_gone(token);
        }
        // A card sent here has reached the session, or it moved on: no longer reserved.
        self.reserved.remove(&id);
        view.update(cx, |v, cx| v.set_claude(state, resumed, cx));
        self.tick_activity(cx);
        if self.pending_wakes.contains_key(&id) {
            self.send_wakes(window, cx);
        }
        if let Some(alert) = alert_for(before, state) {
            self.alert(id, alert, window, cx);
        }
    }

    /// The Claude session in the terminal holding `token` moved a card to Verify: its turn is
    /// clearly finished.
    pub(super) fn session_verified(&mut self, token: &str, cx: &mut Context<Self>) {
        if let Some(view) = self
            .panes
            .iter()
            .find(|(_, v)| v.read(cx).mcp_token() == Some(token))
            .map(|(id, v)| (*id, v.clone()))
        {
            crate::action_log::write(format!(
                "hook: {}: moved a card to Verify (clearly finished)",
                self.pane_name(view.0, cx)
            ));
            view.1.update(cx, |v, _| v.verified());
        }
    }

    /// Whether terminal `id` is in front of you: the window is active and the terminal is the
    /// selected environment's shown one, not behind its Tasks tab.
    fn in_sight(&self, id: PaneId, window: &Window) -> bool {
        window.is_window_active()
            && !self.all_tasks_shown()
            && self.envs.active().and_then(|t| t.shown) == Some(id)
            && self.tasks_tabs.get(&id) != Some(&true)
    }

    /// Tells you a terminal out of sight needs you: its environment tab gets a badge until it
    /// is shown, the taskbar button flashes while the window is in the background, and a toast
    /// names it; clicking the toast shows the terminal.
    fn alert(&mut self, id: PaneId, alert: Alert, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_sight(id, window) {
            return;
        }
        self.attention.insert(id);
        if !window.is_window_active() {
            crate::claim::flash(crate::claim::hwnd_of(window));
        }
        let Some(view) = self.panes.get(&id) else {
            return;
        };
        let place = format!(
            "{} in {}",
            view.read(cx).label(),
            self.envs.env_of(id).unwrap_or("?")
        );
        let ws = cx.entity();
        let note = match alert {
            Alert::NeedsYou => Notification::warning(place)
                .title("Claude needs you")
                .autohide(false),
            Alert::Finished => Notification::info(place).title("Claude finished"),
        };
        let note = note
            .id1::<ClaudeAlert>(SharedString::from(format!("claude-{id}")))
            .on_click(move |_, window, cx| {
                ws.update(cx, |ws, cx| ws.show_terminal(id, window, cx));
            });
        window.push_notification(note, cx);
        cx.notify();
    }

    /// The busiest activity among `panes` (an environment's terminals), or `None` when all
    /// are idle.
    pub(super) fn tab_activity(&self, panes: &[PaneId]) -> Option<PaneActivity> {
        let panes: Vec<PaneActivity> = panes
            .iter()
            .filter_map(|p| self.activity.get(p).copied())
            .collect();
        let top = busiest(panes.iter().map(|a| a.state));
        (top != Activity::Idle)
            .then(|| panes.into_iter().find(|a| a.state == top))
            .flatten()
    }

    /// The tab's dot in the environment's colour: blinking while working, ringed in amber
    /// while quiet, steady when idle.
    pub(super) fn render_tab_dot(
        &self,
        a: Option<PaneActivity>,
        tint: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let state = a.map(|a| a.state).unwrap_or_default();
        let dim = state == Activity::Working && !self.blink;
        div()
            .size(px(12.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .when(state == Activity::Waiting, |d| {
                d.border_2().border_color(cx.theme().warning)
            })
            .child(
                div()
                    .size(px(8.))
                    .rounded_full()
                    .bg(tint)
                    .opacity(if dim { 0.3 } else { 1.0 }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tooltip_says_how_long() {
        let a = |state, secs| PaneActivity {
            state,
            claude: false,
            since: Instant::now() - Duration::from_secs(secs),
        };
        assert_eq!(tooltip_text(a(Activity::Working, 12)), "Working (12s)");
        assert_eq!(
            tooltip_text(a(Activity::Waiting, 180)),
            "Quiet for 3m, maybe waiting for input"
        );
    }
}
