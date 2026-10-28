//! The orchestrator's Linear watcher, its decisions only (phase 2 spec §3, §4): which ticket to
//! pick, when to look, when to give up, and what to write back. The app does the calls.

use std::time::{Duration, SystemTime};

use crate::linear::{LinearApi, LinearError, Ticket};
use crate::pipeline::{Assignee, LinearSettings, Pipeline};
use crate::runs::{Run, RunState, StageStatus};

/// How many failed checks in a row stop watching (the user's three-retries rule).
pub const MAX_FAILURES: u32 = 3;

/// The ticket to take from `tickets` (§3), unassigned only when `unassigned_only`: the one
/// whose SLA breaches soonest; with `sla_only` off, tickets without an SLA come after those
/// with one, most urgent priority first (no priority last), then oldest.
pub fn pick(tickets: &[Ticket], unassigned_only: bool, sla_only: bool) -> Option<&Ticket> {
    let urgency = |p: u8| if p == 0 { 5 } else { p };
    tickets
        .iter()
        .filter(|t| !unassigned_only || t.assignee.is_none())
        .filter(|t| !sla_only || t.sla_breaches_at.is_some())
        // RFC 3339 times in UTC sort as text; a ticket with an SLA before one without.
        .min_by(|a, b| {
            (a.sla_breaches_at.is_none(), &a.sla_breaches_at)
                .cmp(&(b.sla_breaches_at.is_none(), &b.sla_breaches_at))
                .then(urgency(a.priority).cmp(&urgency(b.priority)))
                .then(a.created_at.cmp(&b.created_at))
        })
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

/// How an orchestrator gets its tickets, as its card says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// It picks tickets from Linear itself.
    Automatic,
    /// A person tells it which ticket to run.
    Manual,
    /// It was picking automatically until something went wrong.
    Stopped,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Automatic => "Automatic",
            Mode::Manual => "Manual",
            Mode::Stopped => "Stopped",
        }
    }
}

/// What the card needs to say how tickets arrive.
pub struct TriggerFacts<'a> {
    pub on: bool,
    pub checking: bool,
    /// Why automatic picking stopped by itself.
    pub stopped: Option<&'a str>,
    /// The ticket of the run in progress.
    pub open_run: Option<&'a str>,
    /// That run waits for product's answer to a stage's question.
    pub asking: bool,
    /// What the last look found (`nothing new`), shown before when the next one is.
    pub found: Option<&'a str>,
    pub last: Option<SystemTime>,
    pub now: SystemTime,
    /// The line while a look is out, e.g. `Looking in Linear for the next INTH ticket…`.
    pub looking: &'a str,
    pub every_minutes: u32,
    /// What it runs, `ticket` or `PR`.
    pub noun: &'a str,
    /// Picked already and waiting their turn (a Slack message can bring several).
    pub waiting: usize,
}

/// The card's mode and one short line saying what it means right now; the card is narrow, so
/// what the mode watches is in the switch's tooltip ([`trigger_tooltip`]).
pub fn trigger(f: &TriggerFacts) -> (Mode, String) {
    let mode = match (f.on, f.stopped) {
        (true, _) => Mode::Automatic,
        (false, Some(_)) => Mode::Stopped,
        (false, None) => Mode::Manual,
    };
    // A look in progress, by the timer or Pick next ticket, says so in either mode.
    if f.checking && f.open_run.is_none() {
        return (mode, f.looking.to_string());
    }
    if let Some(why) = f.stopped.filter(|_| !f.on) {
        return (Mode::Stopped, format!("Stopped: {why}"));
    }
    if !f.on {
        return (Mode::Manual, "Only runs tickets you give it".to_string());
    }
    let line = if let (Some(ticket), true) = (f.open_run, f.asking) {
        format!("{ticket} is paused for a product answer")
    } else if let Some(ticket) = f.open_run {
        match f.waiting {
            0 => format!("Next {} after {ticket}", f.noun),
            n => format!("Next {} after {ticket} · {n} waiting", f.noun),
        }
    } else {
        let every = Duration::from_secs(u64::from(f.every_minutes) * 60);
        let left = f
            .last
            .and_then(|l| f.now.duration_since(l).ok())
            .map_or(Duration::ZERO, |since| every.saturating_sub(since));
        // Counted down to the second (the card repaints every second).
        let next = match left.as_secs() {
            0 => "next look now".to_string(),
            s => format!("next look in {}:{:02}", s / 60, s % 60),
        };
        match f.found {
            Some(found) => format!("{found} · {next}"),
            None => capitalised(&next),
        }
    };
    (Mode::Automatic, line)
}

