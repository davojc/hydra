//! The Pipeline view's rules (orchestrator spec §7.2; the design's View 1): each stage's gate
//! badge, each session's line, and which panel the right side shows. The drawing is in
//! `workspace::pipeline_tab`.

use std::time::SystemTime;

use crate::mcp::SessionState;
use crate::rating::{Outcome, Rating};
use crate::runs::{Run, RunState, StageStatus};

/// `text` with its first letter capitalised: the gate's reasons read as sentences on screen.
pub fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How a gate badge is drawn: green, red, or muted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Passed,
    Stopped,
    Muted,
}

/// A stage's gate badge (the design: `92% ✓`, `64% · needs 85`, `needs ≥80%`).
pub fn gate_badge(rating: Option<&Rating>, bar: u8, status: StageStatus) -> (String, Tone) {
    match (status, rating) {
        (StageStatus::Passed, Some(r)) => (format!("{}% ✓", r.score), Tone::Passed),
        (StageStatus::Stopped, Some(r)) if r.outcome == Outcome::Blocked => {
            ("blocked".into(), Tone::Stopped)
        }
        (StageStatus::Stopped, Some(r)) => (format!("{}% · needs {bar}", r.score), Tone::Stopped),
        (StageStatus::Failed, _) => ("no report".into(), Tone::Stopped),
        _ => (format!("needs ≥{bar}%"), Tone::Muted),
    }
}

/// `now - start` as the design shows it, `2m 15s`; nothing negative.
pub fn elapsed(start: SystemTime, now: SystemTime) -> String {
    let s = now.duration_since(start).map_or(0, |d| d.as_secs());
    format!("{}m {:02}s", s / 60, s % 60)
}

/// Attempt `ai` of stage `si` as its session row says it (the design: `Working · 2m 15s`,
/// `Waiting for you`, `Ended · 92%`, `Stopped · 64%`), with `live` its hook state.
pub fn session_line(
    run: &Run,
    si: usize,
    ai: usize,
    live: Option<SessionState>,
    now: SystemTime,
) -> String {
    let s = &run.stages[si];
    let a = &s.attempts[ai];
    let latest = ai + 1 == s.attempts.len();
    if latest && s.status == StageStatus::Starting {
        return "Starting…".into();
    }
    if latest && s.status == StageStatus::Running {
        let doing = match live {
            Some(SessionState::Waiting) => "Asking",
            _ => "Working",
        };
        return format!("{doing} · {}", elapsed(a.started, now));
    }
    match &a.rating {
        Some(_) if latest && s.status == StageStatus::Stopped => "Waiting for you".into(),
        Some(r) if latest && s.status == StageStatus::Passed => format!("Ended · {}%", r.score),
        Some(r) => format!("Stopped · {}%", r.score),
        None => "Ended · no report".into(),
    }
}

/// What the right side shows (the design's panels), with the stage and attempt it is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// No run yet.
    Idle,
    /// A run that hasn't started a stage.
    Waiting,
    /// A session at work: its terminal.
    Live(usize, usize),
    /// A decided session: its rating, then its terminal.
    Report(usize, usize),
    /// The session the run stopped at: the banner and the person's choices.
    Stopped(usize, usize),
    /// A run held before stage `si` started (the weekly limit): the banner without a session.
    Held(usize),
    /// The run finished.
    Done,
}

fn number(label: &str) -> u32 {
    label.trim_start_matches("s-").parse().unwrap_or_default()
}

