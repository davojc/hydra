//! A milestone run (spec §14.5): what to do next for a milestone's tickets, one at a time.
//! hydraui asks after each session ends; this says whether to work a ticket, check one, wait
//! for the lead, or stop because the milestone is finished.

use hydraui_projects::{Decision, Project, Review, Ticket, TicketStatus};

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

/// The milestone's approved tickets for Claude, in the order they're worked: by number.
pub fn queue<'a>(p: &'a Project, milestone: &str) -> Vec<&'a Ticket> {
    let mut out: Vec<&Ticket> = p
        .tickets
        .iter()
        .filter(|t| t.meta.milestone == milestone && t.meta.review == Review::Approved)
        // A task for a person is theirs to do: the run never starts Claude on it.
        .filter(|t| !t.meta.person)
        .collect();
    out.sort_by_key(|t| hydraui_projects::number_of(&t.id).unwrap_or(u32::MAX));
    out
}

/// The next step for `milestone`'s run. `cloned` says whether a code repo is cloned here.
///
/// Every ticket with pull requests is tested before it's in review. A failure goes back to a
/// work session to fix (the check tool moves it back to in progress); failing again stops the
/// run for a person.
pub fn next_step(p: &Project, milestone: &str, cloned: impl Fn(&str) -> bool) -> Step {
    for t in queue(p, milestone) {
        match t.meta.status {
            TicketStatus::Done | TicketStatus::Dropped => continue,
            TicketStatus::Testing | TicketStatus::InReview => match t.meta.check.as_str() {
                "failed" => {
                    return Step::Pause(format!(
                        "{} failed testing again: {}",
                        t.id,
                        if t.meta.check_note.is_empty() {
                            "see its pull request"
                        } else {
                            &t.meta.check_note
                        }
                    ));
                }
                "" if !t.meta.prs.is_empty() => return Step::Check(t.id.clone()),
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
        // A ticket that doesn't name its repo is worked across all the project's repos.
        let repos = if t.meta.repo.is_empty() {
            if p.file.repos.is_empty() {
                return Step::Pause(format!(
                    "{} doesn't name its repo and the project lists none: add its code repos \
                     in Settings",
                    t.id
                ));
            }
            p.file.repos.clone()
        } else {
            vec![t.meta.repo.clone()]
        };
        if let Some(r) = repos.into_iter().find(|r| !cloned(r)) {
            return Step::NeedClone(r);
        }
        return Step::Work(t.id.clone());
    }
    Step::Finished
}

/// Where a ticket's testing stands, for the people looking at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestState {
    /// Waiting for, or in, a testing session.
    Testing,
    /// Failed testing; a work session fixes it.
    Fixing,
    /// Failed again: a person decides.
    Stuck,
    Passed,
    /// In review without a test, from before testing was a stage.
    NotTested,
}

/// Ticket `t`'s testing; `None` before it has pull requests to test.
pub fn test_state(t: &Ticket) -> Option<TestState> {
    if t.meta.prs.is_empty() {
        return None;
    }
    let failed = t.meta.check == "failed";
    Some(match t.meta.status {
        TicketStatus::InProgress if failed => TestState::Fixing,
        TicketStatus::Testing if failed => TestState::Stuck,
        TicketStatus::Testing => TestState::Testing,
        _ if t.meta.check == "passed" => TestState::Passed,
        TicketStatus::InReview if failed => TestState::Stuck,
        TicketStatus::InReview => TestState::NotTested,
        _ => return None,
    })
}

/// Whether ticket `t`'s work session has finished: it reported pull requests (testing), or
/// work that changed no code (done, with its findings).
pub fn work_finished(t: &Ticket) -> bool {
    match t.meta.status {
        TicketStatus::Done => true,
        TicketStatus::Testing | TicketStatus::InReview => !t.meta.prs.is_empty(),
        _ => false,
    }
}

/// The open questions ticket `ticket`'s work session asked and is waiting on, oldest first.
pub fn session_questions<'a>(p: &'a Project, ticket: &str) -> Vec<&'a Decision> {
    p.decisions
        .iter()
        .filter(|d| {
            d.is_open()
                && d.meta.ticket == ticket
                && d.meta.asked_by == crate::mcp::project_tools::ASKED_BY_CLAUDE
        })
        .collect()
}

