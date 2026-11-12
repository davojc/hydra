//! A stage's question for product in the app: the card's **Asked product** block with the
//! question, its Slack thread and **Answer…**; a notification when a stage asks; and every 5
//! minutes, the waiting session is told to look in its thread (`crate::asking`).

use crate::inbox::{Note, PushNote};
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, Window, div, px};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{h_flex, v_flex};

use super::Workspace;
use super::orchestrator::tell;
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::runs::{Ask, Change, Decided, Run, RunState};

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
                        // Waiting on a pull request's author isn't the user's to act on.
                        if r.asking.as_ref()?.whom == crate::runs::Whom::Author {
                            return None;
                        }
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
            window.push_note(Note::warning(text).title(title), cx);
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
                        whom,
                        ..
                    } = r.asking.as_ref()?;
                    let link = link.as_deref()?;
                    let last = self.ask_checks.get(&r.id).copied();
                    // The pipeline's [waiting] timetable (core spec §60).
                    let w = r.pipeline.waiting;
                    if crate::waiting::look(&w, *since, last, now) != crate::waiting::Look::Now {
                        return None;
                    }
                    let pane = self.asking_pane(r, cx)?;
                    Some((
                        pane,
                        r.id.clone(),
                        crate::asking::check_prompt_for(*whom, question, link),
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
            self.queue_wake(pane, run, prompt);
        }
        self.send_wakes(window, cx);
    }

    /// Each second: a run waiting on you whose pipeline looks while it waits (`[waiting]
    /// needs_you`) has its stopped stage's session told to look again on the timetable; once
    /// its hours are up it stops, and with `then = "close"` its session closes (core spec §60).
    pub(super) fn waiting_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sv) = server::get() else {
            return;
        };
        let now = SystemTime::now();
        // (run, stage, session token, what it waits for, settings)
        let waiting: Vec<(
            String,
            usize,
            String,
            String,
            crate::pipeline::WaitingSettings,
        )> = {
            let reg = server::lock(sv.registry());
            reg.runs
                .all()
                .filter_map(|r| {
                    let RunState::NeedsYou(why) = &r.state else {
                        return None;
                    };
                    let si = r
                        .stages
                        .iter()
                        .position(|s| s.status == crate::runs::StageStatus::Stopped)?;
                    // A step that keeps checking looks on its own timetable (core spec §62).
                    let w = r.pipeline.stages[si]
                        .keep
                        .map_or(r.pipeline.waiting, |k| k.waiting());
                    if !w.needs_you || w.look_every == 0 || r.fix_after.is_some() {
                        return None;
                    }
                    let a = r.stages[si].latest()?;
                    let token = a.session.clone()?;
                    let what = a
                        .rating
                        .as_ref()
                        .map(|x| x.ask.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .unwrap_or_else(|| why.clone());
                    Some((r.id.clone(), si, token, what, w))
                })
                .collect()
        };
        let ids: std::collections::HashSet<String> =
            waiting.iter().map(|(id, ..)| id.clone()).collect();
        self.wait_looks.retain(|id, _| ids.contains(id));
        let mut woke = false;
        for (run, si, token, what, w) in waiting {
            let pane = self
                .panes
                .iter()
                .find(|(_, v)| v.read(cx).mcp_token() == Some(token.as_str()))
                .map(|(id, _)| *id);
            let entry = self
                .wait_looks
                .entry(run.clone())
                .or_insert((now, None, false));
            if entry.2 {
                continue;
            }
            match crate::waiting::look(&w, entry.0, entry.1, now) {
                crate::waiting::Look::NotYet => {}
                crate::waiting::Look::Now => {
                    entry.1 = Some(now);
                    let Some(pane) = pane else {
                        continue;
                    };
                    let ok = server::lock(sv.registry()).runs.look_again(&run, si);
                    if ok.is_ok() {
                        crate::action_log::write(format!(
                            "run {run}: waiting on you, it looks again"
                        ));
                        self.queue_wake(pane, run.clone(), crate::waiting::needs_you_prompt(&what));
                        woke = true;
                    }
                }
                crate::waiting::Look::Over => {
                    entry.2 = true;
                    crate::action_log::write(format!(
                        "run {run}: it looked for {} hour(s) and stopped looking",
                        w.look_for
                    ));
                    if w.then == crate::pipeline::WaitThen::Close
                        && let Some(pane) = pane
                    {
                        crate::action_log::write(format!("run {run}: its waiting session closes"));
                        self.close_terminal_now(pane, window, cx);
                    }
                }
            }
        }
        woke |= self.keep_tick(now, cx);
        if woke {
            self.send_wakes(window, cx);
        }
    }

    /// Steps below their bar that keep checking (core spec §62): each one's session is told to
    /// look again on its step's timetable; when its hours are up it stops and the run asks you.
    /// Whether it woke one.
    fn keep_tick(&mut self, now: SystemTime, cx: &mut Context<Self>) -> bool {
        let Some(sv) = server::get() else {
            return false;
        };
        // (run, stage, session token, timetable, latest score, bar)
        let checking: Vec<(String, String, crate::pipeline::KeepChecking, u8, u8)> = {
            let reg = server::lock(sv.registry());
            reg.runs
                .all()
                .filter_map(|r| {
                    let si = r.rechecking?;
                    let keep = r.pipeline.stages.get(si)?.keep?;
                    let a = r.stages.get(si)?.latest()?;
                    let score = a.rating.as_ref().map_or(0, |x| x.score);
                    Some((
                        r.id.clone(),
                        a.session.clone()?,
                        keep,
                        score,
                        r.pipeline.bar_for(si),
                    ))
                })
                .collect()
        };
        let ids: std::collections::HashSet<String> =
            checking.iter().map(|(id, ..)| id.clone()).collect();
        self.keep_looks.retain(|id, _| ids.contains(id));
        let mut woke = false;
        for (run, token, keep, score, bar) in checking {
            let entry = self.keep_looks.entry(run.clone()).or_insert((now, None));
            match crate::waiting::look(&keep.waiting(), entry.0, entry.1, now) {
                crate::waiting::Look::NotYet => {}
                crate::waiting::Look::Now => {
                    entry.1 = Some(now);
                    let pane = self
                        .panes
                        .iter()
                        .find(|(_, v)| v.read(cx).mcp_token() == Some(token.as_str()))
                        .map(|(id, _)| *id);
                    if let Some(pane) = pane {
                        crate::action_log::write(format!(
                            "run {run}: below its bar, it looks again"
                        ));
                        self.queue_wake(
                            pane,
                            run.clone(),
                            crate::waiting::keep_checking_prompt(score, bar),
                        );
                        woke = true;
                    }
                }
                crate::waiting::Look::Over => {
                    self.keep_looks.remove(&run);
                    let changes = server::lock(sv.registry()).runs.stop_checking(&run);
                    crate::action_log::write(format!(
                        "run {run}: it kept checking for {} hours: it asks you now",
                        keep.hours
                    ));
                    tell(changes);
                    cx.notify();
                }
            }
        }
        woke
    }

    /// **Check now**: the waiting session looks in its thread at once, then every 5 minutes
    /// from there.
    fn check_answer_now(&mut self, run: &str, window: &mut Window, cx: &mut Context<Self>) {
        // At once, whatever the timetable; the next look counts from now.
        let woken = server::get().and_then(|sv| {
            let reg = server::lock(sv.registry());
            let r = reg.runs.run(run)?;
            let ask = r.asking.as_ref()?;
            let link = ask.link.as_deref()?;
            let pane = self.asking_pane(r, cx)?;
            Some((
                pane,
                crate::asking::check_prompt_for(ask.whom, &ask.question, link),
            ))
        });
        if let Some((pane, prompt)) = woken {
            self.ask_checks.insert(run.to_string(), SystemTime::now());
            self.queue_wake(pane, run.to_string(), prompt);
            self.send_wakes(window, cx);
        }
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
                    self.queue_wake(pane, run.to_string(), crate::asking::answer_prompt(answer));
                }
                tell(changes);
            }
            Err(e) => window.push_note(Note::error(e).title("Answer"), cx),
        }
    }

    /// The question a ticket of `orch` asked (`run`, else the first asking), where and when,
    /// and the thread, for the dialog and the line.
    fn asked(
        &self,
        orch: PaneId,
        run: Option<&str>,
        cx: &App,
    ) -> Option<(String, Ask, Option<String>)> {
        let token = self.panes.get(&orch)?.read(cx).mcp_token()?.to_string();
        let sv = server::get()?;
        let reg = server::lock(sv.registry());
        let open = reg.runs.open_of(&token);
        let r = match run {
            Some(id) => open.iter().find(|r| r.id == id).copied()?,
            None => open
                .iter()
                .find(|r| r.asking.is_some())
                .copied()
                .or_else(|| reg.runs.current(&token))?,
        };
        Some((
            r.id.clone(),
            r.asking.clone()?,
            r.pipeline.product_channel.clone(),
        ))
    }

    /// Under an orchestrator while its ticket waits on product: one short line; clicking it
    /// shows the question, its thread, **Check now** and **Answer…**.
    /// `run`: that ticket's question, in its box in the sidebar; `None`: the orchestrator's.
    /// `accent`: the pipeline's colour. A wait on a pull request's author is nothing for the
    /// user to do, so it's calm in that colour, as the card's pill is; a question to product
    /// is amber.
    pub(super) fn render_question(
        &self,
        orch: PaneId,
        run: Option<&str>,
        accent: u32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (_, ask, _) = self.asked(orch, run, cx)?;
        let (tone, tint) = match ask.whom {
            crate::runs::Whom::Author => (color(accent), color(accent).opacity(0.16)),
            crate::runs::Whom::Product => (color(theme::AMBER), color(theme::AMBER_TINT)),
        };
        let run = run.map(str::to_string);
        let waited = SystemTime::now()
            .duration_since(ask.since)
            .map_or(0, |d| d.as_secs() / 60);
        Some(
            h_flex()
                .id(SharedString::from(format!(
                    "question-{orch}-{}",
                    run.as_deref().unwrap_or("")
                )))
                .mr_1()
                .my_1()
                .px_2()
                .py_1()
                .gap_2()
                .rounded(px(6.))
                .bg(tint)
                .cursor_pointer()
                .hover(|d| d.opacity(0.85))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(tone)
                        .child(match ask.whom {
                            crate::runs::Whom::Product => {
                                format!("⏸ Waiting for product · {waited}m")
                            }
                            crate::runs::Whom::Author => {
                                format!("⏸ Waiting on the author · {waited}m")
                            }
                        }),
                )
                // Never cut off: the line opens the question.
                .child(div().flex_none().text_xs().text_color(tone).child("›"))
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.question_dialog(orch, run.clone(), window, cx)
                }))
                .into_any_element(),
        )
    }

    /// The question in full, where it was asked, when hydraui next looks for the reply, and
    /// what can be done about it.
    fn question_dialog(
        &mut self,
        orch: PaneId,
        run: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((run, ask, channel)) = self.asked(orch, run.as_deref(), cx) else {
            return;
        };
        let now = SystemTime::now();
        let waited = now
            .duration_since(ask.since)
            .map_or(0, |d| d.as_secs() / 60);
        let asked = match channel {
            Some(ch) => format!("Asked in {ch} {waited} min ago"),
            None => format!("Asked you {waited} min ago"),
        };
        let waiting = server::get()
            .and_then(|sv| Some(server::lock(sv.registry()).runs.run(&run)?.pipeline.waiting))
            .unwrap_or_default();
        let meta = match (&ask.link, waiting.look_every) {
            (None, _) | (_, 0) => asked,
            (Some(_), _) => match crate::waiting::next_in(
                &waiting,
                ask.since,
                self.ask_checks.get(&run).copied(),
                now,
            ) {
                0 => format!("{asked} · checking for a reply"),
                m => format!("{asked} · next check for a reply in {m} min"),
            },
        };
        let ws = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, run, link) = (ws.clone(), run.clone(), ask.link.clone());
            let (ws2, run2) = (ws.clone(), run.clone());
            dialog
                .title(match ask.whom {
                    crate::runs::Whom::Product => "Waiting for product",
                    crate::runs::Whom::Author => "Waiting on the author",
                })
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(color(theme::TEXT))
                                .child(ask.question.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(meta.clone()),
                        ),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .children(link.clone().map(|url| {
                            Button::new("question-thread")
                                .label("Open thread ↗")
                                .ghost()
                                .on_click(move |_, _, cx| cx.open_url(&url))
                        }))
                        .when(link.is_some(), |d| {
                            d.child(Button::new("question-check").label("Check now").on_click(
                                move |_, window, cx| {
                                    window.close_dialog(cx);
                                    ws.update(cx, |ws, cx| ws.check_answer_now(&run, window, cx));
                                },
                            ))
                        })
                        .child(
                            Button::new("question-answer")
                                .label("Answer…")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let run = run2.clone();
                                    ws2.update(cx, |ws, cx| ws.answer_dialog(run, window, cx));
                                }),
                        ),
                )
        });
    }
}
