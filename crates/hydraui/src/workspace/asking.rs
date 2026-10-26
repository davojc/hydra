//! A stage's question for product in the app: the card's **Asked product** block with the
//! question, its Slack thread and **Answer…**; a notification when a stage asks; and every 5
//! minutes, the waiting session is told to look in its thread (`crate::asking`).

use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, Window, div, px};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{Sizable, h_flex, v_flex};

use super::Workspace;
use super::orchestrator::tell;
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::runs::{Ask, Change, Decided, Run};

impl Workspace {
    /// The pane of the session that asked `run`'s question.
    fn asking_pane(&self, run: &Run, cx: &App) -> Option<PaneId> {
        let ask = run.asking.as_ref()?;
        let token = run.stages[ask.stage].latest()?.session.clone()?;
        self.panes
            .iter()
            .find(|(_, v)| v.read(cx).mcp_token() == Some(token.as_str()))
            .map(|(id, _)| *id)
    }

    /// A stage asked: say so, with the question.
    pub(super) fn asked_notify(&mut self, changes: &[Change], window: &mut Window, cx: &mut App) {
        let Some(sv) = server::get() else {
            return;
        };
        let notes: Vec<(String, String)> =
            {
                let reg = server::lock(sv.registry());
                changes
                    .iter()
                    .filter_map(|c| {
                        let Decided::Asked(question) = &c.what else {
                            return None;
                        };
                        let r = reg.runs.run(&c.run)?;
                        let at =
                            r.pipeline.product_channel.as_deref().map_or(
                                " Answer it with Answer… on the card.".to_string(),
                                |ch| format!(" Asked in {ch}; or answer with Answer… on the card."),
                            );
                        Some((
                            r.pipeline.name.clone(),
                            format!("{} is waiting on product: {question}{at}", r.ticket),
                        ))
                    })
                    .collect()
            };
        for (title, text) in notes {
            crate::action_log::write(format!("asked: {text}"));
            window.push_notification(Notification::warning(text).title(title), cx);
        }
    }

