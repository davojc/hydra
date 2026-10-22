//! Stage terminals (orchestrator spec §3.2): the gate let a stage start, so hydraui opens its
//! terminal beside the user, in the run's worktree or the orchestrator's folder, and links it.
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px, relative};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::hydra::{PaneRequest, command_line};
use crate::mcp::server::{self, McpEvent};
use crate::orchestrator_theme::{self as theme, color};
use crate::pickup::{Session, ready_for_prompt};
use crate::pipeline_card::{Card, CardState, Segment, card, card_tokens, meter, ready_card};
use crate::rating::{Outcome, Side};
use crate::runs::{Change, Decided, MAX_ATTEMPTS, Role, Run, RunState, StageStatus, StartStage};
use crate::worktrees;

/// The first prompt of a terminal started as an orchestrator (spec §3.0, §9).
pub fn orchestrator_prompt(pipeline: &str) -> String {
    format!(
        "You are the orchestrator for hydraui's pipeline \"{pipeline}\". You coordinate one \
         ticket at a time through its stages and never do a stage's work yourself. Ask me which \
         ticket to run. Then call run_start with pipeline \"{pipeline}\", the ticket and its \
         title, and start each stage in order with stage_start. After each one, wait: hydraui \
         tells you when it reports. When a run needs a person, tell me why and wait."
    )
}

/// Where an orchestrator's Claude starts: `cwd`, unless that is the home folder, whose trust
/// Claude Code never remembers (it would ask at every start); then a folder of its own under
/// `<HYDRA_HOME>/hydraui/orchestrators`, trusted once and remembered.
pub fn orchestrator_folder(
    cwd: &std::path::Path,
    user_home: &std::path::Path,
    hydra_root: &std::path::Path,
    pipeline: &str,
) -> std::path::PathBuf {
    if cwd == user_home {
        hydra_root
            .join("hydraui")
            .join("orchestrators")
            .join(pipeline)
    } else {
        cwd.to_path_buf()
    }
}

/// What hydraui types into the orchestrator's terminal after `changes` to `run` (§6): the run,
/// the stage and the result, and what to do next; `None` when nothing calls for it (a pause).
pub fn wake_message(run: &Run, changes: &[Change]) -> Option<String> {
    let id = &run.id;
    let stage = |i: usize| run.pipeline.stages[i].id.as_str();
    let (mut finished, mut needs, mut ended, mut failed, mut passed) =
        (false, None, None, None, None);
    let (mut retrying, mut run_resumed, mut resumed) = (None, false, false);
    for c in changes.iter().filter(|c| &c.run == id) {
        match (&c.what, c.stage) {
            (Decided::RunFinished, _) => finished = true,
            (Decided::RunNeedsYou(why), _) => needs = Some(why),
            (Decided::RunStopped(why), _) => ended = Some(why),
            (Decided::Failed(why), Some(i)) => failed = Some((i, why)),
            (Decided::Passed, Some(i)) => passed = Some(i),
            (Decided::Retrying, Some(i)) => retrying = Some(i),
            (Decided::RunResumed, _) => run_resumed = true,
            (Decided::Resumed, _) => resumed = true,
            _ => {}
        }
    }
    Some(if finished {
        format!(
            "Run {id} finished: every stage passed. Tell the user, then wait for them to ask for \
             the next ticket."
        )
    } else if let Some(why) = ended {
        format!(
            "Run {id} was stopped: {why}. That ticket is over: tell the user, and start the next \
             ticket only when they ask."
        )
    } else if let Some(why) = needs {
        format!(
            "Run {id} needs a person: {why}. Tell the user why and wait: nothing you can call \
             moves it on."
        )
    } else if let Some((i, why)) = failed {
        format!(
            "Run {id}: stage {} failed ({why}), attempt {} of {MAX_ATTEMPTS}. Call run_status, \
             then start it again with stage_start.",
            stage(i),
            run.stages[i].attempts.len()
        )
    } else if let Some(i) = passed {
        let score = run.stages[i].rating().map_or(0, |r| r.score);
        format!(
            "Run {id}: stage {} passed ({score}, bar {}). Call run_status, then start the next \
             stage.",
            stage(i),
            run.pipeline.bar_for(i)
        )
    } else if let Some(i) = retrying {
        format!(
            "Run {id}: a person re-ran stage {} with a note. Start it again with stage_start; its \
             session gets the note.",
            stage(i)
        )
    } else if run_resumed {
        format!("Run {id}: a person resumed it. Call run_status, then start the next stage.")
    } else if resumed {
        format!("Run {id}: the user resumed the run. Call run_status and carry on.")
    } else {
        return None;
    })
}

