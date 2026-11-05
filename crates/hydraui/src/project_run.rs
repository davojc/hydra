//! A milestone run (spec §14.5): what to do next for a milestone's tickets, one at a time.
//! hydraui asks after each session ends; this says whether to work a ticket, check one, wait
//! for the lead, or stop because the milestone is finished.

use hydraui_projects::{Project, Review, Ticket, TicketStatus};

use crate::projects::WorkSetup;

/// What a milestone run does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Start (or restart) a work session on this ticket.
    Work(String),
    /// Start a testing session on this ticket's pull requests.
    Check(String),
    /// Wait for the lead: why, in a line.
    Pause(String),
    /// Wait for the lead to say where this code repo is cloned on this machine.
    NeedClone(String),
    /// Every ticket is done, in review and checked, or dropped.
    Finished,
}

/// The milestone's approved tickets in the order they're worked: by number.
pub fn queue<'a>(p: &'a Project, milestone: &str) -> Vec<&'a Ticket> {
    let mut out: Vec<&Ticket> = p
        .tickets
        .iter()
        .filter(|t| t.meta.milestone == milestone && t.meta.review == Review::Approved)
        .collect();
    out.sort_by_key(|t| hydraui_projects::number_of(&t.id).unwrap_or(u32::MAX));
    out
}

/// The next step for `milestone`'s run. `cloned` says whether a code repo is cloned here.
pub fn next_step(
    p: &Project,
    milestone: &str,
    setup: &WorkSetup,
    cloned: impl Fn(&str) -> bool,
) -> Step {
    for t in queue(p, milestone) {
        match t.meta.status {
            TicketStatus::Done | TicketStatus::Dropped => continue,
            TicketStatus::InReview => match t.meta.check.as_str() {
                "failed" => {
                    return Step::Pause(format!(
                        "{}'s check failed: {}",
                        t.id,
                        if t.meta.check_note.is_empty() {
                            "see its pull request"
                        } else {
                            &t.meta.check_note
                        }
                    ));
                }
                "" if setup.testing.is_some() && !t.meta.prs.is_empty() => {
                    return Step::Check(t.id.clone());
                }
                _ => continue,
            },
            TicketStatus::Todo | TicketStatus::InProgress => {}
        }
        if t.meta.change.is_some() {
            return Step::Pause(format!(
                "{} has a change waiting for you: accept it or turn it down",
                t.id
            ));
        }
        if let Some(d) = p
            .decisions
            .iter()
            .find(|d| d.is_open() && d.meta.ticket == t.id)
        {
            return Step::Pause(format!(
                "{} waits on {}",
                t.id,
                crate::projects::decision_name(&d.id)
            ));
        }
        if t.meta.repo.is_empty() {
            return Step::Pause(format!("{} doesn't name its repo: set it with Edit…", t.id));
        }
        if !cloned(&t.meta.repo) {
            return Step::NeedClone(t.meta.repo.clone());
        }
        return Step::Work(t.id.clone());
    }
    Step::Finished
}

