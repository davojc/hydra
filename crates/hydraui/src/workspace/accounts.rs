//! The environment's name in the status bar opens its accounts: what each provider is signed
//! in as, checked again on demand.
use gpui::prelude::*;
use gpui::{Anchor, App, Context, Hsla, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use hydra_core::provider::Status;

use super::Workspace;
use crate::accounts::rows;

impl Workspace {
    /// The status bar's environment name, opening the accounts popover.
    pub(super) fn render_env_accounts(
        &self,
        env: String,
        tint: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ws = cx.entity();
        let on_change = ws.clone();
        Popover::new("env-accounts")
            .anchor(Anchor::BottomLeft)
            .trigger(
                // The name as a child, not a label: the button's own text colour would
                // replace the environment's colour when it isn't open.
                Button::new("env-name")
                    .ghost()
                    .xsmall()
                    .child(div().text_color(tint).child(env.clone())),
            )
            .on_open_change(move |open, window, cx| {
                on_change.update(cx, |ws, cx| {
                    if *open {
                        ws.refresh_status(cx);
                    } else {
                        ws.focus_active(window, cx);
                    }
                });
            })
            .content(move |_, _, cx| accounts_list(&ws, &env, cx))
    }

    /// Checks the focused environment's accounts now, whatever the cache says.
    fn recheck_accounts(&mut self, env: &str, cx: &mut Context<Self>) {
        self.status.expire(env);
        self.refresh_status(cx);
        cx.notify();
    }
}

fn accounts_list(
    ws: &gpui::Entity<Workspace>,
    env: &str,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (ok, warn, bad, muted) = (
        theme.success,
        theme.warning,
        theme.danger,
        theme.muted_foreground,
    );
    let w = ws.read(cx);
    let found = w.status.get(env).map(rows);
    let checking = w.refreshing.contains(env);
    let recheck = (ws.clone(), env.to_string());
    let header = h_flex()
        .gap_3()
        .justify_between()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("Accounts in {env}")),
        )
        .child(
            Button::new("recheck-accounts")
                .label(if checking { "Checking…" } else { "Refresh" })
                .ghost()
                .xsmall()
                .disabled(checking)
                .on_click(move |_, _, cx| {
                    let (ws, env) = &recheck;
                    ws.update(cx, |ws, cx| ws.recheck_accounts(env, cx));
                }),
        );
    let body = match found {
        None => v_flex().child(div().text_color(muted).child("checking…")),
        Some(rows) if rows.is_empty() => v_flex().child(
            div()
                .text_color(muted)
                .child("No accounts are set up for this environment."),
        ),
        Some(rows) => v_flex().gap_1().children(rows.into_iter().map(|r| {
            let colour = match r.status {
                Status::Ok => ok,
                Status::Mismatch => warn,
                Status::Missing => bad,
                Status::Info => muted,
            };
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .text_color(colour)
                        .child(r.mark),
                )
                .child(
                    div()
                        .w(px(64.))
                        .flex_none()
                        .text_color(muted)
                        .child(r.provider),
                )
                .child(div().flex_1().min_w_0().child(r.detail))
        })),
    };
    v_flex()
        .w(px(520.))
        .gap_2()
        .text_sm()
        .child(header)
        .child(body)
}
