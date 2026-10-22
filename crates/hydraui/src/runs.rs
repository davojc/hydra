//! Runs (orchestrator spec §3, §5): one ticket going through a pipeline's stages, owned by
//! one orchestrator terminal, and the gate that decides when a stage may start.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

use crate::pipeline::{Pipeline, PromptVars, render};
use crate::rating::{Rating, passes};

/// Attempts a failed stage gets before it waits for a person (§5.2, the user's decision).
pub const MAX_ATTEMPTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StageStatus {
    /// Not started.
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
    /// A person let the run go on past it (§5.3).
    Overridden,
}

impl StageStatus {
    /// Passed, or let through by a person: the next stage may follow it.
    pub fn cleared(self) -> bool {
        matches!(self, StageStatus::Passed | StageStatus::Overridden)
    }

    pub fn word(self) -> &'static str {
        match self {
            StageStatus::Waiting => "waiting",
            StageStatus::Starting => "starting",
            StageStatus::Running => "running",
            StageStatus::Passed => "passed",
            StageStatus::Stopped => "stopped",
            StageStatus::Failed => "failed",
            StageStatus::Overridden => "overridden",
        }
    }
}

/// One stage of a run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StageRun {
    pub status: StageStatus,
    pub attempts: u8,
    /// The token of the terminal running its latest attempt.
    pub session: Option<String>,
    pub session_name: Option<String>,
    pub rating: Option<Rating>,
    /// A person's note: added to a retried stage's prompt, or why they let it through.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// Waiting for a person, and why.
    Stopped(String),
    Finished,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// `<ticket>-<n>`.
    pub id: String,
    pub ticket: String,
    pub summary: String,
    pub pipeline: Pipeline,
    /// The token of the terminal that started it.
    pub orchestrator: String,
    pub stages: Vec<StageRun>,
    pub state: RunState,
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
    Overridden,
    Retrying,
    RunResumed,
    RunFinished,
    RunStopped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub run: String,
    pub stage: Option<usize>,
    pub what: Decided,
}

/// Why a stage may not start, and what that changed (a usage refusal stops the run, §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub why: String,
    pub changes: Vec<Change>,
}

/// A person's choice for a stopped run (§5.3).
pub enum Choice {
    Continue { note: String },
    Retry { note: String },
    Stop,
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

    /// The run and stage whose latest session is `token`.
    fn stage_of(&self, token: &str) -> Option<(usize, usize)> {
        self.runs.iter().enumerate().find_map(|(ri, r)| {
            r.stages
                .iter()
                .position(|s| s.session.as_deref() == Some(token))
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

    /// Starts a run of `pipeline` for `ticket`, owned by the terminal `orchestrator`. One
    /// unfinished run per orchestrator (the user's decision: one ticket at a time).
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
            .find(|r| r.orchestrator == orchestrator && r.state == RunState::Running)
        {
            return Err(format!("run {} is still going: one ticket at a time", r.id));
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
            worktree: None,
            week_used: None,
        });
        Ok(self.runs.last().expect("just pushed"))
    }

    /// The gate (§5.1): the stage's index, or why not and whether that stops the run.
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
            RunState::Stopped(why) => return no(format!("run {run} is stopped: {why}")),
            RunState::Finished => return no(format!("run {run} is finished")),
        }
        let Some(i) = r.pipeline.stages.iter().position(|s| s.id == stage) else {
            return no(format!("no stage {stage} in pipeline {}", r.pipeline.name));
        };
        if let Some(busy) = r
            .stages
            .iter()
            .position(|s| matches!(s.status, StageStatus::Starting | StageStatus::Running))
        {
            return no(format!(
                "stage {} is still running",
                r.pipeline.stages[busy].id
            ));
        }
        if let Some(before) = r.stages[..i].iter().position(|s| !s.status.cleared()) {
            return no(format!(
                "stage {} hasn't passed yet",
                r.pipeline.stages[before].id
            ));
        }
        let s = &r.stages[i];
        match s.status {
            StageStatus::Waiting => {}
            StageStatus::Failed if s.attempts < MAX_ATTEMPTS => {}
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

    /// Lets `stage` start: it becomes `Starting` and gets its prompt. Refused with the gate's
    /// reason; a usage refusal also stops the run for a person.
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
            Err((why, stops)) => {
                let changes = match (stops, self.index(run)) {
                    (true, Some(ri)) => self.stop_run(ri, why.clone()),
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
            .find_map(|s| s.rating.as_ref())
            .map(|rating| rating.handoff.clone())
            .unwrap_or_default();
        let s = &mut r.stages[i];
        let brief = match &s.note {
            Some(note) => format!("{}\n\n{note}", brief.trim()),
            None => brief.to_string(),
        };
        s.status = StageStatus::Starting;
        s.attempts += 1;
        s.session = None;
        s.session_name = None;
        s.rating = None;
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
            orchestrator: r.orchestrator.clone(),
            worktree: spec.worktree,
            title: spec.title.clone(),
            prompt: render(&spec.prompt, &vars, &brief),
        })
    }

