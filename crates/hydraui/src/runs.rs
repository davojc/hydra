//! Runs (orchestrator spec §3, §5): one ticket going through a pipeline's stages, owned by
//! one orchestrator terminal, and the gate that decides when a stage may start.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::pipeline::{Pipeline, PromptVars, render};
use crate::rating::{Rating, Side, passes};

/// Attempts a failed stage gets before it waits for a person (§5.2, the user's decision).
pub const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StageStatus {
    /// Not started, or put back by a person's re-run.
    #[default]
    Waiting,
    /// Allowed to start; its terminal is opening (§3.2).
    Starting,
    Running,
    /// Reported done at or above its bar.
    Passed,
    /// Reported below its bar, or blocked.
    Stopped,
    /// Its session ended, or its terminal couldn't open, without a report.
    Failed,
}

impl StageStatus {
    pub fn word(self) -> &'static str {
        match self {
            StageStatus::Waiting => "waiting",
            StageStatus::Starting => "starting",
            StageStatus::Running => "running",
            StageStatus::Passed => "passed",
            StageStatus::Stopped => "stopped",
            StageStatus::Failed => "failed",
        }
    }

    fn busy(self) -> bool {
        matches!(self, StageStatus::Starting | StageStatus::Running)
    }
}

/// One attempt at a stage: one session (§3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// `s-01`, `s-02`, …, numbered across the run.
    pub label: String,
    /// The token of the terminal running it, once it opened.
    pub session: Option<String>,
    pub session_name: Option<String>,
    pub started: SystemTime,
    /// The person's note it was started with (§5.3).
    pub note: Option<String>,
    pub rating: Option<Rating>,
}

/// One stage of a run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StageRun {
    pub status: StageStatus,
    pub attempts: Vec<Attempt>,
    /// A person's note for the next attempt.
    pub note: Option<String>,
}

impl StageRun {
    pub fn latest(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    /// The latest attempt's rating.
    pub fn rating(&self) -> Option<&Rating> {
        self.latest().and_then(|a| a.rating.as_ref())
    }
}

/// §3.5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// Waiting for a person, and why.
    NeedsYou(String),
    /// The last stage passed.
    Finished,
    /// Ended by a person or by its orchestrator closing, and why. Final.
    Stopped(String),
}

impl RunState {
    /// Still the orchestrator's ticket: running or waiting for a person.
    pub fn open(&self) -> bool {
        matches!(self, RunState::Running | RunState::NeedsYou(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// `<ticket>-<n>`.
    pub id: String,
    pub ticket: String,
    pub summary: String,
    /// Its stage list as read at the start; bars follow the file (§2).
    pub pipeline: Pipeline,
    /// The token of the terminal that started it.
    pub orchestrator: String,
    pub stages: Vec<StageRun>,
    pub state: RunState,
    pub started: SystemTime,
    /// A person paused it (§5.4).
    pub paused: bool,
    /// Attempts started so far, for their labels.
    pub sessions: u32,
    /// Made for its first worktree stage (§3.2).
    pub worktree: Option<PathBuf>,
    /// The latest seven-day Claude usage reported by one of its sessions, in percent.
    pub week_used: Option<f64>,
}

/// What a terminal is to the runs (§3.3).
pub enum Role<'a> {
    Orchestrator(&'a Run),
    Stage(&'a Run, usize),
    Other,
}

/// What the app needs to open a stage's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartStage {
    pub run: String,
    pub stage: usize,
    /// The attempt's label, `s-NN`.
    pub label: String,
    pub orchestrator: String,
    pub worktree: bool,
    pub title: String,
    pub prompt: String,
}

/// What happened to a run or one of its stages, for the app to show and tell the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    Passed,
    Stopped(String),
    Failed(String),
    /// A person re-runs it.
    Retrying,
    Paused,
    Resumed,
    RunResumed,
    RunFinished,
    RunNeedsYou(String),
    RunStopped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub run: String,
    pub stage: Option<usize>,
    pub what: Decided,
}

/// Why a stage may not start, and what that changed (a usage refusal needs a person, §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub why: String,
    pub changes: Vec<Change>,
}

/// A person's choice for a run (§5.3). There is no choice that lets a stage through.
pub enum Choice {
    /// Re-run the stage that waits, with a note for its next session.
    Retry { note: String },
    /// Stop the ticket.
    Stop,
}

/// Seconds since the epoch, for `run_status`.
fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Every run this hydraui started. Lives behind the MCP registry's lock, so a gate check and the
/// change it allows are one step.
#[derive(Default)]
pub struct Runs {
    runs: Vec<Run>,
}

impl Runs {
    pub fn run(&self, id: &str) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    pub fn all(&self) -> impl Iterator<Item = &Run> {
        self.runs.iter()
    }

