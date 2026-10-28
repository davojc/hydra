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

/// The share of the seven-day Claude limit a stage needs left, in percent, unless hydraui's
/// config sets `min_week_left` (§5.1, the user's decision).
pub const MIN_WEEK_LEFT: u8 = 30;

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

    /// Starting or running: its session is at work.
    pub fn busy(self) -> bool {
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
    /// The skills its session invoked (`hydra hook skill`), in order.
    pub skills: Vec<String>,
    /// Its reasons against that a person marked **Must address**, by their place in
    /// `rating.reasons`, in the order marked.
    pub must: Vec<usize>,
    /// Its reasons against that a person marked **Not needed**: concerns they don't want acted
    /// on, by place in `rating.reasons`.
    pub waived: Vec<usize>,
}

/// One stage of a run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StageRun {
    pub status: StageStatus,
    pub attempts: Vec<Attempt>,
    /// A person's note for the next attempt.
    pub note: Option<String>,
    /// Attempts before this one don't count toward its tries: the run went back to an earlier
    /// stage since (a person's choice).
    pub counted_from: usize,
}

impl StageRun {
    /// Its attempts that count toward `MAX_ATTEMPTS`.
    pub fn tries(&self) -> usize {
        self.attempts.len().saturating_sub(self.counted_from)
    }

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

/// A person's send-back waiting for the stage at work to report: the run then goes back to
/// `stage` with `note` (the reasons against they marked **Must address**).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub stage: usize,
    pub note: String,
}

/// A stage's question for a person, a decision that isn't the stage's to make (product): its
/// ticket waits for the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub stage: usize,
    pub question: String,
    /// Where it was asked, a Slack thread, when the stage gave one.
    pub link: Option<String>,
    pub since: SystemTime,
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
    /// A stage's question waiting for its answer.
    pub asking: Option<Ask>,
    /// A send-back queued until the stage at work reports.
    pub fix_after: Option<Fix>,
    /// What it's about, as a link: the pull request a Slack-triggered run reviews (`{pr}`).
    pub link: Option<String>,
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
    /// What its terminal is called (`session_name`).
    pub name: String,
    pub orchestrator: String,
    pub worktree: bool,
    /// The skill its session must use (§4), checked as installed before it opens.
    pub skill: Option<String>,
    /// The folder its session starts in, as the pipeline writes it.
    pub folder: Option<String>,
    pub title: String,
    pub prompt: String,
    /// How its Claude starts, before the prompt (`Pipeline::stage_claude`).
    pub claude: Vec<String>,
}

/// What happened to a run or one of its stages, for the app to show and tell the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    RunStarted,
    Passed,
    Stopped(String),
    Failed(String),
    /// A person re-runs it.
    Retrying,
    /// A person sent the run back to this earlier stage: its own session goes on.
    Reopened,
    /// A person worked on this stopped stage in its own session and asked it to score again.
    Rescoring,
    /// A stage asked a person to check something by hand, and they say it works: it passes,
    /// with their note.
    Checked(String),
    Paused,
    Resumed,
    /// A stage asked a person a question; the ticket waits.
    Asked(String),
    /// Its answer came; the stage goes on.
    Answered(String),
    RunResumed,
    RunFinished,
    /// The ticket was settled outside hydraui (its pull request merged or closed): the run is
    /// done, whatever its gates said.
    RunSettled(String),
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

/// How a person marks a reason against: **Must address** (the stage goes again for it) or
/// **Not needed** (a concern they don't want acted on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Must,
    NotNeeded,
}

/// A person's choice for a run (§5.3). Only `Checked` lets a stage through, and only one
/// that asked a person to check something by hand.
pub enum Choice {
    /// Re-run the stage that waits, with a note for its next session.
    Retry { note: String },
    /// Stop the ticket.
    Stop,
    /// Go back to `stage`, an earlier stage that passed: its own session is reopened with the
    /// note, and the stages after it wait for it again with fresh tries.
    Back { stage: String, note: String },
    /// The waiting stage asked for a check by hand (`Outcome::Check`) and the person did it:
    /// it works. The stage passes and the run goes on; `note` (may be empty) says what they saw.
    Checked { note: String },
}

/// Whether `used`, a skill a session invoked, is `wanted`: the same name, ignoring case, or a
/// plugin's copy of it (`plugin:name`).
fn same_skill(used: &str, wanted: &str) -> bool {
    let bare = |s: &str| {
        s.rsplit(':')
            .next()
            .unwrap_or(s)
            .trim()
            .to_ascii_lowercase()
    };
    bare(used) == bare(wanted)
}

