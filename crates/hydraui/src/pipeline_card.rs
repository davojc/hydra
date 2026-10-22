//! The pipeline card (orchestrator spec §7.1, §7.2; the design's sidebar card): what it says
//! about a run, which terminals sit under it, and the seven-day allowance meter.

use crate::runs::{Run, RunState, StageStatus};

/// The card's state pill (the design: Working, Needs you, Paused, Done; Polling comes with
/// Linear in phase 2). `Stopped` is a ticket a person ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    /// Started from the launcher, waiting for a ticket.
    Ready,
    Working,
    NeedsYou,
    Paused,
    Done,
    Stopped,
}

impl CardState {
    pub fn word(self) -> &'static str {
        match self {
            CardState::Ready => "Ready",
            CardState::Working => "Working",
            CardState::NeedsYou => "Needs you",
            CardState::Paused => "Paused",
            CardState::Done => "Done",
            CardState::Stopped => "Stopped",
        }
    }
}

/// One stage's segment of the card's progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Passed,
    Running,
    Stopped,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// The pipeline's name.
    pub name: String,
    /// `<ticket> · <current stage's first word>`, or `· Done`.
    pub line: String,
    pub state: CardState,
    pub segments: Vec<Segment>,
}

/// What the card says about `run`.
pub fn card(run: &Run) -> Card {
    let state = match &run.state {
        RunState::Finished => CardState::Done,
        RunState::Stopped(_) => CardState::Stopped,
        RunState::NeedsYou(_) => CardState::NeedsYou,
        RunState::Running if run.paused => CardState::Paused,
        RunState::Running => CardState::Working,
    };
    let current = run
        .stages
        .iter()
        .position(|s| matches!(s.status, StageStatus::Starting | StageStatus::Running))
        .or_else(|| {
            run.stages
                .iter()
                .position(|s| s.status != StageStatus::Passed)
        });
    let stage = match (state, current) {
        (CardState::Done, _) | (_, None) => "Done",
        (_, Some(i)) => run.pipeline.stages[i]
            .title
            .split_whitespace()
            .next()
            .unwrap_or("?"),
    };
    let segments = run
        .stages
        .iter()
        .map(|s| match s.status {
            StageStatus::Passed => Segment::Passed,
            StageStatus::Starting | StageStatus::Running => Segment::Running,
            StageStatus::Stopped | StageStatus::Failed => Segment::Stopped,
            StageStatus::Waiting => Segment::Pending,
        })
        .collect();
    Card {
        name: run.pipeline.name.clone(),
        line: format!("{} · {stage}", run.ticket),
        state,
        segments,
    }
}

/// The card of an orchestrator started from the launcher before it has a ticket: the pipeline
/// it will run, so its settings and history are one click away.
pub fn ready_card(pipeline: &crate::pipeline::Pipeline) -> Card {
    Card {
        name: pipeline.name.clone(),
        line: "No ticket yet".into(),
        state: CardState::Ready,
        segments: vec![Segment::Pending; pipeline.stages.len()],
    }
}

/// The terminals grouped under the card, by token: the orchestrator's, then every attempt's
/// session that opened, in stage and attempt order.
pub fn card_tokens(run: &Run) -> Vec<String> {
    std::iter::once(run.orchestrator.clone())
        .chain(
            run.stages
                .iter()
                .flat_map(|s| &s.attempts)
                .filter_map(|a| a.session.clone()),
        )
        .collect()
}

/// What closing hydraui would end (the window asks first): each run still open, by its status
/// line; `None` when nothing is running or waiting for a person.
pub fn quit_warning(runs: &[Run]) -> Option<String> {
    let open: Vec<String> = runs
        .iter()
        .filter(|r| r.state.open())
        .map(|r| status_text(r).0)
        .collect();
    let what = match open.len() {
        0 => return None,
        1 => "this ticket's run",
        _ => "these tickets' runs",
    };
    Some(format!(
        "Closing hydraui ends {what}: its sessions stop, and a restart won't pick it up again.\n{}",
        open.join("\n")
    ))
}

