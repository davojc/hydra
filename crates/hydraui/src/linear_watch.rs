//! The orchestrator's Linear watcher, its decisions only (phase 2 spec §3, §4): which ticket to
//! pick, when to look, when to give up, and what to write back. The app does the calls.

use std::time::{Duration, SystemTime};

use crate::linear::{LinearApi, LinearError, Ticket};
use crate::pipeline::{Assignee, LinearSettings, Pipeline};
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
pub fn claim_comment(pipeline: &Pipeline) -> String {
    let stages: Vec<&str> = pipeline.stages.iter().map(|s| s.title.as_str()).collect();
    format!(
        "hydraui's orchestrator (pipeline \"{}\") is working on this: {}.",
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

/// The id of `team`'s state named `name` (names ignore case).
fn state_id(api: &dyn LinearApi, team: &str, name: &str) -> Result<String, LinearError> {
    api.states(team)?
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(name.trim()))
        .map(|s| s.id)
        .ok_or_else(|| LinearError::Failed(format!("{team} has no state \"{}\"", name.trim())))
}

/// Claims `ticket` (§4): into the claim state, assigned to the key's user when nobody is, and
/// the claim comment.
fn claim(
    api: &dyn LinearApi,
    ticket: &Ticket,
    claim_state: &str,
    pipeline: &Pipeline,
) -> Result<(), LinearError> {
    let me = match ticket.assignee {
        None => Some(api.viewer()?),
        Some(_) => None,
    };
    api.update(&ticket.id, claim_state, me.as_deref())?;
    api.comment(&ticket.id, &claim_comment(pipeline))
}

/// One check (§3, §4): picks the next ticket for `pipeline` and claims it, or `None` when there
/// is nothing to pick. A ticket someone took meanwhile is passed over for the next.
pub fn find_and_claim(
    api: &dyn LinearApi,
    settings: &LinearSettings,
    pipeline: &Pipeline,
) -> Result<Option<Ticket>, LinearError> {
    let team = settings.team.as_str();
    let pick_state = state_id(api, team, &settings.pick)?;
    let claim_state = state_id(api, team, &settings.claim)?;
    let unassigned_only = settings.assignee == Assignee::Unassigned;
    let mut passed_over: Vec<String> = Vec::new();
    // A few tickets taken meanwhile in a row is plenty for one check.
    for _ in 0..5 {
        let tickets: Vec<Ticket> = api
            .pickable(team, &pick_state, unassigned_only)?
            .into_iter()
            .filter(|t| !passed_over.contains(&t.identifier))
            .collect();
        let Some(next) = pick(&tickets, unassigned_only) else {
            return Ok(None);
        };
        // As it is now: someone may have moved or taken it since the list.
        let still = api.ticket(&next.identifier)?.filter(|t| {
            t.state.eq_ignore_ascii_case(settings.pick.trim())
                && (!unassigned_only || t.assignee.is_none())
        });
        let Some(ticket) = still else {
            passed_over.push(next.identifier.clone());
            continue;
        };
        claim(api, &ticket, &claim_state, pipeline)?;
        return Ok(Some(ticket));
    }
    Ok(None)
}

/// Claims ticket `identifier` for a run started by hand (§4); `false` when Linear has no such
/// ticket or it is already in the claim state (the watcher claimed it).
pub fn claim_named(
    api: &dyn LinearApi,
    settings: &LinearSettings,
    pipeline: &Pipeline,
    identifier: &str,
) -> Result<bool, LinearError> {
    let Some(ticket) = api.ticket(identifier)? else {
        return Ok(false);
    };
    if ticket.state.eq_ignore_ascii_case(settings.claim.trim()) {
        return Ok(false);
    }
    let claim_state = state_id(api, &settings.team, &settings.claim)?;
    claim(api, &ticket, &claim_state, pipeline)?;
    Ok(true)
}

/// The writes when `run` ended (§4): the end comment, and the passed state when every stage
/// passed. Says what it wrote, for the run's log.
pub fn end_writes(
    api: &dyn LinearApi,
    settings: &LinearSettings,
    run: &Run,
) -> Result<Vec<&'static str>, LinearError> {
    let Some(ticket) = api.ticket(&run.ticket)? else {
        return Ok(Vec::new());
    };
    api.comment(&ticket.id, &end_comment(run))?;
    let mut wrote = vec!["comment"];
    if all_passed(run) {
        let passed = state_id(api, &settings.team, &settings.passed)?;
        api.update(&ticket.id, &passed, None)?;
        wrote.push("state");
    }
    Ok(wrote)
}

