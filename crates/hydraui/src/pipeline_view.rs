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
    /// Waiting for a person's check by hand.
    Check,
    Muted,
}

/// A stage's gate badge (the design: `92% ✓`, `64% · needs 85`, `needs ≥80%`).
pub fn gate_badge(rating: Option<&Rating>, bar: u8, status: StageStatus) -> (String, Tone) {
    match (status, rating) {
        (StageStatus::Passed, Some(r)) => (format!("{}% ✓", r.score), Tone::Passed),
        (StageStatus::Stopped, Some(r)) if r.outcome == Outcome::Blocked => {
            ("blocked".into(), Tone::Stopped)
        }
        (StageStatus::Stopped, Some(r)) if r.outcome == Outcome::Check => {
            (format!("{}% · check by hand", r.score), Tone::Check)
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
            // Between turns, not reported yet: waiting on something, not working.
            Some(SessionState::Idle) => "Idle",
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

/// What a run waiting for a person says at a glance (core spec §59): a pill, the one thing to
/// do, and at most two sentences of why. The full report stays under Details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glance {
    pub pill: &'static str,
    /// Starts with a verb: what to do now.
    pub ask: String,
    pub why: String,
    /// The stage passes once the person says they did it (`Choice::Checked`).
    pub person_passes: bool,
}

/// The first `n` sentences of `text`, on one line, cut to `max` characters.
fn sentences(text: &str, n: usize, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = flat.len();
    let mut seen = 0;
    for (i, _) in flat.match_indices(". ") {
        seen += 1;
        if seen == n {
            end = i + 1;
            break;
        }
    }
    let cut = &flat[..end];
    if cut.chars().count() <= max {
        return cut.to_string();
    }
    let short: String = cut.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", short.trim_end())
}

/// The glance for a stage `title` waiting on a person with report `rating` (its latest),
/// else the run's `reason`.
pub fn glance(rating: Option<&Rating>, reason: &str, title: &str) -> Glance {
    const WHY: usize = 240;
    let Some(r) = rating else {
        return Glance {
            pill: "Needs you",
            ask: sentences(&sentence(reason), 1, 160),
            why: String::new(),
            person_passes: false,
        };
    };
    let own = (!r.ask.trim().is_empty()).then(|| r.ask.trim().to_string());
    match r.outcome {
        Outcome::Check => Glance {
            pill: "Check by hand",
            ask: own.unwrap_or_else(|| format!("Check {title} by hand")),
            why: sentences(&r.check, 2, WHY),
            person_passes: true,
        },
        Outcome::Blocked => Glance {
            pill: "Blocked",
            ask: own.unwrap_or_else(|| format!("Unblock {title}")),
            why: sentences(&r.summary, 2, WHY),
            person_passes: false,
        },
        Outcome::Done => Glance {
            pill: "Needs you",
            person_passes: own.is_some(),
            ask: own.unwrap_or_else(|| format!("Decide what {title} does next")),
            why: sentences(&r.summary, 2, WHY),
        },
    }
}

/// What the main button says for a run's link.
pub fn link_label(url: &str) -> &'static str {
    if url.contains("github.com/") && url.contains("/pull/") {
        "Open PR on GitHub"
    } else if url.contains("linear.app/") {
        "Open in Linear"
    } else {
        "Open link"
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    fn report(outcome: &str, summary: &str, ask: &str, check: &str) -> Rating {
        crate::rating::from_args(&serde_json::json!({
            "outcome": outcome, "score": 88, "summary": summary, "ask": ask, "check": check,
            "evidence": [{"kind": "note", "detail": "d"}],
            "reasons": [{"side": "for", "text": "r"}]}))
        .unwrap()
    }

    #[test]
    fn a_glance_leads_with_what_to_do() {
        let long = "Reviewed svc-data-ingestion #5. I found two bugs and one minor issue, and the \
                    author fixed all three in 2638ba9 with tests, verified, CI green. The PR is \
                    RED under POL-SEC-002, so I didn't approve it.";
        let g = glance(
            Some(&report(
                "done",
                long,
                "Approve PR #5 on GitHub as a code owner",
                "",
            )),
            "",
            "Review PR",
        );
        assert_eq!(g.pill, "Needs you");
        assert_eq!(g.ask, "Approve PR #5 on GitHub as a code owner");
        // Two sentences at most.
        assert!(
            g.why
                .starts_with("Reviewed svc-data-ingestion #5. I found two bugs")
        );
        assert!(!g.why.contains("POL-SEC-002"), "{}", g.why);
        assert!(g.person_passes);

        // No ask: a below-bar report asks for a decision, and a person can't pass it.
        let g = glance(
            Some(&report("done", "Two tests fail.", "", "")),
            "",
            "Review PR",
        );
        assert_eq!(g.ask, "Decide what Review PR does next");
        assert_eq!(g.why, "Two tests fail.");
        assert!(!g.person_passes);

        let g = glance(
            Some(&report(
                "check",
                "Built it.",
                "",
                "Open the share dialog. It should list 3 groups.",
            )),
            "",
            "Verify",
        );
        assert_eq!(
            (g.pill, g.ask.as_str()),
            ("Check by hand", "Check Verify by hand")
        );
        assert_eq!(g.why, "Open the share dialog. It should list 3 groups.");
        assert!(g.person_passes);

        let g = glance(
            Some(&report("blocked", "No access to the repo.", "", "")),
            "",
            "Develop",
        );
        assert_eq!((g.pill, g.ask.as_str()), ("Blocked", "Unblock Develop"));

        // No report: the run's reason, first sentence as the ask.
        let g = glance(
            None,
            "Stage a failed 3 times: the session closed. Try again later.",
            "A",
        );
        assert_eq!(g.ask, "Stage a failed 3 times: the session closed.");
        assert!(!g.person_passes);
    }

    #[test]
    fn long_whys_are_cut_and_links_name_what_they_open() {
        let g = glance(
            Some(&report("done", &"word ".repeat(100), "Do it", "")),
            "",
            "S",
        );
        assert!(
            g.why.chars().count() <= 240 && g.why.ends_with('…'),
            "{}",
            g.why
        );
        assert_eq!(
            link_label("https://github.com/iov42/a/pull/5"),
            "Open PR on GitHub"
        );
        assert_eq!(
            link_label("https://linear.app/iov42/issue/PRE-1"),
            "Open in Linear"
        );
        assert_eq!(link_label("https://example.com/x"), "Open link");
    }

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
        let mut check = rated(95, "done");
        check.outcome = Outcome::Check;
        check.check = "Try it in Chrome.".into();
        assert_eq!(
            gate_badge(Some(&check), 85, StageStatus::Stopped),
            ("95% · check by hand".to_string(), Tone::Check)
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
        // Its turn over, waiting on something (a deploy, a monitor) before it reports.
        assert_eq!(
            session_line(r, 0, 0, Some(SessionState::Idle), now),
            "Idle · 2m 15s"
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
