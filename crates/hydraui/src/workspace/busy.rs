//! The tab strip's busy indicators: each terminal's activity (working, waiting, idle) and
//! whether it's Claude Code, checked every second.
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Hsla, SharedString, div, pulsating_between, px,
    rgb,
};
use gpui_component::ActiveTheme;

use super::Workspace;
use crate::activity::{Activity, busiest, is_claude};
use crate::split::PaneId;
use crate::tabs::Tab;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneActivity {
    pub state: Activity,
    pub claude: bool,
    /// The program running (`npm`), when it isn't just the shell.
    pub program: Option<String>,
    /// When `state` last changed.
    pub since: Instant,
}

/// Shells and console hosts aren't "the program".
const HOSTS: [&str; 6] = [
    "pwsh.exe",
    "powershell.exe",
    "bash.exe",
    "cmd.exe",
    "conhost.exe",
    "openconsole.exe",
];

/// The newest non-shell process's name, without `.exe`.
fn program_name(names: &[String]) -> Option<String> {
    names
        .iter()
        .rev()
        .find(|n| !HOSTS.iter().any(|h| n.eq_ignore_ascii_case(h)))
        .map(|n| n.strip_suffix(".exe").unwrap_or(n).to_string())
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

impl Workspace {
    /// Recomputes every terminal's activity; redraws only when one changed.
    pub(super) fn tick_activity(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut changed = false;
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            let Some(view) = self.panes.get(&id).cloned() else {
                continue;
            };
            let v = view.read(cx);
            let state = v.activity(now);
            let (claude, program) = if state == Activity::Idle {
                (false, None)
            } else {
                let names = v.process_names();
                (is_claude(&names, v.title()), program_name(&names))
            };
            let entry = self.activity.entry(id).or_insert(PaneActivity {
                state: Activity::Idle,
                claude: false,
                program: None,
                since: now,
            });
            if entry.state != state {
                entry.since = now;
                changed = true;
            }
            changed |= entry.claude != claude;
            entry.state = state;
            entry.claude = claude;
            entry.program = program;
        }
        let live = &self.panes;
        self.activity.retain(|id, _| live.contains_key(id));
        if changed {
            cx.notify();
        }
    }

    /// The tab's busiest terminal's activity, or `None` when all are idle.
    pub(super) fn tab_activity(&self, tab: &Tab) -> Option<PaneActivity> {
        let panes: Vec<&PaneActivity> = tab
            .tree
            .panes()
            .into_iter()
            .filter_map(|p| self.activity.get(&p))
            .collect();
        let top = busiest(panes.iter().map(|a| a.state));
        if top == Activity::Idle {
            return None;
        }
        // Claude first among equals: it's what people most want to keep an eye on.
        panes
            .into_iter()
            .filter(|a| a.state == top)
            .max_by_key(|a| a.claude)
            .cloned()
    }

    /// The indicator after a tab's name, and its tooltip text.
    pub(super) fn render_busy(
        &self,
        tab_id: u64,
        a: &PaneActivity,
        tint: Hsla,
        cx: &Context<Self>,
    ) -> (AnyElement, SharedString) {
        let waiting = cx.theme().warning;
        let dot = div().size(px(7.)).rounded_full();
        let dot = match a.state {
            Activity::Working => dot
                .bg(tint)
                .with_animation(
                    ("busy", tab_id),
                    Animation::new(Duration::from_millis(1400))
                        .repeat()
                        .with_easing(pulsating_between(0.3, 1.0)),
                    |d, delta| d.opacity(delta),
                )
                .into_any_element(),
            _ => dot.bg(waiting).into_any_element(),
        };
        let what = if a.claude {
            "Claude Code".to_string()
        } else {
            a.program.clone().unwrap_or_else(|| "A command".into())
        };
        let doing = match a.state {
            Activity::Working => "working",
            _ => "quiet, maybe waiting for you",
        };
        let tip = format!("{what} · {doing} ({})", ago(a.since.elapsed()));
        let el = div()
            .flex()
            .items_center()
            .gap_1()
            .when(a.claude, |d| {
                // Claude's own colour, so its tabs stand out from other busy ones.
                d.child(div().text_xs().text_color(rgb(0xd97757)).child("✻"))
            })
            .child(dot)
            .into_any_element();
        (el, tip.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_the_newest_non_shell_process() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            program_name(&names(&["pwsh.exe", "node.exe", "conhost.exe"])),
            Some("node".into())
        );
        assert_eq!(program_name(&names(&["pwsh.exe", "OpenConsole.exe"])), None);
        assert_eq!(ago(Duration::from_secs(12)), "12s");
        assert_eq!(ago(Duration::from_secs(180)), "3m");
    }
}
