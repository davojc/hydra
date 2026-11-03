//! Milestone progress (spec §8.2) and the 5-minute check's rules (spec §6.1).

use std::collections::BTreeMap;

use crate::model::*;

/// How many of a milestone's tickets are in each status. Dropped tickets don't count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub todo: usize,
    pub in_progress: usize,
    pub in_review: usize,
    pub done: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.todo + self.in_progress + self.in_review + self.done
    }

    /// Done tickets as a share of all of them, 0 to 1; 0 for a milestone with no tickets.
    pub fn done_share(&self) -> f32 {
        match self.total() {
            0 => 0.0,
            n => self.done as f32 / n as f32,
        }
    }

    pub fn of(&self, status: TicketStatus) -> usize {
        match status {
            TicketStatus::Todo => self.todo,
            TicketStatus::InProgress => self.in_progress,
            TicketStatus::InReview => self.in_review,
            TicketStatus::Done => self.done,
            TicketStatus::Dropped => 0,
        }
    }

    fn add(&mut self, status: TicketStatus) {
        match status {
            TicketStatus::Todo => self.todo += 1,
            TicketStatus::InProgress => self.in_progress += 1,
            TicketStatus::InReview => self.in_review += 1,
            TicketStatus::Done => self.done += 1,
            TicketStatus::Dropped => {}
        }
    }
}

/// Counts for every milestone of `project`, by milestone id, with `status_of` giving each
/// ticket's status as the viewer sees it (their unsent moves included).
pub fn milestone_counts(
    project: &Project,
    status_of: impl Fn(&Ticket) -> TicketStatus,
) -> BTreeMap<String, Counts> {
    let mut out: BTreeMap<String, Counts> = project
        .milestones
        .iter()
        .map(|m| (m.id.clone(), Counts::default()))
        .collect();
    for t in &project.tickets {
        out.entry(t.meta.milestone.clone())
            .or_default()
            .add(status_of(t));
    }
    out
}

/// Whether a milestone's target has passed and it isn't done. `today` is `YYYY-MM-DD`.
pub fn is_late(milestone: &Milestone, counts: &Counts, today: &str) -> bool {
    let finished = milestone.status == MilestoneStatus::Done
        || (counts.total() > 0 && counts.done == counts.total());
    match &milestone.target {
        Some(target) if !finished => target.to_string().as_str() < today,
        _ => false,
    }
}

/// Today's date in UTC, `YYYY-MM-DD`.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    date_of_unix_days((secs / 86_400) as i64)
}

/// Now in UTC as a TOML date-time, for `asked_at`, `decided_at` and `created`.
pub fn now() -> toml::value::Datetime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    datetime_of_unix(secs)
}

/// The UTC date-time `secs` after the epoch.
pub fn datetime_of_unix(secs: u64) -> toml::value::Datetime {
    let date = date_of_unix_days((secs / 86_400) as i64);
    let t = secs % 86_400;
    let text = format!("{date}T{:02}:{:02}:{:02}Z", t / 3600, t / 60 % 60, t % 60);
    // Only fails for dates TOML can't hold (before year 0): an empty date-time then.
    text.parse().unwrap_or(toml::value::Datetime {
        date: None,
        time: None,
        offset: None,
    })
}