/// Keys a run's escalation toast, so a newer one for the same run replaces it.
struct RunAlert;

/// What the escalation toast says about a run that needs a person (the design's "Pipeline
/// stopped · needs you", spec §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    /// `<ticket> stopped at <stage>`.
    pub title: String,
    /// The score against the bar, why it was blocked, or the failure or usage reason.
    pub body: String,
    /// The first reason against, when the stage gave one.
    pub reason: Option<String>,
}

/// The toast for `run`, when it needs a person; `None` otherwise.
pub fn escalation(run: &Run) -> Option<Escalation> {
    let RunState::NeedsYou(why) = &run.state else {
        return None;
    };
    let i = run
        .stages
        .iter()
        .position(|s| s.status != StageStatus::Passed)?;
    let stage = &run.stages[i];
    let rating = (stage.status == StageStatus::Stopped)
        .then(|| stage.rating())
        .flatten();
    let body = match rating {
        Some(r) if r.outcome == Outcome::Blocked => format!("Blocked: {}", r.summary),
        Some(r) => format!("Rated {}%, bar is {}%.", r.score, run.pipeline.bar_for(i)),
        None => crate::pipeline_view::sentence(why),
    };
    let reason = rating.and_then(|r| {
        r.reasons
            .iter()
            .find(|x| x.side == Side::Against)
            .map(|x| x.text.clone())
    });
    Some(Escalation {
        title: format!("{} stopped at {}", run.ticket, run.pipeline.stages[i].title),
        body,
        reason,
    })
}

/// Whether a waiting wake-up may be typed into `orchestrator` now (§6): ready for a prompt, and
/// its run not paused (§5.4).
pub fn wake_now(orchestrator: &Session, paused: bool) -> bool {
    !paused && ready_for_prompt(orchestrator)
}

/// Writes `changes` to their runs' logs and tells the app (its views, and the orchestrator).
pub(super) fn tell(changes: Vec<Change>) {
    let Some(server) = server::get() else {
        return;
    };
    if changes.is_empty() {
        return;
    }
    {
        let reg = server::lock(server.registry());
        if let Some(root) = &reg.hydra_root {
            crate::run_log::record(root, &reg.runs, &changes);
        }
    }
    server.broadcast(McpEvent::RunChanged(changes));
}