    /// The latest run `orchestrator` started.
    pub fn current(&self, orchestrator: &str) -> Option<&Run> {
        self.runs
            .iter()
            .rev()
            .find(|r| r.orchestrator == orchestrator)
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.runs.iter().position(|r| r.id == id)
    }

    /// The run and stage whose latest attempt's session is `token` (§3.3: an earlier attempt's
    /// session has no say).
    fn stage_of(&self, token: &str) -> Option<(usize, usize)> {
        self.runs.iter().enumerate().find_map(|(ri, r)| {
            r.stages
                .iter()
                .position(|s| s.latest().and_then(|a| a.session.as_deref()) == Some(token))
                .map(|si| (ri, si))
        })
    }

    pub fn role(&self, token: &str) -> Role<'_> {
        if let Some((ri, si)) = self.stage_of(token) {
            return Role::Stage(&self.runs[ri], si);
        }
        match self.current(token) {
            Some(r) => Role::Orchestrator(r),
            None => Role::Other,
        }
    }

    /// Starts a run of `pipeline` for `ticket`, owned by the terminal `orchestrator`. One open
    /// run per orchestrator (the user's decision: one ticket at a time).
    pub fn start_run(
        &mut self,
        orchestrator: &str,
        pipeline: Pipeline,
        ticket: &str,
        summary: &str,
    ) -> Result<&Run, String> {
        let ticket = ticket.trim();
        if ticket.is_empty() {
            return Err("name the ticket".into());
        }
        if self.stage_of(orchestrator).is_some() {
            return Err("a stage session can't start a run".into());
        }
        if let Some(r) = self
            .runs
            .iter()
            .find(|r| r.orchestrator == orchestrator && r.state.open())
        {
            return Err(format!(
                "run {} isn't finished or stopped: one ticket at a time",
                r.id
            ));
        }
        let n = self.runs.iter().filter(|r| r.ticket == ticket).count() + 1;
        let stages = vec![StageRun::default(); pipeline.stages.len()];
        self.runs.push(Run {
            id: format!("{ticket}-{n}"),
            ticket: ticket.to_string(),
            summary: summary.trim().to_string(),
            pipeline,
            orchestrator: orchestrator.to_string(),
            stages,
            state: RunState::Running,
            started: SystemTime::now(),
            paused: false,
            sessions: 0,
            worktree: None,
            week_used: None,
        });
        Ok(self.runs.last().expect("just pushed"))
    }

    /// The gate (§5.1): the stage's index, or why not and whether that needs a person.
    fn gate(
        &self,
        orchestrator: &str,
        run: &str,
        stage: &str,
        min_week_left: u8,
    ) -> Result<usize, (String, bool)> {
        let no = |why: String| Err((why, false));
        let Some(r) = self.run(run) else {
            return no(format!("no run {run}"));
        };
        if r.orchestrator != orchestrator {
            return no("only the run's orchestrator starts its stages".into());
        }
        match &r.state {
            RunState::Running => {}
            RunState::NeedsYou(why) => return no(format!("run {run} needs a person: {why}")),
            RunState::Finished => return no(format!("run {run} is finished")),
            RunState::Stopped(why) => return no(format!("run {run} is stopped: {why}")),
        }
        if r.paused {
            return no(format!("run {run} is paused by the user: wait to be told"));
        }
        let Some(i) = r.pipeline.stages.iter().position(|s| s.id == stage) else {
            return no(format!("no stage {stage} in pipeline {}", r.pipeline.name));
        };
        if let Some(busy) = r.stages.iter().position(|s| s.status.busy()) {
            return no(format!(
                "stage {} is still running",
                r.pipeline.stages[busy].id
            ));
        }
        if let Some(before) = r.stages[..i]
            .iter()
            .position(|s| s.status != StageStatus::Passed)
        {
            return no(format!(
                "stage {} hasn't passed yet",
                r.pipeline.stages[before].id
            ));
        }
        let s = &r.stages[i];
        match s.status {
            StageStatus::Waiting => {}
            StageStatus::Failed if s.attempts.len() < MAX_ATTEMPTS => {}
            StageStatus::Failed => {
                return no(format!(
                    "stage {stage} failed {MAX_ATTEMPTS} times: a person decides"
                ));
            }
            StageStatus::Stopped => {
                return no(format!("stage {stage} is below its bar: a person decides"));
            }
            _ => return no(format!("stage {stage} is already done")),
        }
        if let Some(used) = r.week_used {
            let left = 100.0 - used;
            if left < f64::from(min_week_left) {
                return Err((
                    format!(
                        "only {left:.0}% of the seven-day Claude limit is left; a stage needs \
                         {min_week_left}%"
                    ),
                    true,
                ));
            }
        }
        Ok(i)
    }

