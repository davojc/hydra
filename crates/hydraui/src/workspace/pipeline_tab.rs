//! The Pipeline tab (orchestrator spec §7.2; the design's View 1): beside the orchestrator's
//! terminal, the run's timeline on the left and the selected session on the right: its live
//! terminal, its rating, or the stopped banner with the person's choices (§5.3). The rules are
//! in `crate::pipeline_view`; this is the drawing and the wiring.
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, App, Context, Div, FontWeight, SharedString, Window, div, px,
    relative,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{Disableable, Sizable, WindowExt, h_flex, v_flex};
use serde_json::json;

use super::Workspace;
use super::orchestrator::tell;
use crate::envs::PaneId;
use crate::mcp::{SessionState, server};
use crate::orchestrator_theme::{self as theme, color};
use crate::pipeline_view::{Panel, Tone, elapsed, gate_badge, panel_for, session_line};
use crate::rating::{Outcome, Rating, Side};
use crate::runs::{Choice, Run, RunState, StageStatus, session_name};

/// What a Pipeline tab shows: the run, or the pipeline's settings (the design's sub-links).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum TabView {
    #[default]
    Pipeline,
    Settings,
    History,
}

/// A terminal's Pipeline tab.
#[derive(Debug, Clone, Default)]
pub(super) struct PipelineTab {
    pub view: TabView,
    /// Ticket history's filter chip and expanded row.
    pub history_filter: super::pipeline_history::HistoryFilter,
    pub open_row: Option<String>,
    /// The tab is shown, rather than the terminal.
    pub shown: bool,
    /// The session the user picked, by label; `None` follows the run.
    pub selected: Option<String>,
    /// The user stepped into the stopped session (the design's step-in).
    pub stepped_in: bool,
    /// **Stop this ticket** is asking to be confirmed.
    pub confirm_stop: bool,
}

/// What the tab draws, copied out of the registry so its lock isn't held while drawing.
struct Snapshot {
    run: Option<Run>,
    /// Each session token's hook state.
    live: HashMap<String, SessionState>,
}

/// The muted line under a stage's title: its skill and the folder it starts in.
fn stage_where(skill: Option<&str>, folder: Option<&str>) -> Option<String> {
    let folder = folder.map(|f| {
        let name = std::path::Path::new(f)
            .file_name()
            .map_or(f.to_string(), |n| n.to_string_lossy().into_owned());
        format!("in {name}")
    });
    match (skill, folder) {
        (Some(k), Some(f)) => Some(format!("{k} · {f}")),
        (Some(k), None) => Some(k.to_string()),
        (None, f) => f,
    }
}

fn text(size: f32, hex: u32) -> Div {
    div().text_size(px(size)).text_color(color(hex))
}

/// What **Score again** types into the stopped stage's session.
const SCORE_AGAIN: &str = "A person has worked on this stage in this session since you reported. \
     Look at the stage's work as it is now, check it again the way the stage asks, then call \
     stage_report again: a fresh score, evidence, and reasons for and against.";

/// The note a person started an attempt with (Re-run or Go back), under its panel's header.
fn note_strip(note: Option<&str>) -> Option<AnyElement> {
    let note = note.map(str::trim).filter(|n| !n.is_empty())?;
    Some(
        v_flex()
            .px(px(16.))
            .py(px(8.))
            .gap_0p5()
            .bg(color(theme::RAISED))
            .border_b_1()
            .border_color(color(theme::BORDER_SUBTLE))
            .child(text(11., theme::TEXT_MUTED).child("STARTED WITH YOUR NOTE"))
            .child(
                text(12.5, theme::TEXT)
                    .line_height(relative(1.4))
                    .child(note.to_string()),
            )
            .into_any_element(),
    )
}

impl Workspace {
    /// Whether terminal `id` shows its Pipeline tab.
    pub(super) fn pipeline_shown(&self, id: PaneId) -> bool {
        self.pipeline_tabs.get(&id).is_some_and(|t| t.shown)
    }