impl Workspace {
    /// The gate let stage `s` start: find its orchestrator's environment and folder, make the
    /// run's worktree when the stage wants one and the run has none (off the UI thread), then
    /// open the stage's terminal.
    pub(super) fn start_stage(
        &mut self,
        s: StartStage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let orchestrator = self.panes.values().find_map(|v| {
            let v = v.read(cx);
            (v.mcp_token() == Some(s.orchestrator.as_str()))
                .then(|| (v.request.env.clone(), v.cwd().clone()))
        });
        let Some((env, cwd)) = orchestrator else {
            return stage_failed(&s, "its orchestrator's terminal is gone".into());
        };
        // The skill set in the pipeline must be one its session can use (§4): checked where
        // the session will look, before anything opens.
        if let Some(skill) = s.skill.clone() {
            let folder = s.folder.as_deref().map_or(cwd.clone(), |f| {
                hydra_core::paths::expand_tilde(f, &self.hydra.user_home)
            });
            let claude = hydra_core::name::EnvName::parse(&env)
                .ok()
                .map(|n| self.hydra.paths.state_dir(&n).join("claude"));
            if !crate::skills_list::is_installed(&skill, claude.as_deref(), &folder) {
                return stage_failed(
                    &s,
                    format!(
                        "its skill {skill} isn't installed for a session in {}: add it there or \
                         to ~/.claude/skills, or change the stage's skill",
                        folder.display()
                    ),
                );
            }
        }
        // A stage with a folder starts there: the project whose skills it needs.
        if let Some(folder) = s.folder.clone() {
            let path = hydra_core::paths::expand_tilde(&folder, &self.hydra.user_home);
            if !path.is_dir() {
                return stage_failed(&s, format!("its folder {} doesn't exist", path.display()));
            }
            return self.open_stage(s, &env, path, window, cx);
        }
        if !s.worktree {
            return self.open_stage(s, &env, cwd, window, cx);
        }
        let made = server::get().and_then(|sv| {
            server::lock(sv.registry())
                .runs
                .run(&s.run)?
                .worktree
                .clone()
        });
        if let Some(path) = made {
            return self.open_stage(s, &env, path, window, cx);
        }
        let Some(repo) = crate::tasks_project::git_root(&cwd) else {
            return stage_failed(
                &s,
                format!(
                    "{} isn't in a git repository, so it has no worktree",
                    cwd.display()
                ),
            );
        };
        let run = s.run.clone();
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_spawn(async move { worktrees::create(&repo, &run) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| match made {
                Ok(wt) => {
                    if let Some(sv) = server::get() {
                        server::lock(sv.registry())
                            .runs
                            .set_worktree(&s.run, wt.path.clone());
                    }
                    ws.open_stage(s, &env, wt.path, window, cx);
                }
                Err(e) => stage_failed(&s, format!("couldn't make its worktree: {e}")),
            });
        })
        .detach();
    }

    /// Opens stage `s`'s terminal in `cwd` running Claude with its prompt, names it and links its
    /// token to the run.
    fn open_stage(
        &mut self,
        s: StartStage,
        env: &str,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shell = self.shell();
        let mut request = PaneRequest::new(env, cwd, shell);
        request.command = Some(command_line(
            shell,
            &["claude".to_string(), s.prompt.clone()],
        ));
        let Some(id) = self.open_terminal_beside(env, request, window, cx) else {
            return stage_failed(&s, format!("environment {env} isn't open"));
        };
        let Some(view) = self.panes.get(&id).cloned() else {
            return stage_failed(&s, "its terminal didn't open".into());
        };
        let name = s.name.clone();
        view.update(cx, |v, cx| v.set_name(Some(name.clone()), cx));
        let token = view.read(cx).mcp_token().map(str::to_string);
        let taker = view.read(cx).taker_name().unwrap_or(name);
        match (token, server::get()) {
            (Some(token), Some(sv)) => {
                server::lock(sv.registry())
                    .runs
                    .link(&s.run, s.stage, &token, &taker);
                crate::action_log::write(format!("run {}: {taker} opened", s.run));
                // An open Pipeline tab follows the run to the new session (the design).
                let orch = self
                    .panes
                    .iter()
                    .find(|(_, v)| v.read(cx).mcp_token() == Some(s.orchestrator.as_str()))
                    .map(|(id, _)| *id);
                if let Some(tab) = orch.and_then(|o| self.pipeline_tabs.get_mut(&o)) {
                    tab.selected = None;
                    tab.stepped_in = false;
                    tab.confirm_stop = false;
                }
                cx.notify();
            }
            _ => stage_failed(&s, "its terminal has no MCP token".into()),
        }
    }

    /// Runs changed: each one's orchestrator gets its wake-up, typed now if it can take it, else
    /// once it can (§6).
    pub(super) fn runs_changed(
        &mut self,
        changes: Vec<Change>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut wakes = Vec::new();
        if let Some(sv) = server::get() {
            let reg = server::lock(sv.registry());
            let mut ids: Vec<&str> = changes.iter().map(|c| c.run.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            for run in ids.into_iter().filter_map(|id| reg.runs.run(id)) {
                if let Some(message) = wake_message(run, &changes) {
                    wakes.push((run.orchestrator.clone(), run.id.clone(), message));
                }
            }
        }
        for (token, run, message) in wakes {
            let pane = self
                .panes
                .iter()
                .find(|(_, v)| v.read(cx).mcp_token() == Some(token.as_str()))
                .map(|(id, _)| *id);
            if let Some(pane) = pane {
                self.pending_wakes.insert(pane, (run, message));
            }
        }
        self.send_wakes(window, cx);
        let mut needs_you: Vec<&str> = changes
            .iter()
            .filter(|c| matches!(c.what, Decided::RunNeedsYou(_)))
            .map(|c| c.run.as_str())
            .collect();
        needs_you.sort_unstable();
        needs_you.dedup();
        for run in needs_you {
            self.escalate(run, window, cx);
        }
        // A run that ended lets its worktree go when nothing in it is uncommitted (spec §8).
        let ended: Vec<PathBuf> = server::get()
            .map(|sv| {
                let reg = server::lock(sv.registry());
                changes
                    .iter()
                    .filter(|c| matches!(c.what, Decided::RunFinished | Decided::RunStopped(_)))
                    .filter_map(|c| reg.runs.run(&c.run)?.worktree.clone())
                    .collect()
            })
            .unwrap_or_default();
        for path in ended {
            self.remove_run_worktree(path, window, cx);
        }
        cx.notify();
    }

    /// Removes an ended run's worktree off the UI thread; one with uncommitted work is kept and
    /// the user is told where it is.
    fn remove_run_worktree(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |_, cx| {
            let target = path.clone();
            let removed = cx
                .background_spawn(async move { worktrees::remove_if_clean(&target) })
                .await;
            let shown = path.display().to_string();
            match removed {
                Ok(true) => crate::action_log::write(format!("removed run worktree {shown}")),
                Ok(false) => {
                    let _ = cx.update(|window, cx| {
                        window.push_notification(
                            Notification::warning(format!(
                                "{shown} has uncommitted work, so it stays."
                            ))
                            .title("Worktree kept"),
                            cx,
                        );
                    });
                }
                Err(e) => crate::action_log::write(format!("run worktree {shown}: {e}")),
            }
        })
        .detach();
    }

    /// Run `run_id` needs a person (§5.3): its orchestrator's terminal is badged, the window
    /// flashes when in the background, and the design's escalation toast stays until closed.
    /// **Open ticket** opens the Pipeline tab at the stopped session.
    fn escalate(&mut self, run_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let found = server::get().and_then(|sv| {
            let reg = server::lock(sv.registry());
            let run = reg.runs.run(run_id)?;
            Some((escalation(run)?, run.orchestrator.clone()))
        });
        let Some((e, orchestrator)) = found else {
            return;
        };
        let pane_of = |token: &str| {
            self.panes
                .iter()
                .find(|(_, v)| v.read(cx).mcp_token() == Some(token))
                .map(|(id, _)| *id)
        };
        let Some(orch) = pane_of(&orchestrator) else {
            return;
        };
        crate::action_log::write(format!("run {run_id}: needs you: {} · {}", e.title, e.body));
        self.attention.insert(orch);
        if !window.is_window_active() {
            crate::claim::flash(crate::claim::hwnd_of(window));
        }
        let ws = cx.entity();
        let note = Notification::new()
            .id1::<RunAlert>(SharedString::from(format!("run-{run_id}")))
            .autohide(false)
            .w(px(360.))
            .bg(color(theme::TOAST))
            .border_color(color(theme::RED_BORDER))
            .content(move |_, _, cx| {
                let ws = ws.clone();
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::RED))
                            .child("PIPELINE STOPPED · NEEDS YOU"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(theme::TEXT))
                            .child(e.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(e.body.clone()),
                    )
                    .children(e.reason.clone().map(|r| {
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(format!("− {r}"))
                    }))
                    .child(
                        h_flex()
                            .pt_2()
                            .gap_2()
                            .child(
                                Button::new("open-ticket")
                                    .label("Open ticket")
                                    .primary()
                                    .on_click(cx.listener(move |note, _, window, cx| {
                                        note.dismiss(window, cx);
                                        ws.update(cx, |ws, cx| ws.open_pipeline(orch, window, cx));
                                    })),
                            )
                            .child(Button::new("later").label("Later").ghost().on_click(
                                cx.listener(|note, _, window, cx| note.dismiss(window, cx)),
                            )),
                    )
                    .into_any_element()
            });
        window.push_notification(note, cx);
    }

    /// Types each waiting wake-up whose orchestrator can take it now; a closed one's is dropped.
    pub(super) fn send_wakes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_wakes.is_empty() {
            return;
        }
        let sessions = self.fresh_sessions(window, cx);
        let pending: Vec<(PaneId, String, String)> = self
            .pending_wakes
            .iter()
            .map(|(p, (r, m))| (*p, r.clone(), m.clone()))
            .collect();
        for (pane, run, message) in pending {
            let Some(session) = sessions.iter().find(|s| s.pane == pane) else {
                self.pending_wakes.remove(&pane);
                continue;
            };
            let paused = server::get().is_some_and(|sv| {
                server::lock(sv.registry())
                    .runs
                    .run(&run)
                    .is_some_and(|r| r.paused)
            });
            if !wake_now(session, paused) {
                continue;
            }
            let typed = self
                .panes
                .get(&pane)
                .cloned()
                .is_some_and(|v| v.update(cx, |v, cx| v.type_prompt(&message, cx)));
            if typed {
                crate::action_log::write(format!("run {run}: woke the orchestrator: {message}"));
                self.reserved.insert(pane, std::time::Instant::now());
                self.pending_wakes.remove(&pane);
            }
        }
    }

    /// The selected environment's pipeline cards (§7.1): for each terminal that started a run,
    /// its card and the terminals under it (itself first, then the stages' sessions).
    pub(super) fn pipeline_cards(&self, cx: &gpui::App) -> Vec<(PaneId, Card, Vec<PaneId>)> {
        let (Some(tab), Some(sv)) = (self.envs.active(), server::get()) else {
            return Vec::new();
        };
        let token_of = |id: &PaneId| {
            self.panes
                .get(id)
                .and_then(|v| v.read(cx).mcp_token().map(str::to_string))
        };
        let tokens: Vec<(PaneId, String)> = tab
            .terminals
            .iter()
            .filter_map(|id| Some((*id, token_of(id)?)))
            .collect();
        let reg = server::lock(sv.registry());
        let root = self.hydra.paths.root();
        tokens
            .iter()
            .filter_map(|(id, token)| {
                let Some(run) = reg.runs.current(token) else {
                    // Started from the launcher, no ticket yet: the card of its pipeline.
                    let file = self.orchestrators.get(id)?;
                    let pipeline = crate::pipeline::load(root, file).ok()?;
                    return Some((*id, ready_card(&pipeline), vec![*id]));
                };
                let panes = card_tokens(run)
                    .iter()
                    .filter_map(|t| tokens.iter().find(|(_, x)| x == t).map(|(p, _)| *p))
                    .collect();
                Some((*id, card(run), panes))
            })
            .collect()
    }

    /// The selected environment's orchestrator and its run, for the title and status bars:
    /// the first card's whose run is still open, else the first card's.
    pub(super) fn shown_run(&self, cx: &gpui::App) -> Option<(PaneId, Run)> {
        let sv = server::get()?;
        let runs: Vec<(PaneId, Run)> = {
            let tokens: Vec<(PaneId, String)> = self
                .pipeline_cards(cx)
                .into_iter()
                .filter_map(|(id, _, _)| {
                    Some((id, self.panes.get(&id)?.read(cx).mcp_token()?.to_string()))
                })
                .collect();
            let reg = server::lock(sv.registry());
            tokens
                .into_iter()
                .filter_map(|(id, t)| Some((id, reg.runs.current(&t)?.clone())))
                .collect()
        };
        let open = runs.iter().position(|(_, r)| r.state.open()).unwrap_or(0);
        runs.into_iter().nth(open)
    }

    /// **Pause orchestrator** / **Resume orchestrator** (§5.4): no stage starts and no wake-up
    /// is typed while paused; resuming tells the orchestrator.
    pub(super) fn set_paused(&mut self, run: &str, paused: bool) {
        let changes = server::get().map(|sv| server::lock(sv.registry()).runs.pause(run, paused));
        match changes {
            Some(Ok(changes)) => {
                crate::action_log::write(format!(
                    "run {run}: {} by the user",
                    if paused { "paused" } else { "resumed" }
                ));
                tell(changes);
            }
            Some(Err(e)) => crate::action_log::write(format!("run {run}: {e}")),
            None => {}
        }
    }

    /// The design's pipeline card: the diamond and the pipeline's name, the ticket and stage
    /// with the state pill, and one segment per stage. Clicking it shows the orchestrator (the
    /// Pipeline view takes its place in plan Task 10).
    pub(super) fn render_pipeline_card(
        &self,
        orchestrator: PaneId,
        c: &Card,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (pill_fg, pill_bg) = match c.state {
            CardState::Ready => (theme::TEXT_SECONDARY, theme::RAISED),
            CardState::Working => (theme::TEAL, theme::TEAL_TINT),
            CardState::NeedsYou => (theme::RED, theme::RED_TINT),
            CardState::Paused => (theme::AMBER, theme::AMBER_TINT),
            CardState::Done => (theme::GREEN, theme::GREEN_TINT),
            CardState::Stopped => (theme::TEXT_SECONDARY, theme::RAISED),
        };
        let segments = c.segments.iter().map(|s| {
            let fill = match s {
                Segment::Passed => color(theme::TEAL),
                Segment::Running => color(theme::TEAL).opacity(0.53),
                Segment::Stopped => color(theme::RED),
                Segment::Pending => color(theme::BORDER),
            };
            div().flex_1().h(px(3.)).rounded_full().bg(fill)
        });
        v_flex()
            .id(SharedString::from(format!("pipeline-card-{orchestrator}")))
            .mx_1()
            .my_1()
            .p(px(10.))
            .gap(px(6.))
            .rounded(px(8.))
            .border_1()
            .bg(color(if active {
                theme::TEAL_TINT
            } else {
                theme::SURFACE
            }))
            .border_color(color(if active {
                theme::TEAL_BORDER
            } else {
                theme::BORDER_SUBTLE
            }))
            .cursor_pointer()
            .on_click(
                cx.listener(move |ws, _, window, cx| ws.open_pipeline(orchestrator, window, cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(color(theme::TEAL)).child("◆"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(color(theme::TEXT))
                            .child(c.name.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(c.line.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .text_xs()
                            .bg(color(pill_bg))
                            .text_color(color(pill_fg))
                            .child(c.state.word()),
                    ),
            )
            .child(h_flex().gap(px(3.)).children(segments))
            .into_any_element()
    }

    /// The design's seven-day allowance meter at the foot of the list: what is left of the
    /// environment's Claude week, red below the floor, with a tick at the floor. `None` before
    /// any session in `env` reported its usage.
    pub(super) fn render_allowance_meter(&self, env: &str) -> Option<AnyElement> {
        let used = self
            .usage
            .get(env)
            .and_then(|u| u.seven_day)
            .map(|l| l.used);
        let floor = server::get().map_or(crate::runs::MIN_WEEK_LEFT, |sv| {
            server::lock(sv.registry()).min_week_left
        });
        let m = meter(used, floor)?;
        let fill = color(if m.low { theme::RED } else { theme::GREEN });
        Some(
            v_flex()
                .mx_2()
                .mt_3()
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .text_color(color(theme::TEXT_MUTED))
                        .child("Claude 7-day left")
                        .child(format!("{:.0}%", m.left * 100.0)),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(4.))
                        .rounded_full()
                        .bg(color(theme::BORDER_SUBTLE))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .h_full()
                                .w(relative(m.left))
                                .rounded_full()
                                .bg(fill),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left(relative(f32::from(floor) / 100.0))
                                .w(px(1.))
                                .h_full()
                                .bg(color(theme::TEXT_SECONDARY)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT_MUTED))
                        .child(format!("Pipeline stops below {floor}%")),
                )
                .into_any_element(),
        )
    }

    /// The shell or Claude session behind `token` ended: when it was running a stage, the stage
    /// fails (§3.4). An orchestrator's run stops only when its terminal closes.
    pub(super) fn stage_session_gone(&mut self, token: &str) {
        let changes = server::get().map_or_else(Vec::new, |sv| {
            let mut reg = server::lock(sv.registry());
            match reg.runs.role(token) {
                Role::Stage(..) => reg.runs.session_gone(token),
                _ => Vec::new(),
            }
        });
        tell(changes);
    }
}

