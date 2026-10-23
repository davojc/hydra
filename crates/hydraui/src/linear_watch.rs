//! The orchestrator's Linear watcher, its decisions only (phase 2 spec §3, §4): which ticket to
//! pick, when to look, when to give up, and what to write back. The app does the calls.

use std::time::{Duration, SystemTime};

use crate::linear::{LinearError, Ticket};
use crate::pipeline::Pipeline;
use crate::runs::{Run, RunState, StageStatus};

/// How many failed checks in a row stop watching (the user's three-retries rule).
pub const MAX_FAILURES: u32 = 3;

/// The ticket to take from `tickets` (§3): the one whose SLA breaches soonest, unassigned only
/// when `unassigned_only`.
pub fn pick(tickets: &[Ticket], unassigned_only: bool) -> Option<&Ticket> {
    tickets
        .iter()
        .filter(|t| !unassigned_only || t.assignee.is_none())
        .filter(|t| t.sla_breaches_at.is_some())
        // RFC 3339 times in UTC sort as text.
        .min_by(|a, b| a.sla_breaches_at.cmp(&b.sla_breaches_at))
}

/// Whether a check is due: watching, no open run, and `every` since the last one (or none yet).
pub fn due(
    watching: bool,
    has_open_run: bool,
    last: Option<SystemTime>,
    now: SystemTime,
    every: Duration,
) -> bool {
    watching
        && !has_open_run
        && last.is_none_or(|l| now.duration_since(l).is_ok_and(|since| since >= every))
}

/// Failed checks in a row (§3).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Failures(pub u32);

impl Failures {
    /// Counts `error`; `Some(reason)` when watching must stop: a rejected key at once, anything
    /// else on the third failure in a row.
    pub fn record(&mut self, error: &LinearError) -> Option<String> {
        match error {
            LinearError::Rejected(why) => Some(why.clone()),
            LinearError::Failed(why) => {
                self.0 += 1;
                (self.0 >= MAX_FAILURES)
                    .then(|| format!("{why} ({} checks in a row failed)", self.0))
            }
        }
    }

    /// A check worked.
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

/// Whether `ticket` (`INTH-170`) belongs to `team` (`INTH`), so its run writes back (§4).
pub fn in_team(ticket: &str, team: &str) -> bool {
    ticket
        .split_once('-')
        .is_some_and(|(key, n)| key.eq_ignore_ascii_case(team) && !n.is_empty())
}

/// Another orchestrator already watching `team`, from `(orchestrator, team)` pairs; `me` doesn't
/// count. Only one may watch a team (§3).
pub fn watched_by<'a, K: PartialEq + 'a>(
    team: &str,
    me: &K,
    watching: impl IntoIterator<Item = (&'a K, &'a str)>,
) -> Option<&'a K> {
    watching
        .into_iter()
        .find(|(k, t)| *k != me && t.eq_ignore_ascii_case(team))
        .map(|(k, _)| k)
}

/// What the orchestrator is told when hydraui picked `ticket` for it (§3).
pub fn start_prompt(pipeline: &Pipeline, ticket: &Ticket) -> String {
    let breaches = ticket
        .sla_breaches_at
        .as_deref()
        .map(|t| format!(" (SLA breaches {})", t.get(..10).unwrap_or(t)))
        .unwrap_or_default();
    format!(
        "Linear has {id} \"{title}\"{breaches}. Start it: call run_start with pipeline \"{file}\", \
         ticket {id} and that title.",
        id = ticket.identifier,
        title = ticket.title,
        file = pipeline.file,
    )
}

/// The comment a claim leaves (§4).
pub fn claim_comment(pipeline: &Pipeline, run_id: &str) -> String {
    let stages: Vec<&str> = pipeline.stages.iter().map(|s| s.title.as_str()).collect();
    format!(
        "hydraui's orchestrator (pipeline \"{}\") is working on this: {}. Run {run_id}.",
        pipeline.name,
        stages.join(" → ")
    )
}

/// The pull request URLs in `run`'s reports, once each, in the order found.
pub fn pr_links(run: &Run) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rating in run.stages.iter().filter_map(|s| s.rating()) {
        let texts = std::iter::once(rating.summary.as_str())
            .chain(rating.evidence.iter().map(|e| e.detail.as_str()))
            .chain(std::iter::once(rating.handoff.as_str()));
        for text in texts {
            for link in pull_requests(text) {
                if !found.contains(&link) {
                    found.push(link);
                }
            }
        }
    }
    found
}

