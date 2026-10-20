//! Each environment's Claude usage on the status bar (MCP server spec §5.2): the five-hour and
//! seven-day limits its sessions' status lines report, coloured as they fill, with the account
//! they belong to; clicking opens the environment's accounts, where it can switch account.
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{AnyElement, Context, div};
use gpui_component::ActiveTheme;
use gpui_component::tooltip::Tooltip;

use super::Workspace;
use crate::mcp::{Limit, Usage};

/// At or above this percent a limit shows as a warning, then as danger.
const WARN: f64 = 80.0;
const DANGER: f64 = 95.0;

/// "5h 42% · 7d 18%", for the limits Claude gave; `None` without any.
pub(super) fn usage_text(u: &Usage) -> Option<String> {
    let parts: Vec<String> = [("5h", u.five_hour), ("7d", u.seven_day)]
        .into_iter()
        .filter_map(|(label, l)| l.map(|l| format!("{label} {}%", l.used.round())))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The fullest limit's percent.
fn fullest(u: &Usage) -> f64 {
    [u.five_hour, u.seven_day]
        .into_iter()
        .flatten()
        .map(|l| l.used)
        .fold(0.0, f64::max)
}

/// "2h 10m", "3d 4h", "5m": how long until `resets_at` from `now` (Unix seconds).
fn resets_in(resets_at: i64, now: i64) -> String {
    let s = (resets_at - now).max(0);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

/// The tooltip: whose limits these are, each window's use and when it resets, and where to
/// switch account.
pub(super) fn usage_tooltip(u: &Usage, account: Option<&str>, now: i64) -> String {
    let mut lines = vec![match account {
        Some(a) => format!("Claude: {a}"),
        None => "Claude".to_string(),
    }];
    let window = |name: &str, l: Limit| {
        let reset = l
            .resets_at
            .map(|t| format!(", resets in {}", resets_in(t, now)))
            .unwrap_or_default();
        format!("{name}: {}% used{reset}", l.used.round())
    };
    lines.extend(u.five_hour.map(|l| window("5-hour limit", l)));
    lines.extend(u.seven_day.map(|l| window("7-day limit", l)));
    lines.push("Click to see or switch the environment's accounts".to_string());
    lines.join("\n")
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Workspace {
    /// A Claude status line in the terminal holding `token` reported `usage`: it becomes its
    /// environment's, when it says anything about the limits (the limits are the account's).
    pub(super) fn usage_reported(&mut self, token: &str, usage: Usage, cx: &mut Context<Self>) {
        if !usage.has_limits() {
            return;
        }
        let env = self
            .panes
            .values()
            .find(|v| v.read(cx).mcp_token() == Some(token))
            .map(|v| v.read(cx).env().to_string());
        if let Some(env) = env
            && self.usage.get(&env) != Some(&usage)
        {
            self.usage.insert(env, usage);
            cx.notify();
        }
    }

    /// The status bar's usage for `env`, if its sessions have reported any.
    pub(super) fn render_usage(&self, env: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let u = *self.usage.get(env)?;
        let text = usage_text(&u)?;
        let account = self.status.get(env).and_then(|reports| {
            reports
                .iter()
                .find(|r| r.provider == "claude")
                .map(|r| r.detail.clone())
        });
        let colour = match fullest(&u) {
            p if p >= DANGER => cx.theme().danger,
            p if p >= WARN => cx.theme().warning,
            _ => cx.theme().muted_foreground,
        };
        let tip = usage_tooltip(&u, account.as_deref(), now_secs());
        Some(
            div()
                .id("claude-usage")
                .flex_none()
                .text_color(colour)
                .cursor_pointer()
                .child(format!("claude {text}"))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(|ws, _, _, cx| {
                    ws.accounts_open = true;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(five: Option<f64>, seven: Option<f64>) -> Usage {
        let l = |p: Option<f64>| {
            p.map(|used| Limit {
                used,
                resets_at: Some(10_000),
            })
        };
        Usage {
            five_hour: l(five),
            seven_day: l(seven),
            context: None,
        }
    }

    #[test]
    fn the_text_shows_the_limits_given() {
        assert_eq!(
            usage_text(&usage(Some(42.4), Some(18.0))).as_deref(),
            Some("5h 42% · 7d 18%")
        );
        assert_eq!(
            usage_text(&usage(None, Some(96.0))).as_deref(),
            Some("7d 96%")
        );
        assert_eq!(usage_text(&usage(None, None)), None);
        assert_eq!(fullest(&usage(Some(30.0), Some(85.0))), 85.0);
    }

    #[test]
    fn resets_read_naturally() {
        assert_eq!(resets_in(10_000, 10_000 - 300), "5m");
        assert_eq!(resets_in(10_000, 10_000 - 7_800), "2h 10m");
        assert_eq!(
            resets_in(400_000, 400_000 - 3 * 86_400 - 4 * 3_600),
            "3d 4h"
        );
        assert_eq!(resets_in(0, 50), "0m");
    }

    #[test]
    fn the_tooltip_names_the_account_and_the_resets() {
        let tip = usage_tooltip(
            &usage(Some(42.0), None),
            Some("sam@work.example"),
            10_000 - 600,
        );
        assert!(tip.starts_with("Claude: sam@work.example"), "{tip}");
        assert!(
            tip.contains("5-hour limit: 42% used, resets in 10m"),
            "{tip}"
        );
        assert!(!tip.contains("7-day"), "{tip}");
    }
}