    /// Shows orchestrator `orch` with its Pipeline tab in front, following its run (the card's
    /// click, the escalation toast's **Open ticket**).
    pub(super) fn open_pipeline(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_terminal(orch, window, cx);
        self.pipeline_tabs.insert(
            orch,
            PipelineTab {
                shown: true,
                ..Default::default()
            },
        );
        if let Some(tasks) = self.tasks_tabs.get_mut(&orch) {
            *tasks = false;
        }
        // The other orchestrators of its pipeline get theirs too, behind it.
        if self.orchestrators.contains_key(&orch) {
            for other in self.slots(orch, cx) {
                self.pipeline_tabs.entry(other).or_default();
            }
        }
        self.sync_boards(window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Shows orchestrator `orch`'s Pipeline tab at `view`, opening the tab when it has none.
    pub(super) fn show_pipeline_view(
        &mut self,
        orch: PaneId,
        view: TabView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.pipeline_tabs.contains_key(&orch) {
            self.open_pipeline(orch, window, cx);
        } else {
            self.show_terminal(orch, window, cx);
            self.show_pipeline(orch, true, window, cx);
        }
        if let Some(tab) = self.pipeline_tabs.get_mut(&orch) {
            tab.view = view;
        }
        cx.notify();
    }

    /// The strip's Pipeline tab (`true`) or the terminal's (`false`).
    pub(super) fn show_pipeline(
        &mut self,
        id: PaneId,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.pipeline_tabs.get_mut(&id) else {
            return;
        };
        tab.shown = shown;
        if shown && let Some(tasks) = self.tasks_tabs.get_mut(&id) {
            *tasks = false;
        }
        self.sync_boards(window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// ✕ on the Pipeline tab.
    pub(super) fn close_pipeline(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pipeline_tabs.remove(&id);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// While orchestrator `orch`'s Pipeline tab is shown: the pane the user stepped into, which
    /// gets the focus; `None` keeps the focus off the hidden orchestrator.
    pub(super) fn pipeline_focus(&self, orch: PaneId, cx: &App) -> Option<PaneId> {
        let tab = self.pipeline_tabs.get(&orch).filter(|t| t.stepped_in)?;
        let snap = self.pipeline_snapshot(orch, cx);
        let run = snap.run.as_ref()?;
        match panel_for(Some(run), tab.selected.as_deref()) {
            Panel::Stopped(si, ai) => self.pane_of_session(run, si, ai, cx),
            _ => None,
        }
    }

    fn pipeline_snapshot(&self, orch: PaneId, cx: &App) -> Snapshot {
        let token = self
            .panes
            .get(&orch)
            .and_then(|v| v.read(cx).mcp_token().map(str::to_string));
        let (Some(token), Some(sv)) = (token, server::get()) else {
            return Snapshot {
                run: None,
                live: HashMap::new(),
            };
        };
        let reg = server::lock(sv.registry());
        let run = reg.runs.current(&token).cloned();
        let live = run
            .iter()
            .flat_map(|r| r.stages.iter().flat_map(|s| &s.attempts))
            .filter_map(|a| a.session.clone())
            .filter_map(|t| reg.session(&t).map(|s| (t, s)))
            .collect();
        Snapshot { run, live }
    }

    /// The terminal holding attempt `ai` of stage `si`'s session, while it is open.
    fn pane_of_session(&self, run: &Run, si: usize, ai: usize, cx: &App) -> Option<PaneId> {
        let token = run.stages[si].attempts.get(ai)?.session.as_deref()?;
        self.panes
            .iter()
            .find(|(_, v)| v.read(cx).mcp_token() == Some(token))
            .map(|(id, _)| *id)
    }

    fn pipeline_tab_update(
        &mut self,
        orch: PaneId,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut PipelineTab),
    ) {
        if let Some(tab) = self.pipeline_tabs.get_mut(&orch) {
            f(tab);
        }
        cx.notify();
    }

    /// A person's choice for run `run` (§5.3): applied under the registry's lock, written to the
    /// run's log with the note, then told to the app and the orchestrator.
    pub(super) fn person_choice(
        &mut self,
        orch: PaneId,
        run: &str,
        choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (word, note, stage) = match &choice {
            Choice::Retry { note } => ("re-run", Some(note.trim().to_string()), None),
            Choice::Stop => ("stop", None, None),
            Choice::Back { stage, note } => {
                ("back", Some(note.trim().to_string()), Some(stage.clone()))
            }
        };
        let Some(sv) = server::get() else {
            return;
        };
        let done = {
            let mut reg = server::lock(sv.registry());
            let done = reg.runs.person(run, choice);
            if let (Ok(_), Some(root)) = (&done, &reg.hydra_root) {
                let file = crate::run_log::file_for(root, run);
                if let Err(e) = crate::run_log::append(
                    &file,
                    "person",
                    json!({"choice": word, "note": note, "stage": stage}),
                ) {
                    crate::action_log::write(format!("run log {run}: {e}"));
                }
            }
            done
        };
        match done {
            Ok(changes) => {
                crate::action_log::write(format!("run {run}: the user chose {word}"));
                self.pipeline_tab_update(orch, cx, |t| {
                    t.selected = None;
                    t.stepped_in = false;
                    t.confirm_stop = false;
                });
                self.focus_active(window, cx);
                tell(changes);
            }
            Err(e) => window.push_notification(Notification::error(e).title("Pipeline"), cx),
        }
    }

    /// **Score again**: the stopped stage's own session, which the person has worked in since
    /// its report, is asked to look again and report a new score.
    fn score_again(
        &mut self,
        orch: PaneId,
        run: &str,
        stage: usize,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sv) = server::get() else {
            return;
        };
        let done = server::lock(sv.registry()).runs.rescore(run, stage);
        match done {
            Ok(changes) => {
                crate::action_log::write(format!("run {run}: the user asked for a new score"));
                self.pending_wakes
                    .insert(pane, (run.to_string(), SCORE_AGAIN.to_string()));
                self.send_wakes(window, cx);
                self.pipeline_tab_update(orch, cx, |t| {
                    t.selected = None;
                    t.stepped_in = false;
                });
                tell(changes);
            }
            Err(e) => window.push_notification(Notification::error(e).title("Score again"), cx),
        }
    }

    /// **Must address** on reason `reason` of stage `stage`'s latest report, or off.
    fn mark_reason(
        &mut self,
        run: &str,
        stage: usize,
        reason: usize,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let done = run_choice(
            run,
            "marked",
            |runs| {
                runs.mark(run, stage, reason, on)
                    .map(|()| (Vec::new(), json!({"reason": reason, "on": on})))
            },
            stage,
        );
        match done {
            Ok(_) => cx.notify(),
            Err(e) => window.push_notification(Notification::error(e).title("Must address"), cx),
        }
    }

    /// **Send back**: the stage goes again to deal with what was marked **Must address**.
    fn send_back(
        &mut self,
        orch: PaneId,
        run: &str,
        stage: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let done = run_choice(
            run,
            "sent back",
            |runs| runs.send_back(run, stage).map(|c| (c, json!({}))),
            stage,
        );
        match done {
            Ok(changes) => {
                crate::action_log::write(format!("run {run}: the user sent stage {stage} back"));
                if !changes.is_empty() {
                    self.pipeline_tab_update(orch, cx, |t| {
                        t.selected = None;
                        t.stepped_in = false;
                    });
                }
                cx.notify();
                tell(changes);
            }
            Err(e) => window.push_notification(Notification::error(e).title("Send back"), cx),
        }
    }

    fn cancel_send_back(&mut self, run: &str, window: &mut Window, cx: &mut Context<Self>) {
        let stage = server::get()
            .and_then(|sv| {
                let reg = server::lock(sv.registry());
                Some(reg.runs.run(run)?.fix_after.as_ref()?.stage)
            })
            .unwrap_or_default();
        let done = run_choice(
            run,
            "send-back cancelled",
            |runs| runs.cancel_fix(run).map(|()| (Vec::new(), json!({}))),
            stage,
        );
        match done {
            Ok(_) => cx.notify(),
            Err(e) => window.push_notification(Notification::error(e).title("Send back"), cx),
        }
    }

    /// The re-run dialog (the design's modal): a note for the next session, required.
    fn rerun_dialog(
        &mut self,
        orch: PaneId,
        run: String,
        title: String,
        bar: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("What should the next session do differently?")
        });
        let ws = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let submit = {
                let (ws, input, run) = (ws.clone(), input.clone(), run.clone());
                move |window: &mut Window, cx: &mut App| {
                    let note = input.read(cx).value().trim().to_string();
                    if note.is_empty() {
                        window.push_notification(
                            Notification::warning("A re-run needs a note for its next session.")
                                .title("Re-run"),
                            cx,
                        );
                        return;
                    }
                    window.close_dialog(cx);
                    let run = run.clone();
                    ws.update(cx, |ws, cx| {
                        ws.person_choice(orch, &run, Choice::Retry { note }, window, cx)
                    });
                }
            };
            let on_ok = submit.clone();
            dialog
                .title(format!("Re-run {title}"))
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(v_flex().gap_2().child(Input::new(&input)).child(
                    text(12.5, theme::TEXT_SECONDARY).child(format!(
                        "A fresh session starts with the previous attempt's report and your \
                             note. It still has to clear the {bar}% bar."
                    )),
                ))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("rerun-cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("rerun-go")
                                .label(format!("Re-run {title}"))
                                .primary()
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                )
        });
    }

    /// **Go back…** (Go back): pick an earlier stage that passed and write a note; its own
    /// session reopens with it, and the stages after it run again with fresh tries.
    fn back_dialog(
        &mut self,
        orch: PaneId,
        run: String,
        preselect: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stages: Vec<(String, String)> = server::get()
            .and_then(|sv| {
                let reg = server::lock(sv.registry());
                let r = reg.runs.run(&run)?;
                Some(
                    r.pipeline
                        .stages
                        .iter()
                        .zip(&r.stages)
                        .take_while(|(_, s)| s.status == StageStatus::Passed)
                        .map(|(spec, _)| (spec.id.clone(), spec.title.clone()))
                        .collect(),
                )
            })
            .unwrap_or_default();
        if stages.is_empty() {
            window.push_notification(
                Notification::info("No earlier stage has passed yet: re-run the waiting one.")
                    .title("Go back"),
                cx,
            );
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "What should it fix? e.g. Rename the branch to INTH-166 and reopen the PR",
            )
        });
        // The stage just before the waiting one, unless another is picked.
        let picked = Rc::new(Cell::new(
            preselect
                .filter(|p| *p < stages.len())
                .unwrap_or(stages.len() - 1),
        ));
        let ws = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let submit = {
                let (ws, input, run, stages, picked) = (
                    ws.clone(),
                    input.clone(),
                    run.clone(),
                    stages.clone(),
                    picked.clone(),
                );
                move |window: &mut Window, cx: &mut App| {
                    let note = input.read(cx).value().trim().to_string();
                    if note.is_empty() {
                        window.push_notification(
                            Notification::warning(
                                "Going back needs a note for that stage's session.",
                            )
                            .title("Go back"),
                            cx,
                        );
                        return;
                    }
                    window.close_dialog(cx);
                    let stage = stages[picked.get()].0.clone();
                    let run = run.clone();
                    ws.update(cx, |ws, cx| {
                        ws.person_choice(orch, &run, Choice::Back { stage, note }, window, cx)
                    });
                }
            };
            let on_ok = submit.clone();
            let choices = stages.iter().enumerate().map(|(i, (_, title))| {
                let picked = picked.clone();
                Button::new(SharedString::from(format!("back-to-{i}")))
                    .label(format!("{}. {title}", i + 1))
                    .when(picked.get() == i, |b| b.primary())
                    .when(picked.get() != i, |b| b.outline())
                    .small()
                    .on_click(move |_, window, _| {
                        picked.set(i);
                        window.refresh();
                    })
            });
            dialog
                .title("Go back to an earlier stage")
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(
                    v_flex()
                        .gap_2()
                        .child(h_flex().gap_1().flex_wrap().children(choices))
                        .child(Input::new(&input))
                        .child(text(12.5, theme::TEXT_SECONDARY).child(
                            "That stage's own session reopens with your note and reports again; \
                             it still has to clear its bar. The stages after it run again, with \
                             fresh tries.",
                        )),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("back-cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("back-go")
                                .label("Go back")
                                .primary()
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                )
        });
    }

    /// **Start next ticket** on a finished run: the orchestrator is asked to start one.
    fn start_next_ticket(
        &mut self,
        orch: PaneId,
        run: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = "The user wants the next ticket. Ask them which one if they haven't said, \
                       then start it with run_start."
            .to_string();
        self.pending_wakes.insert(orch, (run, message));
        self.send_wakes(window, cx);
    }

    /// The Pipeline tab for orchestrator `orch`: its views' bar over the one shown.
    pub(super) fn render_pipeline(&self, orch: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.pipeline_tabs.get(&orch).cloned().unwrap_or_default();
        let view = match tab.view {
            TabView::Settings if !self.pipeline_settings.contains_key(&orch) => TabView::Pipeline,
            v => v,
        };
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .child(self.render_view_bar(orch, view, self.accent_of(orch, cx), cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_pipeline_view(orch, &tab, view, cx)),
            )
            .into_any_element()
    }

    /// **Ticket · History · Settings**: the pipeline's views, kept here rather than as links
    /// under its card.
    /// Orchestrator `orch`'s pipeline colour: its run's pipeline, or the one it was started on.
    pub(super) fn accent_of(&self, orch: PaneId, cx: &App) -> u32 {
        let token = self
            .panes
            .get(&orch)
            .and_then(|v| v.read(cx).mcp_token().map(str::to_string));
        let from_run = token.zip(server::get()).and_then(|(token, sv)| {
            let reg = server::lock(sv.registry());
            reg.runs
                .current(&token)
                .map(|r| crate::pipeline_card::accent(&r.pipeline))
        });
        from_run
            .or_else(|| {
                let file = self.orchestrators.get(&orch)?;
                let p = crate::pipeline::load(self.hydra.paths.root(), file).ok()?;
                Some(crate::pipeline_card::accent(&p))
            })
            .unwrap_or(theme::TEAL)
    }

    fn render_view_bar(
        &self,
        orch: PaneId,
        current: TabView,
        accent: u32,
        cx: &mut Context<Self>,
    ) -> Div {
        let item = |id: &str, label: &'static str, view: TabView| {
            let on = view == current;
            div()
                .id(SharedString::from(format!("pipeline-view-{id}-{orch}")))
                .px_2()
                .py(px(3.))
                .rounded(px(5.))
                .text_size(px(12.5))
                .cursor_pointer()
                .when(on, |d| {
                    d.bg(color(theme::tint_of(accent)))
                        .text_color(color(theme::TEXT))
                })
                .when(!on, |d| {
                    d.text_color(color(theme::TEXT_SECONDARY))
                        .hover(|d| d.bg(color(theme::RAISED)))
                })
                .child(label)
                .on_click(cx.listener(move |ws, _, window, cx| match view {
                    TabView::Settings => ws.open_pipeline_settings(orch, window, cx),
                    v => ws.show_pipeline_view(orch, v, window, cx),
                }))
        };
        h_flex()
            .px_3()
            .py_1p5()
            .gap_1()
            .border_b_1()
            .border_color(color(theme::BORDER_SUBTLE))
            .child(item("ticket", "Ticket", TabView::Pipeline))
            .child(item("history", "History", TabView::History))
            .child(item("settings", "Settings", TabView::Settings))
            .child(div().flex_1())
            .child(
                Button::new(SharedString::from(format!("end-pipeline-{orch}")))
                    .label("■  End pipeline")
                    .danger()
                    .outline()
                    .small()
                    .tooltip(
                        "Stop its tickets in progress and close its orchestrators and sessions",
                    )
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.end_pipeline_dialog(orch, window, cx)
                    })),
            )
    }

    fn render_pipeline_view(
        &self,
        orch: PaneId,
        tab: &PipelineTab,
        view: TabView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let snap = self.pipeline_snapshot(orch, cx);
        if view == TabView::Settings {
            return self.render_pipeline_settings(orch, cx);
        }
        if view == TabView::History {
            return self.render_history(orch, cx);
        }
        let Some(run) = snap.run.as_ref() else {
            return self.render_waiting_for_work(orch, cx);
        };
        let panel = panel_for(Some(run), tab.selected.as_deref());
        let now = SystemTime::now();
        h_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .child(self.render_timeline(orch, run, &snap, panel, now, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_panel(orch, run, &snap, tab, panel, now, cx)),
            )
            .into_any_element()
    }

    /// No ticket in progress: for a pipeline that finds its own work, what its trigger is doing
    /// and the wait for its next look, counting down, as under its card.
    fn render_waiting_for_work(&self, orch: PaneId, cx: &mut Context<Self>) -> AnyElement {
        use super::linear::{Trigger, TriggerView, look_bar};
        let t = match self.trigger_view(orch, cx) {
            None => {
                return self.render_quiet(
                    "No ticket in progress",
                    "Give the orchestrator a ticket; its stages show here as they run.",
                );
            }
            Some(Trigger::Follower(text)) => {
                return self.render_quiet("Nothing in progress", &text);
            }
            Some(Trigger::Lead(t)) => t,
        };
        let TriggerView {
            line,
            progress,
            last,
            source,
            ..
        } = t;
        let accent = self.accent_of(orch, cx);
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .items_center()
            .justify_center()
            .gap_3()
            .child(text(16., theme::TEXT).child(format!("No {} in progress", source.noun)))
            .child(text(12.5, theme::TEXT_SECONDARY).child(line))
            .children(progress.map(|p| {
                div().w(px(320.)).child(look_bar(
                    SharedString::from(format!("look-bar-tab-{orch}")),
                    p,
                    last,
                    source.every_minutes,
                    accent,
                ))
            }))
            .into_any_element()
    }

    fn render_quiet(&self, title: &str, body: &str) -> AnyElement {
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .items_center()
            .justify_center()
            .gap_2()
            .child(text(16., theme::TEXT).child(title.to_string()))
            .child(
                text(12.5, theme::TEXT_MUTED)
                    .max_w(px(420.))
                    .text_center()
                    .child(body.to_string()),
            )
            .into_any_element()
    }

    /// The left column: the ticket, then a row per stage with its sessions under it, then Done.
    fn render_timeline(
        &self,
        orch: PaneId,
        run: &Run,
        snap: &Snapshot,
        panel: Panel,
        now: SystemTime,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = match panel {
            Panel::Live(si, ai) | Panel::Report(si, ai) | Panel::Stopped(si, ai) => Some((si, ai)),
            _ => None,
        };
        let stopped_at = run.stages.iter().position(|s| {
            matches!(s.status, StageStatus::Stopped | StageStatus::Failed)
                && matches!(run.state, RunState::NeedsYou(_) | RunState::Stopped(_))
        });
        let mut rows = v_flex().px(px(22.)).py(px(18.));
        let count = run.stages.len();
        let accent = crate::pipeline_card::accent(&run.pipeline);
        for (i, s) in run.stages.iter().enumerate() {
            let spec = &run.pipeline.stages[i];
            let bar = run.pipeline.bar_for(i);
            let (badge, tone) = gate_badge(s.rating(), bar, s.status);
            let (title_hex, badge_hex) = match (s.status, tone) {
                (st, _) if st.busy() => (accent, theme::TEXT_MUTED),
                (StageStatus::Stopped | StageStatus::Failed, _) => (theme::RED, theme::RED),
                (StageStatus::Passed, _) => (theme::TEXT, theme::GREEN),
                (_, Tone::Muted) => (theme::TEXT_SECONDARY, theme::TEXT_MUTED),
                _ => (theme::TEXT, theme::TEXT_MUTED),
            };
            let circle = div()
                .size(px(22.))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .map(|d| match s.status {
                    StageStatus::Passed => d
                        .bg(color(theme::GREEN_TINT))
                        .text_color(color(theme::GREEN))
                        .child("✓"),
                    st if st.busy() => d
                        .border_2()
                        .border_color(color(accent))
                        .child(div().size(px(7.)).rounded_full().bg(color(accent))),
                    StageStatus::Stopped | StageStatus::Failed => d
                        .bg(color(theme::RED_TINT))
                        .text_color(color(theme::RED))
                        .child("!"),
                    _ => d
                        .border_1()
                        .border_color(color(theme::BORDER_STRONG))
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(format!("{}", i + 1)),
                });
            let connector = div().w(px(1.)).flex_1().min_h(px(14.)).bg(color(
                if s.status == StageStatus::Passed {
                    accent
                } else {
                    theme::BORDER
                },
            ));
            let skill = spec.skill.clone().unwrap_or_else(|| spec.title.clone());
            let mut sessions = v_flex().gap_1().pt_1();
            for (ai, a) in s.attempts.iter().enumerate() {
                let live = a.session.as_ref().and_then(|t| snap.live.get(t)).copied();
                let line = session_line(run, i, ai, live, now);
                let latest = ai + 1 == s.attempts.len();
                let (dot, tint, edge) = if latest && s.status.busy() {
                    (accent, theme::tint_of(accent), theme::edge_of(accent))
                } else if latest && s.status == StageStatus::Stopped {
                    (theme::RED, theme::WAITING_TINT, theme::RED_BORDER)
                } else {
                    (theme::TEXT_MUTED, theme::RAISED, theme::BORDER_STRONG)
                };
                let is_selected = selected == Some((i, ai));
                let label = a.label.clone();
                // The note a person started this try with (Re-run or Go back), kept in view.
                let note = a
                    .note
                    .clone()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty());
                sessions = sessions.child(
                    v_flex()
                        .id(SharedString::from(format!(
                            "pipeline-session-{orch}-{label}"
                        )))
                        .px(px(10.))
                        .py(px(7.))
                        .gap_1()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if is_selected {
                            color(edge)
                        } else {
                            gpui::transparent_black()
                        })
                        .when(is_selected, |d| d.bg(color(tint)))
                        .hover(|d| d.bg(color(theme::SURFACE)))
                        .cursor_pointer()
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            let label = label.clone();
                            ws.pipeline_tab_update(orch, cx, |t| {
                                t.selected = Some(label);
                                t.stepped_in = false;
                                t.confirm_stop = false;
                            });
                        }))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().size(px(6.)).flex_none().rounded_full().bg(color(dot)))
                                .child(
                                    text(12., theme::TEXT)
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(format!("Try {} · {skill}", ai + 1)),
                                )
                                .child(text(11.5, theme::TEXT_MUTED).flex_none().child(line)),
                        )
                        .children(note.map(|n| {
                            text(11.5, theme::TEXT_SECONDARY)
                                .pl(px(14.))
                                .line_height(relative(1.35))
                                .child(format!("↩ Your note: {n}"))
                        })),
                );
            }
            // While the run waits for a person, a stage that passed can be gone back to.
            let can_go_back = matches!(run.state, RunState::NeedsYou(_))
                && s.status == StageStatus::Passed
                && stopped_at.is_none_or(|at| i < at);
            if can_go_back {
                let back_run = run.id.clone();
                sessions = sessions.child(
                    div()
                        .id(SharedString::from(format!("pipeline-back-here-{orch}-{i}")))
                        .pl(px(10.))
                        .text_size(px(11.5))
                        .text_color(color(accent))
                        .cursor_pointer()
                        .hover(|d| d.underline())
                        .child("↩ Go back here")
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.back_dialog(orch, back_run.clone(), Some(i), window, cx)
                        })),
                );
            }
            let dim = stopped_at.is_some_and(|at| i > at);
            rows = rows.child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .when(dim, |d| d.opacity(0.45))
                    .child(
                        v_flex()
                            .items_center()
                            .self_stretch()
                            .child(circle)
                            .when(i + 1 < count, |d| d.child(connector)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .pb_4()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .gap_2()
                                    .child({
                                        let title = text(13.5, title_hex)
                                            .when(title_hex != theme::TEXT, |d| {
                                                d.font_weight(FontWeight::MEDIUM)
                                            })
                                            .child(spec.title.clone());
                                        // The stage at work breathes in the pipeline's colour.
                                        if s.status.busy() {
                                            title
                                                .with_animation(
                                                    SharedString::from(format!(
                                                        "stage-title-{orch}-{i}"
                                                    )),
                                                    super::busy::pulse(),
                                                    move |d, a| {
                                                        d.text_color(color(accent).opacity(a))
                                                    },
                                                )
                                                .into_any_element()
                                        } else {
                                            title.into_any_element()
                                        }
                                    })
                                    .child(text(11.5, badge_hex).flex_none().child(badge)),
                            )
                            .children(
                                stage_where(spec.skill.as_deref(), spec.folder.as_deref())
                                    .map(|k| text(11.5, theme::TEXT_MUTED).child(k)),
                            )
                            .child(sessions),
                    ),
            );
        }
        let finished = run.state == RunState::Finished;
        rows = rows.child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .size(px(22.))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.))
                        .map(|d| {
                            if finished {
                                d.bg(color(theme::GREEN_TINT))
                                    .text_color(color(theme::GREEN))
                                    .child("✓")
                            } else {
                                d.border_1()
                                    .border_color(color(theme::BORDER_STRONG))
                                    .text_color(color(theme::TEXT_SECONDARY))
                                    .child(format!("{}", count + 1))
                            }
                        }),
                )
                .child(
                    v_flex()
                        .child(text(13.5, theme::TEXT_SECONDARY).child("Done"))
                        .child(
                            text(11.5, theme::TEXT_MUTED).child("PR left for a person to merge"),
                        ),
                ),
        );
        v_flex()
            .w(relative(0.34))
            .min_w(px(280.))
            .max_w(px(380.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(color(theme::BORDER_SUBTLE))
            .child(
                v_flex()
                    .px(px(22.))
                    .py(px(18.))
                    .gap_1()
                    .border_b_1()
                    .border_color(color(theme::BORDER_SUBTLE))
                    .child(text(12.5, theme::TEXT_SECONDARY).child(run.ticket.clone()))
                    .children(self.render_ticket_link(
                        &run.ticket,
                        &crate::linear_watch::pr_links(run),
                        crate::pipeline_card::accent(&run.pipeline),
                    ))
                    .child(
                        text(16., theme::TEXT)
                            .font_weight(FontWeight::MEDIUM)
                            .line_height(relative(1.35))
                            .child(run.summary.clone()),
                    )
                    .child(text(12., theme::TEXT_MUTED).child(format!(
                        "{} · started {} ago",
                        run.pipeline.name,
                        elapsed(run.started, now)
                    ))),
            )
            .child(
                div()
                    .id(SharedString::from(format!("pipeline-timeline-{orch}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(rows),
            )
            .into_any_element()
    }

    /// The right side, for `panel`.
    #[allow(clippy::too_many_arguments)]
    fn render_panel(
        &self,
        orch: PaneId,
        run: &Run,
        snap: &Snapshot,
        tab: &PipelineTab,
        panel: Panel,
        now: SystemTime,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminal = |si: usize, ai: usize| {
            self.pane_of_session(run, si, ai, cx)
                .and_then(|id| self.panes.get(&id).cloned())
                .map(|v| div().flex_1().min_h_0().child(v).into_any_element())
                .unwrap_or_else(|| {
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(text(12.5, theme::TEXT_MUTED).child("Its terminal was closed."))
                        .into_any_element()
                })
        };
        match panel {
            Panel::Idle => self.render_quiet("No ticket in progress", ""),
            Panel::Waiting => self.render_quiet(
                "Waiting for the first stage",
                "The orchestrator starts it with stage_start.",
            ),
            Panel::Live(si, ai) => {
                let spec = &run.pipeline.stages[si];
                let a = &run.stages[si].attempts[ai];
                let live = a.session.as_ref().and_then(|t| snap.live.get(t)).copied();
                let bar = run.pipeline.bar_for(si);
                let next = run
                    .pipeline
                    .stages
                    .get(si + 1)
                    .map_or("The run finishes".to_string(), |n| {
                        format!("{} starts", n.title)
                    });
                v_flex()
                    .size_full()
                    .child(self.panel_header(
                        crate::pipeline_card::accent(&run.pipeline),
                        session_name(si, &spec.title, ai),
                        format!(
                            "{} · {}",
                            spec.skill.clone().unwrap_or_else(|| spec.title.clone()),
                            session_line(run, si, ai, live, now)
                        ),
                    ))
                    .children(note_strip(a.note.as_deref()))
                    .child(terminal(si, ai))
                    .child(
                        v_flex()
                            .px(px(16.))
                            .py(px(10.))
                            .gap_1()
                            .border_t_1()
                            .border_color(color(theme::BORDER_SUBTLE))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(text(11.5, theme::TEXT_MUTED).child("Confidence"))
                                    .child(
                                        div()
                                            .relative()
                                            .flex_1()
                                            .h(px(6.))
                                            .rounded_full()
                                            .bg(color(theme::RAISED))
                                            .child(
                                                div()
                                                    .absolute()
                                                    .top_0()
                                                    .left(relative(f32::from(bar) / 100.0))
                                                    .w(px(1.))
                                                    .h_full()
                                                    .bg(color(theme::TEXT_SECONDARY)),
                                            ),
                                    ),
                            )
                            .child(text(11.5, theme::TEXT_MUTED).child(format!(
                                "Rated when this session ends · {next} at ≥{bar}%"
                            ))),
                    )
                    .into_any_element()
            }
            Panel::Report(si, ai) => {
                let spec = &run.pipeline.stages[si];
                let a = &run.stages[si].attempts[ai];
                let bar = run.pipeline.bar_for(si);
                let term = terminal(si, ai);
                let body = match &a.rating {
                    Some(r) => {
                        let passed = r.outcome == Outcome::Done && r.score >= bar;
                        let verdict = if passed {
                            match run.pipeline.stages.get(si + 1) {
                                Some(n) => format!("Cleared the {bar}% bar · {} started", n.title),
                                None => format!("Cleared the {bar}% bar · the run finished"),
                            }
                        } else if r.outcome == Outcome::Blocked {
                            "Blocked".to_string()
                        } else {
                            format!("Below the {bar}% bar")
                        };
                        self.rating_report(r, passed, verdict, true, marking(orch, run, si, ai), cx)
                    }
                    None => v_flex()
                        .px(px(28.))
                        .py(px(22.))
                        .child(text(16., theme::TEXT).child("Ended without a report"))
                        .child(text(12.5, theme::TEXT_MUTED).child(
                            "Its session ended before it called stage_report, so the stage \
                             failed.",
                        ))
                        .into_any_element(),
                };
                v_flex()
                    .size_full()
                    .child(self.panel_header(
                        theme::TEXT_MUTED,
                        session_name(si, &spec.title, ai),
                        session_line(run, si, ai, None, now),
                    ))
                    .children(note_strip(a.note.as_deref()))
                    .child(body)
                    .child(term)
                    .into_any_element()
            }
            Panel::Stopped(si, ai) if tab.stepped_in => {
                let spec = &run.pipeline.stages[si];
                let (run_id, title, bar) =
                    (run.id.clone(), spec.title.clone(), run.pipeline.bar_for(si));
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .px(px(16.))
                            .py(px(10.))
                            .gap_3()
                            .border_b_1()
                            .border_color(color(theme::BORDER_SUBTLE))
                            .child(div().size(px(8.)).rounded_full().bg(color(theme::AMBER)))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .child(text(13., theme::TEXT).child(format!(
                                        "{} · you're in control",
                                        session_name(si, &spec.title, ai)
                                    )))
                                    .child(text(11.5, theme::TEXT_MUTED).child(
                                        "The orchestrator waits while you work in this session",
                                    )),
                            )
                            .child(
                                Button::new("pipeline-back")
                                    .label("Back to report")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.pipeline_tab_update(orch, cx, |t| t.stepped_in = false);
                                        ws.focus_active(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("pipeline-hand-back")
                                    .label(format!("Hand back · re-run {title}"))
                                    .primary()
                                    .small()
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.rerun_dialog(
                                            orch,
                                            run_id.clone(),
                                            title.clone(),
                                            bar,
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                    .child(terminal(si, ai))
                    .into_any_element()
            }
            Panel::Stopped(si, ai) => self.render_stopped(orch, run, tab, si, Some(ai), cx),
            Panel::Held(si) => self.render_stopped(orch, run, tab, si, None, cx),
            Panel::Done => {
                let chips =
                    run.pipeline
                        .stages
                        .iter()
                        .zip(&run.stages)
                        .map(|(spec, s)| {
                            h_flex()
                                .px(px(8.))
                                .py(px(3.))
                                .gap_1()
                                .rounded(px(5.))
                                .bg(color(theme::RAISED))
                                .child(text(11.5, theme::TEXT_SECONDARY).child(spec.title.clone()))
                                .child(text(11.5, theme::GREEN).child(
                                    s.rating().map_or("–".into(), |r| format!("{}%", r.score)),
                                ))
                        });
                let run_id = run.id.clone();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(44.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(color(theme::GREEN_TINT))
                            .text_color(color(theme::GREEN))
                            .text_size(px(20.))
                            .child("✓"),
                    )
                    .child(text(20., theme::TEXT).child(format!("{} is done", run.ticket)))
                    .child(text(12.5, theme::TEXT_MUTED).child(
                        "Every stage cleared its bar. What it opened waits for a person to merge.",
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .justify_center()
                            .children(chips),
                    )
                    .child(
                        Button::new("pipeline-next")
                            .label("Start next ticket")
                            .primary()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.start_next_ticket(orch, run_id.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            }
        }
    }

    fn panel_header(&self, dot: u32, title: String, meta: String) -> Div {
        h_flex()
            .px(px(16.))
            .py(px(10.))
            .gap_3()
            .border_b_1()
            .border_color(color(theme::BORDER_SUBTLE))
            .child(div().size(px(8.)).rounded_full().bg(color(dot)))
            .child(text(13., theme::TEXT).child(title))
            .child(text(11.5, theme::TEXT_MUTED).child(meta))
    }

    /// A rating as the design's report shows it: the score, the verdict, Reasons and Evidence.
    /// `summary` is false where the summary already shows (the stopped banner's headline).
    /// With `marking`, each reason against can be marked **Must address** and the stage sent
    /// back to deal with them.
    fn rating_report(
        &self,
        r: &Rating,
        passed: bool,
        verdict: String,
        summary: bool,
        marking: Option<Marking>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reasons: Vec<Div> = r
            .reasons
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let (sign, hex) = match x.side {
                    Side::For => ("+", theme::GREEN),
                    Side::Against => ("−", theme::RED),
                };
                let row = h_flex()
                    .items_start()
                    .gap_2()
                    .child(text(12.5, hex).flex_none().child(sign))
                    // A width it can't grow past, so a long reason wraps inside its column.
                    .child(
                        text(12.5, theme::TEXT)
                            .flex_1()
                            .min_w_0()
                            .child(x.text.clone()),
                    );
                match (&marking, x.side) {
                    (Some(m), Side::Against) => row.child(must_toggle(m, i, cx)),
                    _ => row,
                }
            })
            .collect();
        let send_back = marking
            .as_ref()
            .filter(|m| !m.must.is_empty())
            .map(|m| send_back_row(m, cx));
        let evidence = r.evidence.iter().map(|e| {
            h_flex()
                .px(px(12.))
                .py(px(9.))
                .gap_2()
                .rounded(px(7.))
                .bg(color(theme::SURFACE))
                .child(
                    text(12., theme::TEXT)
                        .flex_1()
                        .min_w_0()
                        .child(e.detail.clone()),
                )
                .child(
                    text(11.5, theme::TEXT_MUTED).flex_none().child(
                        serde_json::to_value(e.kind)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default(),
                    ),
                )
        });
        let column = |title: &str| {
            v_flex()
                .flex_1()
                .min_w(px(260.))
                .gap_2()
                .child(text(11., theme::TEXT_MUTED).child(title.to_uppercase()))
        };
        v_flex()
            .px(px(28.))
            .py(px(22.))
            .gap_3()
            .child(
                h_flex()
                    .items_end()
                    .gap_3()
                    .child(
                        text(48., if passed { theme::GREEN } else { theme::RED })
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{}%", r.score)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .pb_2()
                            .child(text(13., theme::TEXT).child(verdict))
                            .when(summary, |d| {
                                d.child(text(12., theme::TEXT_SECONDARY).child(r.summary.clone()))
                            }),
                    ),
            )
            .child(
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap_6()
                    .child(column("Reasons").children(reasons).children(send_back))
                    .child(column("Evidence").children(evidence)),
            )
            .into_any_element()
    }

    /// The stopped banner and the person's choices (§5.3): for the session the run stopped
    /// at, or, with `ai` `None`, a run held before stage `si` started (the weekly limit).
    fn render_stopped(
        &self,
        orch: PaneId,
        run: &Run,
        tab: &PipelineTab,
        si: usize,
        ai: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spec = &run.pipeline.stages[si];
        let s = &run.stages[si];
        let bar = run.pipeline.bar_for(si);
        let reason = match &run.state {
            RunState::NeedsYou(why) => crate::pipeline_view::sentence(why),
            _ => String::new(),
        };
        let rating = ai
            .and_then(|ai| s.attempts.get(ai))
            .and_then(|a| a.rating.as_ref())
            .filter(|_| s.status == StageStatus::Stopped);
        let (overline, headline) = match (ai, rating) {
            (_, Some(r)) if r.outcome == Outcome::Blocked => (
                format!("Blocked at the {} gate", spec.title),
                r.summary.clone(),
            ),
            (_, Some(r)) => (
                format!(
                    "Stopped at the {} gate · {}% · bar {bar}%",
                    spec.title, r.score
                ),
                r.summary.clone(),
            ),
            (Some(_), None) => (
                format!("{} failed {} times", spec.title, s.attempts.len()),
                reason.clone(),
            ),
            (None, None) => (
                "Waiting for the weekly Claude limit".to_string(),
                reason.clone(),
            ),
        };
        let label = ai.map(|ai| session_name(si, &spec.title, ai));
        let can_step_in = ai.is_some_and(|ai| self.pane_of_session(run, si, ai, cx).is_some());
        let (run_id, title) = (run.id.clone(), spec.title.clone());
        let rerun_label = if ai.is_some() {
            format!("Re-run {} with a note", spec.title)
        } else {
            format!("Resume at {} with a note", spec.title)
        };
        let mut actions = h_flex()
            .px(px(28.))
            .py(px(14.))
            .gap_2()
            .border_t_1()
            .border_color(color(theme::BORDER_SUBTLE));
        if tab.confirm_stop {
            let run_id = run_id.clone();
            actions = actions
                .bg(color(theme::RED_TINT))
                .child(text(12.5, theme::TEXT).flex_1().child(format!(
                    "Stop {}? It's recorded as stopped at {title}; its terminals stay open and \
                     the orchestrator may take the next ticket.",
                    run.ticket
                )))
                .child(
                    Button::new("pipeline-stop-cancel")
                        .label("Cancel")
                        .ghost()
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.pipeline_tab_update(orch, cx, |t| t.confirm_stop = false)
                        })),
                )
                .child(
                    Button::new("pipeline-stop-go")
                        .label("Stop ticket")
                        .danger()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.person_choice(orch, &run_id, Choice::Stop, window, cx)
                        })),
                );
        } else {
            if let (true, Some(label)) = (can_step_in, label.clone()) {
                actions = actions.child(
                    Button::new("pipeline-step-in")
                        .label(format!("Step into {label}"))
                        .primary()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.pipeline_tab_update(orch, cx, |t| t.stepped_in = true);
                            ws.focus_active(window, cx);
                        })),
                );
            }
            let pane = ai.and_then(|ai| self.pane_of_session(run, si, ai, cx));
            if let (Some(_), Some(pane)) = (rating, pane) {
                let again_run = run_id.clone();
                actions = actions.child(
                    Button::new("pipeline-score-again")
                        .label("Score again")
                        .tooltip(
                            "Fixed it in the session? It checks its work again and reports a \
                             new score",
                        )
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.score_again(orch, &again_run, si, pane, window, cx)
                        })),
                );
            }
            let (rr_run, rr_title) = (run_id.clone(), title.clone());
            actions = actions
                .child(
                    Button::new("pipeline-rerun")
                        .label(rerun_label)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.rerun_dialog(orch, rr_run.clone(), rr_title.clone(), bar, window, cx)
                        })),
                )
                .child({
                    let back_run = run_id.clone();
                    Button::new("pipeline-back")
                        .label("Go back…")
                        .tooltip("Reopen an earlier stage's own session to fix its work")
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.back_dialog(orch, back_run.clone(), None, window, cx)
                        }))
                })
                .child(
                    Button::new("pipeline-stop")
                        .label("Stop this ticket")
                        .ghost()
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.pipeline_tab_update(orch, cx, |t| t.confirm_stop = true)
                        })),
                )
                .child(div().flex_1())
                .child(text(11.5, theme::TEXT_MUTED).child("Gates can't be skipped"));
        }
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .bg(color(theme::BANNER))
                    .px(px(28.))
                    .py(px(22.))
                    .gap_2()
                    .child(text(12., theme::RED).child(overline.to_uppercase()))
                    .child(
                        text(20., theme::TEXT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(headline),
                    )
                    .child(text(12.5, theme::TEXT_SECONDARY).child(
                        "You were notified in hydraui. Nothing downstream starts until you act.",
                    )),
            )
            .child(
                div()
                    .id(SharedString::from(format!("pipeline-stopped-{orch}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rating.map(|r| {
                        let marking = ai.and_then(|ai| marking(orch, run, si, ai));
                        self.rating_report(
                            r,
                            false,
                            format!("Below the {bar}% bar"),
                            false,
                            marking,
                            cx,
                        )
                    })),
            )
            .child(actions)
            .into_any_element()
    }
}