    /// The terminal for a starting stage opened: `token` is its session from now on.
    pub fn link(&mut self, run: &str, stage: usize, token: &str, name: &str) {
        if let Some(s) = self
            .index(run)
            .and_then(|ri| self.runs[ri].stages.get_mut(stage))
            .filter(|s| s.status == StageStatus::Starting)
        {
            s.status = StageStatus::Running;
            s.session = Some(token.to_string());
            s.session_name = Some(name.to_string());
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

    fn stop_run(&mut self, ri: usize, why: String) -> Vec<Change> {
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
        let attempts = s.attempts;
        let id = r.pipeline.stages[si].id.clone();
        let mut changes = vec![Change {
            run: r.id.clone(),
            stage: Some(si),
            what: Decided::Failed(why.to_string()),
        }];
        if attempts >= MAX_ATTEMPTS {
            changes.extend(
                self.stop_run(ri, format!("stage {id} failed {MAX_ATTEMPTS} times: {why}")),
            );
        }
        changes
    }

    /// `token`'s stage reported (§4). Only its own running session reports, once.
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
        s.rating = Some(rating);
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
            changes.extend(self.stop_run(ri, why));
            Ok(changes)
        }
    }

    /// The terminal holding `token` closed, its shell exited or its Claude session ended: a
    /// stage it was running fails, a run it orchestrated stops.
    pub fn session_gone(&mut self, token: &str) -> Vec<Change> {
        let mut changes = Vec::new();
        if let Some((ri, si)) = self.stage_of(token)
            && matches!(
                self.runs[ri].stages[si].status,
                StageStatus::Starting | StageStatus::Running
            )
        {
            changes.extend(self.fail_stage(ri, si, "its session ended without reporting"));
        }
        let orchestrated: Vec<usize> = (0..self.runs.len())
            .filter(|&ri| {
                self.runs[ri].orchestrator == token && self.runs[ri].state == RunState::Running
            })
            .collect();
        for ri in orchestrated {
            changes.extend(self.stop_run(ri, "the orchestrator's terminal closed".into()));
        }
        changes
    }

    /// A session reported its seven-day usage: kept on the running run it belongs to.
    pub fn usage(&mut self, token: &str, week_used: f64) {
        for r in &mut self.runs {
            let ours = r.orchestrator == token
                || r.stages.iter().any(|s| s.session.as_deref() == Some(token));
            if ours && r.state == RunState::Running {
                r.week_used = Some(week_used);
            }
        }
    }