/// The orchestrator's line in the status bar (the design's status text): the pipeline, the
/// ticket, and what the run is doing or why it waits; `true` when it needs a person (red).
pub fn status_text(run: &Run) -> (String, bool) {
    let busy = run
        .stages
        .iter()
        .position(|s| s.status.busy())
        .and_then(|i| Some((i, run.stages[i].latest()?)));
    let (what, red) = match (&run.state, busy) {
        (RunState::NeedsYou(why), _) => (format!("needs you: {why}"), true),
        (RunState::Finished, _) => ("done: every stage passed".to_string(), false),
        (RunState::Stopped(why), _) => (format!("stopped: {why}"), false),
        (RunState::Running, _) if run.paused => (
            "paused: no stage starts until you resume".to_string(),
            false,
        ),
        (RunState::Running, Some((i, _))) => (
            format!(
                "{} working",
                crate::runs::session_name(
                    i,
                    &run.pipeline.stages[i].title,
                    run.stages[i].attempts.len().saturating_sub(1)
                )
            ),
            false,
        ),
        (RunState::Running, None) => ("waiting for the orchestrator".to_string(), false),
    };
    (
        format!("{} · {} · {what}", run.pipeline.name, run.ticket),
        red,
    )
}

/// The seven-day allowance meter (the design's footer): how much is left, 0 to 1, and whether
/// that is below the floor at which no stage starts (§5.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    pub left: f32,
    pub low: bool,
}