/// The branch ticket `ticket` builds on: the branch of the latest earlier ticket in the same
/// milestone and repo that's still in review, so later work sees earlier unmerged work. None
/// means the repo's default branch.
pub fn stack_base(p: &Project, milestone: &str, ticket: &str) -> Option<String> {
    let q = queue(p, milestone);
    let at = q.iter().position(|t| t.id == ticket)?;
    let repo = &q[at].meta.repo;
    q[..at]
        .iter()
        .rev()
        .find(|t| {
            t.meta.repo.eq_ignore_ascii_case(repo)
                && t.meta.status == TicketStatus::InReview
                && !t.meta.branch.is_empty()
        })
        .map(|t| t.meta.branch.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydraui_projects::{Decision, DecisionMeta, TicketMeta};
    use std::path::PathBuf;

    const FE: &str = "iov42/version2-frontend";
    const BE: &str = "iov42/version-2-backend";

    fn ticket(id: &str, repo: &str, status: TicketStatus) -> Ticket {
        Ticket {
            id: id.into(),
            meta: TicketMeta {
                title: id.into(),
                milestone: "M1".into(),
                repo: repo.into(),
                status,
                review: Review::Approved,
                branch: format!("{}-x", id.to_lowercase()),
                ..Default::default()
            },
            body: String::new(),
        }
    }

    fn project(tickets: Vec<Ticket>) -> Project {
        Project {
            slug: "pg".into(),
            file: toml::from_str("name = \"PG\"\nkey = \"PG\"\nlead = \"me\"").unwrap(),
            spec: String::new(),
            milestones: vec![],
            tickets,
            decisions: vec![],
            meetings: vec![],
            files: vec![],
            links: Default::default(),
            progress: vec![],
        }
    }

    fn setup(testing: bool) -> WorkSetup {
        WorkSetup {
            skills: [
                (FE.to_string(), PathBuf::from("/fe")),
                (BE.into(), "/be".into()),
            ]
            .into_iter()
            .collect(),
            testing: testing.then(|| PathBuf::from("/qa")),
            ..Default::default()
        }
    }

    #[test]
    fn tickets_run_in_number_order_and_the_milestone_finishes() {
        let mut p = project(vec![
            ticket("PG-10", FE, TicketStatus::Todo),
            ticket("PG-2", BE, TicketStatus::Done),
            ticket("PG-3", BE, TicketStatus::Todo),
        ]);
        let s = setup(false);
        assert_eq!(next_step(&p, "M1", &s, |_| true), Step::Work("PG-3".into()));
        p.tickets[2].meta.status = TicketStatus::InReview;
        assert_eq!(
            next_step(&p, "M1", &s, |_| true),
            Step::Work("PG-10".into())
        );
        p.tickets[0].meta.status = TicketStatus::InReview;
        assert_eq!(next_step(&p, "M1", &s, |_| true), Step::Finished);
    }

    #[test]
    fn with_a_testing_skill_each_pull_request_is_checked_before_moving_on() {
        let mut p = project(vec![
            ticket("PG-1", BE, TicketStatus::InReview),
            ticket("PG-2", FE, TicketStatus::Todo),
        ]);
        p.tickets[0].meta.prs = vec!["u1".into()];
        let s = setup(true);
        assert_eq!(
            next_step(&p, "M1", &s, |_| true),
            Step::Check("PG-1".into())
        );
        p.tickets[0].meta.check = "failed".into();
        p.tickets[0].meta.check_note = "two tests fail".into();
        assert_eq!(
            next_step(&p, "M1", &s, |_| true),
            Step::Pause("PG-1's check failed: two tests fail".into())
        );
        p.tickets[0].meta.check = "passed".into();
        assert_eq!(next_step(&p, "M1", &s, |_| true), Step::Work("PG-2".into()));
    }

    #[test]
    fn it_pauses_for_what_only_the_lead_can_give() {
        let mut p = project(vec![ticket("PG-1", "", TicketStatus::Todo)]);
        let s = setup(false);
        assert!(matches!(next_step(&p, "M1", &s, |_| true), Step::Pause(w) if w.contains("repo")));
        // A repo with no skill chosen goes ahead with the repo's own CLAUDE.md and skills.
        p.tickets[0].meta.repo = "iov42/svc-usage".into();
        assert_eq!(next_step(&p, "M1", &s, |_| true), Step::Work("PG-1".into()));
        p.tickets[0].meta.repo = BE.into();
        assert_eq!(
            next_step(&p, "M1", &s, |_| false),
            Step::NeedClone(BE.into())
        );
        p.decisions.push(Decision {
            id: "D-4".into(),
            meta: DecisionMeta {
                question: "q".into(),
                ticket: "PG-1".into(),
                ..Default::default()
            },
            body: String::new(),
        });
        assert_eq!(
            next_step(&p, "M1", &s, |_| true),
            Step::Pause("PG-1 waits on Decision 4".into())
        );
        // Unapproved and other milestones' tickets aren't part of the run.
        let mut q = project(vec![ticket("PG-1", BE, TicketStatus::Todo)]);
        q.tickets[0].meta.review = Review::Proposed;
        assert_eq!(next_step(&q, "M1", &s, |_| true), Step::Finished);
        assert_eq!(next_step(&p, "M2", &s, |_| true), Step::Finished);
    }

    #[test]
    fn a_run_pauses_at_a_ticket_with_a_change_waiting() {
        let mut p = project(vec![
            ticket("PG-1", BE, TicketStatus::InReview),
            ticket("PG-2", FE, TicketStatus::Todo),
        ]);
        p.tickets[1].meta.change = Some(hydraui_projects::Change {
            why: "Joshua".into(),
            ..Default::default()
        });
        let s = setup(false);
        assert!(
            matches!(next_step(&p, "M1", &s, |_| true), Step::Pause(w) if w.starts_with("PG-2 has a change"))
        );
        p.tickets[1].meta.change = None;
        assert_eq!(next_step(&p, "M1", &s, |_| true), Step::Work("PG-2".into()));
    }

    #[test]
    fn a_ticket_builds_on_the_last_unmerged_one_in_its_repo() {
        let p = project(vec![
            ticket("PG-1", BE, TicketStatus::InReview),
            ticket("PG-2", FE, TicketStatus::InReview),
            ticket("PG-3", BE, TicketStatus::Done),
            ticket("PG-4", BE, TicketStatus::Todo),
            ticket("PG-5", FE, TicketStatus::Todo),
        ]);
        assert_eq!(stack_base(&p, "M1", "PG-4").as_deref(), Some("pg-1-x"));
        assert_eq!(stack_base(&p, "M1", "PG-5").as_deref(), Some("pg-2-x"));
        assert_eq!(stack_base(&p, "M1", "PG-1"), None);
    }
}