/// Applies `act` to the runs and, when it works, logs `event` with stage `stage` and its fields
/// to the run's log, for a restart to replay.
fn run_choice(
    run: &str,
    event: &str,
    act: impl FnOnce(
        &mut crate::runs::Runs,
    ) -> Result<(Vec<crate::runs::Change>, serde_json::Value), String>,
    stage: usize,
) -> Result<Vec<crate::runs::Change>, String> {
    let sv = server::get().ok_or("hydraui's MCP server isn't running")?;
    let mut reg = server::lock(sv.registry());
    let (changes, mut fields) = act(&mut reg.runs)?;
    let id = reg
        .runs
        .run(run)
        .and_then(|r| r.pipeline.stages.get(stage))
        .map(|s| s.id.clone());
    if let (Some(root), Some(obj)) = (&reg.hydra_root, fields.as_object_mut()) {
        obj.insert("stage".into(), json!(id));
        let file = crate::run_log::file_for(root, run);
        if let Err(e) = crate::run_log::append(&file, event, fields) {
            crate::action_log::write(format!("run log {run}: {e}"));
        }
    }
    Ok(changes)
}

/// What a report needs so a person can mark its reasons against **Must address** and send its
/// stage back.
struct Marking {
    orch: PaneId,
    run: String,
    stage: usize,
    title: String,
    must: Vec<usize>,
    /// This stage's send-back waits for the stage at work.
    queued: bool,
    /// The stage at work now, by title.
    at_work: Option<String>,
}