/// The meter for `week_used` percent used, with the floor at `floor` percent left; `None`
/// before any session reported.
pub fn meter(week_used: Option<f64>, floor: u8) -> Option<Meter> {
    let left = (100.0 - week_used?).clamp(0.0, 100.0);
    Some(Meter {
        left: (left / 100.0) as f32,
        low: left < f64::from(floor),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runs::Runs;

    fn runs_of(stages: &[(&str, &str)]) -> (Runs, String) {
        let mut t = String::from("name = \"INTH SLA tickets\"\n");
        for (id, title) in stages {
            t += &format!("[[stage]]\nid = \"{id}\"\ntitle = \"{title}\"\n");
        }
        let mut runs = Runs::default();
        let p = crate::pipeline::parse(&t).unwrap();
        let id = runs
            .start_run("orch", p, "INTH-482", "x")
            .unwrap()
            .id
            .clone();
        (runs, id)
    }

    fn start(runs: &mut Runs, id: &str, stage: &str, tok: &str) {
        let s = runs.begin("orch", id, stage, "", 30).unwrap();
        runs.link(id, s.stage, tok, tok);
    }

    fn rated(score: u8) -> crate::rating::Rating {
        crate::rating::from_args(&json!({"outcome": "done", "score": score, "summary": "s",
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "for", "text": "r"}]}))
        .unwrap()
    }

    const STAGES: [(&str, &str); 3] = [
        ("replicate", "Understand & replicate"),
        ("develop", "Develop"),
        ("verify", "Verify"),
    ];

    #[test]
    fn the_card_names_the_ticket_and_current_stage() {
        let (mut runs, id) = runs_of(&STAGES);
        let c = card(runs.run(&id).unwrap());
        assert_eq!(c.name, "INTH SLA tickets");
        assert_eq!(c.line, "INTH-482 · Understand");
        assert_eq!(c.state, CardState::Working);
        start(&mut runs, &id, "replicate", "t1");
        runs.report("t1", rated(92)).unwrap();
        start(&mut runs, &id, "develop", "t2");
        assert_eq!(card(runs.run(&id).unwrap()).line, "INTH-482 · Develop");
        runs.pause(&id, true).unwrap();
        assert_eq!(card(runs.run(&id).unwrap()).state, CardState::Paused);
        runs.pause(&id, false).unwrap();
        runs.report("t2", rated(10)).unwrap();
        let c = card(runs.run(&id).unwrap());
        assert_eq!(
            (c.line.as_str(), c.state),
            ("INTH-482 · Develop", CardState::NeedsYou)
        );
        runs.person(&id, crate::runs::Choice::Stop).unwrap();
        assert_eq!(card(runs.run(&id).unwrap()).state, CardState::Stopped);
    }

    #[test]
    fn an_orchestrator_without_a_ticket_is_ready() {
        let p = crate::pipeline::parse(
            "name = \"INTH SLA tickets\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n\
             [[stage]]\nid = \"b\"\ntitle = \"B\"\n",
        )
        .unwrap();
        let c = ready_card(&p);
        assert_eq!(c.name, "INTH SLA tickets");
        assert_eq!(c.line, "No ticket yet");
        assert_eq!(c.state, CardState::Ready);
        assert_eq!(c.state.word(), "Ready");
        assert_eq!(c.segments, [Segment::Pending, Segment::Pending]);
    }

    #[test]
    fn a_finished_run_is_done() {
        let (mut runs, id) = runs_of(&STAGES[..1]);
        start(&mut runs, &id, "replicate", "t1");
        runs.report("t1", rated(95)).unwrap();
        let c = card(runs.run(&id).unwrap());
        assert_eq!(
            (c.line.as_str(), c.state),
            ("INTH-482 · Done", CardState::Done)
        );
    }

    #[test]
    fn segments_follow_stage_statuses() {
        use Segment::*;
        let (mut runs, id) = runs_of(&STAGES);
        assert_eq!(
            card(runs.run(&id).unwrap()).segments,
            [Pending, Pending, Pending]
        );
        start(&mut runs, &id, "replicate", "t1");
        assert_eq!(
            card(runs.run(&id).unwrap()).segments,
            [Running, Pending, Pending]
        );
        runs.report("t1", rated(92)).unwrap();
        start(&mut runs, &id, "develop", "t2");
        runs.report("t2", rated(10)).unwrap();
        assert_eq!(
            card(runs.run(&id).unwrap()).segments,
            [Passed, Stopped, Pending]
        );
    }

    #[test]
    fn card_tokens_follow_the_orchestrator() {
        let (mut runs, id) = runs_of(&STAGES);
        start(&mut runs, &id, "replicate", "t1");
        runs.session_gone("t1");
        start(&mut runs, &id, "replicate", "t2");
        runs.report("t2", rated(92)).unwrap();
        // Not linked yet: no token to show.
        runs.begin("orch", &id, "develop", "", 30).unwrap();
        assert_eq!(card_tokens(runs.run(&id).unwrap()), ["orch", "t1", "t2"]);
    }

    #[test]
    fn closing_warns_only_while_a_ticket_is_open() {
        let (mut runs, id) = runs_of(&STAGES);
        start(&mut runs, &id, "replicate", "t1");
        let open = runs.run(&id).unwrap().clone();
        let warning = quit_warning(std::slice::from_ref(&open)).unwrap();
        assert!(warning.contains("INTH-482"), "{warning}");
        assert!(
            warning.contains("1. Understand & replicate working"),
            "{warning}"
        );
        let mut done = open.clone();
        done.state = RunState::Finished;
        assert_eq!(quit_warning(&[done]), None);
        assert_eq!(quit_warning(&[]), None);
    }

    #[test]
    fn status_text_names_the_stage_or_why_it_waits() {
        let (mut runs, id) = runs_of(&STAGES);
        let st = |runs: &Runs| status_text(runs.run(&id).unwrap());
        assert_eq!(
            st(&runs),
            (
                "INTH SLA tickets · INTH-482 · waiting for the orchestrator".to_string(),
                false
            )
        );
        start(&mut runs, &id, "replicate", "t1");
        assert_eq!(
            st(&runs),
            (
                "INTH SLA tickets · INTH-482 · 1. Understand & replicate working".to_string(),
                false
            )
        );
        runs.pause(&id, true).unwrap();
        assert_eq!(
            st(&runs),
            (
                "INTH SLA tickets · INTH-482 · paused: no stage starts until you resume"
                    .to_string(),
                false
            )
        );
        runs.pause(&id, false).unwrap();
        runs.report("t1", rated(40)).unwrap();
        let (text, red) = st(&runs);
        assert!(
            red && text.starts_with("INTH SLA tickets · INTH-482 · needs you: "),
            "{text}"
        );
        assert!(text.contains("40, bar 80"), "{text}");
        runs.person(&id, crate::runs::Choice::Stop).unwrap();
        let (text, red) = st(&runs);
        assert!(
            !red && text.ends_with("stopped: stopped by you at Understand & replicate"),
            "{text}"
        );
    }

    #[test]
    fn a_finished_run_says_done() {
        let (mut runs, id) = runs_of(&STAGES[..1]);
        start(&mut runs, &id, "replicate", "t1");
        runs.report("t1", rated(95)).unwrap();
        assert_eq!(
            status_text(runs.run(&id).unwrap()),
            (
                "INTH SLA tickets · INTH-482 · done: every stage passed".to_string(),
                false
            )
        );
    }

    #[test]
    fn the_meter_turns_red_below_the_floor() {
        assert_eq!(meter(None, 30), None);
        assert_eq!(
            meter(Some(25.0), 30),
            Some(Meter {
                left: 0.75,
                low: false
            })
        );
        assert_eq!(
            meter(Some(70.0), 30),
            Some(Meter {
                left: 0.3,
                low: false
            })
        );
        assert_eq!(meter(Some(71.0), 30).map(|m| m.low), Some(true));
        assert_eq!(
            meter(Some(130.0), 30),
            Some(Meter {
                left: 0.0,
                low: true
            })
        );
    }
}