/// How far through the wait for its next look a watcher is, 0.0 just after a look to 1.0 when
/// one is due; `None` before its first look.
pub fn look_progress(last: Option<SystemTime>, now: SystemTime, every_minutes: u32) -> Option<f32> {
    let since = now.duration_since(last?).unwrap_or_default();
    let every = Duration::from_secs(u64::from(every_minutes.max(1)) * 60);
    Some((since.as_secs_f32() / every.as_secs_f32()).min(1.0))
}

/// What a **Pick next ticket** found, for the notification it ends with.
pub fn pick_result(s: &LinearSettings, found: &Result<Check, LinearError>) -> (bool, String) {
    match found {
        Ok(Check {
            claimed: Some(t), ..
        }) => (
            true,
            format!(
                "Picked {}: {}. The orchestrator is starting it.",
                t.identifier, t.title
            ),
        ),
        Ok(check) => {
            let who = match s.assignee {
                Assignee::Unassigned => "unassigned ",
                Assignee::Any => "",
            };
            let ran = match check.skipped.len() {
                0 => String::new(),
                n => format!(" ({n} more already running here)"),
            };
            (
                true,
                format!(
                    "No {} ticket to pick: none {who}in {} with an SLA{ran}.",
                    s.team, s.pick
                ),
            )
        }
        Err(e) => (false, format!("Couldn't look in Linear: {e}")),
    }
}

/// The switch's tooltip: what automatic picks, and that off means nothing starts by itself.
pub fn trigger_tooltip(s: &LinearSettings) -> String {
    let who = match s.assignee {
        Assignee::Unassigned => "unassigned ",
        Assignee::Any => "",
    };
    format!(
        "On: every {} min, picks the {who}{} ticket in {} {}. Off: nothing starts unless you \
         give the orchestrator a ticket or press Pick next ticket.",
        s.every_minutes,
        s.team,
        s.pick,
        if s.sla_only {
            "whose SLA breaches first"
        } else {
            "whose SLA breaches first, else the most urgent and oldest"
        }
    )
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
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
            for link in crate::branch_rule::pull_requests(text) {
                if !found.contains(&link) {
                    found.push(link);
                }
            }
        }
    }
    found
}