/// The civil date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
pub fn date_of_unix_days(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The ticket ids with `key` named in `text` (a branch name, commit message or PR title), in
/// the order they appear, each once. `PG-12` matches in `PG-12`, `pg-12-share-dialog`,
/// `feat(PG-12):`, but not in `XPG-12` or `PG-123` for `PG-12`.
pub fn ticket_ids_in(text: &str, key: &str) -> Vec<String> {
    let key_upper = key.to_ascii_uppercase();
    let upper = text.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut found: Vec<String> = Vec::new();
    let mut from = 0;
    let needle = format!("{key_upper}-");
    while let Some(at) = upper[from..].find(&needle) {
        let start = from + at;
        from = start + needle.len();
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let digits: String = upper[from..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() || digits.starts_with('0') {
            continue;
        }
        let id = format!("{key_upper}-{digits}");
        if !found.contains(&id) {
            found.push(id);
        }
    }
    found
}

/// What git or GitHub showed about a ticket (spec §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// A branch or commit names it.
    Commit,
    /// An open PR names it.
    PrOpened,
    /// A merged PR names it.
    PrMerged,
}

/// The status a signal moves a ticket to, or `None` when it wouldn't move it forward. The check
/// only ever moves a ticket forward, and never touches a dropped ticket.
pub fn move_for(current: TicketStatus, signal: Signal) -> Option<TicketStatus> {
    let to = match signal {
        Signal::Commit => TicketStatus::InProgress,
        Signal::PrOpened => TicketStatus::InReview,
        Signal::PrMerged => TicketStatus::Done,
    };
    (current != TicketStatus::Dropped && to > current).then_some(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(id: &str, milestone: &str, status: TicketStatus) -> Ticket {
        Ticket {
            id: id.into(),
            meta: TicketMeta {
                title: id.into(),
                kind: TicketKind::Feature,
                status,
                milestone: milestone.into(),
                assignee: String::new(),
                created_by: String::new(),
                created: None,
                extra: Default::default(),
            },
            body: String::new(),
        }
    }

    fn milestone(id: &str, target: Option<&str>) -> Milestone {
        Milestone {
            id: id.into(),
            title: id.into(),
            target: target.map(|t| t.parse().unwrap()),
            status: MilestoneStatus::Active,
            description: String::new(),
        }
    }

    fn project(tickets: Vec<Ticket>) -> Project {
        Project {
            slug: "pg".into(),
            file: toml::from_str("name = \"PG\"\nkey = \"PG\"\nlead = \"me\"").unwrap(),
            spec: String::new(),
            milestones: vec![milestone("M1", None), milestone("M2", None)],
            tickets,
            decisions: vec![],
            meetings: vec![],
            files: vec![],
            links: vec![],
            progress: vec![],
        }
    }

    #[test]
    fn counts_tickets_per_milestone_and_skips_dropped() {
        use TicketStatus::*;
        let p = project(vec![
            ticket("PG-1", "M1", Done),
            ticket("PG-2", "M1", InProgress),
            ticket("PG-3", "M1", Dropped),
            ticket("PG-4", "M2", Todo),
        ]);
        let counts = milestone_counts(&p, |t| t.meta.status);
        assert_eq!(counts["M1"].total(), 2);
        assert_eq!(counts["M1"].done_share(), 0.5);
        assert_eq!(counts["M2"].todo, 1);
    }

    #[test]
    fn the_viewers_unsent_moves_count() {
        let p = project(vec![ticket("PG-1", "M1", TicketStatus::Todo)]);
        let counts = milestone_counts(&p, |_| TicketStatus::Done);
        assert_eq!(counts["M1"].done, 1);
    }

    #[test]
    fn a_milestone_is_late_only_past_its_target_and_unfinished() {
        let m = milestone("M1", Some("2026-11-01"));
        let open = Counts {
            todo: 1,
            ..Default::default()
        };
        let all_done = Counts {
            done: 2,
            ..Default::default()
        };
        assert!(is_late(&m, &open, "2026-11-03"));
        assert!(!is_late(&m, &open, "2026-11-01"));
        assert!(!is_late(&m, &all_done, "2026-11-03"));
        assert!(!is_late(&milestone("M2", None), &open, "2026-11-03"));
    }

    #[test]
    fn dates_from_unix_days() {
        assert_eq!(date_of_unix_days(0), "1970-01-01");
        assert_eq!(date_of_unix_days(20_760), "2026-11-03");
        assert_eq!(date_of_unix_days(11_016), "2000-02-29");
    }

    #[test]
    fn datetimes_from_unix_seconds() {
        assert_eq!(
            datetime_of_unix(20_760 * 86_400 + 3_723).to_string(),
            "2026-11-03T01:02:03Z"
        );
    }

    #[test]
    fn finds_ticket_ids_in_branches_commits_and_titles() {
        assert_eq!(ticket_ids_in("pg-12-share-dialog", "PG"), ["PG-12"]);
        assert_eq!(
            ticket_ids_in("feat(PG-12): x, refs PG-3 and PG-12", "PG"),
            ["PG-12", "PG-3"]
        );
        assert!(ticket_ids_in("XPG-12 PG- PG-0 PGX-1", "PG").is_empty());
        assert_eq!(ticket_ids_in("PG-123", "PG"), ["PG-123"]);
    }

    #[test]
    fn the_check_only_moves_forward() {
        use TicketStatus::*;
        assert_eq!(move_for(Todo, Signal::Commit), Some(InProgress));
        assert_eq!(move_for(InProgress, Signal::PrOpened), Some(InReview));
        assert_eq!(move_for(Todo, Signal::PrMerged), Some(Done));
        assert_eq!(move_for(InReview, Signal::Commit), None);
        assert_eq!(move_for(Done, Signal::PrOpened), None);
        assert_eq!(move_for(Dropped, Signal::PrMerged), None);
    }
}
