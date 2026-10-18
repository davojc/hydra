//! The tab strip's busy indicators: each terminal's activity (working, waiting, idle), checked
//! every second, for any program.
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Hsla, div, px};
use gpui_component::ActiveTheme;

use super::Workspace;
use crate::activity::{Activity, busiest};
use crate::split::PaneId;
use crate::tabs::Tab;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneActivity {
    pub state: Activity,
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
    match a.state {
        Activity::Working => format!("Working ({for_})"),
        _ => format!("Quiet for {for_}, maybe waiting for input"),
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
            let state = view.read(cx).activity(now);
            working |= state == Activity::Working;
            let entry = self.activity.entry(*id).or_insert(PaneActivity {
                state: Activity::Idle,
                since: now,
            });
            if entry.state != state {
                *entry = PaneActivity { state, since: now };
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

    /// The tab's busiest terminal's activity, or `None` when all are idle.
    pub(super) fn tab_activity(&self, tab: &Tab) -> Option<PaneActivity> {
        let panes: Vec<PaneActivity> = tab
            .tree
            .panes()
            .into_iter()
            .filter_map(|p| self.activity.get(&p).copied())
            .collect();
        let top = busiest(panes.iter().map(|a| a.state));
        (top != Activity::Idle)
            .then(|| panes.into_iter().find(|a| a.state == top))
            .flatten()
    }

    /// The dot after a tab's name: blinking in the environment's colour while working,
    /// steady amber while quiet.
    pub(super) fn render_busy(
        &self,
        a: PaneActivity,
        tint: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let dot = div().size(px(7.)).rounded_full();
        match a.state {
            Activity::Working => dot.bg(tint).opacity(if self.blink { 1.0 } else { 0.4 }),
            _ => dot.bg(cx.theme().warning),
        }
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
            since: Instant::now() - Duration::from_secs(secs),
        };
        assert_eq!(tooltip_text(a(Activity::Working, 12)), "Working (12s)");
        assert_eq!(
            tooltip_text(a(Activity::Waiting, 180)),
            "Quiet for 3m, maybe waiting for input"
        );
    }
}
