//! Stage terminals (orchestrator spec §3.2): the gate let a stage start, so hydraui opens its
//! terminal beside the user, in the run's worktree or the orchestrator's folder, and links it.
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{AnimationExt, AnyElement, Context, SharedString, Window, div, px, relative};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{Sizable, h_flex, v_flex};

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

/// The first prompt of a terminal started as an orchestrator (spec §3.0, §9). A pipeline
/// with a `[linear]` table may get its tickets from hydraui, whichever mode its card is in, so
/// its orchestrator waits for one rather than asking.
pub fn orchestrator_prompt(pipeline: &str, from_linear: bool) -> String {
    let ticket = if from_linear {
        "Tickets reach you from hydraui, which picks them (from Linear, or pull requests posted \
         in Slack) and sends you one, or from me. Don't ask me for one: say you're ready and \
         wait."
    } else {
        "Ask me which ticket to run."
    };
    format!(
        "You are the orchestrator for hydraui's pipeline \"{pipeline}\". You coordinate one \
         ticket at a time through its stages and never do a stage's work yourself. {ticket} \
         Then call run_start with pipeline \"{pipeline}\", the ticket and its title, and start \
         each stage in order with stage_start. After each one, wait: hydraui tells you when it \
         reports. When a run needs a person, tell me why and wait."
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

/// What a reopened orchestrator is told (§13): hydraui restarted, which run it has back, and to
/// carry on from the run's status.
pub fn resume_prompt(run: &str, ticket: &str) -> String {
    format!(
        "hydraui restarted, and you're picking run {run} ({ticket}) back up as its \
         orchestrator. Call run_status to see where it is, then carry on as before: start the \
         next stage when one is due, wait while a stage runs (hydraui tells you when it \
         reports), and when the run needs a person, tell me why and wait."
    )
}

/// What a stage's own session is told when a person sends the run back to it: their note, that
/// a later stage found a problem, the branch rule, and to report again.
pub fn reopen_prompt(run: &Run, stage: usize, note: &str) -> String {
    let title = &run.pipeline.stages[stage].title;
    let branch = run
        .pipeline
        .branch_for(&run.ticket)
        .map_or(String::new(), |b| {
            format!(" Every branch must be named exactly {b}, in each repository.")
        });
    format!(
        "{note}\n\n---\nhydraui: a person sent run {} ({}) back to this stage, {title}, after a \
         later stage found a problem. Fix it as the note says, then call stage_report again: a \
         new report replaces your last.{branch}",
        run.id, run.ticket
    )
}

/// What a reopened stage session is told (§13).
pub const STAGE_RESUME_PROMPT: &str = "hydraui restarted and reopened this session. Please \
     carry on with your stage where you left off, and call stage_report when it's done.";

/// What hydraui types into the orchestrator's terminal after `changes` to `run` (§6): the run,
/// the stage and the result, and what to do next; `None` when nothing calls for it (a pause).
pub fn wake_message(run: &Run, changes: &[Change]) -> Option<String> {
    let id = &run.id;
    let stage = |i: usize| run.pipeline.stages[i].id.as_str();
    let (mut finished, mut needs, mut ended, mut failed, mut passed) =
        (false, None, None, None, None);
    let (mut retrying, mut run_resumed, mut resumed) = (None, false, false);
    let (mut reopened, mut rescoring) = (None, None);
    let (mut asked, mut answered) = (None, None);
    for c in changes.iter().filter(|c| &c.run == id) {
        match (&c.what, c.stage) {
            (Decided::RunFinished, _) => finished = true,
            (Decided::RunNeedsYou(why), _) => needs = Some(why),
            (Decided::RunStopped(why), _) => ended = Some(why),
            (Decided::Failed(why), Some(i)) => failed = Some((i, why)),
            (Decided::Passed, Some(i)) => passed = Some(i),
            (Decided::Retrying, Some(i)) => retrying = Some(i),
            (Decided::Reopened, Some(i)) => reopened = Some(i),
            (Decided::Rescoring, Some(i)) => rescoring = Some(i),
            (Decided::RunResumed, _) => run_resumed = true,
            (Decided::Resumed, _) => resumed = true,
            (Decided::Asked(q), Some(i)) => asked = Some((i, q)),
            (Decided::Answered(_), Some(i)) => answered = Some(i),
            _ => {}
        }
    }
    Some(if finished {
        format!(
            "Run {id} finished: every stage passed. Tell the user, then wait for the next \
             ticket, from hydraui or from them."
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
    } else if let Some((i, question)) = asked {
        format!(
            "Run {id}: stage {} asked product a question, and the ticket waits for the answer: \
             {question} Do nothing meanwhile: hydraui tells you when it's answered.",
            stage(i)
        )
    } else if let Some(i) = answered {
        format!(
            "Run {id}: stage {} has its answer and carries on. Wait: hydraui tells you when it \
             reports.",
            stage(i)
        )
    } else if let Some(i) = reopened {
        format!(
            "Run {id}: a person sent it back to stage {}; its own session is reopened beside them \
             with their note. Wait: hydraui tells you when it reports, then start the next stage.",
            stage(i)
        )
    } else if let Some(i) = rescoring {
        format!(
            "Run {id}: a person worked on stage {} in its own session and asked it to score \
             again. Wait: hydraui tells you when it reports.",
            stage(i)
        )
    } else if let Some((i, why)) = failed {
        format!(
            "Run {id}: stage {} failed ({why}), attempt {} of {MAX_ATTEMPTS}. Call run_status, \
             then start it again with stage_start.",
            stage(i),
            run.stages[i].tries()
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

/// Writes `event` to run `run`'s log (§8), noting a failure in hydraui's log: what only the app
/// knows, which a restart needs to pick the run back up (§13).
pub(super) fn note_run(run: &str, event: &str, fields: serde_json::Value) {
    let Some(root) = server::get().and_then(|sv| server::lock(sv.registry()).hydra_root.clone())
    else {
        return;
    };
    if let Err(e) = crate::run_log::append(&crate::run_log::file_for(&root, run), event, fields) {
        crate::action_log::write(format!("run log {run}: {e}"));
    }
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
                    note_run(
                        &s.run,
                        "worktree",
                        serde_json::json!({"path": wt.path.display().to_string()}),
                    );
                    ws.open_stage(s, &env, wt.path, window, cx);
                }
                Err(e) => stage_failed(&s, format!("couldn't make its worktree: {e}")),
            });
        })
        .detach();
    }

    /// **Resume** on interrupted run `idx` (§13): reopens its orchestrator's conversation in a
    /// terminal of its own, moves the run to it, then reopens the stage that was running (or
    /// ends it as if its session had gone when that session wasn't logged).
    pub(super) fn resume_run(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.interrupted.len() {
            return;
        }
        let i = self.interrupted.remove(idx);
        let run_id = i.run.id.clone();
        let shell = self.shell();
        let folder = if i.folder.is_dir() {
            i.folder.clone()
        } else {
            self.env_cwd(&i.env, cx)
        };
        let mut request = PaneRequest::new(&i.env, folder.clone(), shell);
        request.command = Some(command_line(
            shell,
            &[
                "claude".into(),
                "--resume".into(),
                i.claude.clone(),
                resume_prompt(&run_id, &i.run.ticket),
            ],
        ));
        let id = self.open_terminal(&i.env, request, window, cx);
        self.starting_up.insert(id, std::time::Instant::now());
        let Some(view) = self.panes.get(&id).cloned() else {
            return;
        };
        // Numbered like a fresh one when its pipeline runs several at once.
        let n = self
            .orchestrators
            .iter()
            .filter(|(o, f)| {
                **f == i.run.pipeline.file
                    && self.panes.get(o).is_some_and(|v| v.read(cx).env() == i.env)
            })
            .count();
        let name = if i.run.pipeline.runs_at_once > 1 {
            format!("orchestrator {}", n + 1)
        } else {
            "orchestrator".to_string()
        };
        view.update(cx, |v, cx| v.set_name(Some(name), cx));
        self.orchestrators.insert(id, i.run.pipeline.file.clone());
        let (Some(token), Some(sv)) =
            (view.read(cx).mcp_token().map(str::to_string), server::get())
        else {
            window.push_notification(
                Notification::error("Its terminal has no MCP token.").title("Resume"),
                cx,
            );
            return;
        };
        {
            let mut reg = server::lock(sv.registry());
            reg.runs.restore(i.run.clone());
            reg.runs.adopt(&run_id, &token);
        }
        // Its Linear ticket's link and SLA, as logged (phase 2 §5).
        if let Some(t) = i.ticket.clone() {
            self.linear_tickets
                .insert(t.identifier.to_ascii_uppercase(), t);
        }
        note_run(
            &run_id,
            "orchestrator",
            serde_json::json!({"env": i.env, "folder": folder.display().to_string(),
                               "pid": std::process::id()}),
        );
        note_run(&run_id, "resumed after restart", serde_json::json!({}));
        crate::action_log::write(format!("run {run_id}: resumed after a restart"));
        // The stage that was at work when hydraui closed.
        let busy = i.run.stages.iter().position(|s| s.status.busy());
        if let Some(si) = busy {
            let attempt = i.run.stages[si].latest().cloned();
            let label = attempt
                .as_ref()
                .map(|a| a.label.clone())
                .unwrap_or_default();
            match i.stages.get(&label) {
                Some((Some(dir), Some(claude))) if dir.is_dir() => {
                    let mut request = PaneRequest::new(&i.env, dir.clone(), shell);
                    let mut argv = i.run.pipeline.stage_claude();
                    argv.extend([
                        "--resume".into(),
                        claude.clone(),
                        STAGE_RESUME_PROMPT.into(),
                    ]);
                    request.command = Some(command_line(shell, &argv));
                    let name = crate::runs::session_name(
                        si,
                        &i.run.pipeline.stages[si].title,
                        i.run.stages[si].attempts.len().saturating_sub(1),
                    );
                    let opened = self
                        .open_terminal_beside(&i.env, request, window, cx)
                        .and_then(|sid| self.panes.get(&sid).cloned());
                    if let Some(sview) = opened {
                        sview.update(cx, |v, cx| v.set_name(Some(name.clone()), cx));
                        let stoken = sview.read(cx).mcp_token().map(str::to_string);
                        let taker = sview.read(cx).taker_name().unwrap_or(name);
                        if let Some(stoken) = stoken {
                            server::lock(sv.registry())
                                .runs
                                .relink(&run_id, si, &stoken, &taker);
                        }
                    }
                }
                _ => {
                    let changes = {
                        let mut reg = server::lock(sv.registry());
                        let changes = reg.runs.session_gone(&label);
                        if let Some(root) = reg.hydra_root.clone() {
                            crate::run_log::record(&root, &reg.runs, &changes);
                        }
                        changes
                    };
                    self.runs_changed(changes, window, cx);
                }
            }
        }
        cx.notify();
    }

    /// Reopens stage `stage` of `run`'s own conversation after a person sent the run back to it
    /// (Go back): `claude --resume` of its last attempt's session in its folder, with their
    /// note, linked as the new attempt. Without a logged session it fails, and the orchestrator
    /// starts it afresh.
    fn reopen_stage(
        &mut self,
        run: &str,
        stage: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sv) = server::get() else {
            return;
        };
        let (r, root) = {
            let reg = server::lock(sv.registry());
            (reg.runs.run(run).cloned(), reg.hydra_root.clone())
        };
        let (Some(r), Some(root)) = (r, root) else {
            return;
        };
        let s = &r.stages[stage];
        let (Some(now), Some(before)) = (s.latest(), s.attempts.iter().rev().nth(1)) else {
            return;
        };
        let note = now.note.clone().unwrap_or_default();
        let (folder, claude) = crate::run_resume::logged_session(&root, run, &before.label);
        let env = self.panes.values().find_map(|v| {
            let v = v.read(cx);
            (v.mcp_token() == Some(r.orchestrator.as_str())).then(|| v.request.env.clone())
        });
        let fail = |why: &str| {
            let mut reg = server::lock(sv.registry());
            let changes = reg.runs.start_failed(run, stage, why);
            crate::run_log::record(&root, &reg.runs, &changes);
            changes
        };
        let (Some(env), Some(dir), Some(claude)) = (env, folder.filter(|d| d.is_dir()), claude)
        else {
            let changes = fail("its earlier session can't be reopened (not in the run's log)");
            return self.runs_changed(changes, window, cx);
        };
        let shell = self.shell();
        let mut request = PaneRequest::new(&env, dir.clone(), shell);
        let mut argv = r.pipeline.stage_claude();
        argv.extend(["--resume".into(), claude, reopen_prompt(&r, stage, &note)]);
        request.command = Some(command_line(shell, &argv));
        let name =
            crate::runs::session_name(stage, &r.pipeline.stages[stage].title, s.attempts.len() - 1);
        let opened = self
            .open_terminal_beside(&env, request, window, cx)
            .and_then(|id| self.panes.get(&id).cloned());
        let Some(view) = opened else {
            let changes = fail("its terminal didn't open");
            return self.runs_changed(changes, window, cx);
        };
        view.update(cx, |v, cx| v.set_name(Some(name.clone()), cx));
        let token = view.read(cx).mcp_token().map(str::to_string);
        let taker = view.read(cx).taker_name().unwrap_or(name);
        let Some(token) = token else {
            let changes = fail("its terminal has no MCP token");
            return self.runs_changed(changes, window, cx);
        };
        server::lock(sv.registry())
            .runs
            .link(run, stage, &token, &taker);
        note_run(
            run,
            "stage reopened",
            serde_json::json!({"stage": r.pipeline.stages[stage].id, "label": now.label}),
        );
        note_run(
            run,
            "stage folder",
            serde_json::json!({"label": now.label, "folder": dir.display().to_string()}),
        );
        crate::action_log::write(format!("run {run}: reopened {taker} for a person"));
        cx.notify();
    }

    /// **Stop ticket** on interrupted run `idx` (§13): its log ends, and its card goes.
    pub(super) fn stop_interrupted(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.interrupted.len() {
            return;
        }
        let i = self.interrupted.remove(idx);
        note_run(
            &i.run.id,
            "run ended",
            serde_json::json!({"outcome": "stopped", "reason": "hydraui closed while it ran"}),
        );
        crate::action_log::write(format!("run {}: stopped after a restart", i.run.id));
        cx.notify();
    }

    /// Interrupted run `idx`'s card (§13): its pipeline card with an **Interrupted** pill, and
    /// **Resume** and **Stop ticket**.
    pub(super) fn render_interrupted_card(&self, idx: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(i) = self.interrupted.get(idx) else {
            return div().into_any_element();
        };
        let c = card(&i.run);
        let segments = c.segments.iter().map(|s| {
            let fill = match s {
                Segment::Passed => color(theme::TEAL),
                Segment::Running => color(theme::AMBER),
                Segment::Stopped => color(theme::RED),
                Segment::Pending => color(theme::BORDER),
            };
            div().flex_1().h(px(3.)).rounded_full().bg(fill)
        });
        v_flex()
            .id(SharedString::from(format!("interrupted-{}", i.run.id)))
            .mx_1()
            .my_1()
            .p(px(10.))
            .gap(px(6.))
            .rounded(px(10.))
            .border_1()
            .bg(color(theme::SURFACE))
            .border_color(color(theme::AMBER).opacity(0.5))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(color(theme::AMBER)).child("◆"))
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
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(5.))
                            .text_xs()
                            .bg(color(theme::AMBER_TINT))
                            .text_color(color(theme::AMBER))
                            .child("Interrupted"),
                    ),
            )
            .child(h_flex().gap(px(3.)).children(segments))
            .child(
                div()
                    .text_xs()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child("hydraui closed while this ran."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("resume-{}", i.run.id)))
                            .label("Resume")
                            .primary()
                            .small()
                            .on_click(
                                cx.listener(move |ws, _, window, cx| {
                                    ws.resume_run(idx, window, cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("stop-{}", i.run.id)))
                            .label("Stop ticket")
                            .ghost()
                            .small()
                            .on_click(
                                cx.listener(move |ws, _, _, cx| ws.stop_interrupted(idx, cx)),
                            ),
                    ),
            )
            .into_any_element()
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
        note_run(
            &s.run,
            "stage folder",
            serde_json::json!({"label": s.label, "folder": cwd.display().to_string()}),
        );
        let mut request = PaneRequest::new(env, cwd, shell);
        let mut argv = s.claude.clone();
        argv.push(s.prompt.clone());
        request.command = Some(command_line(shell, &argv));
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
        // Linear hears of runs that started or ended (phase 2 §4).
        self.linear_runs_changed(&changes, cx);
        // A person sent a run back to an earlier stage: its own session reopens.
        for c in changes.iter().filter(|c| c.what == Decided::Reopened) {
            if let Some(stage) = c.stage {
                self.reopen_stage(&c.run, stage, window, cx);
            }
        }
        // Where a new run's orchestrator is, and which hydraui drives it (§13).
        let started: Vec<(String, String)> = server::get()
            .map(|sv| {
                let reg = server::lock(sv.registry());
                changes
                    .iter()
                    .filter(|c| matches!(c.what, Decided::RunStarted))
                    .filter_map(|c| {
                        let r = reg.runs.run(&c.run)?;
                        Some((r.id.clone(), r.orchestrator.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (run, token) in started {
            let found = self.panes.values().find_map(|v| {
                let v = v.read(cx);
                (v.mcp_token() == Some(token.as_str()))
                    .then(|| (v.request.env.clone(), v.cwd().display().to_string()))
            });
            if let Some((env, folder)) = found {
                note_run(
                    &run,
                    "orchestrator",
                    serde_json::json!({"env": env, "folder": folder, "pid": std::process::id()}),
                );
            }
        }
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
        self.asked_notify(&changes, window, cx);
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
            // Still on its first turn: what's typed now never reaches it.
            if self
                .starting_up
                .get(&pane)
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(90))
            {
                continue;
            }
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
                crate::action_log::write(format!("run {run}: woke a session: {message}"));
                self.reserved.insert(pane, std::time::Instant::now());
                self.pending_wakes.remove(&pane);
                let tries = self.wake_tries.remove(&pane).unwrap_or(0) + 1;
                self.sent_wakes
                    .insert(pane, (run, message, std::time::Instant::now(), tries));
            }
        }
    }

    /// A wake-up typed 30 seconds ago whose session never started working didn't reach Claude
    /// (a Claude Code prompt was open over the input): it's sent again, up to 3 times, then
    /// the user is told to look at that terminal.
    pub(super) fn recheck_wakes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const TAKE: std::time::Duration = std::time::Duration::from_secs(30);
        let stale: Vec<PaneId> = self
            .sent_wakes
            .iter()
            .filter(|(_, (_, _, at, _))| at.elapsed() >= TAKE)
            .map(|(p, _)| *p)
            .collect();
        for pane in stale {
            let Some((run, message, _, tries)) = self.sent_wakes.remove(&pane) else {
                continue;
            };
            let Some(view) = self.panes.get(&pane) else {
                continue;
            };
            if view.read(cx).claude() != Some(crate::mcp::SessionState::Idle) {
                continue;
            }
            if tries >= 3 {
                let name = self.pane_name(pane, cx);
                crate::action_log::write(format!(
                    "run {run}: {name} didn't take a wake-up after 3 tries"
                ));
                window.push_notification(
                    Notification::warning(format!(
                        "{name} isn't taking messages from hydraui. Look at its terminal: \
                         Claude Code may be showing a question. Answer it, and hydraui carries \
                         on."
                    ))
                    .title(run.clone()),
                    cx,
                );
                continue;
            }
            crate::action_log::write(format!(
                "run {run}: the wake-up didn't take; sending it again"
            ));
            // Back out, with the tries already made counted.
            self.wake_tries.insert(pane, tries);
            self.pending_wakes.insert(pane, (run, message));
        }
        self.send_wakes(window, cx);
    }

    /// The colour of the pipeline terminal `pane` belongs to (its orchestrator or a stage's
    /// session), when it belongs to one.
    pub(super) fn pipeline_tint(&self, pane: PaneId, cx: &gpui::App) -> Option<gpui::Hsla> {
        self.pipeline_cards(cx)
            .into_iter()
            .find(|(_, _, panes)| panes.contains(&pane))
            .map(|(_, c, _)| color(c.accent))
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
                // The colour as the file has it now, so a change in Settings shows at once.
                let mut c = card(run);
                if let Ok(p) = crate::pipeline::load(root, &run.pipeline.file) {
                    c.accent = crate::pipeline_card::accent(&p);
                }
                Some((*id, c, panes))
            })
            .collect()
    }

    /// Every run an orchestrator in this window started that is still open, in any environment.
    pub(super) fn open_runs(&self, cx: &gpui::App) -> Vec<Run> {
        let Some(sv) = server::get() else {
            return Vec::new();
        };
        let reg = server::lock(sv.registry());
        self.envs
            .iter()
            .flat_map(|t| t.terminals.iter())
            .filter_map(|id| {
                let token = self.panes.get(id)?.read(cx).mcp_token()?.to_string();
                reg.runs.current(&token).filter(|r| r.state.open()).cloned()
            })
            .collect()
    }

    /// Asks before closing the window while `warning`'s tickets are open; **Close anyway**
    /// closes it (after any unsaved edits are dealt with).
    pub(super) fn confirm_close_runs(
        &mut self,
        warning: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ws = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let ws = ws.clone();
            dialog
                .title("A ticket is still running")
                .child(div().whitespace_normal().child(warning.clone()))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("keep-open")
                                .label("Keep open")
                                .primary()
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("close-anyway")
                                .label("Close anyway")
                                .danger()
                                .on_click(move |_, w, cx| {
                                    w.close_dialog(cx);
                                    ws.update(cx, |ws, cx| {
                                        let all: Vec<PaneId> = ws.files.keys().copied().collect();
                                        ws.confirm_unsaved(all, w, cx, |_, w, _| {
                                            closing();
                                            w.remove_window()
                                        });
                                    });
                                }),
                        ),
                )
        });
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
    /// `ticket`: show the run's ticket link (one run per card); a pipeline's card over several
    /// orchestrators leaves it to each orchestrator's line.
    pub(super) fn render_pipeline_card(
        &self,
        orchestrator: PaneId,
        c: &Card,
        active: bool,
        ticket: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (pill_fg, pill_bg) = pill_colors(c.state, c.accent);
        let accent = c.accent;
        let segments = c.segments.iter().enumerate().map(|(i, s)| {
            let bar = div().flex_1().h(px(3.)).rounded_full();
            match s {
                // The stage at work breathes.
                Segment::Running => bar
                    .bg(color(accent))
                    .with_animation(
                        SharedString::from(format!("segment-{orchestrator}-{i}")),
                        super::busy::pulse(),
                        move |d, a| d.bg(color(accent).opacity(a)),
                    )
                    .into_any_element(),
                Segment::Passed => bar.bg(color(accent)).into_any_element(),
                Segment::Stopped => bar.bg(color(theme::RED)).into_any_element(),
                Segment::Pending => bar.bg(color(theme::BORDER)).into_any_element(),
            }
        });
        v_flex()
            .id(SharedString::from(format!("pipeline-card-{orchestrator}")))
            .mx_1()
            .my_1()
            .p(px(10.))
            .gap(px(6.))
            .rounded(px(8.))
            .border_1()
            // Grey whatever its colour: the colour is on its frame, pills and bars.
            .bg(color(theme::SURFACE))
            .border_color(if active {
                color(c.accent).opacity(0.45)
            } else {
                color(theme::BORDER_SUBTLE)
            })
            .cursor_pointer()
            .on_click(
                cx.listener(move |ws, _, window, cx| ws.open_pipeline(orchestrator, window, cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(color(c.accent)).child("◆"))
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
                            .bg(pill_bg)
                            .text_color(pill_fg)
                            .child(c.state.word()),
                    ),
            )
            .when(!c.segments.is_empty(), |d| {
                d.child(h_flex().gap(px(3.)).children(segments))
            })
            .when(ticket, |d| {
                d.children(
                    self.current_ticket(orchestrator, cx)
                        .and_then(|(t, prs)| self.render_ticket_link(&t, &prs, c.accent)),
                )
            })
            .into_any_element()
    }

    /// Under a numbered orchestrator of a pipeline with several runs: its ticket and stage, its
    /// state, and its ticket link; clicking it opens that orchestrator's Pipeline tab.
    pub(super) fn render_run_line(
        &self,
        orchestrator: PaneId,
        c: &Card,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (fg, bg) = pill_colors(c.state, c.accent);
        v_flex()
            .id(SharedString::from(format!("run-line-{orchestrator}")))
            .ml(px(26.))
            .mr_1()
            .gap_0p5()
            .cursor_pointer()
            .on_click(
                cx.listener(move |ws, _, window, cx| ws.open_pipeline(orchestrator, window, cx)),
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
                            .py(px(1.))
                            .rounded(px(4.))
                            .text_xs()
                            .bg(bg)
                            .text_color(fg)
                            .child(c.state.word()),
                    ),
            )
            .children(
                self.current_ticket(orchestrator, cx)
                    .and_then(|(t, prs)| self.render_ticket_link(&t, &prs, c.accent)),
            )
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
                // A shell that dies as hydraui closes isn't a stage failing (§13).
                Role::Stage(..) if !reg.closing => reg.runs.session_gone(token),
                _ => Vec::new(),
            }
        });
        tell(changes);
    }
}