/// GitHub pull request URLs in `text`: `https://github.com/<owner>/<repo>/pull/<n>`.
fn pull_requests(text: &str) -> Vec<String> {
    const START: &str = "https://github.com/";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(START) {
        let tail = &rest[at + START.len()..];
        let parts: Vec<&str> = tail.splitn(4, '/').collect();
        if let [owner, repo, "pull", n] = parts.as_slice() {
            let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
            let plain = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            };
            if !digits.is_empty() && plain(owner) && plain(repo) {
                out.push(format!("{START}{owner}/{repo}/pull/{digits}"));
            }
        }
        rest = tail;
    }
    out
}

/// The comment an ended run leaves (§4): how it ended, each stage's score and summary, and
/// its PR links.
pub fn end_comment(run: &Run) -> String {
    let how = match &run.state {
        RunState::Finished => "passed every stage".to_string(),
        RunState::Stopped(why) => format!("stopped: {why}"),
        RunState::NeedsYou(why) => format!("is waiting for a person: {why}"),
        RunState::Running => "is still running".to_string(),
    };
    let mut out = format!("hydraui's orchestrator: run {} {how}.\n", run.id);
    for (spec, stage) in run.pipeline.stages.iter().zip(&run.stages) {
        let line = match stage.rating() {
            Some(r) => format!("\n- {}: {} · {}", spec.title, r.score, r.summary.trim()),
            None if stage.status == StageStatus::Waiting => format!("\n- {}: not run", spec.title),
            None => format!("\n- {}: no report", spec.title),
        };
        out.push_str(&line);
    }
    let links = pr_links(run);
    if !links.is_empty() {
        out.push_str("\n\nPull requests:");
        for l in links {
            out.push_str(&format!("\n- {l}"));
        }
    }
    out
}

