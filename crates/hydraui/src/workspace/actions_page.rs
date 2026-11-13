//! **Actions** (core spec §80): drafts stages proposed for a person to approve, one row each.
//! A row opens to what it answers, the draft in an editable box, and **Approve**, **Reject** and
//! **Edit**. Approving an email's reply saves it as a Gmail draft in its thread; never sends.

use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{Disableable, Sizable, h_flex, v_flex};

use super::Workspace;
use super::orchestrator::tell;
use crate::actions::{self, Action, State};
use crate::gmail::{self, Endpoints};
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};

/// The Actions page, while shown.
#[derive(Default)]
pub(super) struct ActionsPage {
    /// The action opened, by id.
    open: Option<String>,
    /// Each opened action's draft box, by id, with the revision it was made for: a revised
    /// draft gets a fresh box.
    drafts: HashMap<String, (u32, Entity<TextareaState>)>,
    /// Approvals saving their Gmail draft.
    busy: HashSet<String>,
    /// What the last button did, under the buttons: (went wrong, text).
    said: HashMap<String, (bool, String)>,
}

impl Workspace {
    /// The Actions button, left of Projects, with how many wait for you.
    pub(super) fn actions_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.actions.is_some();
        let waiting = actions::waiting(&actions::list(
            self.hydra.paths.root(),
            &crate::action_log::now(),
        ));
        let tone = if open {
            theme::TEAL
        } else if waiting > 0 {
            theme::AMBER
        } else {
            theme::TEXT
        };
        h_flex()
            .id("open-actions")
            .flex_none()
            .gap_1p5()
            .items_center()
            .h(px(26.))
            .px_3()
            .mr_1()
            .rounded_md()
            .border_1()
            .text_sm()
            .cursor_pointer()
            .border_color(color(if open || waiting > 0 {
                tone
            } else {
                theme::BORDER_STRONG
            }))
            .bg(color(if open {
                theme::TEAL_TINT
            } else if waiting > 0 {
                theme::AMBER_TINT
            } else {
                theme::RAISED
            }))
            .text_color(color(tone))
            .hover(|s| s.border_color(color(theme::TEAL)))
            .child("Actions")
            .when(waiting > 0, |d| {
                d.child(
                    div()
                        .px(px(6.))
                        .rounded_full()
                        .text_xs()
                        .bg(color(theme::AMBER))
                        .text_color(color(theme::PANEL))
                        .child(waiting.to_string()),
                )
            })
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("Drafts waiting for you to approve")
                    .build(window, cx)
            })
            .on_click(cx.listener(|ws, _, window, cx| {
                if ws.actions.is_some() {
                    ws.close_actions(window, cx)
                } else {
                    ws.open_actions(window, cx)
                }
            }))
            .into_any_element()
    }

    pub(super) fn open_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.projects = None;
        self.history = None;
        self.tickets = None;
        // The oldest waiting one opens, so there's always something to act on.
        let open = actions::list(self.hydra.paths.root(), &crate::action_log::now())
            .into_iter()
            .rev()
            .find(|a| a.state == State::Waiting)
            .map(|a| a.id);
        self.actions = Some(ActionsPage {
            open,
            ..Default::default()
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.actions = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn toggle_action(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.actions {
            page.open = if page.open.as_deref() == Some(id.as_str()) {
                None
            } else {
                Some(id)
            };
        }
        cx.notify();
    }

    pub(super) fn render_actions_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let all = actions::list(self.hydra.paths.root(), &crate::action_log::now());
        // The opened action's draft box, made once per revision.
        let open = self.actions.as_ref().and_then(|p| p.open.clone());
        if let Some(a) = open
            .as_ref()
            .and_then(|id| all.iter().find(|a| &a.id == id))
        {
            let stale = self
                .actions
                .as_ref()
                .and_then(|p| p.drafts.get(&a.id))
                .is_none_or(|(rev, _)| *rev != a.revision);
            if stale {
                let text = a.proposal.clone();
                let editor = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(6, 24)
                        .default_value(text)
                });
                if let Some(page) = &mut self.actions {
                    page.drafts.insert(a.id.clone(), (a.revision, editor));
                }
            }
        }
        let waiting = actions::waiting(&all);
        let summary = match (all.len(), waiting) {
            (0, _) => "Nothing yet. A pipeline's stage puts a draft here when its prompt says \
                       to, for you to approve before it goes anywhere."
                .to_string(),
            (_, 0) => "Nothing waits for you.".to_string(),
            (_, 1) => "1 draft waits for you.".to_string(),
            (_, n) => format!("{n} drafts wait for you."),
        };
        let header = v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_xl()
                    .text_color(color(theme::TEXT))
                    .child("Actions"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(summary),
            );
        let rows: Vec<AnyElement> = all
            .iter()
            .map(|a| self.action_row(a, open.as_deref() == Some(a.id.as_str()), cx))
            .collect();
        div()
            .id("actions-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_5()
            .child(
                v_flex()
                    .max_w(px(980.))
                    .gap_3()
                    .child(header)
                    .child(v_flex().gap_2().children(rows)),
            )
            .into_any_element()
    }

    fn action_row(&self, a: &Action, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let root = self.hydra.paths.root();
        let pipeline = crate::pipeline::load(root, &a.pipeline).ok();
        let accent = pipeline
            .as_ref()
            .map_or(theme::TEAL, crate::pipeline_card::accent);
        let icon = pipeline.as_ref().and_then(|p| p.icon.clone());
        let waited = crate::run_history::ago(&a.created, &crate::action_log::now())
            .map_or(String::new(), |d| {
                format!(" · {}", crate::asking::waited(d.as_secs() / 60))
            });
        let (tone, tint) = match &a.state {
            State::Waiting => (theme::AMBER, theme::AMBER_TINT),
            State::Changing { .. } => (theme::TEXT_SECONDARY, theme::RAISED),
            State::Approved { .. } => (theme::TEAL, theme::TEAL_TINT),
            State::Rejected { .. } => (theme::TEXT_MUTED, theme::RAISED),
        };
        let revision = match a.revision {
            1 => String::new(),
            n => format!(" · draft {n}"),
        };
        let id = a.id.clone();
        let head = h_flex()
            .id(SharedString::from(format!("action-head-{}", a.id)))
            .gap_3()
            .items_center()
            .px_3()
            .py_2()
            .cursor_pointer()
            .hover(|d| d.bg(color(theme::RAISED)))
            .child(crate::pipeline_icon::mark(icon.as_deref(), accent, 18.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(color(theme::TEXT))
                            .child(a.title.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(color(theme::TEXT_MUTED))
                            .child(format!(
                                "{} · {} · {}{revision}{waited}",
                                a.pipeline_name, a.ticket, a.stage_title
                            )),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_xs()
                    .bg(color(tint))
                    .text_color(color(tone))
                    .child(a.state.word()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(color(theme::TEXT_MUTED))
                    .child(if open { "▾" } else { "▸" }),
            )
            .on_click(cx.listener(move |ws, _, _, cx| ws.toggle_action(id.clone(), cx)));
        let mut row = v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(color(if a.state == State::Waiting {
                accent
            } else {
                theme::BORDER_STRONG
            }))
            .overflow_hidden()
            .child(head);
        if open {
            row = row.child(self.action_body(a, cx));
        }
        row.into_any_element()
    }

    fn action_body(&self, a: &Action, cx: &mut Context<Self>) -> AnyElement {
        let page = self.actions.as_ref();
        let editor = page
            .and_then(|p| p.drafts.get(&a.id))
            .map(|(_, e)| e.clone());
        let busy = page.is_some_and(|p| p.busy.contains(&a.id));
        let said = page.and_then(|p| p.said.get(&a.id)).cloned();
        let label = |t: &str| {
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(color(theme::TEXT_SECONDARY))
                .child(t.to_string())
        };
        let answers = match &a.email {
            Some(e) => format!("The email: {} · from {}", e.subject, e.from),
            None => "What it answers".to_string(),
        };
        let context = div()
            .id(SharedString::from(format!("action-context-{}", a.id)))
            .max_h(px(260.))
            .overflow_y_scroll()
            .p_3()
            .rounded(px(6.))
            .bg(color(theme::RAISED))
            .text_sm()
            .text_color(color(theme::TEXT_SECONDARY))
            .child(if a.context.is_empty() {
                "(The stage gave no context.)".to_string()
            } else {
                a.context.clone()
            });
        let draft_label = if a.email.is_some() {
            "Proposed reply (edit it here before approving)"
        } else {
            "Proposed draft (edit it here before approving)"
        };
        let mut body = v_flex()
            .gap_2()
            .px_3()
            .pb_3()
            .pt_1()
            .child(label(&answers))
            .child(context)
            .child(label(draft_label));
        match (&a.state, editor) {
            (State::Waiting, Some(editor)) => {
                body = body.child(Textarea::new(&editor));
            }
            _ => {
                body = body.child(
                    div()
                        .p_3()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(color(theme::BORDER_STRONG))
                        .text_sm()
                        .text_color(color(theme::TEXT))
                        .child(a.proposal.clone()),
                );
            }
        }
        match &a.state {
            State::Waiting => {
                let (i1, i2, i3) = (a.id.clone(), a.id.clone(), a.id.clone());
                let approve_tip = if a.email.is_some() {
                    "Save this reply as a Gmail draft in the email's thread. Nothing is sent: you \
                     send it from Gmail."
                } else {
                    "Approve this draft as it stands in the box"
                };
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!("action-approve-{}", a.id)))
                                .label(if busy {
                                    "Saving the draft…"
                                } else {
                                    "Approve"
                                })
                                .primary()
                                .small()
                                .disabled(busy)
                                .tooltip(approve_tip)
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.approve_action(i1.clone(), window, cx)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("action-edit-{}", a.id)))
                                .label("Edit…")
                                .outline()
                                .small()
                                .disabled(busy)
                                .tooltip("Tell the stage what to change; it sends a new draft")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.edit_action(i2.clone(), window, cx)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("action-reject-{}", a.id)))
                                .label("Reject…")
                                .ghost()
                                .small()
                                .disabled(busy)
                                .tooltip("Nothing is saved; the stage reports it was rejected")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.reject_action(i3.clone(), window, cx)
                                })),
                        ),
                );
            }
            State::Changing { ask } => {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(format!(
                            "You asked: {ask}. The stage is revising it; the new draft shows \
                             here."
                        )),
                );
            }
            State::Rejected { why } if !why.is_empty() => {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(format!("Rejected: {why}")),
                );
            }
            _ => {}
        }
        if let Some((bad, text)) = said {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(color(if bad { theme::RED } else { theme::TEAL }))
                    .child(text),
            );
        }
        body.into_any_element()
    }

    /// The draft as it stands in its box, else as proposed.
    fn action_text(&self, a: &Action, cx: &gpui::App) -> String {
        self.actions
            .as_ref()
            .and_then(|p| p.drafts.get(&a.id))
            .filter(|(rev, _)| *rev == a.revision)
            .map_or_else(
                || a.proposal.clone(),
                |(_, e)| e.read(cx).value().trim().to_string(),
            )
    }

    fn action_said(&mut self, id: &str, bad: bool, text: String, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.actions {
            page.said.insert(id.to_string(), (bad, text));
        }
        cx.notify();
    }

    fn approve_action(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.hydra.paths.root().to_path_buf();
        let Some(a) = actions::load(&root, &id).filter(|a| a.state == State::Waiting) else {
            return;
        };
        let text = self.action_text(&a, cx);
        if text.is_empty() {
            self.action_said(&id, true, "The draft is empty.".into(), cx);
            return;
        }
        let Some(email) = a.email.clone() else {
            let prompt = actions::approved_prompt(&text, false);
            self.settle_action(a, State::Approved { draft: None }, text, prompt, window, cx);
            return;
        };
        let env = crate::pipeline::load(&root, &a.pipeline)
            .ok()
            .and_then(|p| {
                let m = p.mail.clone()?;
                self.mail_env(&p, &m, cx)
            })
            .or_else(|| self.ui.default_env.clone());
        let Some(env) = env else {
            self.action_said(
                &id,
                true,
                "There's no environment with Gmail connected to save the draft in.".into(),
                cx,
            );
            return;
        };
        let Some(client) = self.gmail_client("Approve", window, cx) else {
            return;
        };
        if let Some(page) = &mut self.actions {
            page.busy.insert(id.clone());
            page.said.remove(&id);
        }
        cx.notify();
        let hydra = self.hydra.clone();
        let body = text.clone();
        cx.spawn_in(window, async move |ws, cx| {
            let saved = cx
                .background_spawn(async move {
                    let ep = Endpoints::google();
                    let access = gmail::refresh(&ep, &client, &hydra.gmail_sign_in(&env)?)?;
                    let messages = gmail::thread_messages(&ep, &access, &email.thread)?;
                    let last = messages.last().cloned().unwrap_or_default();
                    let to = [&last.reply_to, &last.from, &email.from]
                        .into_iter()
                        .find(|t| !t.trim().is_empty())
                        .cloned()
                        .unwrap_or_default();
                    let subject = if last.subject.trim().is_empty() {
                        email.subject.clone()
                    } else {
                        last.subject.clone()
                    };
                    let mime = actions::reply_mime(&to, &subject, &last.message_id, &body);
                    gmail::create_draft(&ep, &access, &email.thread, &mime)
                })
                .await;
            let _ = ws.update_in(cx, |ws, window, cx| {
                if let Some(page) = &mut ws.actions {
                    page.busy.remove(&a.id);
                }
                match saved {
                    Ok(draft) => {
                        crate::action_log::write(format!(
                            "action {}: approved, saved as Gmail draft {draft}",
                            a.id
                        ));
                        let prompt = actions::approved_prompt(&text, true);
                        let id = a.id.clone();
                        ws.settle_action(
                            a,
                            State::Approved { draft: Some(draft) },
                            text,
                            prompt,
                            window,
                            cx,
                        );
                        ws.action_said(
                            &id,
                            false,
                            "Saved as a draft in the email's Gmail thread. Send it from Gmail."
                                .into(),
                            cx,
                        );
                    }
                    Err(e) => {
                        crate::action_log::write(format!("action {}: draft failed: {e}", a.id));
                        ws.action_said(
                            &a.id,
                            true,
                            format!(
                                "Couldn't save the Gmail draft: {e}. Nothing was sent; try \
                                 Approve again."
                            ),
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    fn reject_action(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.text_dialog_labelled(
            "Reject this draft".into(),
            Some("Nothing is saved or sent. The stage is told it was rejected, and why."),
            String::new(),
            "Why (optional), e.g. it's for sales, not us",
            "Reject",
            move |ws, why, window, cx| {
                let root = ws.hydra.paths.root().to_path_buf();
                let a = actions::load(&root, &id)
                    .filter(|a| a.state == State::Waiting)
                    .ok_or("It's no longer waiting.")?;
                let text = ws.action_text(&a, cx);
                let prompt = actions::rejected_prompt(&why);
                ws.settle_action(a, State::Rejected { why }, text, prompt, window, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    fn edit_action(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.text_dialog_labelled(
            "Ask for a change".into(),
            Some(
                "Say what to change. The stage revises the draft (with any edits you made in the \
                 box) and sends a new one here.",
            ),
            String::new(),
            "e.g. shorter, and point them to the Share button on the record",
            "Ask for the change",
            move |ws, ask, window, cx| {
                if ask.is_empty() {
                    return Err("Say what to change.".into());
                }
                let root = ws.hydra.paths.root().to_path_buf();
                let a = actions::load(&root, &id)
                    .filter(|a| a.state == State::Waiting)
                    .ok_or("It's no longer waiting.")?;
                let text = ws.action_text(&a, cx);
                let prompt = actions::edit_prompt(&ask, &text);
                ws.settle_action(a, State::Changing { ask }, text, prompt, window, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    /// Records what a person decided on `a`, and tells its waiting stage `prompt`.
    fn settle_action(
        &mut self,
        mut a: Action,
        state: State,
        text: String,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let what = match &state {
            State::Approved { .. } => "approved it in Actions".to_string(),
            State::Rejected { why } => format!("rejected it in Actions ({why})"),
            State::Changing { ask } => format!("asked for a change in Actions: {ask}"),
            State::Waiting => String::new(),
        };
        let told = server::get().map(|sv| {
            let mut reg = server::lock(sv.registry());
            let pane = reg.runs.run(&a.run).and_then(|r| self.asking_pane(r, cx));
            (reg.runs.answered(&a.run, &format!("The user {what}")), pane)
        });
        match told {
            Some((Ok(changes), pane)) => {
                if let Some(pane) = pane {
                    self.queue_wake(pane, a.run.clone(), prompt);
                    self.send_wakes(window, cx);
                }
                tell(changes);
            }
            Some((Err(e), _)) => {
                crate::action_log::write(format!("action {}: its stage wasn't waiting: {e}", a.id))
            }
            None => {}
        }
        crate::action_log::write(format!("action {}: the user {what}", a.id));
        a.state = state;
        a.proposal = text;
        a.updated = crate::action_log::now();
        if let Err(e) = actions::save(self.hydra.paths.root(), &a) {
            self.action_said(&a.id, true, format!("Couldn't keep it: {e}"), cx);
        }
        cx.notify();
    }
}