/// The window is about to close: from now on its terminals going is hydraui quitting, not
/// their work ending, so open runs stay open for a restart to pick up (§13).
pub(super) fn closing() {
    if let Some(sv) = server::get() {
        server::lock(sv.registry()).closing = true;
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

/// A card state's pill: text and background colour.
/// A card's state pill: in the pipeline's own colour while it works or when it's done; red,
/// amber and grey keep their meaning whatever the colour.
fn pill_colors(state: CardState, accent: u32) -> (gpui::Hsla, gpui::Hsla) {
    let pair = |fg: u32, bg: u32| (color(fg), color(bg));
    match state {
        CardState::Working | CardState::Done => (color(accent), color(accent).opacity(0.16)),
        CardState::Ready | CardState::Stopped => pair(theme::TEXT_SECONDARY, theme::RAISED),
        CardState::NeedsYou => pair(theme::RED, theme::RED_TINT),
        CardState::Asking | CardState::Paused => pair(theme::AMBER, theme::AMBER_TINT),
    }
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
    fn the_orchestrator_waits_while_a_stage_asks_product() {
        let (mut runs, id) = runs_of(&["a", "b"]);
        let s = runs.begin("orch", &id, "a", "", 30).unwrap();
        runs.link(&id, s.stage, "t1", "t1");
        let asked = runs
            .ask("t1", "Remove it?", None, std::time::SystemTime::now())
            .unwrap();
        let m = wake_message(runs.run(&id).unwrap(), &asked).unwrap();
        assert!(
            m.contains("asked product") && m.contains("Remove it?"),
            "{m}"
        );
        assert!(m.contains("Do nothing"), "{m}");
        let answered = runs.answered(&id, "No").unwrap();
        let m = wake_message(runs.run(&id).unwrap(), &answered).unwrap();
        assert!(m.contains("has its answer"), "{m}");
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
    fn a_reopened_session_is_told_hydraui_restarted_and_what_to_do() {
        let p = resume_prompt("INTH-7-2", "INTH-7");
        for says in ["restarted", "INTH-7-2", "INTH-7", "run_status", "wait"] {
            assert!(p.contains(says), "{says}: {p}");
        }
        for says in ["restarted", "carry on", "stage_report"] {
            assert!(STAGE_RESUME_PROMPT.contains(says), "{says}");
        }
    }

    #[test]
    fn going_back_tells_the_orchestrator_and_the_reopened_session() {
        let mut runs = crate::runs::Runs::default();
        let mut p = crate::pipeline::parse(
            "name = \"t\"\nbranch = \"{ticket}\"\n[[stage]]\nid = \"develop\"\ntitle = \"Develop\"\n\
             [[stage]]\nid = \"verify\"\ntitle = \"Verify\"\n",
        )
        .unwrap();
        p.file = "t".into();
        let id = runs
            .start_run("orch", p, "INTH-166", "x")
            .unwrap()
            .id
            .clone();
        let changes = vec![Change {
            run: id.clone(),
            stage: Some(0),
            what: Decided::Reopened,
        }];
        let run = runs.run(&id).unwrap();
        let wake = wake_message(run, &changes).unwrap();
        assert!(
            wake.contains("sent it back to stage develop") && wake.contains("Wait"),
            "{wake}"
        );
        let prompt = reopen_prompt(run, 0, "Rename the branch to INTH-166.");
        assert!(
            prompt.starts_with("Rename the branch to INTH-166."),
            "{prompt}"
        );
        for says in ["Develop", "stage_report again", "named exactly INTH-166"] {
            assert!(prompt.contains(says), "{says}: {prompt}");
        }
    }

    #[test]
    fn a_linear_orchestrator_waits_for_tickets_instead_of_asking() {
        let p = orchestrator_prompt("inth", true);
        assert!(p.contains("Don't ask me for one"), "{p}");
        assert!(!p.contains("Ask me which ticket"), "{p}");
        assert!(orchestrator_prompt("inth", false).contains("Ask me which ticket to run."));
    }

    #[test]
    fn the_orchestrator_prompt_names_the_pipeline_and_its_role() {
        let p = orchestrator_prompt("ticket", false);
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