    /// Each second: a session waiting on its Slack thread whose 5 minutes are up is told to
    /// look again.
    pub(super) fn asking_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sv) = server::get() else {
            return;
        };
        let now = SystemTime::now();
        let due: Vec<(PaneId, String, String)> = {
            let reg = server::lock(sv.registry());
            reg.runs
                .all()
                .filter_map(|r| {
                    let Ask {
                        question,
                        link,
                        since,
                        ..
                    } = r.asking.as_ref()?;
                    let link = link.as_deref()?;
                    let last = self.ask_checks.get(&r.id).copied();
                    if !crate::asking::check_due(*since, last, now) {
                        return None;
                    }
                    let pane = self.asking_pane(r, cx)?;
                    Some((
                        pane,
                        r.id.clone(),
                        crate::asking::check_prompt(question, link),
                    ))
                })
                .collect()
        };
        if due.is_empty() {
            return;
        }
        for (pane, run, prompt) in due {
            self.ask_checks.insert(run.clone(), now);
            crate::action_log::write(format!("run {run}: the waiting stage looks for its answer"));
            self.pending_wakes.insert(pane, (run, prompt));
        }
        self.send_wakes(window, cx);
    }

    /// **Check now**: the waiting session looks in its thread at once, then every 5 minutes
    /// from there.
    fn check_answer_now(&mut self, run: &str, window: &mut Window, cx: &mut Context<Self>) {
        let due = SystemTime::now()
            .checked_sub(crate::asking::CHECK_EVERY)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.ask_checks.insert(run.to_string(), due);
        self.asking_tick(window, cx);
    }

    /// **Answer…**: the user's answer to `run`'s question, recorded and typed to the stage.
    fn answer_dialog(&mut self, run: String, window: &mut Window, cx: &mut Context<Self>) {
        let question = server::get()
            .and_then(|sv| {
                let reg = server::lock(sv.registry());
                Some(reg.runs.run(&run)?.asking.as_ref()?.question.clone())
            })
            .unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("e.g. Keep translation on; product (Ana)")
        });
        let ws = cx.entity();
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let submit = {
                let (ws, input, run) = (ws.clone(), input.clone(), run.clone());
                move |window: &mut Window, cx: &mut App| {
                    let answer = input.read(cx).value().trim().to_string();
                    if answer.is_empty() {
                        return;
                    }
                    window.close_dialog(cx);
                    ws.update(cx, |ws, cx| ws.answer(&run, &answer, window, cx));
                }
            };
            let on_ok = submit.clone();
            dialog
                .title("Answer the product question")
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(question.clone()),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("answer-cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("answer-go")
                                .label("Answer")
                                .primary()
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                )
        });
    }

    fn answer(&mut self, run: &str, answer: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sv) = server::get() else {
            return;
        };
        let (done, pane) = {
            let mut reg = server::lock(sv.registry());
            let pane = reg.runs.run(run).and_then(|r| self.asking_pane(r, cx));
            (reg.runs.answered(run, answer), pane)
        };
        match done {
            Ok(changes) => {
                crate::action_log::write(format!("run {run}: the user answered: {answer}"));
                self.ask_checks.remove(run);
                if let Some(pane) = pane {
                    self.pending_wakes.insert(
                        pane,
                        (run.to_string(), crate::asking::answer_prompt(answer)),
                    );
                }
                tell(changes);
            }
            Err(e) => window.push_notification(Notification::error(e).title("Answer"), cx),
        }
    }

    /// Under an orchestrator's card while its ticket waits on product: the question, where it
    /// was asked, its thread and **Answer…**.
    pub(super) fn render_question(
        &self,
        orch: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let token = self.panes.get(&orch)?.read(cx).mcp_token()?.to_string();
        let sv = server::get()?;
        let (run, ask, channel) = {
            let reg = server::lock(sv.registry());
            let r = reg.runs.current(&token)?;
            let ask = r.asking.clone()?;
            (r.id.clone(), ask, r.pipeline.product_channel.clone())
        };
        let now = SystemTime::now();
        let waited = now
            .duration_since(ask.since)
            .map_or(0, |d| d.as_secs() / 60);
        let asked = match channel {
            Some(ch) => format!("Asked in {ch} {waited} min ago"),
            None => format!("Asked you {waited} min ago"),
        };
        // Only a question with a Slack thread is looked for; one asked of the user waits for
        // Answer….
        let meta = if ask.link.is_some() {
            match crate::asking::next_check_in(ask.since, self.ask_checks.get(&run).copied(), now) {
                0 => format!("{asked} · checking Slack for a reply"),
                m => format!("{asked} · next check for a reply in {m} min"),
            }
        } else {
            asked
        };
        let has_thread = ask.link.is_some();
        Some(
            v_flex()
                .id(SharedString::from(format!("question-{orch}")))
                .ml(px(26.))
                .mr_1()
                .my_1()
                .p_2()
                .gap_1()
                .rounded(px(6.))
                .bg(color(theme::AMBER_TINT))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(color(theme::AMBER))
                        .child("⏸ Paused: waiting for product's answer"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT))
                        .line_clamp(4)
                        .child(ask.question.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(meta),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .children(ask.link.clone().map(|url| {
                            div()
                                .id(SharedString::from(format!("question-link-{orch}")))
                                .text_xs()
                                .text_color(color(theme::TEAL))
                                .cursor_pointer()
                                .hover(|d| d.underline())
                                .child("Open thread ↗")
                                .on_click(move |_, _, cx| cx.open_url(&url))
                        }))
                        .when(has_thread, |d| {
                            let run = run.clone();
                            d.child(
                                Button::new(SharedString::from(format!("question-check-{orch}")))
                                    .label("Check now")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.check_answer_now(&run, window, cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("question-answer-{orch}")))
                                .label("Answer…")
                                .outline()
                                .xsmall()
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.answer_dialog(run.clone(), window, cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}