/// A UTC time as Linear sends it (`2026-11-04T14:04:05.535Z`).
pub fn parse_time(s: &str) -> Option<SystemTime> {
    let s = s.trim().strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let num = |t: &str| t.parse::<i64>().ok();
    let d: Vec<i64> = date.split('-').map(num).collect::<Option<_>>()?;
    let time = time.split('.').next()?;
    let t: Vec<i64> = time.split(':').map(num).collect::<Option<_>>()?;
    let ([y, m, day], [hh, mm, ss]) = (d.as_slice(), t.as_slice()) else {
        return None;
    };
    if !(1..=12).contains(m) || !(1..=31).contains(day) || *hh > 23 || *mm > 59 || *ss > 60 {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if *m <= 2 { y - 1 } else { *y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss;
    u64::try_from(secs)
        .ok()
        .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s))
}

/// How close a ticket is to breaching its SLA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlaLevel {
    Ok,
    /// Inside the SLA's high-risk window.
    Risk,
    Breached,
}

/// The card's SLA pill (§5): the time left, or that it's breached, and how worried to look.
pub fn sla_pill(
    breaches_at: &str,
    high_risk_at: Option<&str>,
    now: SystemTime,
) -> Option<(String, SlaLevel)> {
    let breach = parse_time(breaches_at)?;
    let span = |d: Duration| {
        let s = d.as_secs();
        match s {
            s if s >= 86_400 => format!("{}d", s / 86_400),
            s if s >= 3_600 => format!("{}h", s / 3_600),
            s => format!("{}m", (s / 60).max(1)),
        }
    };
    if let Ok(left) = breach.duration_since(now) {
        let risky = high_risk_at.and_then(parse_time).is_some_and(|r| now >= r);
        let level = if risky { SlaLevel::Risk } else { SlaLevel::Ok };
        return Some((format!("SLA {}", span(left)), level));
    }
    let over = now.duration_since(breach).unwrap_or_default();
    Some((
        format!("SLA breached {} ago", span(over)),
        SlaLevel::Breached,
    ))
}

