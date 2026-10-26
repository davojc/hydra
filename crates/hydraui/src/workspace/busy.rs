//! The tab strip's busy indicators: each terminal's activity (working, waiting, idle), checked
//! every second, for any program.
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, ElementId, Hsla, SharedString, Window, div,
    pulsating_between, px,
};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, WindowExt};

use super::Workspace;
use crate::activity::{Activity, busiest};
use crate::envs::PaneId;
use crate::mcp::{Alert, SessionState, alert_for};

/// How soon after a turn ends a "waiting" is taken as stray rather than a real prompt.
const STRAY_WAIT: Duration = Duration::from_secs(10);

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
    /// Whether terminal `pane` is the session of a stage in progress (starting or running).
    pub(super) fn stage_at_work(&self, pane: PaneId, cx: &gpui::App) -> bool {
        let Some(token) = self
            .panes
            .get(&pane)
            .and_then(|v| v.read(cx).mcp_token().map(str::to_string))
        else {
            return false;
        };
        crate::mcp::server::get().is_some_and(|sv| {
            let reg = crate::mcp::server::lock(sv.registry());
            matches!(reg.runs.role(&token), crate::runs::Role::Stage(r, si) if r.stages[si].status.busy())
        })
    }

    /// Recomputes every terminal's activity. Redraws when one changed, and while any is
    /// working (once a second, for the times beside them).
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
        // A permission prompt or a question comes during a turn. Claude sometimes sends one
        // just after its turn ended (an orchestrator waiting for its stage showed "needs you"
        // for nothing): it isn't one.
        let idle_for = self
            .activity
            .get(&id)
            .filter(|_| before == Some(SessionState::Idle))
            .map(|a| a.since.elapsed());
        if state == SessionState::Waiting && idle_for.is_some_and(|d| d < STRAY_WAIT) {
            crate::action_log::write(format!(
                "hook: {}: ignored waiting just after its turn ended",
                self.pane_name(id, cx)
            ));
            return;
        }
        crate::action_log::write(format!(
            "hook: {}: {before:?} -> {state:?}{}",
            self.pane_name(id, cx),
            if resumed { " (resumed session)" } else { "" }
        ));
        // A wake-up typed to it took.
        if state == SessionState::Working {
            self.sent_wakes.remove(&id);
            self.wake_tries.remove(&id);
        }
        // Its first turn is over: it can be handed work.
        if before == Some(SessionState::Working) && state == SessionState::Idle {
            self.starting_up.remove(&id);
        }
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
            && !self.pipeline_shown(id)
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
        key: impl Into<ElementId>,
        a: Option<PaneActivity>,
        at_work: bool,
        tint: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = key.into();
        let state = a.map(|a| a.state).unwrap_or_default();
        let inner = div().size(px(8.)).rounded_full().bg(tint);
        let inner = if at_work || state == Activity::Working {
            inner
                .with_animation(key, pulse(), |d, a| d.opacity(a))
                .into_any_element()
        } else {
            inner.into_any_element()
        };
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
            .child(inner)
            .into_any_element()
    }
}

/// Something at work breathing: a slow pulse, in step everywhere, at most 30 frames a second.
pub(super) fn pulse() -> Animation {
    Animation::new(Duration::from_millis(1600))
        .repeat_synced()
        .with_easing(pulsating_between(0.15, 1.0))
        .with_max_fps(30.)
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