/// Stage `s` couldn't open: it fails, as a session that ended without reporting does.
fn stage_failed(s: &StartStage, why: String) {
    crate::action_log::write(format!(
        "run {}: stage {} didn't open: {why}",
        s.run, s.title
    ));
    let changes = server::get().map_or_else(Vec::new, |sv| {
        server::lock(sv.registry())
            .runs
            .start_failed(&s.run, s.stage, &why)
    });
    tell(changes);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runs::{Choice, Runs};

    fn runs_of(stages: &[&str]) -> (Runs, String) {
        let mut t = String::from("name = \"t\"\n");
        for s in stages {
            t += &format!("[[stage]]\nid = \"{s}\"\ntitle = \"{s}\"\n");
        }
        let mut runs = Runs::default();
        let p = crate::pipeline::parse(&t).unwrap();
        let id = runs.start_run("orch", p, "INTH-1", "x").unwrap().id.clone();
        (runs, id)
    }

    fn start(runs: &mut Runs, id: &str, stage: &str, tok: &str) {
        let s = runs.begin("orch", id, stage, "", 30).unwrap();
        runs.link(id, s.stage, tok, tok);
    }

    fn rated(score: u8) -> crate::rating::Rating {
        crate::rating::from_args(&json!({"outcome": "done", "score": score, "summary": "s",
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "against", "text": "Not tried on mobile."}]}))
        .unwrap()
    }

    fn wake(runs: &Runs, id: &str, changes: &[Change]) -> String {
        wake_message(runs.run(id).unwrap(), changes).unwrap()
    }

    #[test]
    fn wake_messages_name_run_stage_and_result() {
        let (mut runs, id) = runs_of(&["a", "b"]);
        start(&mut runs, &id, "a", "t1");
        let passed = runs.report("t1", rated(86)).unwrap();
        let m = wake(&runs, &id, &passed);
        assert!(
            m.starts_with("Run INTH-1-1: stage a passed (86, bar 80)."),
            "{m}"
        );
        assert!(m.contains("run_status") && m.contains("next stage"), "{m}");

        start(&mut runs, &id, "b", "t2");
        let failed = runs.session_gone("t2");
        let m = wake(&runs, &id, &failed);
        assert!(
            m.contains("stage b failed") && m.contains("attempt 1 of 3"),
            "{m}"
        );
        assert!(m.contains("start it again"), "{m}");

        start(&mut runs, &id, "b", "t3");
        let stopped = runs.report("t3", rated(40)).unwrap();
        let m = wake(&runs, &id, &stopped);
        assert!(
            m.contains("needs a person") && m.contains("40, bar 80"),
            "{m}"
        );
        assert!(m.contains("Tell the user why and wait"), "{m}");

        let resumed = runs
            .person(
                &id,
                Choice::Retry {
                    note: "Use staging.".into(),
                },
            )
            .unwrap();
        let m = wake(&runs, &id, &resumed);
        assert!(
            m.contains("re-ran stage b") && m.contains("stage_start"),
            "{m}"
        );

        start(&mut runs, &id, "b", "t4");
        let finished = runs.report("t4", rated(95)).unwrap();
        let m = wake(&runs, &id, &finished);
        assert!(m.contains("finished") && m.contains("Tell the user"), "{m}");
        assert!(!m.contains("next stage"), "{m}");
    }

    #[test]
    fn some_changes_wake_nobody() {
        let (mut runs, id) = runs_of(&["a"]);
        let paused = runs.pause(&id, true).unwrap();
        assert_eq!(wake_message(runs.run(&id).unwrap(), &paused), None);
        let resumed = runs.pause(&id, false).unwrap();
        assert!(wake(&runs, &id, &resumed).contains("resumed"));
        let stopped = runs.person(&id, Choice::Stop).unwrap();
        assert!(wake(&runs, &id, &stopped).contains("stopped by you"));
    }

    fn reasoned(score: u8, outcome: &str) -> crate::rating::Rating {
        crate::rating::from_args(&json!({"outcome": outcome, "score": score,
            "summary": "No account to test with.",
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "for", "text": "Steps are clear."},
                        {"side": "against", "text": "The dialog may paginate on its own."}]}))
        .unwrap()
    }

    #[test]
    fn escalations_name_the_ticket_stage_score_bar_and_reason() {
        // A low score.
        let (mut runs, id) = runs_of(&["replicate", "develop"]);
        assert_eq!(
            escalation(runs.run(&id).unwrap()),
            None,
            "running: nothing to say"
        );
        start(&mut runs, &id, "replicate", "t1");
        runs.report("t1", reasoned(64, "done")).unwrap();
        let e = escalation(runs.run(&id).unwrap()).unwrap();
        assert_eq!(e.title, "INTH-1 stopped at replicate");
        assert_eq!(e.body, "Rated 64%, bar is 80%.");
        assert_eq!(
            e.reason.as_deref(),
            Some("The dialog may paginate on its own.")
        );

        // Blocked, whatever the score.
        let (mut runs, id) = runs_of(&["replicate"]);
        start(&mut runs, &id, "replicate", "t1");
        runs.report("t1", reasoned(95, "blocked")).unwrap();
        let e = escalation(runs.run(&id).unwrap()).unwrap();
        assert_eq!(e.body, "Blocked: No account to test with.");

        // Failed three times.
        let (mut runs, id) = runs_of(&["develop"]);
        for n in 1..=3 {
            start(&mut runs, &id, "develop", &format!("t{n}"));
            runs.session_gone(&format!("t{n}"));
        }
        let e = escalation(runs.run(&id).unwrap()).unwrap();
        assert_eq!(e.title, "INTH-1 stopped at develop");
        assert!(e.body.contains("failed 3 times"), "{}", e.body);
        assert_eq!(e.reason, None);

        // The weekly limit, before the stage started.
        let (mut runs, id) = runs_of(&["develop"]);
        runs.usage("orch", 80.0);
        let _ = runs.begin("orch", &id, "develop", "", 30);
        let e = escalation(runs.run(&id).unwrap()).unwrap();
        assert!(e.body.contains("20%"), "{}", e.body);
    }

    #[test]
    fn a_paused_run_wakes_nobody() {
        let ready = crate::pickup::Session {
            pane: 1,
            project_root: None,
            cwd: "C:/w".into(),
            claude: Some(crate::mcp::SessionState::Idle),
            focused: false,
            reserved: false,
            draft: false,
            finished: true,
        };
        assert!(wake_now(&ready, false));
        assert!(!wake_now(&ready, true));
    }

    #[test]
    fn an_orchestrator_launched_from_home_gets_its_own_folder() {
        let home = std::path::Path::new("/Users/me");
        let root = std::path::Path::new("/Users/me/.hydra");
        assert_eq!(
            orchestrator_folder(home, home, root, "inth"),
            root.join("hydraui").join("orchestrators").join("inth")
        );
        let project = std::path::Path::new("/Users/me/Projects/app");
        assert_eq!(orchestrator_folder(project, home, root, "inth"), project);
    }

    #[test]
    fn the_orchestrator_prompt_names_the_pipeline_and_its_role() {
        let p = orchestrator_prompt("ticket");
        for says in [
            "\"ticket\"",
            "run_start",
            "stage_start",
            "never do a stage's work",
            "wait",
        ] {
            assert!(p.contains(says), "{says}: {p}");
        }
    }
}