/// Only the stage's latest report with a reason against, in a run still open and not waiting on
/// a product answer, can be marked.
fn marking(orch: PaneId, run: &Run, si: usize, ai: usize) -> Option<Marking> {
    let s = run.stages.get(si)?;
    let a = s.attempts.get(ai).filter(|_| ai + 1 == s.attempts.len())?;
    let against = a
        .rating
        .as_ref()?
        .reasons
        .iter()
        .any(|x| x.side == Side::Against);
    if !against || !run.state.open() || run.asking.is_some() {
        return None;
    }
    Some(Marking {
        orch,
        run: run.id.clone(),
        stage: si,
        title: run.pipeline.stages[si].title.clone(),
        must: a.must.clone(),
        queued: run.fix_after.as_ref().is_some_and(|f| f.stage == si),
        at_work: run
            .stages
            .iter()
            .position(|s| s.status.busy())
            .map(|i| run.pipeline.stages[i].title.clone()),
    })
}

/// A reason against's **Must address** toggle.
fn must_toggle(m: &Marking, i: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let on = m.must.contains(&i);
    let (run, stage) = (m.run.clone(), m.stage);
    let b = Button::new(SharedString::from(format!("must-{}-{stage}-{i}", m.run)))
        .label(if on {
            "✓ Must address"
        } else {
            "Must address"
        })
        .xsmall()
        .disabled(m.queued)
        .tooltip("Mark it, then Send back: the stage goes again to deal with it")
        .on_click(
            cx.listener(move |ws, _, window, cx| ws.mark_reason(&run, stage, i, !on, window, cx)),
        );
    let b = if on { b.danger() } else { b.ghost() };
    div().flex_none().child(b).into_any_element()
}