    /// Whether `stage` of `run` may start now (§5.1): its index, or the first rule that fails.
    pub fn may_start(
        &self,
        orchestrator: &str,
        run: &str,
        stage: &str,
        min_week_left: u8,
    ) -> Result<usize, String> {
        self.gate(orchestrator, run, stage, min_week_left)
            .map_err(|(why, _)| why)
    }

    /// Lets `stage` start: a new attempt, the stage `Starting`, and its prompt. Refused with the
    /// gate's reason; a usage refusal also makes the run wait for a person.
    pub fn begin(
        &mut self,
        orchestrator: &str,
        run: &str,
        stage: &str,
        brief: &str,
        min_week_left: u8,
    ) -> Result<StartStage, Refusal> {
        let i = match self.gate(orchestrator, run, stage, min_week_left) {
            Ok(i) => i,
            Err((why, needs_you)) => {
                let changes = match (needs_you, self.index(run)) {
                    (true, Some(ri)) => self.needs_you(ri, why.clone()),
                    _ => Vec::new(),
                };
                return Err(Refusal { why, changes });
            }
        };
        let ri = self.index(run).expect("the gate found it");
        let r = &mut self.runs[ri];
        let handoff = r.stages[..i]
            .iter()
            .rev()
            .find_map(StageRun::rating)
            .map(|rating| rating.handoff.clone())
            .unwrap_or_default();
        r.sessions += 1;
        let label = format!("s-{:02}", r.sessions);
        let s = &mut r.stages[i];
        let note = s.note.take();
        let mut brief = brief.trim().to_string();
        if let Some(note) = &note {
            let previous = rerun_context(s.latest());
            for part in [previous, format!("Note from you: {note}")] {
                if !brief.is_empty() {
                    brief.push_str("\n\n");
                }
                brief.push_str(&part);
            }
        }
        s.status = StageStatus::Starting;
        s.attempts.push(Attempt {
            label: label.clone(),
            session: None,
            session_name: None,
            started: SystemTime::now(),
            note,
            rating: None,
        });
        let spec = &r.pipeline.stages[i];
        let vars = PromptVars {
            ticket: &r.ticket,
            run: &r.id,
            stage: &spec.id,
            handoff: &handoff,
        };
        Ok(StartStage {
            run: r.id.clone(),
            stage: i,
            label,
            orchestrator: r.orchestrator.clone(),
            worktree: spec.worktree,
            title: spec.title.clone(),
            prompt: render(&spec.prompt, &vars, &brief),
        })
    }

    /// The terminal for a starting stage opened: `token` is its latest attempt's session.
    pub fn link(&mut self, run: &str, stage: usize, token: &str, name: &str) {
        if let Some(s) = self
            .index(run)
            .and_then(|ri| self.runs[ri].stages.get_mut(stage))
            .filter(|s| s.status == StageStatus::Starting)
        {
            s.status = StageStatus::Running;
            if let Some(a) = s.attempts.last_mut() {
                a.session = Some(token.to_string());
                a.session_name = Some(name.to_string());
            }
        }
    }

    /// A starting stage's terminal or worktree couldn't be made.
    pub fn start_failed(&mut self, run: &str, stage: usize, why: &str) -> Vec<Change> {
        match self.index(run) {
            Some(ri)
                if self.runs[ri]
                    .stages
                    .get(stage)
                    .is_some_and(|s| s.status == StageStatus::Starting) =>
            {
                self.fail_stage(ri, stage, why)
            }
            _ => Vec::new(),
        }
    }

    fn needs_you(&mut self, ri: usize, why: String) -> Vec<Change> {
        let r = &mut self.runs[ri];
        r.state = RunState::NeedsYou(why.clone());
        vec![Change {
            run: r.id.clone(),
            stage: None,
            what: Decided::RunNeedsYou(why),
        }]
    }

    fn end(&mut self, ri: usize, why: String) -> Vec<Change> {
        let r = &mut self.runs[ri];
        r.state = RunState::Stopped(why.clone());
        vec![Change {
            run: r.id.clone(),
            stage: None,
            what: Decided::RunStopped(why),
        }]
    }

    fn fail_stage(&mut self, ri: usize, si: usize, why: &str) -> Vec<Change> {
        let r = &mut self.runs[ri];
        let s = &mut r.stages[si];
        s.status = StageStatus::Failed;
        let attempts = s.attempts.len();
        let id = r.pipeline.stages[si].id.clone();
        let mut changes = vec![Change {
            run: r.id.clone(),
            stage: Some(si),
            what: Decided::Failed(why.to_string()),
        }];
        if attempts >= MAX_ATTEMPTS {
            changes
                .extend(self.needs_you(ri, format!("stage {id} failed {attempts} times: {why}")));
        }
        changes
    }