/// The milestone's tickets that have a reported outcome, latest first: by when their session
/// finished, then (for ones reported before that was recorded) the highest number first.
pub fn latest_outcomes<'a>(p: &'a Project, milestone: &str) -> Vec<&'a Ticket> {
    let mut out: Vec<&Ticket> = p
        .tickets
        .iter()
        .filter(|t| t.meta.milestone == milestone)
        .filter(|t| !crate::projects::ticket_sections(&t.body).work.is_empty())
        .collect();
    out.sort_by_key(|t| {
        std::cmp::Reverse((
            t.meta.finished_at.map(|d| d.to_string()),
            hydraui_projects::number_of(&t.id),
        ))
    });
    out
}

/// Whether starting `milestone`'s run picks up work already begun: one of its tickets is in
/// progress, its session gone (hydraui closed, or the run was stopped). Start then says Continue.
pub fn picks_up(p: &Project, milestone: &str) -> bool {
    queue(p, milestone)
        .iter()
        .any(|t| t.meta.status == TicketStatus::InProgress)
}

/// The branch ticket `ticket` builds on in code repo `repo`: the branch of the latest earlier
/// ticket in the same milestone that's unmerged (testing or in review) and is in that repo, or names none (so
/// may have changed it), so later work sees earlier unmerged work. None means the repo's
/// default branch; so does a branch that was never pushed to `repo`.
pub fn stack_base(p: &Project, milestone: &str, ticket: &str, repo: &str) -> Option<String> {
    let q = queue(p, milestone);
    let at = q.iter().position(|t| t.id == ticket)?;
    q[..at]
        .iter()
        .rev()
        .find(|t| {
            (t.meta.repo.is_empty() || t.meta.repo.eq_ignore_ascii_case(repo))
                && matches!(
                    t.meta.status,
                    TicketStatus::Testing | TicketStatus::InReview
                )
                && !t.meta.branch.is_empty()
        })
        .map(|t| t.meta.branch.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydraui_projects::{Decision, DecisionMeta, TicketMeta};

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

    #[test]
    fn tickets_run_in_number_order_and_the_milestone_finishes() {
        let mut p = project(vec![
            ticket("PG-10", FE, TicketStatus::Todo),
            ticket("PG-2", BE, TicketStatus::Done),
            ticket("PG-3", BE, TicketStatus::Todo),
        ]);
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-3".into()));
        p.tickets[2].meta.status = TicketStatus::InReview;
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-10".into()));
        p.tickets[0].meta.status = TicketStatus::InReview;
        assert_eq!(next_step(&p, "M1", |_| true), Step::Finished);
    }

    #[test]
    fn every_pull_request_is_tested_and_a_failure_goes_back_to_be_fixed() {
        let mut p = project(vec![
            ticket("PG-1", BE, TicketStatus::Testing),
            ticket("PG-2", FE, TicketStatus::Todo),
        ]);
        p.tickets[0].meta.prs = vec!["u1".into()];
        // Tested whatever Settings say: there's no turning it off.
        assert_eq!(next_step(&p, "M1", |_| true), Step::Check("PG-1".into()));
        // The first failure sends it back to a work session with what failed.
        let t = &mut p.tickets[0].meta;
        (t.status, t.check, t.check_failures) = (TicketStatus::InProgress, "failed".into(), 1);
        t.check_note = "two tests fail".into();
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-1".into()));
        // Fixed and reported again: tested again.
        let t = &mut p.tickets[0].meta;
        (t.status, t.check) = (TicketStatus::Testing, String::new());
        assert_eq!(next_step(&p, "M1", |_| true), Step::Check("PG-1".into()));
        // Failing again stops the run for a person.
        let t = &mut p.tickets[0].meta;
        (t.check, t.check_failures) = ("failed".into(), 2);
        assert_eq!(
            next_step(&p, "M1", |_| true),
            Step::Pause("PG-1 failed testing again: two tests fail".into())
        );
        // Passed, it's in review and the run moves on.
        let t = &mut p.tickets[0].meta;
        (t.status, t.check) = (TicketStatus::InReview, "passed".into());
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-2".into()));
        // One left in review untested, from before testing was a stage, is tested too.
        p.tickets[0].meta.check.clear();
        assert_eq!(next_step(&p, "M1", |_| true), Step::Check("PG-1".into()));
    }

    #[test]
    fn a_tickets_testing_reads_as_one_state() {
        let mut t = ticket("PG-1", BE, TicketStatus::InProgress);
        assert_eq!(test_state(&t), None, "no pull request, nothing to test");
        t.meta.prs = vec!["u".into()];
        assert_eq!(test_state(&t), None, "still being worked on");
        t.meta.status = TicketStatus::Testing;
        assert_eq!(test_state(&t), Some(TestState::Testing));
        (t.meta.status, t.meta.check) = (TicketStatus::InProgress, "failed".into());
        assert_eq!(test_state(&t), Some(TestState::Fixing));
        t.meta.status = TicketStatus::Testing;
        assert_eq!(test_state(&t), Some(TestState::Stuck));
        (t.meta.status, t.meta.check) = (TicketStatus::InReview, "passed".into());
        assert_eq!(test_state(&t), Some(TestState::Passed));
        t.meta.check.clear();
        assert_eq!(test_state(&t), Some(TestState::NotTested));
    }

    #[test]
    fn it_pauses_for_what_only_the_lead_can_give() {
        let mut p = project(vec![ticket("PG-1", BE, TicketStatus::Todo)]);
        // A repo with no skill chosen goes ahead with the repo's own CLAUDE.md and skills.
        p.tickets[0].meta.repo = "iov42/svc-usage".into();
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-1".into()));
        p.tickets[0].meta.repo = BE.into();
        assert_eq!(next_step(&p, "M1", |_| false), Step::NeedClone(BE.into()));
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
            next_step(&p, "M1", |_| true),
            Step::Pause("PG-1 waits on Decision 4".into())
        );
        // Unapproved and other milestones' tickets aren't part of the run.
        let mut q = project(vec![ticket("PG-1", BE, TicketStatus::Todo)]);
        q.tickets[0].meta.review = Review::Proposed;
        assert_eq!(next_step(&q, "M1", |_| true), Step::Finished);
        assert_eq!(next_step(&p, "M2", |_| true), Step::Finished);
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
        assert!(
            matches!(next_step(&p, "M1", |_| true), Step::Pause(w) if w.starts_with("PG-2 has a change"))
        );
        p.tickets[1].meta.change = None;
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-2".into()));
    }

    #[test]
    fn a_ticket_builds_on_the_last_unmerged_one_in_its_repo() {
        let p = project(vec![
            ticket("PG-1", BE, TicketStatus::InReview),
            ticket("PG-2", FE, TicketStatus::Testing),
            ticket("PG-3", BE, TicketStatus::Done),
            ticket("PG-4", BE, TicketStatus::Todo),
            ticket("PG-5", FE, TicketStatus::Todo),
        ]);
        assert_eq!(stack_base(&p, "M1", "PG-4", BE).as_deref(), Some("pg-1-x"));
        assert_eq!(stack_base(&p, "M1", "PG-5", FE).as_deref(), Some("pg-2-x"));
        assert_eq!(stack_base(&p, "M1", "PG-1", BE), None);
    }

    #[test]
    fn a_ticket_that_names_no_repo_is_worked_across_all_the_projects_repos() {
        let mut p = project(vec![
            ticket("PG-1", "", TicketStatus::InReview),
            ticket("PG-2", "", TicketStatus::Todo),
        ]);
        // With no repos to work in, only the lead can help.
        assert!(matches!(next_step(&p, "M1", |_| true), Step::Pause(w) if w.contains("Settings")));
        p.file.repos = vec![BE.into(), FE.into()];
        // Every repo must be cloned here first, since the work may land in any of them.
        assert_eq!(next_step(&p, "M1", |r| r != FE), Step::NeedClone(FE.into()));
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-2".into()));
        // In each repo it builds on the earlier unmerged ticket, which may have changed it.
        assert_eq!(stack_base(&p, "M1", "PG-2", BE).as_deref(), Some("pg-1-x"));
        assert_eq!(stack_base(&p, "M1", "PG-2", FE).as_deref(), Some("pg-1-x"));
    }

    #[test]
    fn a_run_with_a_ticket_in_progress_picks_it_back_up() {
        let mut p = project(vec![
            ticket("PG-1", BE, TicketStatus::Done),
            ticket("PG-2", FE, TicketStatus::Todo),
        ]);
        assert!(!picks_up(&p, "M1"));
        p.tickets[1].meta.status = TicketStatus::InProgress;
        assert!(picks_up(&p, "M1"));
        assert!(!picks_up(&p, "M2"));
        // Starting the run again works that ticket first.
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-2".into()));
    }

    #[test]
    fn a_session_finishes_with_pull_requests_or_with_findings_alone() {
        let mut t = ticket("PG-1", BE, TicketStatus::InProgress);
        assert!(!work_finished(&t));
        t.meta.status = TicketStatus::InReview;
        assert!(
            !work_finished(&t),
            "in review but no pull request reported yet"
        );
        t.meta.prs = vec!["u".into()];
        assert!(work_finished(&t));
        // Reporting pull requests sends it to testing.
        t.meta.status = TicketStatus::Testing;
        assert!(work_finished(&t));
        let mut found = ticket("PG-2", BE, TicketStatus::Done);
        found.meta.prs.clear();
        assert!(work_finished(&found));
    }

    #[test]
    fn a_running_session_waits_on_the_questions_it_asked() {
        let mut p = project(vec![ticket("PG-1", BE, TicketStatus::InProgress)]);
        let d = |id: &str, ticket: &str, by: &str, answer: &str| Decision {
            id: id.into(),
            meta: DecisionMeta {
                question: format!("{id}?"),
                ticket: ticket.into(),
                asked_by: by.into(),
                answer: answer.into(),
                ..Default::default()
            },
            body: String::new(),
        };
        let claude = crate::mcp::project_tools::ASKED_BY_CLAUDE;
        p.decisions = vec![
            d("D-1", "", "me", ""),          // the project's, not the session's
            d("D-2", "PG-1", "me", ""),      // a person's question on the ticket
            d("D-3", "PG-1", claude, "yes"), // answered
            d("D-4", "PG-1", claude, ""),
            d("D-5", "PG-2", claude, ""), // another ticket's session
        ];
        let ids: Vec<&str> = session_questions(&p, "PG-1")
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(ids, ["D-4"]);
    }

    #[test]
    fn the_latest_outcomes_come_first() {
        let mut p = project(vec![
            ticket("PG-1", BE, TicketStatus::Done),
            ticket("PG-2", BE, TicketStatus::InReview),
            ticket("PG-3", FE, TicketStatus::InReview),
            ticket("PG-4", FE, TicketStatus::Todo),
        ]);
        for t in &mut p.tickets[..3] {
            t.body = format!("Ask.\n\n## Work\n\n{} done.\n", t.id);
        }
        p.tickets[1].meta.finished_at = Some("2026-11-06T10:00:00Z".parse().unwrap());
        p.tickets[2].meta.finished_at = Some("2026-11-06T09:00:00Z".parse().unwrap());
        let ids: Vec<&str> = latest_outcomes(&p, "M1")
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        // Timed ones latest first; one reported before times were kept comes after.
        assert_eq!(ids, ["PG-2", "PG-3", "PG-1"]);
    }

    #[test]
    fn a_run_leaves_tasks_for_a_person_to_them() {
        let mut p = project(vec![
            ticket("PG-1", BE, TicketStatus::Todo),
            ticket("PG-2", FE, TicketStatus::Todo),
        ]);
        p.tickets[0].meta.person = true;
        assert_eq!(next_step(&p, "M1", |_| true), Step::Work("PG-2".into()));
        p.tickets[1].meta.status = TicketStatus::Done;
        // The person's task still open doesn't hold the run.
        assert_eq!(next_step(&p, "M1", |_| true), Step::Finished);
        p.tickets[0].meta.status = TicketStatus::InProgress;
        assert!(!picks_up(&p, "M1"));
    }
}