/// Under the reasons once one is marked: **Send back**, or the send-back waiting for the stage
/// at work, with **Cancel**.
fn send_back_row(m: &Marking, cx: &mut Context<Workspace>) -> AnyElement {
    let n = m.must.len();
    let what = if n == 1 {
        "1 reason".to_string()
    } else {
        format!("{n} reasons")
    };
    let run = m.run.clone();
    let row = v_flex()
        .mt_2()
        .p_2()
        .gap_1()
        .rounded(px(6.))
        .bg(color(theme::RED_TINT));
    if m.queued {
        let at = m
            .at_work
            .clone()
            .unwrap_or_else(|| "the stage at work".into());
        return row
            .child(text(12., theme::TEXT).child(format!(
                "↩ Going back to {} to fix {what} once {at} reports.",
                m.title
            )))
            .child(
                div().child(
                    Button::new(SharedString::from(format!("send-back-cancel-{run}")))
                        .label("Cancel send back")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.cancel_send_back(&run, window, cx)
                        })),
                ),
            )
            .into_any_element();
    }
    let when = match &m.at_work {
        Some(at) => format!(
            "It goes once {at} reports: {}'s session works on them, then the stages after it \
             run again.",
            m.title
        ),
        None => format!(
            "{}'s session goes again told to fix them and say how; the stages after it run \
             again.",
            m.title
        ),
    };
    let (stage, orch) = (m.stage, m.orch);
    row.child(
        div().child(
            Button::new(SharedString::from(format!("send-back-{run}-{stage}")))
                .label(format!("Send back to {} · fix {what}", m.title))
                .danger()
                .small()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.send_back(orch, &run, stage, window, cx)
                })),
        ),
    )
    .child(text(11.5, theme::TEXT_SECONDARY).child(when))
    .into_any_element()
}