    /// `token`'s stage reported (§4). Only its latest attempt's running session reports, once.
    pub fn report(&mut self, token: &str, rating: Rating) -> Result<Vec<Change>, String> {
        let Some((ri, si)) = self.stage_of(token) else {
            return Err("only a running stage's own session reports".into());
        };
        let r = &mut self.runs[ri];
        if r.stages[si].status != StageStatus::Running {
            return Err("this stage has already reported, or isn't running".into());
        }
        let bar = r.pipeline.bar_for(si);
        let id = r.pipeline.stages[si].id.clone();
        let passed = passes(&rating, bar);
        let why = match rating.outcome {
            crate::rating::Outcome::Blocked => format!("stage {id} is blocked: {}", rating.summary),
            crate::rating::Outcome::Done => {
                format!(
                    "stage {id}: {}, bar {bar}. {}",
                    rating.score, rating.summary
                )
            }
        };
        let s = &mut r.stages[si];
        if let Some(a) = s.attempts.last_mut() {
            a.rating = Some(rating);
        }
        let run = r.id.clone();
        if passed {
            s.status = StageStatus::Passed;
            let mut changes = vec![Change {
                run: run.clone(),
                stage: Some(si),
                what: Decided::Passed,
            }];
            if si + 1 == r.stages.len() {
                r.state = RunState::Finished;
                changes.push(Change {
                    run,
                    stage: None,
                    what: Decided::RunFinished,
                });
            }
            Ok(changes)
        } else {
            s.status = StageStatus::Stopped;
            let mut changes = vec![Change {
                run,
                stage: Some(si),
                what: Decided::Stopped(why.clone()),
            }];
            changes.extend(self.needs_you(ri, why));
            Ok(changes)
        }
    }

    /// The terminal holding `token` closed, its shell exited or its Claude session ended: a
    /// stage it was running fails, a run it orchestrated stops for good.
    pub fn session_gone(&mut self, token: &str) -> Vec<Change> {
        let mut changes = Vec::new();
        if let Some((ri, si)) = self.stage_of(token)
            && self.runs[ri].stages[si].status.busy()
        {
            changes.extend(self.fail_stage(ri, si, "its session ended without reporting"));
        }
        let orchestrated: Vec<usize> = (0..self.runs.len())
            .filter(|&ri| self.runs[ri].orchestrator == token && self.runs[ri].state.open())
            .collect();
        for ri in orchestrated {
            changes.extend(self.end(ri, "the orchestrator's terminal closed".into()));
        }
        changes
    }

    /// A session reported its seven-day usage: kept on the open run it belongs to.
    pub fn usage(&mut self, token: &str, week_used: f64) {
        for r in &mut self.runs {
            let ours = r.orchestrator == token
                || r.stages.iter().any(|s| {
                    s.attempts
                        .iter()
                        .any(|a| a.session.as_deref() == Some(token))
                });
            if ours && r.state.open() {
                r.week_used = Some(week_used);
            }
        }
    }

    /// The pipeline file `file` now reads `now` (Pipeline settings saved it, or a stage is about
    /// to report): open runs of it take its bars for the stages they share. Their stage lists
    /// stay as they started (§2).
    pub fn refresh_bars(&mut self, file: &str, now: &Pipeline) {
        for r in self
            .runs
            .iter_mut()
            .filter(|r| r.pipeline.file == file && r.state.open())
        {
            r.pipeline.bar = now.bar;
            for stage in &mut r.pipeline.stages {
                if let Some(same) = now.stages.iter().find(|s| s.id == stage.id) {
                    stage.bar = same.bar;
                }
            }
        }
    }

