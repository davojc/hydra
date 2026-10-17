//! The environment's name in the status bar opens its accounts: what each provider is signed
//! in as, checked again on demand; and adding one, by signing in or borrowing another
//! environment's login.
use std::cell::Cell;

use gpui::prelude::*;
use gpui::{Anchor, App, Context, Hsla, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::notification::Notification;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use hydra_core::provider::Status;

use super::Workspace;
use crate::accounts::{AddChoice, rows};

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
            .open(self.accounts_open)
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
                    ws.accounts_open = *open;
                    cx.notify();
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

    /// Picks an account to add to `env`: a sign-in, or another environment's login.
    fn pick_account(&mut self, env: String, window: &mut Window, cx: &mut Context<Self>) {
        self.accounts_open = false;
        cx.notify();
        let choices = self.hydra.account_choices(&env);
        if choices.is_empty() {
            window.push_notification(
                Notification::info(format!(
                    "{env} has every account hydraui can add; others: hydra add <tool> {env}"
                ))
                .title("Add account"),
                cx,
            );
            return;
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (list, workspace, env) = (choices.clone(), workspace.clone(), env.clone());
            dialog.title(format!("Add an account to {env}")).child(
                Command::new(&state)
                    .items(choices.iter().map(|c| CommandItem::new().label(c.label())))
                    .placeholder("Account")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(choice) = list.get(ix.row).cloned() {
                            window.close_dialog(cx);
                            workspace.update(cx, |ws, cx| {
                                ws.add_account(env.clone(), choice, window, cx)
                            });
                        }
                    }),
            )
        });
    }

    /// Turns the account on in env.toml, then signs in to it in a new tab, or says whose
    /// login it now uses.
    fn add_account(
        &mut self,
        env: String,
        choice: AddChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (tool, from) = match &choice {
            AddChoice::SignIn(tool) => (tool.as_str(), None),
            AddChoice::Borrow { tool, from } => (tool.as_str(), Some(from.as_str())),
        };
        if let Err(e) = self.hydra.add_tool(&env, tool, from) {
            window.push_notification(Notification::error(e).title("Add account"), cx);
            return;
        }
        let reopen = format!("Terminals already open in {env} see it once reopened.");
        match choice {
            AddChoice::SignIn(tool) => {
                let cwd = self.focused_cwd(cx);
                let shell = self.hydra.shell(self.ui.default_shell.as_deref());
                match self.hydra.sign_in_request(&env, &tool, cwd, shell) {
                    Ok(request) => self.open_request(request, window, cx),
                    Err(e) => window.push_notification(Notification::error(e).title("Sign in"), cx),
                }
                window.push_notification(
                    Notification::info(format!("{tool} is on in {env}. {reopen}"))
                        .title("Add account"),
                    cx,
                );
            }
            AddChoice::Borrow { tool, from } => window.push_notification(
                Notification::success(format!("{tool} in {env} now uses {from}'s login. {reopen}"))
                    .title("Add account"),
                cx,
            ),
        }
        self.recheck_accounts(&env, cx);
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
    let add = (ws.clone(), env.to_string());
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
    let footer = h_flex().justify_end().child(
        Button::new("add-account")
            .label("Add account…")
            .ghost()
            .xsmall()
            .on_click(move |_, window, cx| {
                let (ws, env) = &add;
                ws.update(cx, |ws, cx| ws.pick_account(env.clone(), window, cx));
            }),
    );
    v_flex()
        .w(px(520.))
        .gap_2()
        .text_sm()
        .child(header)
        .child(body)
        .child(footer)
}