/// What people see a session called: its stage's number and title, and which try it is after
/// the first, e.g. `2. Develop · try 2`. `label` (`s-02`) stays the run's own id for it.
pub fn session_name(stage: usize, title: &str, attempt: usize) -> String {
    match attempt {
        0 => format!("{}. {title}", stage + 1),
        n => format!("{}. {title} · try {}", stage + 1, n + 1),
    }
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

    /// The runs `orchestrator` holds open, oldest first.
    pub fn open_of(&self, orchestrator: &str) -> Vec<&Run> {
        self.runs
            .iter()
            .filter(|r| r.orchestrator == orchestrator && r.state.open())
            .collect()
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

    /// Starts a run of `pipeline` for `ticket`, owned by the terminal `orchestrator`. One
    /// orchestrator holds up to the pipeline's `runs_at_once` open runs, each its own ticket
    /// with its own stage sessions under it (one at a time unless the pipeline says more).
    pub fn start_run(
        &mut self,
        orchestrator: &str,
        pipeline: Pipeline,
        ticket: &str,
        summary: &str,
    ) -> Result<&Run, String> {
        self.start_run_avoiding(orchestrator, pipeline, ticket, summary, |_| false)
    }

    /// `start_run`, skipping ids `taken` says are in use already (a run logged by an earlier
    /// hydraui, §8), so two runs never share a log.
    pub fn start_run_avoiding(
        &mut self,
        orchestrator: &str,
        pipeline: Pipeline,
        ticket: &str,
        summary: &str,
        taken: impl Fn(&str) -> bool,
    ) -> Result<&Run, String> {
        let ticket = ticket.trim();
        if ticket.is_empty() {
            return Err("name the ticket".into());
        }
        if self.stage_of(orchestrator).is_some() {
            return Err("a stage session can't start a run".into());
        }
        let open: Vec<&str> = self
            .open_of(orchestrator)
            .into_iter()
            .map(|r| r.id.as_str())
            .collect();
        if let Some(same) = self
            .open_of(orchestrator)
            .into_iter()
            .find(|r| r.ticket.eq_ignore_ascii_case(ticket))
        {
            return Err(format!("{ticket} is already running as {}", same.id));
        }
        let most = usize::from(pipeline.runs_at_once.max(1));
        if open.len() >= most {
            return Err(match most {
                1 => format!(
                    "run {} isn't finished or stopped: one ticket at a time",
                    open[0]
                ),
                n => format!(
                    "{n} tickets are open already ({}), the most this pipeline runs at once",
                    open.join(", ")
                ),
            });
        }
        let id = (self.runs.iter().filter(|r| r.ticket == ticket).count() + 1..)
            .map(|n| format!("{ticket}-{n}"))
            .find(|id| !taken(id) && self.run(id).is_none())
            .expect("an unbounded range has a free id");
        let stages = vec![StageRun::default(); pipeline.stages.len()];
        self.runs.push(Run {
            id,
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
            asking: None,
            fix_after: None,
            link: None,
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
            StageStatus::Failed if s.tries() < MAX_ATTEMPTS => {}
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
            skills: Vec::new(),
            must: Vec::new(),
            waived: Vec::new(),
        });
        let spec = &r.pipeline.stages[i];
        let feature_env = r.pipeline.feature_env_for(&r.ticket);
        let vars = PromptVars {
            ticket: &r.ticket,
            run: &r.id,
            stage: &spec.id,
            handoff: &handoff,
            feature_env: &feature_env,
            link: r.link.as_deref().unwrap_or(""),
        };
        let name = session_name(i, &spec.title, r.stages[i].attempts.len() - 1);
        Ok(StartStage {
            run: r.id.clone(),
            stage: i,
            label,
            name,
            orchestrator: r.orchestrator.clone(),
            worktree: spec.worktree,
            skill: spec.skill.clone(),
            folder: spec.folder.clone(),
            title: spec.title.clone(),
            claude: r.pipeline.stage_claude(),
            prompt: format!(
                "{}\n\n{}",
                render(&spec.prompt, &vars, &brief),
                stage_note(r, i)
            ),
        })
    }

    /// The worktree made for `run`'s first worktree stage, which its later ones reuse (§3.2).
    pub fn set_worktree(&mut self, run: &str, path: PathBuf) {
        if let Some(ri) = self.index(run) {
            self.runs[ri].worktree = Some(path);
        }
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

    /// Puts back a run rebuilt from its log (§13), replacing one with its id.
    pub fn restore(&mut self, run: Run) {
        match self.index(&run.id) {
            Some(i) => self.runs[i] = run,
            None => self.runs.push(run),
        }
    }

    /// Run `run` now belongs to the orchestrator terminal `orchestrator` (§13: a reopened
    /// orchestrator).
    pub fn adopt(&mut self, run: &str, orchestrator: &str) {
        if let Some(ri) = self.index(run) {
            self.runs[ri].orchestrator = orchestrator.to_string();
        }
    }

    /// Stage `stage`'s latest attempt goes on in the terminal `token` (§13: its session
    /// reopened), not as a new attempt.
    pub fn relink(&mut self, run: &str, stage: usize, token: &str, name: &str) {
        if let Some(a) = self
            .index(run)
            .and_then(|ri| self.runs[ri].stages.get_mut(stage))
            .and_then(|s| s.attempts.last_mut())
        {
            a.session = Some(token.to_string());
            a.session_name = Some(name.to_string());
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

    /// Run `run`'s ticket was settled outside hydraui (`why`: its pull request merged or
    /// closed): nothing it does now changes that, so it ends as done, waiting gates and all.
    pub fn settle(&mut self, run: &str, why: &str) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let r = &mut self.runs[ri];
        if !r.state.open() {
            return Err(format!("run {run} has already ended"));
        }
        r.state = RunState::Finished;
        r.asking = None;
        r.fix_after = None;
        Ok(vec![Change {
            run: r.id.clone(),
            stage: None,
            what: Decided::RunSettled(why.to_string()),
        }])
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
        if r.asking.take().is_some() {
            r.state = RunState::Running;
        }
        let s = &mut r.stages[si];
        s.status = StageStatus::Failed;
        let attempts = s.tries();
        let id = r.pipeline.stages[si].id.clone();
        let mut changes = vec![Change {
            run: r.id.clone(),
            stage: Some(si),
            what: Decided::Failed(why.to_string()),
        }];
        if let Some(fix) = self.runs[ri].fix_after.take() {
            changes.extend(self.reopen(ri, fix.stage, &fix.note));
        } else if attempts >= MAX_ATTEMPTS {
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
        // The skill the pipeline set must have been used by this session (§4).
        if let Some(skill) = &r.pipeline.stages[si].skill {
            let used = r.stages[si]
                .latest()
                .is_some_and(|a| a.skills.iter().any(|u| same_skill(u, skill)));
            if !used {
                return Err(format!(
                    "Not recorded: this stage's skill is {skill}, and this session hasn't used \
                     it. Invoke the {skill} skill (the Skill tool), do the stage with it, then \
                     report again."
                ));
            }
        }
        // A stage that reports without waiting for its answer has decided after all.
        if r.asking.take().is_some() {
            r.state = RunState::Running;
        }
        let bar = r.pipeline.bar_for(si);
        let id = r.pipeline.stages[si].id.clone();
        let passed = passes(&rating, bar);
        let why = match rating.outcome {
            crate::rating::Outcome::Blocked => format!("stage {id} is blocked: {}", rating.summary),
            crate::rating::Outcome::Check => {
                format!("stage {id} needs a person to check: {}", rating.check)
            }
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
        // A person's send-back waited for this report: the run goes back whatever it says.
        if let Some(fix) = r.fix_after.take() {
            let mut changes = vec![Change {
                run,
                stage: Some(si),
                what: if passed {
                    Decided::Passed
                } else {
                    Decided::Stopped(why)
                },
            }];
            s.status = if passed {
                StageStatus::Passed
            } else {
                StageStatus::Stopped
            };
            changes.extend(self.reopen(ri, fix.stage, &fix.note));
            return Ok(changes);
        }
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

    /// The session holding `token` invoked `skill`: kept on its stage's latest attempt, which
    /// may report only once it has used the stage's skill (§4).
    pub fn skill_used(&mut self, token: &str, skill: &str) {
        if let Some((ri, si)) = self.stage_of(token)
            && let Some(a) = self.runs[ri].stages[si].attempts.last_mut()
            && !a.skills.iter().any(|s| s == skill)
        {
            a.skills.push(skill.to_string());
        }
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
    /// The pipeline file `file` now reads `now` (Pipeline settings saved it, or a stage is about
    /// to report): open runs of it take its name, its bar, and each stage's settings for the
    /// stages they share. Their stage lists stay as they started (§2).
    pub fn refresh_from_file(&mut self, file: &str, now: &Pipeline) {
        for r in self
            .runs
            .iter_mut()
            .filter(|r| r.pipeline.file == file && r.state.open())
        {
            r.pipeline.name = now.name.clone();
            r.pipeline.bar = now.bar;
            // How it looks and how it asks product follow Save at once too.
            r.pipeline.color = now.color;
            r.pipeline.product_channel = now.product_channel.clone();
            r.pipeline.product_style = now.product_style.clone();
            // A stage's own settings reach it from its next start; only the list (adding,
            // removing, reordering) waits for the next ticket.
            for stage in &mut r.pipeline.stages {
                if let Some(same) = now.stages.iter().find(|s| s.id == stage.id) {
                    *stage = same.clone();
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
    /// `token`'s running stage asks a person `question` (asked at `link`, a Slack thread): the
    /// ticket waits until [`Runs::answered`].
    pub fn ask(
        &mut self,
        token: &str,
        question: &str,
        link: Option<&str>,
        now: SystemTime,
    ) -> Result<Vec<Change>, String> {
        let Some((ri, si)) = self.stage_of(token) else {
            return Err("only a running stage's own session asks".into());
        };
        let r = &mut self.runs[ri];
        if r.stages[si].status != StageStatus::Running {
            return Err("this stage isn't running".into());
        }
        if r.asking.is_some() {
            return Err("this run is already waiting for an answer".into());
        }
        if !r.pipeline.product_asks {
            return Err("this pipeline doesn't ask product: decide it within your stage, and                         name it in your report's reasons against if it matters"
                .into());
        }
        let question = question.trim();
        if question.is_empty() {
            return Err("the question is empty".into());
        }
        r.asking = Some(Ask {
            stage: si,
            question: question.to_string(),
            link: link
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string),
            since: now,
        });
        r.state = RunState::NeedsYou(format!(
            "{} is waiting for an answer: {question}",
            r.pipeline.stages[si].title
        ));
        Ok(vec![Change {
            run: r.id.clone(),
            stage: Some(si),
            what: Decided::Asked(question.to_string()),
        }])
    }

    /// The answer to run `run`'s question came (found by its stage, or given in hydraui): the
    /// stage goes on.
    pub fn answered(&mut self, run: &str, answer: &str) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let answer = answer.trim();
        if answer.is_empty() {
            return Err("the answer is empty".into());
        }
        let r = &mut self.runs[ri];
        let Some(ask) = r.asking.take() else {
            return Err(format!("run {run} isn't waiting for an answer"));
        };
        r.state = RunState::Running;
        Ok(vec![Change {
            run: r.id.clone(),
            stage: Some(ask.stage),
            what: Decided::Answered(answer.to_string()),
        }])
    }

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
                self.runs[ri].asking = None;
                return Ok(self.end(ri, format!("stopped by you at {at}")));
            }
            _ if r.asking.is_some() => {
                return Err(format!(
                    "run {run} is waiting for the answer to its stage's question: answer it, or \
                     stop the ticket"
                ));
            }
            Choice::Retry { note } => note.trim().to_string(),
            Choice::Back { stage, note } => return self.go_back(ri, &stage, note.trim()),
            Choice::Checked { note } => return self.checked(ri, note.trim()),
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

    /// A person's **It works**: the stage stopped asking for a check by hand passes, and the
    /// run finishes or goes on to the next stage.
    fn checked(&mut self, ri: usize, note: &str) -> Result<Vec<Change>, String> {
        let r = &mut self.runs[ri];
        if !matches!(r.state, RunState::NeedsYou(_)) {
            return Err(format!("run {} isn't waiting for you", r.id));
        }
        if r.fix_after.is_some() {
            return Err(format!("run {} is being sent back", r.id));
        }
        let Some(si) = r
            .stages
            .iter()
            .position(|s| s.status != StageStatus::Passed)
        else {
            return Err(format!("run {} has nothing left to do", r.id));
        };
        let s = &mut r.stages[si];
        let asked = s.status == StageStatus::Stopped
            && s.rating()
                .is_some_and(|x| x.outcome == crate::rating::Outcome::Check);
        if !asked {
            return Err(
                "only a stage that asked a person to check something by hand can pass this way"
                    .into(),
            );
        }
        s.status = StageStatus::Passed;
        let id = r.id.clone();
        let mut changes = vec![Change {
            run: id.clone(),
            stage: Some(si),
            what: Decided::Checked(note.to_string()),
        }];
        if si + 1 == r.stages.len() {
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

    /// A person's **Go back** (§5.3): the run returns to `stage`, an earlier stage that passed,
    /// whose own session goes on as a new attempt (`Starting` until its terminal links); the
    /// stages after it wait again, their earlier tries no longer counted.
    fn go_back(&mut self, ri: usize, stage: &str, note: &str) -> Result<Vec<Change>, String> {
        let r = &self.runs[ri];
        if !matches!(r.state, RunState::NeedsYou(_)) {
            return Err(format!("run {} isn't waiting for you", r.id));
        }
        if note.is_empty() {
            return Err("going back needs a note for that stage's session".into());
        }
        let Some(t) = r.pipeline.stages.iter().position(|s| s.id == stage) else {
            return Err(format!("no stage {stage} in pipeline {}", r.pipeline.name));
        };
        let waiting = r
            .stages
            .iter()
            .position(|s| s.status != StageStatus::Passed)
            .unwrap_or(r.stages.len());
        if t >= waiting {
            return Err(format!(
                "stage {stage} isn't an earlier stage that passed: re-run the waiting one instead"
            ));
        }
        Ok(self.reopen(ri, t, note))
    }

    /// The run goes back to stage `t`, whose own session goes on as a new attempt with `note`
    /// (`Starting` until its terminal links); the stages after it wait again, their earlier
    /// tries no longer counted.
    fn reopen(&mut self, ri: usize, t: usize, note: &str) -> Vec<Change> {
        let r = &mut self.runs[ri];
        r.sessions += 1;
        let label = format!("s-{:02}", r.sessions);
        let skills = r.stages[t]
            .latest()
            .map(|a| a.skills.clone())
            .unwrap_or_default();
        let s = &mut r.stages[t];
        s.status = StageStatus::Starting;
        s.attempts.push(Attempt {
            label,
            session: None,
            session_name: None,
            started: SystemTime::now(),
            note: Some(note.to_string()),
            rating: None,
            skills,
            must: Vec::new(),
            waived: Vec::new(),
        });
        for later in &mut r.stages[t + 1..] {
            later.status = StageStatus::Waiting;
            later.note = None;
            later.counted_from = later.attempts.len();
        }
        r.state = RunState::Running;
        let id = r.id.clone();
        vec![
            Change {
                run: id.clone(),
                stage: Some(t),
                what: Decided::Reopened,
            },
            Change {
                run: id,
                stage: None,
                what: Decided::RunResumed,
            },
        ]
    }

    /// A person marks (or unmarks) reason `reason` of stage `stage`'s latest report **Must
    /// address**. Only a reason against, in an open run.
    pub fn mark(&mut self, run: &str, stage: usize, reason: usize, on: bool) -> Result<(), String> {
        self.mark_as(run, stage, reason, Mark::Must, on)
    }

    /// A person marks (or unmarks) reason `reason` of stage `stage`'s latest report `mark`:
    /// **Must address** or **Not needed**, never both. Only a reason against, in an open run.
    pub fn mark_as(
        &mut self,
        run: &str,
        stage: usize,
        reason: usize,
        mark: Mark,
        on: bool,
    ) -> Result<(), String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let r = &mut self.runs[ri];
        if !r.state.open() {
            return Err(format!("run {run} has ended"));
        }
        if r.fix_after.as_ref().is_some_and(|f| f.stage == stage) {
            return Err("this stage is already being sent back: cancel that first".into());
        }
        let Some(a) = r.stages.get_mut(stage).and_then(|s| s.attempts.last_mut()) else {
            return Err("that stage hasn't run".into());
        };
        let against = a
            .rating
            .as_ref()
            .and_then(|x| x.reasons.get(reason))
            .is_some_and(|x| x.side == Side::Against);
        if !against {
            return Err("only a reason against can be marked".into());
        }
        a.must.retain(|&i| i != reason);
        a.waived.retain(|&i| i != reason);
        if on {
            match mark {
                Mark::Must => a.must.push(reason),
                Mark::NotNeeded => a.waived.push(reason),
            }
        }
        Ok(())
    }

    /// A person's **Send back**: stage `stage` goes again to deal with the reasons they marked
    /// **Must address**. A stage that waits for them re-runs, an earlier one reopens; while a
    /// later stage is at work, it happens once that stage reports (`fix_after`).
    pub fn send_back(&mut self, run: &str, stage: usize) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let r = &self.runs[ri];
        if !r.state.open() {
            return Err(format!("run {run} has ended"));
        }
        if r.asking.is_some() {
            return Err(format!(
                "run {run} is waiting for the answer to its stage's question: answer it first"
            ));
        }
        if r.fix_after.is_some() {
            return Err(format!("run {run} is already being sent back"));
        }
        let note = r
            .stages
            .get(stage)
            .and_then(StageRun::latest)
            .and_then(must_note)
            .ok_or("mark at least one reason against Must address first")?;
        let waiting = r
            .stages
            .iter()
            .position(|s| s.status != StageStatus::Passed)
            .unwrap_or(r.stages.len());
        if stage > waiting {
            return Err("that stage hasn't run in this pass".into());
        }
        let r = &mut self.runs[ri];
        if r.stages.iter().any(|s| s.status.busy()) {
            r.fix_after = Some(Fix { stage, note });
            return Ok(Vec::new());
        }
        if stage < waiting {
            return Ok(self.reopen(ri, stage, &note));
        }
        // The stage that waits: its next attempt gets the note.
        let id = r.id.clone();
        let s = &mut r.stages[stage];
        let mut changes = Vec::new();
        if matches!(s.status, StageStatus::Stopped | StageStatus::Failed) {
            s.status = StageStatus::Waiting;
            changes.push(Change {
                run: id.clone(),
                stage: Some(stage),
                what: Decided::Retrying,
            });
        }
        s.note = Some(note);
        if matches!(r.state, RunState::NeedsYou(_)) {
            r.state = RunState::Running;
            changes.push(Change {
                run: id,
                stage: None,
                what: Decided::RunResumed,
            });
        }
        Ok(changes)
    }

    /// A person's **Score again**: the stopped stage's own session, which they worked in since
    /// its report, looks again and reports anew. The same attempt goes back to `Running`, so it
    /// isn't counted as another try.
    pub fn rescore(&mut self, run: &str, stage: usize) -> Result<Vec<Change>, String> {
        let Some(ri) = self.index(run) else {
            return Err(format!("no run {run}"));
        };
        let r = &mut self.runs[ri];
        if !matches!(r.state, RunState::NeedsYou(_)) {
            return Err(format!("run {run} isn't waiting for you"));
        }
        if r.fix_after.is_some() {
            return Err(format!("run {run} is being sent back"));
        }
        let Some(s) = r.stages.get_mut(stage) else {
            return Err("no such stage".into());
        };
        if s.status != StageStatus::Stopped {
            return Err("only a stage stopped at its gate can score again".into());
        }
        if s.latest().and_then(|a| a.session.as_deref()).is_none() {
            return Err("that stage's session isn't known".into());
        }
        s.status = StageStatus::Running;
        r.state = RunState::Running;
        let id = r.id.clone();
        Ok(vec![
            Change {
                run: id.clone(),
                stage: Some(stage),
                what: Decided::Rescoring,
            },
            Change {
                run: id,
                stage: None,
                what: Decided::RunResumed,
            },
        ])
    }

    /// Sets what run `run` is about, as a link (`{pr}` in its prompts).
    pub fn set_link(&mut self, run: &str, link: &str) {
        let link = link.trim();
        if let Some(r) = self.index(run).map(|ri| &mut self.runs[ri]) {
            r.link = (!link.is_empty()).then(|| link.to_string());
        }
    }

    /// Drops a send-back that waits for the stage at work.
    pub fn cancel_fix(&mut self, run: &str) -> Result<(), String> {
        let r = self
            .index(run)
            .map(|ri| &mut self.runs[ri])
            .ok_or(format!("no run {run}"))?;
        r.fix_after
            .take()
            .map(|_| ())
            .ok_or(format!("run {run} isn't being sent back"))
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
                            "skills": a.skills,
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

/// What hydraui adds after a stage's own prompt, kept short so the skill's own way of working
/// leads: where the stage sits, its skill, and how to report (the user's request in the
/// walkthrough: the skills know what to do from a short prompt).
fn stage_note(run: &Run, i: usize) -> String {
    let spec = &run.pipeline.stages[i];
    let skill = spec
        .skill
        .as_deref()
        .map_or(String::new(), |k| format!(" · use the {k} skill"));
    let earlier: Vec<String> = run.stages[..i]
        .iter()
        .zip(&run.pipeline.stages)
        .enumerate()
        .map(|(j, (s, spec))| match s.rating() {
            Some(r) => format!("{}. {} passed ({})", j + 1, spec.title, r.score),
            None => format!("{}. {} done", j + 1, spec.title),
        })
        .collect();
    let later: Vec<String> = run.pipeline.stages[i + 1..]
        .iter()
        .enumerate()
        .map(|(k, spec)| format!("{}. {}", i + k + 2, spec.title))
        .collect();
    let mut around = String::new();
    if !earlier.is_empty() {
        around.push_str(&format!("Earlier: {}. ", earlier.join(", ")));
    }
    if !later.is_empty() {
        around.push_str(&format!("Later: {}. ", later.join(", ")));
    }
    // The pipeline's branch rule, which the report is held to (hydraui refuses a PR on another
    // branch).
    let branch = run.pipeline.branch_for(&run.ticket).map_or(String::new(), |b| {
        format!(
            "\nName every branch you create or push exactly {b}, in each repository: no prefix, \
             no suffix, no other name. Put each pull request's link in your report; one on \
             another branch is refused."
        )
    });
    // Product decisions aren't a stage's to make (INTH-166 removed browser translation): it
    // asks, in the pipeline's Slack channel when it has one, and its ticket waits.
    let ask = match &run.pipeline.product_channel {
        Some(channel) => format!(
            "post it in Slack {channel} (the ticket, the options, your pick), then call \
             stage_ask with the question and the message's link"
        ),
        None => "call stage_ask with the question, the options and your pick".to_string(),
    };
    let product = format!(
        "\nNot your call: a change beyond the ticket that alters what users see or do (removing \
         or changing a feature, copy or a flow) is a product decision. Ask before making it: \
         {ask}, and wait. Don't ask the user whether to ask: just ask. A product question \
         already open (in the ticket, a thread, or an earlier run's handoff) counts too: call \
         stage_ask with it and its link rather than carrying it on as pending or marking its \
         test @fixme, so the run shows what holds it up."
    );
    let product = match &run.pipeline.product_style {
        Some(style) => format!("{product} How to write a product question: {style}"),
        None => product,
    };
    // A pipeline can leave product out (`[product] ask = false`), e.g. reviews.
    let product = if run.pipeline.product_asks {
        product
    } else {
        String::new()
    };
    format!(
        "---\nhydraui pipeline \"{}\" · {} · stage {} of {}: {}{skill}.\n{around}Do only this \
         stage. When it's done, call stage_report (score, evidence, reasons for and against, \
         handoff); stage_brief has what earlier stages handed over.{branch}{product}",
        run.pipeline.name,
        run.ticket,
        i + 1,
        run.stages.len(),
        spec.title
    )
}

/// What a stage sent back is told: the reasons against a person marked **Must address** on
/// attempt `a`; `None` when none are marked.
pub fn must_note(a: &Attempt) -> Option<String> {
    let rating = a.rating.as_ref()?;
    let items: Vec<&str> = a
        .must
        .iter()
        .filter_map(|&i| rating.reasons.get(i))
        .map(|x| x.text.as_str())
        .collect();
    if items.is_empty() {
        return waived_note(a);
    }
    let mut note = format!(
        "The user marked these reasons against your report ({}) Must address. Deal with each          one, then report again, saying in your reasons how each was dealt with:",
        a.label
    );
    for item in items {
        note.push_str(&format!("\n- {item}"));
    }
    if let Some(waived) = waived_note(a) {
        note.push_str("\n\n");
        note.push_str(&waived);
    }
    Some(note)
}

/// What a session is told about the reasons against a person marked **Not needed**.
pub fn waived_note(a: &Attempt) -> Option<String> {
    let rating = a.rating.as_ref()?;
    let items: Vec<&str> = a
        .waived
        .iter()
        .filter_map(|&i| rating.reasons.get(i))
        .map(|x| x.text.as_str())
        .collect();
    if items.is_empty() {
        return None;
    }
    let mut note = format!(
        "The user marked these reasons against your report ({}) Not needed: don't act on \
         them, and don't count them against your score again:",
        a.label
    );
    for item in items {
        note.push_str(&format!("\n- {item}"));
    }
    Some(note)
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
    for (i, reason) in rating.reasons.iter().enumerate() {
        let sign = match reason.side {
            Side::For => '+',
            Side::Against => '-',
        };
        let waived = if a.waived.contains(&i) {
            " (the user says this isn't needed: don't act on it)"
        } else {
            ""
        };
        text.push_str(&format!("\n{sign} {}{waived}", reason.text));
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
        assert_eq!(s.folder, None);
        assert!(s.prompt.starts_with("do a."), "{}", s.prompt);
        assert!(s.prompt.contains("Look at the API."), "{}", s.prompt);
        assert_eq!(status(&runs, &id, 0), StageStatus::Starting);
        runs.link(&id, 0, "tok-a", "s-01 · a");
        assert_eq!(status(&runs, &id, 0), StageStatus::Running);
        assert!(matches!(runs.role("tok-a"), Role::Stage(r, 0) if r.id == id));
        assert!(matches!(runs.role("orch"), Role::Orchestrator(r) if r.id == id));
        assert!(matches!(runs.role("other"), Role::Other));
    }

    /// A run of a, b where a passed (session `t1`) and b scored below its bar (session `t2`).
    fn b_stopped(runs: &mut Runs) -> String {
        let id = run(runs, &["a", "b"]);
        started(runs, &id, "a", "t1");
        runs.skill_used("t1", "qa");
        runs.report("t1", rated(90)).unwrap();
        started(runs, &id, "b", "t2");
        runs.report("t2", rated(40)).unwrap();
        assert!(matches!(
            runs.run(&id).unwrap().state,
            RunState::NeedsYou(_)
        ));
        id
    }

    fn back(stage: &str, note: &str) -> Choice {
        Choice::Back {
            stage: stage.to_string(),
            note: note.to_string(),
        }
    }

    #[test]
    fn a_stage_asks_and_its_ticket_waits_for_the_answer() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "s1");
        let now = SystemTime::now();
        assert!(runs.ask("nobody", "Remove it?", None, now).is_err());
        assert!(runs.ask("s1", "  ", None, now).is_err());
        let changes = runs
            .ask(
                "s1",
                "Remove auto-translate?",
                Some("https://x.slack.com/t/1"),
                now,
            )
            .unwrap();
        assert_eq!(
            changes[0].what,
            Decided::Asked("Remove auto-translate?".into())
        );
        let r = runs.run(&id).unwrap();
        assert!(
            matches!(&r.state, RunState::NeedsYou(why) if why.contains("Remove auto-translate?"))
        );
        assert_eq!(
            r.asking.as_ref().unwrap().link.as_deref(),
            Some("https://x.slack.com/t/1")
        );
        assert!(
            runs.ask("s1", "Again?", None, now).is_err(),
            "one question at a time"
        );
        // A person's re-run or go-back would lose the question; only stopping is allowed.
        assert!(runs.person(&id, retry("redo")).is_err());
        assert!(runs.answered(&id, " ").is_err());
        let changes = runs.answered(&id, "Keep it, product says").unwrap();
        assert_eq!(
            changes[0].what,
            Decided::Answered("Keep it, product says".into())
        );
        let r = runs.run(&id).unwrap();
        assert_eq!(r.state, RunState::Running);
        assert!(r.asking.is_none());
        assert!(runs.answered(&id, "twice").is_err());
    }

    #[test]
    fn reporting_or_failing_while_asking_drops_the_question() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "s1");
        runs.ask("s1", "Q?", None, SystemTime::now()).unwrap();
        runs.report("s1", rated(90)).unwrap();
        let r = runs.run(&id).unwrap();
        assert!(r.asking.is_none());
        assert_eq!(r.state, RunState::Running);
        started(&mut runs, &id, "b", "s2");
        runs.ask("s2", "Q2?", None, SystemTime::now()).unwrap();
        runs.session_gone("s2");
        let r = runs.run(&id).unwrap();
        assert!(r.asking.is_none());
        assert_eq!(r.state, RunState::Running);
    }

    #[test]
    fn a_person_sends_the_run_back_to_an_earlier_stage() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        let changes = runs
            .person(&id, back("a", "rename the branch to INTH-1"))
            .unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c.what == Decided::Reopened && c.stage == Some(0))
        );
        let r = runs.run(&id).unwrap();
        assert_eq!(r.state, RunState::Running);
        // The earlier stage goes on in its own conversation: a new attempt, starting.
        assert_eq!(status(&runs, &id, 0), StageStatus::Starting);
        let a = runs.run(&id).unwrap().stages[0].latest().unwrap().clone();
        assert_eq!(runs.run(&id).unwrap().stages[0].attempts.len(), 2);
        assert_eq!(a.note.as_deref(), Some("rename the branch to INTH-1"));
        assert_eq!(a.label, "s-03");
        assert_eq!(a.skills, ["qa"], "its conversation already used its skill");
        // The later stage waits for it, its earlier tries no longer counted.
        assert_eq!(status(&runs, &id, 1), StageStatus::Waiting);
        assert_eq!(runs.run(&id).unwrap().stages[1].tries(), 0);
        // Reopened, it reports and passes; the later stage may start again.
        runs.link(&id, 0, "t3", "1. a");
        runs.report("t3", rated(92)).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Passed);
        assert!(runs.begin("orch", &id, "b", "", 30).is_ok());
    }

    #[test]
    fn only_a_reason_against_is_marked_must_address() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        assert!(runs.mark(&id, 1, 0, true).is_err(), "reason 0 is for");
        assert!(runs.mark(&id, 1, 5, true).is_err(), "no reason 5");
        let err = runs.send_back(&id, 1).unwrap_err();
        assert!(err.contains("Must address"), "{err}");
        runs.mark(&id, 1, 1, true).unwrap();
        runs.mark(&id, 1, 1, true).unwrap();
        assert_eq!(runs.run(&id).unwrap().stages[1].latest().unwrap().must, [1]);
        runs.mark(&id, 1, 1, false).unwrap();
        assert!(
            runs.run(&id).unwrap().stages[1]
                .latest()
                .unwrap()
                .must
                .is_empty()
        );
    }

    #[test]
    fn a_stopped_stage_sent_back_reruns_told_what_it_must_address() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        runs.mark(&id, 1, 1, true).unwrap();
        let changes = runs.send_back(&id, 1).unwrap();
        assert!(changes.iter().any(|c| c.what == Decided::Retrying));
        let r = runs.run(&id).unwrap();
        assert_eq!(r.state, RunState::Running);
        assert_eq!(status(&runs, &id, 1), StageStatus::Waiting);
        let note = r.stages[1].note.clone().unwrap();
        assert!(
            note.contains("Must address") && note.contains("\n- q"),
            "{note}"
        );
        let s = runs.begin("orch", &id, "b", "", 30).unwrap();
        assert!(s.prompt.contains("- q"), "{}", s.prompt);
    }

    fn wants_check(score: u8) -> Rating {
        let mut r = rated(score);
        r.outcome = rating::Outcome::Check;
        r.check = "Open it in real Chrome.".into();
        r
    }

    fn checked(note: &str) -> Choice {
        Choice::Checked {
            note: note.to_string(),
        }
    }

    #[test]
    fn a_stage_asking_for_a_check_by_hand_passes_when_a_person_says_it_works() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b", "c"]);
        started(&mut runs, &id, "a", "t1");
        runs.skill_used("t1", "qa");
        runs.report("t1", rated(90)).unwrap();
        started(&mut runs, &id, "b", "t2");
        let changes = runs.report("t2", wants_check(95)).unwrap();
        let RunState::NeedsYou(why) = &runs.run(&id).unwrap().state else {
            panic!("waits for a person");
        };
        assert!(why.contains("needs a person to check: Open it"), "{why}");
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.what, Decided::Stopped(_)))
        );
        let changes = runs.person(&id, checked(" Works in Chrome 155 ")).unwrap();
        assert!(changes.iter().any(
            |c| c.stage == Some(1) && c.what == Decided::Checked("Works in Chrome 155".into())
        ));
        assert_eq!(status(&runs, &id, 1), StageStatus::Passed);
        assert_eq!(runs.run(&id).unwrap().state, RunState::Running);
        assert!(
            runs.begin("orch", &id, "c", "", 30).is_ok(),
            "the next stage may start"
        );
    }

    #[test]
    fn the_last_stage_checked_by_hand_finishes_the_run() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        started(&mut runs, &id, "a", "t1");
        runs.skill_used("t1", "qa");
        runs.report("t1", wants_check(80)).unwrap();
        let changes = runs.person(&id, checked("")).unwrap();
        assert!(changes.iter().any(|c| c.what == Decided::RunFinished));
        assert_eq!(runs.run(&id).unwrap().state, RunState::Finished);
    }

    #[test]
    fn only_a_stage_that_asked_for_a_check_passes_by_a_persons_word() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        let e = runs.person(&id, checked("fine")).unwrap_err();
        assert!(e.contains("asked a person to check"), "{e}");
        assert_eq!(status(&runs, &id, 1), StageStatus::Stopped);
    }

    #[test]
    fn a_concern_marked_not_needed_is_told_to_the_next_session_and_isnt_also_a_must() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        runs.mark(&id, 1, 1, true).unwrap();
        runs.mark_as(&id, 1, 1, Mark::NotNeeded, true).unwrap();
        let a = runs.run(&id).unwrap().stages[1].latest().unwrap().clone();
        assert!(a.must.is_empty(), "one mark at a time");
        assert_eq!(a.waived, [1]);
        let note = must_note(&a).expect("a note for the not-needed concern");
        assert!(
            note.contains("Not needed") && note.contains("\n- q"),
            "{note}"
        );
        assert!(
            runs.mark_as(&id, 1, 0, Mark::NotNeeded, true).is_err(),
            "a reason for"
        );
        runs.person(&id, retry("again")).unwrap();
        let s = runs.begin("orch", &id, "b", "", 30).unwrap();
        assert!(s.prompt.contains("isn't needed"), "{}", s.prompt);
    }

    #[test]
    fn a_ticket_settled_outside_ends_its_run_as_done_whatever_its_gate_said() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        let changes = runs.settle(&id, "its pull request was merged").unwrap();
        assert_eq!(
            changes[0].what,
            Decided::RunSettled("its pull request was merged".into())
        );
        assert_eq!(runs.run(&id).unwrap().state, RunState::Finished);
        assert!(runs.settle(&id, "again").is_err(), "already ended");
        assert!(runs.open_of("orch").is_empty());
    }

    #[test]
    fn one_orchestrator_holds_up_to_runs_at_once_tickets() {
        let mut runs = Runs::default();
        let mut p = pipeline(&["a"]);
        p.runs_at_once = 2;
        let first = runs
            .start_run("orch", p.clone(), "PR-1", "x")
            .unwrap()
            .id
            .clone();
        let e = runs.start_run("orch", p.clone(), "pr-1", "x").unwrap_err();
        assert!(e.contains("already running as PR-1-1"), "{e}");
        let second = runs
            .start_run("orch", p.clone(), "PR-2", "y")
            .unwrap()
            .id
            .clone();
        let e = runs.start_run("orch", p.clone(), "PR-3", "z").unwrap_err();
        assert!(
            e.contains("2 tickets are open already (PR-1-1, PR-2-1)"),
            "{e}"
        );
        let open: Vec<&str> = runs.open_of("orch").iter().map(|r| r.id.as_str()).collect();
        assert_eq!(open, [first.as_str(), second.as_str()]);
        // Each run's stages go on by themselves.
        started(&mut runs, &first, "a", "t1");
        started(&mut runs, &second, "a", "t2");
        runs.report("t2", rated(90)).unwrap();
        assert_eq!(runs.open_of("orch").len(), 1);
        assert!(runs.start_run("orch", p, "PR-3", "z").is_ok());
    }

    #[test]
    fn a_stopped_stage_scores_again_in_its_own_session_without_using_a_try() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        assert!(runs.rescore(&id, 0).is_err(), "a passed stage");
        let changes = runs.rescore(&id, 1).unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c.what == Decided::Rescoring && c.stage == Some(1))
        );
        assert_eq!(runs.run(&id).unwrap().state, RunState::Running);
        assert_eq!(status(&runs, &id, 1), StageStatus::Running);
        assert!(runs.rescore(&id, 1).is_err(), "already scoring");
        // The same session reports again, on the same attempt.
        runs.report("t2", rated(92)).unwrap();
        let r = runs.run(&id).unwrap();
        assert_eq!(r.stages[1].attempts.len(), 1);
        assert_eq!(r.stages[1].rating().unwrap().score, 92);
        assert_eq!(r.state, RunState::Finished);
    }

    #[test]
    fn an_earlier_stage_sent_back_while_another_runs_goes_once_it_reports() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "t1");
        runs.report("t1", rated(90)).unwrap();
        started(&mut runs, &id, "b", "t2");
        runs.mark(&id, 0, 1, true).unwrap();
        assert_eq!(runs.send_back(&id, 0).unwrap(), []);
        assert!(runs.send_back(&id, 0).is_err(), "already queued");
        assert!(runs.mark(&id, 0, 1, false).is_err(), "cancel first");
        // b passes, but the run goes back to a rather than finishing.
        let changes = runs.report("t2", rated(95)).unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c.what == Decided::Reopened && c.stage == Some(0))
        );
        assert!(!changes.iter().any(|c| c.what == Decided::RunFinished));
        let r = runs.run(&id).unwrap();
        assert_eq!(r.state, RunState::Running);
        assert_eq!(r.fix_after, None);
        assert_eq!(status(&runs, &id, 0), StageStatus::Starting);
        assert_eq!(status(&runs, &id, 1), StageStatus::Waiting);
        let note = r.stages[0].latest().unwrap().note.clone().unwrap();
        assert!(note.contains("- q"), "{note}");
    }

    #[test]
    fn a_queued_send_back_goes_when_the_running_stage_fails_or_is_cancelled() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "t1");
        runs.report("t1", rated(90)).unwrap();
        started(&mut runs, &id, "b", "t2");
        runs.mark(&id, 0, 1, true).unwrap();
        runs.send_back(&id, 0).unwrap();
        runs.cancel_fix(&id).unwrap();
        assert!(runs.cancel_fix(&id).is_err());
        runs.send_back(&id, 0).unwrap();
        let changes = runs.session_gone("t2");
        assert!(
            changes
                .iter()
                .any(|c| c.what == Decided::Reopened && c.stage == Some(0))
        );
        assert_eq!(status(&runs, &id, 0), StageStatus::Starting);
    }

    #[test]
    fn going_back_is_only_to_an_earlier_passed_stage_with_a_note() {
        let mut runs = Runs::default();
        let id = b_stopped(&mut runs);
        assert!(
            runs.person(&id, back("a", "  "))
                .unwrap_err()
                .contains("note")
        );
        assert!(
            runs.person(&id, back("b", "x"))
                .unwrap_err()
                .contains("earlier")
        );
        assert!(
            runs.person(&id, back("zz", "x"))
                .unwrap_err()
                .contains("zz")
        );
        runs.person(&id, back("a", "fix")).unwrap();
        // Running again: nothing to go back from.
        assert!(
            runs.person(&id, back("a", "again"))
                .unwrap_err()
                .contains("waiting")
        );
    }

    #[test]
    fn a_later_stage_gets_its_three_tries_again_after_going_back() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        started(&mut runs, &id, "a", "t1");
        runs.report("t1", rated(90)).unwrap();
        for tok in ["b1", "b2", "b3"] {
            started(&mut runs, &id, "b", tok);
            runs.session_gone(tok);
        }
        assert!(matches!(
            runs.run(&id).unwrap().state,
            RunState::NeedsYou(_)
        ));
        runs.person(&id, back("a", "fix the env")).unwrap();
        runs.link(&id, 0, "t2", "1. a");
        runs.report("t2", rated(90)).unwrap();
        started(&mut runs, &id, "b", "b4");
        runs.session_gone("b4");
        assert_eq!(status(&runs, &id, 1), StageStatus::Failed);
        assert_eq!(
            runs.run(&id).unwrap().state,
            RunState::Running,
            "1 try of 3, not 4"
        );
    }

    #[test]
    fn every_stage_is_told_the_branch_it_must_use() {
        let mut runs = Runs::default();
        let mut p = pipeline(&["develop"]);
        p.branch = Some("{ticket}".into());
        let id = runs
            .start_run("orch", p, "PRE-151", "x")
            .unwrap()
            .id
            .clone();
        let s = runs.begin("orch", &id, "develop", "", 30).unwrap();
        assert!(
            s.prompt
                .contains("Name every branch you create or push exactly PRE-151"),
            "{}",
            s.prompt
        );
        assert!(s.prompt.contains("pull request's link"), "{}", s.prompt);
        // No rule, no line.
        let mut plain = Runs::default();
        let id = plain
            .start_run("orch", pipeline(&["develop"]), "PRE-152", "x")
            .unwrap()
            .id
            .clone();
        let s = plain.begin("orch", &id, "develop", "", 30).unwrap();
        assert!(!s.prompt.contains("Name every branch"), "{}", s.prompt);
    }

    #[test]
    fn a_restored_run_moves_to_its_new_terminals() {
        let mut old = Runs::default();
        let id = run(&mut old, &["a", "b"]);
        started(&mut old, &id, "a", "s-01");
        let replayed = old.run(&id).unwrap().clone();

        let mut runs = Runs::default();
        runs.restore(replayed);
        runs.adopt(&id, "orch-2");
        runs.relink(&id, 0, "tok-2", "1. a");
        assert!(matches!(runs.role("orch-2"), Role::Orchestrator(r) if r.id == id));
        assert!(matches!(runs.role("tok-2"), Role::Stage(r, 0) if r.id == id));
        assert!(
            matches!(runs.role("s-01"), Role::Other),
            "the old session has no say"
        );
        assert_eq!(status(&runs, &id, 0), StageStatus::Running);
        assert_eq!(
            runs.run(&id).unwrap().stages[0].attempts.len(),
            1,
            "not a new attempt"
        );
        // The new stage session reports, and the run goes on as before.
        runs.report("tok-2", rated(95)).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Passed);
        // Restoring it again replaces it rather than adding a second.
        runs.restore(old.run(&id).unwrap().clone());
        assert_eq!(runs.all().count(), 1);
    }

    #[test]
    fn a_session_is_named_after_its_stage() {
        assert_eq!(
            session_name(0, "Understand and replicate", 0),
            "1. Understand and replicate"
        );
        assert_eq!(session_name(1, "Develop", 1), "2. Develop · try 2");
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        let s = runs.begin("orch", &id, "a", "", 30).unwrap();
        assert_eq!(s.name, "1. a");
        runs.start_failed(&id, 0, "no terminal");
        assert_eq!(
            runs.begin("orch", &id, "a", "", 30).unwrap().name,
            "1. a · try 2"
        );
    }

    #[test]
    fn a_stage_with_a_skill_reports_only_after_using_it() {
        let mut runs = Runs::default();
        let p = pipeline::parse(
            "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nskill = \"qa\"\n\
             prompt = \"Replicate {ticket}.\"\n",
        )
        .unwrap();
        let id = runs.start_run("orch", p, "INTH-1", "x").unwrap().id.clone();
        let s = runs.begin("orch", &id, "a", "", 30).unwrap();
        // The prompt as written, then the note naming the settings' skill.
        assert!(s.prompt.starts_with("Replicate INTH-1."), "{}", s.prompt);
        assert!(s.prompt.contains("use the qa skill"), "{}", s.prompt);
        runs.link(&id, 0, "tok", "1. A");
        let refused = runs.report("tok", rated(95)).unwrap_err();
        assert!(refused.contains("qa"), "{refused}");
        assert_eq!(status(&runs, &id, 0), StageStatus::Running);
        // Another skill, or another session using this one, doesn't count.
        runs.skill_used("tok", "angular");
        runs.skill_used("someone-else", "qa");
        assert!(runs.report("tok", rated(95)).is_err());
        // A plugin's copy of it does.
        runs.skill_used("tok", "iov42:QA");
        assert_eq!(
            runs.run(&id).unwrap().stages[0].attempts[0].skills,
            ["angular", "iov42:QA"]
        );
        assert!(runs.report("tok", rated(95)).is_ok());
    }

    #[test]
    fn a_stage_prompt_is_the_pipelines_with_a_short_note() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["replicate", "develop", "verify"]);
        started(&mut runs, &id, "replicate", "t1");
        runs.report("t1", rated(88)).unwrap();
        let p = runs.begin("orch", &id, "develop", "", 30).unwrap().prompt;
        // The pipeline's own words first, as the user would type them.
        assert!(p.starts_with("do develop. after 88"), "{p}");
        let note = &p[p.find("\n\n---\n").expect("a note after the prompt")..];
        // Short beside the pipeline's own words, though the product rule is in every note.
        assert!(note.len() < 800, "{} chars: {note}", note.len());
        for says in [
            "stage 2 of 3: develop",
            "Earlier: 1. replicate passed (88)",
            "Later: 3. verify",
            "stage_report",
            "stage_brief",
        ] {
            assert!(note.contains(says), "{says}: {note}");
        }
    }

    #[test]
    fn every_stage_is_told_to_ask_before_a_product_decision() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["develop"]);
        let s = runs.begin("orch", &id, "develop", "", 30).unwrap();
        let p = s.prompt;
        assert!(p.contains("is a product decision"), "{p}");
        assert!(p.contains("call stage_ask with the question"), "{p}");
        assert!(p.contains("Don't ask the user whether to ask"), "{p}");
        assert!(p.contains("already open"), "{p}");
        assert!(!p.contains("Slack"), "{p}");
        assert_eq!(
            s.claude,
            ["claude"],
            "nothing to pre-approve without a channel"
        );

        let mut runs = Runs::default();
        let mut pipe = pipeline(&["develop"]);
        pipe.product_channel = Some("#product-".into());
        let id = runs
            .start_run("orch", pipe, "INTH-166", "x")
            .unwrap()
            .id
            .clone();
        let s = runs.begin("orch", &id, "develop", "", 30).unwrap();
        assert_eq!(
            s.claude,
            [
                "claude".to_string(),
                format!("--allowedTools={}", crate::pipeline::PRODUCT_TOOLS)
            ],
            "posting the question and reading replies never wait for the user"
        );
        let p = s.prompt;
        assert!(p.contains("post it in Slack #product-"), "{p}");
        assert!(p.contains("the message's link"), "{p}");
        assert!(!p.contains("How to write a product question"), "{p}");

        let mut runs = Runs::default();
        let mut pipe = pipeline(&["develop"]);
        pipe.product_style = Some("Keep it short.".into());
        let id = runs
            .start_run("orch", pipe, "INTH-167", "x")
            .unwrap()
            .id
            .clone();
        let p = runs.begin("orch", &id, "develop", "", 30).unwrap().prompt;
        assert!(
            p.contains("How to write a product question: Keep it short."),
            "{p}"
        );

        // A pipeline that leaves product out: no rule, no pre-approved tools, no stage_ask.
        let mut runs = Runs::default();
        let mut pipe = pipeline(&["review"]);
        pipe.product_channel = Some("#product-".into());
        pipe.product_asks = false;
        let id = runs
            .start_run("orch", pipe, "version2-frontend-5536", "x")
            .unwrap()
            .id
            .clone();
        let s = runs.begin("orch", &id, "review", "tok", 30).unwrap();
        assert!(!s.prompt.contains("product decision"), "{}", s.prompt);
        assert_eq!(s.claude, ["claude"]);
    }

    #[test]
    fn a_stage_starts_in_its_folder() {
        let mut runs = Runs::default();
        let p = pipeline::parse(
            "name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nfolder = \"~/Projects/QA-Skills\"\n",
        )
        .unwrap();
        let id = runs.start_run("orch", p, "INTH-1", "x").unwrap().id.clone();
        let s = runs.begin("orch", &id, "a", "", 30).unwrap();
        assert_eq!(s.folder.as_deref(), Some("~/Projects/QA-Skills"));
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
        now.name = "Renamed".into();
        now.color = Some(0xd8c35a);
        now.product_style = Some("Keep it short.".into());
        runs.refresh_from_file("t", &now);
        // A new name and colour show at once (the design: the card follows Save).
        assert_eq!(runs.run(&id).unwrap().pipeline.name, "Renamed");
        assert_eq!(runs.run(&id).unwrap().pipeline.color, Some(0xd8c35a));
        assert_eq!(
            runs.run(&id).unwrap().pipeline.product_style.as_deref(),
            Some("Keep it short.")
        );
        // A stage list change waits for the next run; the bar applies now.
        let r = runs.run(&id).unwrap();
        assert_eq!(r.pipeline.stages[0].id, "a");
        assert_eq!(r.pipeline.bar_for(0), 95);
        runs.report("tok-a", rated(90)).unwrap();
        assert_eq!(status(&runs, &id, 0), StageStatus::Stopped);
        // Another pipeline's file changes nothing.
        let mut other = pipeline(&["a"]);
        other.stages[0].bar = Some(10);
        runs.refresh_from_file("other", &other);
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
    fn stage_settings_reach_stages_not_yet_started() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a", "b"]);
        let mut now = pipeline(&["a", "b"]);
        now.stages[1].folder = Some("~/Projects/QA-Skills".into());
        now.stages[1].skill = Some("qa".into());
        now.stages[1].prompt = "Use the qa skill.".into();
        runs.refresh_from_file("t", &now);
        started(&mut runs, &id, "a", "tok-a");
        runs.report("tok-a", rated(90)).unwrap();
        let b = runs.begin("orch", &id, "b", "", 30).unwrap();
        assert_eq!(b.folder.as_deref(), Some("~/Projects/QA-Skills"));
        // The stage's new prompt, then the note naming its new skill.
        assert!(b.prompt.starts_with("Use the qa skill."), "{}", b.prompt);
        assert!(b.prompt.contains("use the qa skill"), "{}", b.prompt);
        assert_eq!(
            runs.run(&id).unwrap().pipeline.stages[1].skill.as_deref(),
            Some("qa")
        );
    }

    #[test]
    fn run_ids_skip_ones_already_logged() {
        let mut runs = Runs::default();
        let taken = |id: &str| id == "INTH-1-1" || id == "INTH-1-2";
        let r = runs
            .start_run_avoiding("orch", pipeline(&["a"]), "INTH-1", "x", taken)
            .unwrap();
        assert_eq!(r.id, "INTH-1-3");
    }

    #[test]
    fn a_run_keeps_its_worktree() {
        let mut runs = Runs::default();
        let id = run(&mut runs, &["a"]);
        runs.set_worktree(&id, "C:/r-worktrees/inth-1-1".into());
        assert_eq!(
            runs.run(&id).unwrap().worktree.as_deref(),
            Some(std::path::Path::new("C:/r-worktrees/inth-1-1"))
        );
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