    /// A person paused or resumed `run` (§5.4).
    pub fn pause(&mut self, run: &str, paused: bool) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let r = &mut self.runs[ri];
        if !r.state.open() {
            return Err(format!("run {run} has ended"));
        }
        if r.paused == paused {
            return Err(format!(
                "run {run} is already {}",
                if paused { "paused" } else { "going" }
            ));
        }
        r.paused = paused;
        Ok(vec![Change {
            run: r.id.clone(),
            stage: None,
            what: if paused {
                Decided::Paused
            } else {
                Decided::Resumed
            },
        }])
    }

    /// A person's choice for run `run` (§5.3).
    pub fn person(&mut self, run: &str, choice: Choice) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let r = &self.runs[ri];
        let waiting = r
            .stages
            .iter()
            .position(|s| s.status != StageStatus::Passed);
        let note = match choice {
            Choice::Stop => {
                if !r.state.open() {
                    return Err(format!("run {run} has already ended"));
                }
                let at = waiting.map_or("the end".to_string(), |si| {
                    r.pipeline.stages[si].title.clone()
                });
                return Ok(self.end(ri, format!("stopped by you at {at}")));
            }
            Choice::Retry { note } => note.trim().to_string(),
        };
        if !matches!(r.state, RunState::NeedsYou(_)) {
            return Err(format!("run {run} isn't waiting for you"));
        }
        if note.is_empty() {
            return Err("a re-run needs a note for its next session".into());
        }
        let Some(si) = waiting else {
            return Err(format!("run {run} has nothing left to do"));
        };
        let id = r.id.clone();
        let r = &mut self.runs[ri];
        let s = &mut r.stages[si];
        let mut changes = Vec::new();
        if matches!(s.status, StageStatus::Stopped | StageStatus::Failed) {
            s.status = StageStatus::Waiting;
            changes.push(Change {
                run: id.clone(),
                stage: Some(si),
                what: Decided::Retrying,
            });
        }
        s.note = Some(note);
        r.state = RunState::Running;
        changes.push(Change {
            run: id,
            stage: None,
            what: Decided::RunResumed,
        });
        Ok(changes)
    }

    /// What `run_status` returns: the run's state, each stage's status and attempts (session,
    /// its live state from `live(token)`, rating), and the next stage with whether it may start.
    pub fn status_json(
        &self,
        run: &Run,
        min_week_left: u8,
        live: impl Fn(&str) -> Option<&'static str>,
    ) -> Value {
        let stages: Vec<Value> = run
            .stages
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let spec = &run.pipeline.stages[i];
                let attempts: Vec<Value> = s
                    .attempts
                    .iter()
                    .map(|a| {
                        json!({
                            "label": a.label,
                            "session": a.session_name,
                            "live": a.session.as_deref().and_then(&live),
                            "started": secs(a.started),
                            "note": a.note,
                            "rating": a.rating,
                        })
                    })
                    .collect();
                json!({
                    "id": spec.id,
                    "title": spec.title,
                    "skill": spec.skill,
                    "bar": run.pipeline.bar_for(i),
                    "status": s.status.word(),
                    "attempts": attempts,
                })
            })
            .collect();
        let next = run
            .stages
            .iter()
            .position(|s| s.status != StageStatus::Passed && !s.status.busy())
            .map(|i| {
                let id = &run.pipeline.stages[i].id;
                let may = self.may_start(&run.orchestrator, &run.id, id, min_week_left);
                json!({"stage": id, "may_start": may.is_ok(), "why_not": may.err()})
            });
        let (state, reason) = match &run.state {
            RunState::Running => ("running", None),
            RunState::NeedsYou(why) => ("needs_you", Some(why.as_str())),
            RunState::Finished => ("finished", None),
            RunState::Stopped(why) => ("stopped", Some(why.as_str())),
        };
        json!({
            "run": run.id,
            "ticket": run.ticket,
            "summary": run.summary,
            "pipeline": run.pipeline.name,
            "started": secs(run.started),
            "state": state,
            "reason": reason,
            "paused": run.paused,
            "week_used": run.week_used,
            "stages": stages,
            "next": next,
        })
    }
}