/// Whether `run` ended with every stage passed, so the ticket moves on (§4).
pub fn all_passed(run: &Run) -> bool {
    run.state == RunState::Finished && run.stages.iter().all(|s| s.status == StageStatus::Passed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::{Choice, Runs};
    use serde_json::json;

    fn t(id: &str, sla: &str, assignee: Option<&str>) -> Ticket {
        Ticket {
            id: format!("uuid-{id}"),
            identifier: id.into(),
            title: format!("Title of {id}"),
            url: format!("https://linear.app/iov42/issue/{id}"),
            sla_breaches_at: Some(sla.into()),
            sla_high_risk_at: None,
            assignee: assignee.map(str::to_string),
            state: "Ready for Investigation".into(),
        }
    }

    #[test]
    fn the_soonest_breach_goes_first_and_assigned_ones_only_when_wanted() {
        let tickets = [
            t("INTH-1", "2026-11-04T14:00:00.000Z", None),
            t("INTH-2", "2026-11-01T09:00:00.000Z", Some("u-9")),
            t("INTH-3", "2026-11-02T09:00:00.000Z", None),
        ];
        assert_eq!(pick(&tickets, true).unwrap().identifier, "INTH-3");
        assert_eq!(pick(&tickets, false).unwrap().identifier, "INTH-2");
        assert_eq!(pick(&[], true), None);
        // Taken meanwhile: the claim found it assigned, so it's left out and the next one goes.
        let rest: Vec<Ticket> = tickets
            .iter()
            .filter(|x| x.identifier != "INTH-3")
            .cloned()
            .collect();
        assert_eq!(pick(&rest, true).unwrap().identifier, "INTH-1");
    }

    #[test]
    fn a_check_is_due_only_while_watching_and_free() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let five = Duration::from_secs(300);
        let ago = |s: u64| Some(now - Duration::from_secs(s));
        assert!(due(true, false, None, now, five));
        assert!(due(true, false, ago(300), now, five));
        assert!(!due(true, false, ago(299), now, five));
        assert!(!due(false, false, None, now, five), "not watching");
        assert!(!due(true, true, None, now, five), "a run is open");
    }

    #[test]
    fn three_failures_in_a_row_or_a_rejected_key_stop_watching() {
        let mut f = Failures::default();
        let down = LinearError::Failed("Linear isn't reachable".into());
        assert_eq!(f.record(&down), None);
        assert_eq!(f.record(&down), None);
        f.clear();
        assert_eq!(f.record(&down), None);
        assert_eq!(f.record(&down), None);
        let stop = f.record(&down).unwrap();
        assert!(stop.contains("Linear isn't reachable"), "{stop}");
        assert!(stop.contains('3'), "{stop}");
        let mut g = Failures::default();
        let key = LinearError::Rejected("Linear rejected the API key".into());
        assert!(g.record(&key).unwrap().contains("rejected"));
    }

    #[test]
    fn a_ticket_belongs_to_its_teams_key() {
        assert!(in_team("INTH-170", "INTH"));
        assert!(in_team("inth-170", "INTH"));
        assert!(!in_team("INTHX-1", "INTH"));
        assert!(!in_team("PRE-170", "INTH"));
        assert!(!in_team("INTH", "INTH"));
    }

    #[test]
    fn only_one_orchestrator_watches_a_team() {
        let (a, b, c) = (1u32, 2u32, 3u32);
        let watching = vec![(&a, "INTH"), (&b, "PRE")];
        assert_eq!(watched_by("INTH", &c, watching.clone()), Some(&a));
        assert_eq!(watched_by("INTH", &a, watching.clone()), None, "itself");
        assert_eq!(watched_by("OPS", &c, watching), None);
    }

    fn pipeline() -> Pipeline {
        let mut p = crate::pipeline::parse(
            "name = \"INTH SLA tickets\"\n[[stage]]\nid = \"replicate\"\ntitle = \"Replicate\"\n\
             [[stage]]\nid = \"develop\"\ntitle = \"Develop\"\n[linear]\nteam = \"INTH\"\n",
        )
        .unwrap();
        p.file = "inth".into();
        p
    }

    fn rated(score: u8, summary: &str, evidence: &str) -> crate::rating::Rating {
        crate::rating::from_args(
            &json!({"outcome": "done", "score": score, "summary": summary,
            "evidence": [{"kind": "note", "detail": evidence}],
            "reasons": [{"side": "for", "text": "r"}], "handoff": "h"}),
        )
        .unwrap()
    }

    /// A run of `pipeline()` with replicate passed at 88 and develop reported at `develop`.
    fn run_with(develop: u8) -> Run {
        let mut runs = Runs::default();
        let id = runs
            .start_run("orch", pipeline(), "INTH-170", "Dutch")
            .unwrap()
            .id
            .clone();
        for (stage, tok, score, summary, ev) in [
            ("replicate", "t1", 88, "Reproduced on dev.", "screenshots"),
            (
                "develop",
                "t2",
                develop,
                "Fixed in https://github.com/iov42/version2-frontend/pull/5542.",
                "PR https://github.com/iov42/version2-frontend/pull/5542 and \
                 https://github.com/iov42/backend/pull/1700, CI green",
            ),
        ] {
            let s = runs.begin("orch", &id, stage, "", 0).unwrap();
            runs.link(&id, s.stage, tok, tok);
            runs.report(tok, rated(score, summary, ev)).unwrap();
        }
        runs.run(&id).unwrap().clone()
    }

    #[test]
    fn the_prompts_and_the_claim_name_the_ticket_and_the_pipeline() {
        let ticket = t("INTH-170", "2026-11-04T14:04:05.535Z", None);
        let p = start_prompt(&pipeline(), &ticket);
        for says in [
            "INTH-170",
            "Title of INTH-170",
            "run_start",
            "\"inth\"",
            "2026-11-04",
        ] {
            assert!(p.contains(says), "{says}: {p}");
        }
        let c = claim_comment(&pipeline(), "INTH-170-1");
        for says in ["INTH SLA tickets", "Replicate → Develop", "INTH-170-1"] {
            assert!(c.contains(says), "{says}: {c}");
        }
    }

    #[test]
    fn pr_links_are_found_once_each() {
        assert_eq!(
            pr_links(&run_with(90)),
            [
                "https://github.com/iov42/version2-frontend/pull/5542",
                "https://github.com/iov42/backend/pull/1700",
            ]
        );
    }

    #[test]
    fn a_passed_run_says_so_with_scores_and_prs() {
        let run = run_with(90);
        assert!(all_passed(&run));
        let c = end_comment(&run);
        for says in [
            "passed every stage",
            "Replicate: 88",
            "Develop: 90",
            "Reproduced on dev.",
            "https://github.com/iov42/backend/pull/1700",
        ] {
            assert!(c.contains(says), "{says}: {c}");
        }
    }

    #[test]
    fn a_stopped_run_says_why_and_where() {
        let mut run = run_with(60);
        assert!(!all_passed(&run));
        // A person stopped it at the low develop rating.
        let mut runs = Runs::default();
        runs.restore(run.clone());
        runs.person(&run.id, Choice::Stop).unwrap();
        run = runs.run(&run.id).unwrap().clone();
        assert!(matches!(run.state, RunState::Stopped(_)));
        assert_eq!(run.stages[1].status, StageStatus::Stopped);
        let c = end_comment(&run);
        for says in ["stopped", "Develop: 60", "Replicate: 88"] {
            assert!(c.contains(says), "{says}: {c}");
        }
        assert!(!all_passed(&run));
    }
}