/// How a PR shows under its ticket's Linear link: `INTH-169 · PR version2-frontend #5546`.
/// Says which ticket it belongs to, as a card can sit beside others; `None` for a link that
/// isn't a GitHub pull request.
pub fn pr_label(ticket: &str, url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (_owner, repo, pull, n) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (pull == "pull" && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{ticket} · PR {repo} #{n}"))
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
/// What a check did: the ticket it claimed, and the ones it passed over because hydraui has a
/// run of them still open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Check {
    pub claimed: Option<Ticket>,
    pub skipped: Vec<String>,
}

pub fn find_and_claim(
    api: &dyn LinearApi,
    settings: &LinearSettings,
    pipeline: &Pipeline,
    already_ran: &[String],
) -> Result<Check, LinearError> {
    let ran = |id: &str| already_ran.iter().any(|r| r.eq_ignore_ascii_case(id));
    let mut skipped: Vec<String> = Vec::new();
    let team = settings.team.as_str();
    let pick_state = state_id(api, team, &settings.pick)?;
    let claim_state = state_id(api, team, &settings.claim)?;
    let unassigned_only = settings.assignee == Assignee::Unassigned;
    let mut passed_over: Vec<String> = Vec::new();
    // A few tickets taken meanwhile in a row is plenty for one check.
    for _ in 0..5 {
        let mut tickets: Vec<Ticket> = api
            .pickable(team, &pick_state, unassigned_only)?
            .into_iter()
            .filter(|t| !passed_over.contains(&t.identifier))
            .collect();
        // A ticket with a run still open here (running, or interrupted and waiting to be
        // resumed) isn't taken twice. One that ended and is back in the trigger state was put
        // back by a person, so it's taken again (the caller passes only open runs).
        let mut by_breach = tickets.clone();
        by_breach.sort_by(|a, b| a.sla_breaches_at.cmp(&b.sla_breaches_at));
        for t in by_breach.iter().filter(|t| ran(&t.identifier)) {
            if !skipped.contains(&t.identifier) {
                skipped.push(t.identifier.clone());
            }
        }
        tickets.retain(|t| !ran(&t.identifier));
        let Some(next) = pick(&tickets, unassigned_only, settings.sla_only) else {
            return Ok(Check {
                claimed: None,
                skipped,
            });
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
        return Ok(Check {
            claimed: Some(ticket),
            skipped,
        });
    }
    Ok(Check {
        claimed: None,
        skipped,
    })
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
    #[test]
    fn the_wait_fills_from_a_look_to_the_next() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        assert_eq!(look_progress(None, at, 2), None);
        assert_eq!(look_progress(Some(at), at, 2), Some(0.0));
        assert_eq!(
            look_progress(Some(at), at + Duration::from_secs(60), 2),
            Some(0.5)
        );
        assert_eq!(
            look_progress(Some(at), at + Duration::from_secs(600), 2),
            Some(1.0)
        );
        assert_eq!(
            look_progress(Some(at + Duration::from_secs(5)), at, 2),
            Some(0.0),
            "clock skew"
        );
    }

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
            priority: 0,
            created_at: String::new(),
        }
    }

    #[test]
    fn the_soonest_breach_goes_first_and_assigned_ones_only_when_wanted() {
        let tickets = [
            t("INTH-1", "2026-11-04T14:00:00.000Z", None),
            t("INTH-2", "2026-11-01T09:00:00.000Z", Some("u-9")),
            t("INTH-3", "2026-11-02T09:00:00.000Z", None),
        ];
        assert_eq!(pick(&tickets, true, true).unwrap().identifier, "INTH-3");
        assert_eq!(pick(&tickets, false, true).unwrap().identifier, "INTH-2");
        assert_eq!(pick(&[], true, true), None);
        // Taken meanwhile: the claim found it assigned, so it's left out and the next one goes.
        let rest: Vec<Ticket> = tickets
            .iter()
            .filter(|x| x.identifier != "INTH-3")
            .cloned()
            .collect();
        assert_eq!(pick(&rest, true, true).unwrap().identifier, "INTH-1");
    }

    #[test]
    fn without_sla_only_a_ticket_with_no_sla_is_taken_by_priority_then_age() {
        let none = |id: &str, priority: u8, created: &str| Ticket {
            sla_breaches_at: None,
            priority,
            created_at: created.into(),
            ..t(id, "", None)
        };
        let tickets = [
            none("PRE-1978", 0, "2026-10-01T00:00:00Z"),
            none("PRE-1482", 4, "2026-09-01T00:00:00Z"),
            none("PRE-2005", 3, "2026-10-20T00:00:00Z"),
            none("PRE-1363", 3, "2026-08-01T00:00:00Z"),
        ];
        assert_eq!(pick(&tickets, true, true), None, "SLA only: none of them");
        assert_eq!(pick(&tickets, true, false).unwrap().identifier, "PRE-1363");
        // One with an SLA still goes first.
        let mut with_sla = tickets.to_vec();
        with_sla.push(t("PRE-9", "2026-12-01T00:00:00.000Z", None));
        assert_eq!(pick(&with_sla, true, false).unwrap().identifier, "PRE-9");
        // No priority comes after Low.
        let low_or_none = [tickets[0].clone(), tickets[1].clone()];
        assert_eq!(
            pick(&low_or_none, true, false).unwrap().identifier,
            "PRE-1482"
        );
    }

    fn inth() -> LinearSettings {
        crate::pipeline::LinearSettings {
            team: "INTH".into(),
            pick: "Ready for Investigation".into(),
            assignee: Assignee::Unassigned,
            claim: "In Investigation".into(),
            passed: "Ready for Peer Review".into(),
            every_minutes: 5,
            watch: true,
            sla_only: true,
        }
    }

    fn facts(settings: &LinearSettings) -> TriggerFacts<'_> {
        TriggerFacts {
            on: true,
            checking: false,
            stopped: None,
            open_run: None,
            asking: false,
            found: None,
            last: None,
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(10_000),
            looking: "Looking in Linear for the next INTH ticket…",
            every_minutes: settings.every_minutes,
            noun: "ticket",
            waiting: 0,
        }
    }

    #[test]
    fn automatic_says_what_it_found_and_when_it_looks_next() {
        let s = inth();
        let mut f = facts(&s);
        assert_eq!(trigger(&f), (Mode::Automatic, "Next look now".to_string()));
        f.last = Some(f.now - Duration::from_secs(61));
        f.found = Some("Nothing new");
        assert_eq!(trigger(&f).1, "Nothing new · next look in 3:59");
        f.last = Some(f.now - Duration::from_secs(250));
        assert_eq!(trigger(&f).1, "Nothing new · next look in 0:50");
        f.last = Some(f.now - Duration::from_secs(400));
        assert_eq!(trigger(&f).1, "Nothing new · next look now");
        f.checking = true;
        assert_eq!(trigger(&f).1, "Looking in Linear for the next INTH ticket…");
        f.open_run = Some("INTH-169");
        assert_eq!(trigger(&f).1, "Next ticket after INTH-169");
        f.noun = "PR";
        f.waiting = 2;
        assert_eq!(trigger(&f).1, "Next PR after INTH-169 · 2 waiting");
        f.noun = "ticket";
        f.waiting = 0;
        f.asking = true;
        assert_eq!(trigger(&f).1, "INTH-169 is paused for a product answer");
    }

    #[test]
    fn off_is_manual_and_a_stop_by_itself_says_why() {
        let s = inth();
        let mut f = facts(&s);
        f.on = false;
        assert_eq!(
            trigger(&f),
            (Mode::Manual, "Only runs tickets you give it".to_string())
        );
        f.stopped = Some("the API key was rejected");
        assert_eq!(
            trigger(&f),
            (
                Mode::Stopped,
                "Stopped: the API key was rejected".to_string()
            )
        );
        // Back on, an old stop no longer counts.
        f.on = true;
        assert_eq!(trigger(&f).0, Mode::Automatic);
    }

    #[test]
    fn a_pick_by_hand_says_it_is_looking_in_manual_too() {
        let s = inth();
        let mut f = facts(&s);
        f.on = false;
        f.checking = true;
        assert_eq!(
            trigger(&f),
            (
                Mode::Manual,
                "Looking in Linear for the next INTH ticket…".to_string()
            )
        );
    }

    #[test]
    fn a_pick_by_hand_ends_saying_what_it_found() {
        let s = inth();
        let picked = Ok(Check {
            claimed: Some(t("INTH-170", "2026-11-04T14:00:00.000Z", None)),
            skipped: vec![],
        });
        assert_eq!(
            pick_result(&s, &picked),
            (
                true,
                "Picked INTH-170: Title of INTH-170. The orchestrator is starting it.".to_string()
            )
        );
        let none = Ok(Check {
            claimed: None,
            skipped: vec!["INTH-165".into(), "INTH-166".into()],
        });
        assert_eq!(
            pick_result(&s, &none).1,
            "No INTH ticket to pick: none unassigned in Ready for Investigation with an SLA \
             (2 more already running here)."
        );
        let failed = Err(LinearError::Failed("timed out".into()));
        let (ok, text) = pick_result(&s, &failed);
        assert!(!ok);
        assert!(text.starts_with("Couldn't look in Linear: "), "{text}");
    }

    #[test]
    fn a_pr_is_labelled_with_its_ticket_repo_and_number() {
        assert_eq!(
            pr_label(
                "INTH-169",
                "https://github.com/iov42/version2-frontend/pull/5546"
            )
            .as_deref(),
            Some("INTH-169 · PR version2-frontend #5546")
        );
        assert_eq!(pr_label("INTH-1", "https://github.com/a/b/issues/3"), None);
        assert_eq!(pr_label("INTH-1", "https://example.com/a/b/pull/3"), None);
    }

    #[test]
    fn the_tooltip_says_what_automatic_picks_and_what_off_means() {
        let tip = trigger_tooltip(&inth());
        assert!(
            tip.contains("picks the unassigned INTH ticket in Ready for Investigation"),
            "{tip}"
        );
        assert!(tip.contains("Off: nothing starts unless you"), "{tip}");
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
        let got = find_and_claim(&api, &settings(), &pipeline(), &[])
            .unwrap()
            .claimed
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
        let got = find_and_claim(&api, &settings(), &pipeline(), &[])
            .unwrap()
            .claimed
            .unwrap();
        assert_eq!(got.identifier, "INTH-1");
        assert!(api.writes.borrow().iter().all(|w| !w.contains("INTH-2")));
    }

    #[test]
    fn nothing_to_pick_writes_nothing() {
        let api = fake_with(vec![t("INTH-1", "2026-11-04T00:00:00.000Z", Some("u-9"))]);
        assert_eq!(
            find_and_claim(&api, &settings(), &pipeline(), &[])
                .unwrap()
                .claimed,
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
        let got = find_and_claim(&api, &s, &pipeline(), &[])
            .unwrap()
            .claimed
            .unwrap();
        assert_eq!(got.identifier, "INTH-5");
        assert_eq!(api.writes.borrow()[0], "update uuid-INTH-5 s-claim -");
    }

    #[test]
    fn a_state_the_team_lacks_is_named() {
        let api = fake_with(vec![]);
        let mut s = settings();
        s.claim = "Doing".into();
        assert_eq!(
            find_and_claim(&api, &s, &pipeline(), &[]).unwrap_err(),
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

    #[test]
    fn tickets_hydraui_already_ran_are_skipped_and_named() {
        let api = fake_with(vec![
            t("INTH-166", "2026-11-04T00:00:00.000Z", None),
            t("INTH-169", "2026-11-05T00:00:00.000Z", None),
            t("INTH-165", "2026-11-18T00:00:00.000Z", None),
        ]);
        let ran = ["INTH-166".to_string(), "inth-165".to_string()];
        let check = find_and_claim(&api, &settings(), &pipeline(), &ran).unwrap();
        assert_eq!(check.claimed.unwrap().identifier, "INTH-169");
        assert_eq!(check.skipped, ["INTH-166", "INTH-165"]);
        assert!(api.writes.borrow().iter().all(|w| w.contains("INTH-169")));
        // Only already-run ones left: nothing claimed, both named.
        let api = fake_with(vec![t("INTH-166", "2026-11-04T00:00:00.000Z", None)]);
        let check = find_and_claim(&api, &settings(), &pipeline(), &ran).unwrap();
        assert_eq!(
            (check.claimed, check.skipped),
            (None, vec!["INTH-166".to_string()])
        );
        assert!(api.writes.borrow().is_empty());
    }
}