/// Whether `at` is before the SLA breaches; `None` when the time can't be read.
pub fn inside_sla(breaches_at: &str, at: SystemTime) -> Option<bool> {
    parse_time(breaches_at).map(|b| at < b)
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
        let c = claim_comment(&pipeline());
        for says in ["INTH SLA tickets", "Replicate → Develop"] {
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

    /// Linear in memory: the team's states, its tickets, and every write.
    #[derive(Default)]
    struct Fake {
        tickets: std::cell::RefCell<Vec<Ticket>>,
        /// Tickets someone else changes between the pick and the re-check.
        taken_meanwhile: Vec<String>,
        writes: std::cell::RefCell<Vec<String>>,
    }

    const STATES: [(&str, &str); 4] = [
        ("s-pick", "Ready for Investigation"),
        ("s-claim", "In Investigation"),
        ("s-passed", "Ready for Peer Review"),
        ("s-review", "Ready for Review"),
    ];

    impl LinearApi for Fake {
        fn viewer(&self) -> Result<String, LinearError> {
            Ok("u-me".into())
        }
        fn states(&self, team: &str) -> Result<Vec<crate::linear::State>, LinearError> {
            assert_eq!(team, "INTH");
            Ok(STATES
                .iter()
                .map(|(id, name)| crate::linear::State {
                    id: (*id).into(),
                    name: (*name).into(),
                })
                .collect())
        }
        fn pickable(
            &self,
            _team: &str,
            state_id: &str,
            unassigned_only: bool,
        ) -> Result<Vec<Ticket>, LinearError> {
            let name = STATES.iter().find(|(id, _)| *id == state_id).unwrap().1;
            Ok(self
                .tickets
                .borrow()
                .iter()
                .filter(|t| t.state == name && (!unassigned_only || t.assignee.is_none()))
                .cloned()
                .collect())
        }
        fn ticket(&self, identifier: &str) -> Result<Option<Ticket>, LinearError> {
            let mut t = self
                .tickets
                .borrow()
                .iter()
                .find(|t| t.identifier == identifier)
                .cloned();
            if let Some(t) = t
                .as_mut()
                .filter(|t| self.taken_meanwhile.contains(&t.identifier))
            {
                t.assignee = Some("u-someone".into());
                t.state = "In Development".into();
            }
            Ok(t)
        }
        fn update(
            &self,
            id: &str,
            state_id: &str,
            assignee: Option<&str>,
        ) -> Result<(), LinearError> {
            self.writes.borrow_mut().push(format!(
                "update {id} {state_id} {}",
                assignee.unwrap_or("-")
            ));
            Ok(())
        }
        fn comment(&self, id: &str, body: &str) -> Result<(), LinearError> {
            let first = body.lines().next().unwrap_or_default();
            self.writes
                .borrow_mut()
                .push(format!("comment {id} {first}"));
            Ok(())
        }
    }

    fn settings() -> LinearSettings {
        pipeline().linear.unwrap()
    }

    fn fake_with(tickets: Vec<Ticket>) -> Fake {
        Fake {
            tickets: std::cell::RefCell::new(tickets),
            ..Fake::default()
        }
    }

    #[test]
    fn a_check_claims_the_soonest_ticket() {
        let api = fake_with(vec![
            t("INTH-1", "2026-11-04T00:00:00.000Z", None),
            t("INTH-2", "2026-11-02T00:00:00.000Z", None),
        ]);
        let got = find_and_claim(&api, &settings(), &pipeline())
            .unwrap()
            .unwrap();
        assert_eq!(got.identifier, "INTH-2");
        assert_eq!(
            *api.writes.borrow(),
            [
                "update uuid-INTH-2 s-claim u-me",
                "comment uuid-INTH-2 hydraui's orchestrator (pipeline \"INTH SLA tickets\") is \
                 working on this: Replicate → Develop.",
            ]
        );
    }

    #[test]
    fn a_ticket_taken_meanwhile_is_passed_over() {
        let mut api = fake_with(vec![
            t("INTH-1", "2026-11-04T00:00:00.000Z", None),
            t("INTH-2", "2026-11-02T00:00:00.000Z", None),
        ]);
        api.taken_meanwhile = vec!["INTH-2".into()];
        let got = find_and_claim(&api, &settings(), &pipeline())
            .unwrap()
            .unwrap();
        assert_eq!(got.identifier, "INTH-1");
        assert!(api.writes.borrow().iter().all(|w| !w.contains("INTH-2")));
    }

    #[test]
    fn nothing_to_pick_writes_nothing() {
        let api = fake_with(vec![t("INTH-1", "2026-11-04T00:00:00.000Z", Some("u-9"))]);
        assert_eq!(
            find_and_claim(&api, &settings(), &pipeline()).unwrap(),
            None
        );
        assert!(api.writes.borrow().is_empty());
    }

    #[test]
    fn another_trigger_state_and_assigned_tickets_keep_their_assignee() {
        let mut ready = t("INTH-5", "2026-11-04T00:00:00.000Z", Some("u-9"));
        ready.state = "Ready for Review".into();
        let api = fake_with(vec![ready]);
        let mut s = settings();
        s.pick = "ready for review".into();
        s.assignee = Assignee::Any;
        let got = find_and_claim(&api, &s, &pipeline()).unwrap().unwrap();
        assert_eq!(got.identifier, "INTH-5");
        assert_eq!(api.writes.borrow()[0], "update uuid-INTH-5 s-claim -");
    }

    #[test]
    fn a_state_the_team_lacks_is_named() {
        let api = fake_with(vec![]);
        let mut s = settings();
        s.claim = "Doing".into();
        assert_eq!(
            find_and_claim(&api, &s, &pipeline()).unwrap_err(),
            LinearError::Failed("INTH has no state \"Doing\"".into())
        );
    }

    #[test]
    fn a_run_started_by_hand_is_claimed_unless_it_already_was() {
        let mut claimed = t("INTH-8", "2026-11-04T00:00:00.000Z", Some("u-me"));
        claimed.state = "In Investigation".into();
        let api = fake_with(vec![t("INTH-7", "2026-11-04T00:00:00.000Z", None), claimed]);
        assert!(claim_named(&api, &settings(), &pipeline(), "INTH-7").unwrap());
        assert!(!claim_named(&api, &settings(), &pipeline(), "INTH-8").unwrap());
        assert!(!claim_named(&api, &settings(), &pipeline(), "INTH-404").unwrap());
        assert_eq!(
            api.writes.borrow().len(),
            2,
            "INTH-7's update and comment only"
        );
    }

    #[test]
    fn a_passed_run_comments_and_moves_on_a_stopped_one_only_comments() {
        let mut ticket = t("INTH-170", "2026-11-04T00:00:00.000Z", Some("u-me"));
        ticket.state = "In Investigation".into();
        let api = fake_with(vec![ticket]);
        let mut passed = run_with(90);
        // The run's last stage is develop; it finished.
        assert!(all_passed(&passed));
        assert_eq!(
            end_writes(&api, &settings(), &passed).unwrap(),
            ["comment", "state"]
        );
        assert!(api.writes.borrow()[1].starts_with("update uuid-INTH-170 s-passed -"));
        api.writes.borrow_mut().clear();
        passed.state = RunState::Stopped("a person stopped it".into());
        assert_eq!(end_writes(&api, &settings(), &passed).unwrap(), ["comment"]);
        assert_eq!(api.writes.borrow().len(), 1);
    }

    #[test]
    fn linears_times_are_read() {
        let t = parse_time("2026-11-04T14:04:05.535Z").unwrap();
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_793_801_045);
        assert_eq!(
            parse_time("1970-01-01T00:00:00Z"),
            Some(SystemTime::UNIX_EPOCH)
        );
        for bad in [
            "",
            "2026-11-04",
            "2026-13-04T00:00:00Z",
            "2026-11-04T25:00:00Z",
            "soon",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_sla_pill_says_the_time_left_and_how_worried_to_look() {
        let breach = "2026-11-04T14:00:00.000Z";
        let risk = Some("2026-11-03T14:00:00.000Z");
        let at = |s: &str| parse_time(s).unwrap();
        assert_eq!(
            sla_pill(breach, risk, at("2026-10-23T10:00:00Z")),
            Some(("SLA 12d".into(), SlaLevel::Ok))
        );
        assert_eq!(
            sla_pill(breach, risk, at("2026-11-04T09:00:00Z")),
            Some(("SLA 5h".into(), SlaLevel::Risk))
        );
        assert_eq!(
            sla_pill(breach, risk, at("2026-11-04T13:20:00Z")),
            Some(("SLA 40m".into(), SlaLevel::Risk))
        );
        assert_eq!(
            sla_pill(breach, None, at("2026-11-06T14:00:00Z")),
            Some(("SLA breached 2d ago".into(), SlaLevel::Breached))
        );
        assert_eq!(sla_pill("whenever", risk, at("2026-10-23T10:00:00Z")), None);
    }

    #[test]
    fn a_run_ended_inside_its_sla_or_not() {
        let breach = "2026-11-04T14:00:00.000Z";
        let at = |s: &str| parse_time(s).unwrap();
        assert_eq!(inside_sla(breach, at("2026-11-04T13:59:59Z")), Some(true));
        assert_eq!(inside_sla(breach, at("2026-11-04T14:00:01Z")), Some(false));
        assert_eq!(inside_sla("?", at("2026-11-04T14:00:01Z")), None);
    }
}