    /// A person's choice for run `run` (§5.3).
    pub fn person(&mut self, run: &str, choice: Choice) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        if let Choice::Stop = choice {
            return match self.runs[ri].state {
                RunState::Running => Ok(self.stop_run(ri, "stopped by you".into())),
                RunState::Stopped(_) => Err(format!("run {run} is already stopped")),
                RunState::Finished => Err(format!("run {run} is finished")),
            };
        }
        let r = &mut self.runs[ri];
        if !matches!(r.state, RunState::Stopped(_)) {
            return Err(format!("run {run} isn't stopped"));
        }
        let Some(si) = r.stages.iter().position(|s| !s.status.cleared()) else {
            return Err(format!("run {run} has nothing left to do"));
        };
        let last = si + 1 == r.stages.len();
        let id = r.id.clone();
        let s = &mut r.stages[si];
        let change = |what| Change {
            run: id.clone(),
            stage: Some(si),
            what,
        };
        let (note, retry) = match choice {
            Choice::Continue { note } => (note, false),
            Choice::Retry { note } => (note, true),
            Choice::Stop => unreachable!("handled above"),
        };
        let note = Some(note.trim().to_string()).filter(|n| !n.is_empty());
        let mut changes = Vec::new();
        match s.status {
            StageStatus::Stopped | StageStatus::Failed if retry => {
                s.status = StageStatus::Waiting;
                s.note = note;
                changes.push(change(Decided::Retrying));
            }
            StageStatus::Stopped | StageStatus::Failed => {
                s.status = StageStatus::Overridden;
                s.note = note;
                changes.push(change(Decided::Overridden));
            }
            // Stopped before it started (the usage line): carry on.
            StageStatus::Waiting => {}
            _ => {
                return Err(format!(
                    "run {run}: nothing to {}",
                    if retry { "retry" } else { "continue" }
                ));
            }
        }
        if !retry && last && s.status == StageStatus::Overridden {
            r.state = RunState::Finished;
            changes.push(Change {
                run: id,
                stage: None,
                what: Decided::RunFinished,
            });
        } else {
            r.state = RunState::Running;
            changes.push(Change {
                run: id,
                stage: None,
                what: Decided::RunResumed,
            });
        }
        Ok(changes)
    }

    /// What `run_status` returns: each stage's status, attempts, session and its live state
    /// (`live(token)`), rating, and the next stage with whether it may start.
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
                json!({
                    "id": spec.id,
                    "title": spec.title,
                    "bar": run.pipeline.bar_for(i),
                    "status": s.status.word(),
                    "attempts": s.attempts,
                    "session": s.session_name,
                    "live": s.session.as_deref().and_then(&live),
                    "rating": s.rating,
                    "note": s.note,
                })
            })
            .collect();
        let next = run
            .stages
            .iter()
            .position(|s| {
                !s.status.cleared()
                    && !matches!(s.status, StageStatus::Starting | StageStatus::Running)
            })
            .map(|i| {
                let id = &run.pipeline.stages[i].id;
                let may = self.may_start(&run.orchestrator, &run.id, id, min_week_left);
                json!({"stage": id, "may_start": may.is_ok(), "why_not": may.err()})
            });
        let (state, reason) = match &run.state {
            RunState::Running => ("running", None),
            RunState::Stopped(why) => ("stopped", Some(why.as_str())),
            RunState::Finished => ("finished", None),
        };
        json!({
            "run": run.id,
            "ticket": run.ticket,
            "summary": run.summary,
            "pipeline": run.pipeline.name,
            "state": state,
            "reason": reason,
            "week_used": run.week_used,
            "stages": stages,
            "next": next,
        })
    }
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
        pipeline::parse(&t).unwrap()
    }

    fn rated(score: u8) -> Rating {
        rating::from_args(&json!({"outcome": "done", "score": score, "summary": "s",
            "evidence": [{"kind": "note", "detail": "d"}], "reasons": "r",
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
        runs.link(id, s.stage, tok, &format!("{id} · {stage}"));
        s
    }

    fn status(runs: &Runs, id: &str, stage: usize) -> StageStatus {
        runs.run(id).unwrap().stages[stage].status
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
        assert!(s.prompt.starts_with("do a."), "{}", s.prompt);
        assert!(s.prompt.contains("Look at the API."), "{}", s.prompt);
        assert_eq!(status(&runs, &id, 0), StageStatus::Starting);
        runs.link(&id, 0, "tok-a", "INTH-1-1 · a");
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
        assert_eq!(runs.run(&id).unwrap().stages[0].attempts, 1);
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
    fn a_low_score_stops_the_run_and_the_orchestrator_cannot_retry_it() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        let changes = runs.report("tok-a", rated(79)).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Stopped);
        assert!(matches!(runs.run(&id).unwrap().state, RunState::Stopped(_)));
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.what, Decided::RunStopped(_)))
        );
        assert!(runs.begin("orch", &id, "a", "", 30).is_err());
        assert!(runs.begin("orch", &id, "b", "", 30).is_err());
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
    fn a_failed_stage_is_retried_three_times_then_stops() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        for attempt in 1..=3 {
            started(&mut runs, &id, "a", &format!("tok-{attempt}"));
            let changes = runs.session_gone(&format!("tok-{attempt}"));
            assert!(changes.iter().any(|c| matches!(c.what, Decided::Failed(_))));
            assert_eq!(status(&runs, &id, 0), StageStatus::Failed);
        }
        assert!(matches!(runs.run(&id).unwrap().state, RunState::Stopped(_)));
        assert!(runs.begin("orch", &id, "a", "", 30).is_err());
    }

    #[test]
    fn a_terminal_that_never_opened_fails_its_stage() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        let s = runs.begin("orch", &id, "a", "", 30).unwrap();
        runs.start_failed(&id, s.stage, "no worktree");
        assert_eq!(status(&runs, &id, 0), StageStatus::Failed);
        assert!(runs.begin("orch", &id, "a", "", 30).is_ok());
    }

    #[test]
    fn less_than_the_weekly_share_left_stops_the_run() {
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
                .any(|c| matches!(c.what, Decided::RunStopped(_)))
        );
        assert!(matches!(runs.run(&id).unwrap().state, RunState::Stopped(_)));
    }

    #[test]
    fn a_person_continues_retries_or_stops() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "tok-a");
        runs.report("tok-a", rated(10)).unwrap();
        runs.person(
            &id,
            Choice::Retry {
                note: "Try the staging data.".into(),
            },
        )
        .unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Waiting);
        assert_eq!(runs.run(&id).unwrap().state, RunState::Running);
        let again = started(&mut runs, &id, "a", "tok-a2");
        assert!(
            again.prompt.contains("Try the staging data."),
            "{}",
            again.prompt
        );
        runs.report("tok-a2", rated(20)).unwrap();
        runs.person(
            &id,
            Choice::Continue {
                note: "Good enough.".into(),
            },
        )
        .unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Overridden);
        assert!(runs.begin("orch", &id, "b", "", 30).is_ok());
        runs.person(&id, Choice::Stop).unwrap();
        assert!(matches!(runs.run(&id).unwrap().state, RunState::Stopped(_)));
        assert!(
            runs.person(
                &id,
                Choice::Continue {
                    note: String::new()
                }
            )
            .is_err()
        );
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
        let next = runs
            .start_run("orch", pipeline(&["a"]), "INTH-1", "x")
            .unwrap();
        assert_eq!(next.id, "INTH-1-2");
        assert_eq!(runs.current("orch").unwrap().id, "INTH-1-2");
    }

    #[test]
    fn a_closed_orchestrator_stops_its_run() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        let changes = runs.session_gone("orch");
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.what, Decided::RunStopped(_)))
        );
        assert!(matches!(runs.run(&id).unwrap().state, RunState::Stopped(_)));
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
        assert_eq!(s["stages"][0]["status"], "running");
        assert_eq!(s["stages"][0]["live"], "working");
        assert_eq!(s["stages"][1]["bar"], 80);
        assert_eq!(s["next"]["stage"], "b");
        assert!(s["next"]["why_not"].as_str().unwrap().contains("running"));
    }
}