/// The panel for `run` with the session labelled `selected` picked, or, with none (or one
/// that isn't there), the one the run is at: the stopped session, the newest one, or done.
pub fn panel_for(run: Option<&Run>, selected: Option<&str>) -> Panel {
    let Some(run) = run else {
        return Panel::Idle;
    };
    let waiting_at = run
        .stages
        .iter()
        .position(|s| s.status != StageStatus::Passed);
    let classify = |si: usize, ai: usize| {
        let s = &run.stages[si];
        let latest = ai + 1 == s.attempts.len();
        if latest && s.status.busy() {
            Panel::Live(si, ai)
        } else if latest && matches!(run.state, RunState::NeedsYou(_)) && waiting_at == Some(si) {
            Panel::Stopped(si, ai)
        } else {
            Panel::Report(si, ai)
        }
    };
    let attempts = || {
        run.stages.iter().enumerate().flat_map(|(si, s)| {
            s.attempts
                .iter()
                .enumerate()
                .map(move |(ai, a)| (si, ai, number(&a.label)))
        })
    };
    if let Some((si, ai, _)) =
        selected.and_then(|label| attempts().find(|(_, _, n)| *n == number(label)))
    {
        return classify(si, ai);
    }
    match (&run.state, waiting_at) {
        (RunState::Finished, _) => Panel::Done,
        (RunState::NeedsYou(_), Some(si)) => match run.stages[si].attempts.len() {
            n if n > 0 && run.stages[si].status != StageStatus::Waiting => {
                Panel::Stopped(si, n - 1)
            }
            _ => Panel::Held(si),
        },
        _ => attempts()
            .max_by_key(|(_, _, n)| *n)
            .map_or(Panel::Waiting, |(si, ai, _)| classify(si, ai)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use serde_json::json;

    use super::*;
    use crate::mcp::SessionState;
    use crate::runs::{Choice, Runs};

    fn runs_of(stages: &[&str]) -> (Runs, String) {
        let mut t = String::from("name = \"t\"\n");
        for s in stages {
            t += &format!("[[stage]]\nid = \"{s}\"\ntitle = \"{s}\"\n");
        }
        t = t.replace("id = \"develop\"\n", "id = \"develop\"\nbar = 85\n");
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

    fn rated(score: u8, outcome: &str) -> Rating {
        crate::rating::from_args(&json!({"outcome": outcome, "score": score, "summary": "s",
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "for", "text": "r"}]}))
        .unwrap()
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(sentence("only 10% is left"), "Only 10% is left");
        assert_eq!(sentence("Already"), "Already");
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn gate_badges_follow_the_rating_and_bar() {
        assert_eq!(
            gate_badge(Some(&rated(92, "done")), 80, StageStatus::Passed),
            ("92% ✓".to_string(), Tone::Passed)
        );
        assert_eq!(
            gate_badge(Some(&rated(64, "done")), 85, StageStatus::Stopped),
            ("64% · needs 85".to_string(), Tone::Stopped)
        );
        assert_eq!(
            gate_badge(Some(&rated(99, "blocked")), 85, StageStatus::Stopped),
            ("blocked".to_string(), Tone::Stopped)
        );
        assert_eq!(
            gate_badge(None, 80, StageStatus::Waiting),
            ("needs ≥80%".to_string(), Tone::Muted)
        );
        assert_eq!(
            gate_badge(None, 80, StageStatus::Failed),
            ("no report".to_string(), Tone::Stopped)
        );
    }

    #[test]
    fn session_lines_cover_every_state() {
        let (mut runs, id) = runs_of(&["replicate", "develop"]);
        start(&mut runs, &id, "replicate", "t1");
        let r = runs.run(&id).unwrap();
        let a = &r.stages[0].attempts[0];
        let now = a.started + Duration::from_secs(135);
        assert_eq!(
            session_line(r, 0, 0, Some(SessionState::Working), now),
            "Working · 2m 15s"
        );
        runs.report("t1", rated(92, "done")).unwrap();
        let r = runs.run(&id).unwrap();
        assert_eq!(session_line(r, 0, 0, None, now), "Ended · 92%");

        start(&mut runs, &id, "develop", "t2");
        runs.report("t2", rated(64, "done")).unwrap();
        let r = runs.run(&id).unwrap();
        assert_eq!(session_line(r, 1, 0, None, now), "Waiting for you");
        runs.person(&id, Choice::Retry { note: "n".into() })
            .unwrap();
        start(&mut runs, &id, "develop", "t3");
        runs.session_gone("t3");
        let r = runs.run(&id).unwrap();
        assert_eq!(session_line(r, 1, 0, None, now), "Stopped · 64%");
        assert_eq!(session_line(r, 1, 1, None, now), "Ended · no report");
        // A clock behind the start says nothing silly.
        assert_eq!(
            elapsed(
                SystemTime::UNIX_EPOCH,
                SystemTime::UNIX_EPOCH - Duration::from_secs(5)
            ),
            "0m 00s"
        );
    }

    #[test]
    fn the_panel_follows_the_selected_session() {
        let (mut runs, id) = runs_of(&["replicate", "develop"]);
        assert_eq!(panel_for(runs.run(&id), None), Panel::Waiting);
        assert_eq!(panel_for(None, None), Panel::Idle);
        start(&mut runs, &id, "replicate", "t1");
        let r = runs.run(&id);
        assert_eq!(panel_for(r, None), Panel::Live(0, 0));
        runs.report("t1", rated(92, "done")).unwrap();
        start(&mut runs, &id, "develop", "t2");
        // Following the run: the newest session; an earlier one when the user picks it.
        assert_eq!(panel_for(runs.run(&id), None), Panel::Live(1, 0));
        assert_eq!(panel_for(runs.run(&id), Some("s-01")), Panel::Report(0, 0));
        runs.report("t2", rated(40, "done")).unwrap();
        assert_eq!(panel_for(runs.run(&id), None), Panel::Stopped(1, 0));
        assert_eq!(panel_for(runs.run(&id), Some("s-99")), Panel::Stopped(1, 0));
        // An earlier, passed session stays a report while the run waits elsewhere.
        assert_eq!(panel_for(runs.run(&id), Some("s-01")), Panel::Report(0, 0));
        runs.person(&id, Choice::Retry { note: "n".into() })
            .unwrap();
        start(&mut runs, &id, "develop", "t3");
        runs.report("t3", rated(95, "done")).unwrap();
        assert_eq!(panel_for(runs.run(&id), None), Panel::Done);
        assert_eq!(panel_for(runs.run(&id), Some("s-02")), Panel::Report(1, 0));
    }

    #[test]
    fn a_weekly_limit_stop_has_no_session_to_show() {
        let (mut runs, id) = runs_of(&["replicate"]);
        runs.usage("orch", 90.0);
        let _ = runs.begin("orch", &id, "replicate", "", 30);
        assert_eq!(panel_for(runs.run(&id), None), Panel::Held(0));
    }
}
