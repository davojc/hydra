//! The environment's name in the status bar opens its accounts: what each provider is signed
//! in as, checked again on demand; and adding one, by signing in or borrowing another
//! environment's login.
use std::cell::Cell;

use gpui::prelude::*;
use gpui::{Anchor, App, Context, Hsla, SharedString, Window, div, px, rgb};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use hydra_core::envedit::ToolValues;
use hydra_core::provider::Status;

use super::Workspace;
use crate::accounts::{AddChoice, FormResult, form_fields, form_values, rows};

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
                        // Opening the popover checks again now (as Refresh does).
                        if let Some(env) = ws.focused().map(|v| v.read(cx).env().to_string()) {
                            ws.recheck_accounts(&env, cx);
                        }
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
        let (tool, values) = match &choice {
            AddChoice::SignIn(tool) => (tool.as_str(), ToolValues::default()),
            AddChoice::Borrow { tool, from } => (
                tool.as_str(),
                ToolValues {
                    from: Some(from.clone()),
                    ..Default::default()
                },
            ),
            AddChoice::SetUp(tool) => {
                self.set_up_form(env, tool.clone(), window, cx);
                return;
            }
        };
        if let Err(e) = self.hydra.add_tool(&env, tool, &values) {
            window.push_notification(Notification::error(e).title("Add account"), cx);
            return;
        }
        match choice {
            AddChoice::SignIn(tool) => self.added(&env, &tool, window, cx),
            AddChoice::Borrow { tool, from } => {
                window.push_notification(
                    Notification::success(format!(
                        "{tool} in {env} now uses {from}'s login. {}",
                        reopen(&env)
                    ))
                    .title("Add account"),
                    cx,
                );
                self.recheck_accounts(&env, cx);
            }
            AddChoice::SetUp(_) => {}
        }
    }

    /// Picks the environment whose login `tool` uses from now on (spec §7.1).
    fn pick_lender(
        &mut self,
        env: String,
        tool: String,
        lenders: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.accounts_open = false;
        cx.notify();
        let swatches: Vec<(String, Hsla)> = lenders
            .iter()
            .map(|e| (e.clone(), rgb(self.hydra.tint(e)).into()))
            .collect();
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (list, workspace, env, tool) = (
                lenders.clone(),
                workspace.clone(),
                env.clone(),
                tool.clone(),
            );
            dialog.title(format!("Use {tool} in {env} from…")).child(
                Command::new(&state)
                    .items(swatches.iter().map(|(e, c)| super::env_item(e, Some(*c))))
                    .placeholder("Environment")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(from) = list.get(ix.row).cloned() {
                            window.close_dialog(cx);
                            workspace.update(cx, |ws, cx| {
                                ws.add_account(
                                    env.clone(),
                                    AddChoice::Borrow {
                                        tool: tool.clone(),
                                        from,
                                    },
                                    window,
                                    cx,
                                )
                            });
                        }
                    }),
            )
        });
    }

    /// After a tool was turned on: its sign-in in a new terminal of `env` when it has one,
    /// and a note.
    fn added(&mut self, env: &str, tool: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.hydra.sign_ins(env).iter().any(|t| t == tool) {
            self.sign_in(env, tool, window, cx);
        }
        window.push_notification(
            Notification::info(format!("{tool} is on in {env}. {}", reopen(env)))
                .title("Add account"),
            cx,
        );
        self.recheck_accounts(env, cx);
    }

    /// The form for a tool that needs values first (git, aws, gemini, kube). It stays open,
    /// showing what's wrong, until the values check out.
    fn set_up_form(
        &mut self,
        env: String,
        tool: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fields = form_fields(&tool);
        let inputs: Vec<_> = fields
            .iter()
            .map(|f| {
                let (placeholder, masked) = (f.placeholder, f.masked);
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .masked(masked)
                })
            })
            .collect();
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false)
                && let Some(i) = inputs.first()
            {
                i.update(cx, |s, cx| s.focus(window, cx));
            }
            let danger = cx.theme().danger;
            let mut body = v_flex().gap_2();
            for (f, input) in fields.iter().zip(&inputs) {
                body = body
                    .child(div().text_sm().child(f.label))
                    .child(Input::new(input));
            }
            if let Some(p) = problem.read(cx).clone() {
                body = body.child(div().text_sm().text_color(danger).child(p));
            }
            let (ws, env, tool, inputs, problem, keys) = (
                workspace.clone(),
                env.clone(),
                tool.clone(),
                inputs.clone(),
                problem.clone(),
                fields.iter().map(|f| f.key).collect::<Vec<_>>(),
            );
            dialog
                .title(format!("Set up {tool} in {env}"))
                .child(body)
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("add").label("Add").primary().on_click(
                            move |_, window, cx| {
                                let typed: Vec<(&str, String)> = keys
                                    .iter()
                                    .zip(&inputs)
                                    .map(|(k, i)| (*k, i.read(cx).value().to_string()))
                                    .collect();
                                let result =
                                    ws.update(cx, |ws, _| match form_values(&tool, &typed)? {
                                        FormResult::Values(v) => ws.hydra.add_tool(&env, &tool, &v),
                                        FormResult::Gemini(key) => ws.hydra.add_gemini(&env, &key),
                                    });
                                match result {
                                    Ok(()) => {
                                        window.close_dialog(cx);
                                        ws.update(cx, |ws, cx| ws.added(&env, &tool, window, cx));
                                    }
                                    Err(e) => problem.update(cx, |p, cx| {
                                        *p = Some(e);
                                        cx.notify();
                                    }),
                                }
                            },
                        )),
                )
        });
    }
}

fn reopen(env: &str) -> String {
    format!("Terminals already open in {env} see it once reopened.")
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
    let borrowing = w.borrowing.get(env).cloned().unwrap_or_default();
    // Tools with a sign-in: signing in again switches the environment to another account.
    let sign_ins = w.hydra.sign_ins(env);
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
                        .child(r.provider.clone()),
                )
                .child(div().flex_1().min_w_0().child(r.detail))
                .when(sign_ins.contains(&r.provider), |row| {
                    let (ws, env, tool) = (ws.clone(), env.to_string(), r.provider.clone());
                    row.child(
                        Button::new(SharedString::from(format!("switch-{tool}")))
                            .label("Switch account…")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, window, cx| {
                                ws.update(cx, |ws, cx| {
                                    ws.accounts_open = false;
                                    ws.sign_in(&env, &tool, window, cx)
                                });
                            }),
                    )
                })
                .when_some(borrowing.from.get(&r.provider).cloned(), |row, owner| {
                    row.child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(format!("from {owner}")),
                    )
                })
                .when_some(
                    borrowing.lenders.get(&r.provider).cloned(),
                    |row, lenders| {
                        let (ws, env, tool) = (ws.clone(), env.to_string(), r.provider.clone());
                        row.child(
                            Button::new(SharedString::from(format!("use-from-{tool}")))
                                .label("Use from…")
                                .ghost()
                                .xsmall()
                                .on_click(move |_, window, cx| {
                                    ws.update(cx, |ws, cx| {
                                        ws.pick_lender(
                                            env.clone(),
                                            tool.clone(),
                                            lenders.clone(),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
                    },
                )
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