/// What a re-run's session is told about the attempt before it (§5.3).
fn rerun_context(previous: Option<&Attempt>) -> String {
    let Some(a) = previous else {
        return "This stage hasn't run before.".into();
    };
    let Some(rating) = &a.rating else {
        return format!(
            "Your previous attempt ({}) ended without a report.",
            a.label
        );
    };
    let mut text = format!(
        "Your previous attempt ({}) reported {}: {}",
        a.label, rating.score, rating.summary
    );
    for reason in &rating.reasons {
        let sign = match reason.side {
            Side::For => '+',
            Side::Against => '-',
        };
        text.push_str(&format!("\n{sign} {}", reason.text));
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{pipeline, rating};

    fn pipeline(stages: &[&str]) -> Pipeline {
        let mut t = String::from("name = \"t\"\n");
        for s in stages {
            t += &format!(
                "[[stage]]\nid = \"{s}\"\ntitle = \"{s}\"\nprompt = \"do {s}. {{handoff}}\"\n"
            );
        }
        let mut p = pipeline::parse(&t).unwrap();
        p.file = "t".into();
        p
    }

    fn rated(score: u8) -> Rating {
        rating::from_args(&json!({"outcome": "done", "score": score, "summary": "s",
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "for", "text": "r"}, {"side": "against", "text": "q"}],
            "handoff": format!("after {score}")}))
        .unwrap()
    }

    /// A run of `stages` for orchestrator `orch`, its id.
    fn run(runs: &mut Runs, stages: &[&str]) -> String {
        runs.start_run("orch", pipeline(stages), "INTH-1", "x")
            .unwrap()
            .id
            .clone()
    }

    /// Starts stage `stage` and links it to session `tok`.
    fn started(runs: &mut Runs, id: &str, stage: &str, tok: &str) -> StartStage {
        let s = runs.begin("orch", id, stage, "", 30).unwrap();
        runs.link(id, s.stage, tok, &format!("{} · {stage}", s.label));
        s
    }

    fn status(runs: &Runs, id: &str, stage: usize) -> StageStatus {
        runs.run(id).unwrap().stages[stage].status
    }

    fn retry(note: &str) -> Choice {
        Choice::Retry {
            note: note.to_string(),
        }
    }

    #[test]
    fn a_run_starts_its_first_stage_and_links_its_session() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        assert_eq!(id, "INTH-1-1");
        let s = runs
            .begin("orch", &id, "a", "Look at the API.", 30)
            .unwrap();
        assert_eq!((s.stage, s.orchestrator.as_str()), (0, "orch"));
        assert_eq!(s.label, "s-01");
        assert!(s.prompt.starts_with("do a."), "{}", s.prompt);
        assert!(s.prompt.contains("Look at the API."), "{}", s.prompt);
        assert_eq!(status(&runs, &id, 0), StageStatus::Starting);
        runs.link(&id, 0, "tok-a", "s-01 · a");
        assert_eq!(status(&runs, &id, 0), StageStatus::Running);
        assert!(matches!(runs.role("tok-a"), Role::Stage(r, 0) if r.id == id));
        assert!(matches!(runs.role("orch"), Role::Orchestrator(r) if r.id == id));
        assert!(matches!(runs.role("other"), Role::Other));
    }

    #[test]
    fn the_next_stage_waits_for_a_passing_rating() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        assert!(
            runs.begin("orch", &id, "b", "", 30)
                .unwrap_err()
                .why
                .contains("running")
        );
        runs.report("tok-a", rated(80)).unwrap();
        let next = runs.begin("orch", &id, "b", "", 30).unwrap();
        assert!(next.prompt.contains("after 80"), "{}", next.prompt);
    }

    #[test]
    fn stages_go_in_order() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        assert!(
            runs.begin("orch", &id, "b", "", 30)
                .unwrap_err()
                .why
                .contains("a")
        );
        assert!(
            runs.begin("orch", &id, "zz", "", 30)
                .unwrap_err()
                .why
                .contains("zz")
        );
    }

    #[test]
    fn only_one_stage_runs_at_a_time() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        runs.begin("orch", &id, "a", "", 30).unwrap();
        // Still starting: the terminal hasn't opened yet, and a second start must not race it.
        let refused = runs.begin("orch", &id, "a", "", 30).unwrap_err();
        assert!(refused.why.contains("still running"), "{}", refused.why);
        assert_eq!(runs.run(&id).unwrap().stages[0].attempts.len(), 1);
    }

    #[test]
    fn a_stage_token_cannot_orchestrate_and_others_cannot_report() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        assert!(runs.begin("tok-a", &id, "b", "", 30).is_err());
        assert!(
            runs.start_run("tok-a", pipeline(&["x"]), "INTH-2", "x")
                .is_err()
        );
        assert!(runs.report("orch", rated(90)).is_err());
        assert!(runs.report("stranger", rated(90)).is_err());
        assert_eq!(status(&runs, &id, 0), StageStatus::Running);
    }

    #[test]
    fn a_low_score_needs_a_person_and_the_orchestrator_cannot_retry_it() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        let changes = runs.report("tok-a", rated(79)).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Stopped);
        assert!(matches!(
            runs.run(&id).unwrap().state,
            RunState::NeedsYou(_)
        ));
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.what, Decided::RunNeedsYou(_)))
        );
        assert!(runs.begin("orch", &id, "a", "", 30).is_err());
        assert!(runs.begin("orch", &id, "b", "", 30).is_err());
        // Still this ticket's: no new one until a person decides.
        assert!(
            runs.start_run("orch", pipeline(&["a"]), "INTH-2", "y")
                .is_err()
        );
    }

    #[test]
    fn blocked_stops_the_run_too() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        started(&mut runs, &id, "a", "tok-a");
        let mut blocked = rated(100);
        blocked.outcome = rating::Outcome::Blocked;
        runs.report("tok-a", blocked).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Stopped);
    }

    #[test]
    fn a_failed_stage_is_retried_three_times_then_needs_a_person() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        for attempt in 1..=3 {
            started(&mut runs, &id, "a", &format!("tok-{attempt}"));
            let changes = runs.session_gone(&format!("tok-{attempt}"));
            assert!(changes.iter().any(|c| matches!(c.what, Decided::Failed(_))));
            assert_eq!(status(&runs, &id, 0), StageStatus::Failed);
        }
        assert!(matches!(
            runs.run(&id).unwrap().state,
            RunState::NeedsYou(_)
        ));
        assert!(runs.begin("orch", &id, "a", "", 30).is_err());
        // A person may re-run it past the limit; failing again needs them again.
        runs.person(&id, retry("One more go.")).unwrap();
        started(&mut runs, &id, "a", "tok-4");
        runs.session_gone("tok-4");
        assert!(matches!(
            runs.run(&id).unwrap().state,
            RunState::NeedsYou(_)
        ));
    }

    #[test]
    fn a_terminal_that_never_opened_fails_its_stage() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        let s = runs.begin("orch", &id, "a", "", 30).unwrap();
        runs.start_failed(&id, s.stage, "no worktree");
        assert_eq!(status(&runs, &id, 0), StageStatus::Failed);
        // Still running, but nothing gets past the failed stage.
        assert!(
            runs.begin("orch", &id, "b", "", 30)
                .unwrap_err()
                .why
                .contains("hasn't passed")
        );
        assert!(runs.begin("orch", &id, "a", "", 30).is_ok());
    }

    #[test]
    fn less_than_the_weekly_share_left_needs_a_person() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        // No usage report yet: allowed.
        started(&mut runs, &id, "a", "tok-a");
        runs.usage("tok-a", 70.0);
        runs.report("tok-a", rated(90)).unwrap();
        assert!(runs.may_start("orch", &id, "b", 30).is_ok());
        runs.usage("orch", 71.0);
        let refused = runs.begin("orch", &id, "b", "", 30).unwrap_err();
        assert!(refused.why.contains("29%"), "{}", refused.why);
        assert!(
            refused
                .changes
                .iter()
                .any(|c| matches!(c.what, Decided::RunNeedsYou(_)))
        );
        assert!(matches!(
            runs.run(&id).unwrap().state,
            RunState::NeedsYou(_)
        ));
        // The person's re-run resumes it once there is room again.
        runs.person(&id, retry("Limit reset.")).unwrap();
        runs.usage("orch", 10.0);
        assert!(runs.begin("orch", &id, "b", "", 30).is_ok());
    }

    #[test]
    fn a_person_retries_with_a_note_or_stops() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        assert!(runs.person(&id, retry("early")).is_err());
        started(&mut runs, &id, "a", "tok-a");
        runs.report("tok-a", rated(10)).unwrap();
        // A re-run needs a note, and nothing waves the stage through.
        assert!(runs.person(&id, retry("  ")).unwrap_err().contains("note"));
        assert!(runs.begin("orch", &id, "b", "", 30).is_err());
        let changes = runs.person(&id, retry("Try the staging data.")).unwrap();
        assert!(changes.iter().any(|c| c.what == Decided::Retrying));
        assert!(changes.iter().any(|c| c.what == Decided::RunResumed));
        assert_eq!(status(&runs, &id, 0), StageStatus::Waiting);
        assert_eq!(runs.run(&id).unwrap().state, RunState::Running);
        assert!(runs.begin("orch", &id, "b", "", 30).is_err());
        let changes = runs.person(&id, Choice::Stop).unwrap();
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.what, Decided::RunStopped(_)))
        );
        assert!(matches!(&runs.run(&id).unwrap().state,
            RunState::Stopped(why) if why.contains("stopped by you at a")));
        assert!(runs.person(&id, retry("again")).is_err());
        assert!(runs.person(&id, Choice::Stop).is_err());
        assert!(runs.begin("orch", &id, "a", "", 30).is_err());
        // Stopped is final: the next ticket may start.
        assert!(
            runs.start_run("orch", pipeline(&["a"]), "INTH-2", "y")
                .is_ok()
        );
    }

    #[test]
    fn a_rerun_prompt_carries_the_previous_report_and_the_note() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        started(&mut runs, &id, "a", "tok-a");
        runs.report("tok-a", rated(40)).unwrap();
        runs.person(&id, retry("Use the staging data.")).unwrap();
        let again = started(&mut runs, &id, "a", "tok-a2");
        assert_eq!(again.label, "s-02");
        for says in [
            "(s-01) reported 40: s",
            "+ r",
            "- q",
            "Note from you: Use the staging data.",
        ] {
            assert!(again.prompt.contains(says), "{says}: {}", again.prompt);
        }
        let attempts = &runs.run(&id).unwrap().stages[0].attempts;
        assert_eq!(attempts[1].note.as_deref(), Some("Use the staging data."));
        assert_eq!(attempts[0].note, None);
    }

    #[test]
    fn attempts_are_numbered_across_the_run_and_kept() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-1");
        runs.session_gone("tok-1");
        started(&mut runs, &id, "a", "tok-2");
        runs.report("tok-2", rated(90)).unwrap();
        let b = started(&mut runs, &id, "b", "tok-3");
        assert_eq!(b.label, "s-03");
        let a = &runs.run(&id).unwrap().stages[0].attempts;
        let labels: Vec<&str> = a.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["s-01", "s-02"]);
        assert!(a[0].rating.is_none());
        assert_eq!(a[1].rating.as_ref().unwrap().score, 90);
        assert_eq!(
            (a[1].session.as_deref(), a[1].session_name.as_deref()),
            (Some("tok-2"), Some("s-02 · a"))
        );
    }

    #[test]
    fn an_earlier_attempt_cannot_report() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        started(&mut runs, &id, "a", "tok-1");
        runs.report("tok-1", rated(10)).unwrap();
        runs.person(&id, retry("again")).unwrap();
        started(&mut runs, &id, "a", "tok-2");
        assert!(matches!(runs.role("tok-1"), Role::Other));
        assert!(runs.report("tok-1", rated(99)).is_err());
        // The old session ending leaves the new attempt alone.
        assert!(runs.session_gone("tok-1").is_empty());
        assert_eq!(status(&runs, &id, 0), StageStatus::Running);
    }

    #[test]
    fn a_paused_run_starts_nothing() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        let changes = runs.pause(&id, true).unwrap();
        assert!(changes.iter().any(|c| c.what == Decided::Paused));
        assert!(runs.pause(&id, true).is_err());
        // The stage already running still reports.
        runs.report("tok-a", rated(90)).unwrap();
        let refused = runs.begin("orch", &id, "b", "", 30).unwrap_err();
        assert!(refused.why.contains("paused"), "{}", refused.why);
        assert!(refused.changes.is_empty());
        let changes = runs.pause(&id, false).unwrap();
        assert!(changes.iter().any(|c| c.what == Decided::Resumed));
        assert!(runs.begin("orch", &id, "b", "", 30).is_ok());
    }

    #[test]
    fn a_bar_changed_since_the_start_applies_at_the_gate() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        let mut now = pipeline(&["b", "a", "c"]);
        now.stages[1].bar = Some(95);
        runs.refresh_bars("t", &now);
        // A stage list change waits for the next run; the bar applies now.
        let r = runs.run(&id).unwrap();
        assert_eq!(r.pipeline.stages[0].id, "a");
        assert_eq!(r.pipeline.bar_for(0), 95);
        runs.report("tok-a", rated(90)).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Stopped);
        // Another pipeline's file changes nothing.
        let mut other = pipeline(&["a"]);
        other.stages[0].bar = Some(10);
        runs.refresh_bars("other", &other);
        assert_eq!(runs.run(&id).unwrap().pipeline.bar_for(0), 95);
    }

    #[test]
    fn the_last_stage_passing_finishes_the_run_and_a_new_one_may_start() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        assert!(
            runs.start_run("orch", pipeline(&["a"]), "INTH-2", "x")
                .is_err()
        );
        started(&mut runs, &id, "a", "tok-a");
        let changes = runs.report("tok-a", rated(95)).unwrap();
        assert!(changes.iter().any(|c| c.what == Decided::RunFinished));
        assert_eq!(runs.run(&id).unwrap().state, RunState::Finished);
        assert!(runs.pause(&id, true).is_err());
        let next = runs
            .start_run("orch", pipeline(&["a"]), "INTH-1", "x")
            .unwrap();
        assert_eq!(next.id, "INTH-1-2");
        assert_eq!(runs.current("orch").unwrap().id, "INTH-1-2");
    }

    #[test]
    fn a_closed_orchestrator_stops_its_run_for_good() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        let changes = runs.session_gone("orch");
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.what, Decided::RunStopped(_)))
        );
        assert!(matches!(runs.run(&id).unwrap().state, RunState::Stopped(_)));
        assert!(runs.person(&id, retry("again")).is_err());
    }

    #[test]
    fn one_report_per_stage() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        runs.report("tok-a", rated(90)).unwrap();
        assert!(
            runs.report("tok-a", rated(10))
                .unwrap_err()
                .contains("already")
        );
        assert_eq!(status(&runs, &id, 0), StageStatus::Passed);
    }

    #[test]
    fn the_status_shows_what_the_gate_uses() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        let r = runs.run(&id).unwrap();
        let s = runs.status_json(r, 30, |t| (t == "tok-a").then_some("working"));
        assert_eq!(s["run"], "INTH-1-1");
        assert_eq!(
            (s["state"].as_str(), s["paused"].as_bool()),
            (Some("running"), Some(false))
        );
        assert_eq!(s["stages"][0]["status"], "running");
        let attempt = &s["stages"][0]["attempts"][0];
        assert_eq!(
            (attempt["label"].as_str(), attempt["live"].as_str()),
            (Some("s-01"), Some("working"))
        );
        assert_eq!(attempt["session"], "s-01 · a");
        assert!(attempt["started"].as_u64().is_some());
        assert_eq!(s["stages"][1]["bar"], 80);
        assert_eq!(s["stages"][1]["attempts"], json!([]));
        assert_eq!(s["next"]["stage"], "b");
        assert!(s["next"]["why_not"].as_str().unwrap().contains("running"));
    }
}
