//! The local check and the repo sync (spec §6.1, §7).
//!
//! Every 5 minutes, each engineer's hydraui looks at their own work in the project's code
//! repos: their pull requests, through the environment's gh, and their branches and recent
//! commits in the local clone. A ticket key (`PG-12`) in a branch, commit or pull request
//! moves that ticket forward: in progress, in review, done. The moves are local edits, part of
//! the engineer's pending change. At the project's sync times the pending change gets a short
//! write-up in `progress/<login>/<date>.md` and is sent.

use std::collections::BTreeMap;
use std::path::Path;

use hydraui_projects::progress::{Signal, move_for, ticket_ids_in};
use hydraui_projects::{Progress, Project, Review, TicketStatus, front_matter, repo};
use serde::Deserialize;

use crate::project_send::Runner;

/// Something git or GitHub showed that may name tickets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub signal: Signal,
    /// The branch name, commit subject or pull request title and branch.
    pub text: String,
    /// The pull request, for a pull request.
    pub url: Option<String>,
}

/// A ticket the check moves forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub ticket: String,
    pub from: TicketStatus,
    pub to: TicketStatus,
    /// Pull requests that named it, to show on the ticket.
    pub prs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    title: String,
    head_ref_name: String,
    state: String,
    url: String,
}

/// The `--json` fields [`prs_observed`] reads.
const PR_FIELDS: &str = "title,headRefName,state,url";

/// Pull requests in `gh pr list --json` output: open ones say "in review", merged ones "done".
/// Closed without merging say nothing.
pub fn prs_observed(json: &str) -> Result<Vec<Observation>, String> {
    let raw: Vec<RawPr> = serde_json::from_str(json)
        .map_err(|e| format!("gh gave pull requests hydraui can't read: {e}"))?;
    Ok(raw
        .into_iter()
        .filter_map(|p| {
            let signal = match p.state.as_str() {
                "OPEN" => Signal::PrOpened,
                "MERGED" => Signal::PrMerged,
                _ => return None,
            };
            Some(Observation {
                signal,
                text: format!("{} {}", p.title, p.head_ref_name),
                url: Some(p.url),
            })
        })
        .collect())
}

/// Branch names and commit subjects, one per line: each says "in progress".
pub fn work_observed(lines: &str) -> Vec<Observation> {
    lines
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| Observation {
            signal: Signal::Commit,
            text: l.to_string(),
            url: None,
        })
        .collect()
}

/// The moves `seen` makes in project `p`: each ticket it names goes as far forward as the
/// furthest signal says, never back, and never for a rejected or dropped ticket.
pub fn moves(p: &Project, seen: &[Observation]) -> Vec<Move> {
    let mut best: BTreeMap<String, (Signal, Vec<String>)> = BTreeMap::new();
    for o in seen {
        for id in ticket_ids_in(&o.text, &p.file.key) {
            let entry = best.entry(id).or_insert((o.signal, Vec::new()));
            entry.0 = entry.0.max(o.signal);
            if let Some(url) = &o.url
                && !entry.1.contains(url)
            {
                entry.1.push(url.clone());
            }
        }
    }
    let mut out = Vec::new();
    for t in &p.tickets {
        let Some((signal, prs)) = best.get(&t.id) else {
            continue;
        };
        if t.meta.review == Review::Rejected {
            continue;
        }
        let new_prs: Vec<String> = prs
            .iter()
            .filter(|u| !t.meta.prs.contains(u))
            .cloned()
            .collect();
        match move_for(t.meta.status, *signal) {
            Some(to) => out.push(Move {
                ticket: t.id.clone(),
                from: t.meta.status,
                to,
                prs: new_prs,
            }),
            None if !new_prs.is_empty() && t.meta.status != TicketStatus::Dropped => {
                out.push(Move {
                    ticket: t.id.clone(),
                    from: t.meta.status,
                    to: t.meta.status,
                    prs: new_prs,
                })
            }
            None => {}
        }
    }
    out
}

/// Writes `moves` to project `p`'s tickets in `dir`.
pub fn apply(dir: &Path, p: &Project, moves: &[Move]) -> Result<(), String> {
    for m in moves {
        let Some(t) = p.tickets.iter().find(|t| t.id == m.ticket) else {
            continue;
        };
        let mut t = t.clone();
        t.meta.status = m.to;
        t.meta.prs.extend(m.prs.iter().cloned());
        repo::write_ticket(dir, &t).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// What the engineer did in code repo `repo`: their pull requests (through gh, as the
/// environment), and when it's cloned here, its local branches and the last two weeks of
/// commit subjects by `email`. Blocks.
pub fn observe(
    r: &dyn Runner,
    repo: &str,
    clone: Option<&Path>,
    email: Option<&str>,
) -> Result<Vec<Observation>, String> {
    let json = r.gh(&[
        "pr", "list", "--repo", repo, "--author", "@me", "--state", "all", "--limit", "50",
        "--json", PR_FIELDS,
    ])?;
    let mut seen = prs_observed(&json)?;
    if let Some(clone) = clone {
        let branches = r.git(
            clone,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            None,
        )?;
        seen.extend(work_observed(&branches));
        if let Some(email) = email.filter(|e| !e.is_empty()) {
            let author = format!("--author={email}");
            let subjects = r.git(
                clone,
                &["log", "--all", "--since=14.days", &author, "--format=%s"],
                None,
            )?;
            seen.extend(work_observed(&subjects));
        }
    }
    Ok(seen)
}

/// The ticket moves in a project's pending change: each pending ticket whose status differs
/// from the repo's (HEAD) copy, as (id, title, from, to). A new ticket isn't a move.
pub fn pending_moves(
    r: &dyn Runner,
    checkout: &Path,
    p: &Project,
) -> Vec<(String, String, TicketStatus, TicketStatus)> {
    let mut out = Vec::new();
    for t in &p.tickets {
        let path = format!("{}/tickets/{}.md", p.slug, t.id);
        let Ok(before) = r.git(checkout, &["show", &format!("HEAD:{path}")], None) else {
            continue;
        };
        let Ok((meta, _)) = front_matter::parse::<hydraui_projects::TicketMeta>(&before) else {
            continue;
        };
        if meta.status != t.meta.status {
            out.push((
                t.id.clone(),
                t.meta.title.clone(),
                meta.status,
                t.meta.status,
            ));
        }
    }
    out
}

/// One repo sync's write-up (spec §7): what moved and what else changed, in plain words,
/// headed with the time. Built from the pending change, never from transcripts or code.
pub fn write_up(
    time: &str,
    moved: &[(String, String, TicketStatus, TicketStatus)],
    other: &[String],
) -> String {
    let mut out = format!("## {time}\n");
    if !moved.is_empty() {
        out.push_str("\n**Moved**\n\n");
        for (id, title, from, to) in moved {
            out.push_str(&format!(
                "- {id} {title}: {} → {}\n",
                from.label(),
                to.label()
            ));
        }
    }
    if !other.is_empty() {
        out.push_str("\n**Also changed**\n\n");
        for o in other {
            out.push_str(&format!("- {o}\n"));
        }
    }
    if moved.is_empty() && other.is_empty() {
        out.push_str("\nNothing changed.\n");
    }
    out
}

/// Adds `section` to `login`'s progress file for `date` in project folder `dir`: a second
/// sync that day goes under the first.
pub fn add_progress(dir: &Path, login: &str, date: &str, section: &str) -> Result<(), String> {
    let path = dir.join("progress").join(login).join(format!("{date}.md"));
    let body = match std::fs::read_to_string(&path) {
        Ok(before) => format!("{}\n\n{section}", before.trim_end()),
        Err(_) => format!("# {login}, {date}\n\n{section}"),
    };
    repo::write_progress(
        dir,
        &Progress {
            login: login.to_string(),
            date: date.to_string(),
            body,
        },
    )
    .map_err(|e| e.to_string())
}

/// The sync slot due now, if any: the latest of today's `times` (local `HH:MM`) that has passed
/// by `now` (local `YYYY-MM-DD HH:MM`) and is later than the `last` slot run. With no `last`
/// nothing is due: a project starts syncing from its next time, not with a send at once.
pub fn sync_due(times: &[String], last: Option<&str>, now: &str) -> Option<String> {
    let last = last?;
    let date = now.get(..10)?;
    times
        .iter()
        .map(|t| format!("{date} {}", t.trim()))
        .filter(|slot| slot.as_str() <= now && slot.as_str() > last)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydraui_projects::{Ticket, TicketMeta};

    fn project(tickets: &[(&str, TicketStatus)]) -> Project {
        Project {
            slug: "pg".into(),
            file: toml::from_str("name = \"PG\"\nkey = \"PG\"\nlead = \"me\"").unwrap(),
            spec: String::new(),
            milestones: vec![],
            tickets: tickets
                .iter()
                .map(|(id, status)| Ticket {
                    id: (*id).into(),
                    meta: TicketMeta {
                        title: format!("{id} title"),
                        status: *status,
                        milestone: "M1".into(),
                        ..Default::default()
                    },
                    body: String::new(),
                })
                .collect(),
            decisions: vec![],
            meetings: vec![],
            files: vec![],
            links: Default::default(),
            progress: vec![],
        }
    }

    #[test]
    fn pull_requests_say_in_review_or_done_and_closed_ones_nothing() {
        let json = r#"[
          {"title":"PG-3 share dialog","headRefName":"pg-3-share","state":"OPEN","url":"u3"},
          {"title":"Fix PG-1","headRefName":"x","state":"MERGED","url":"u1"},
          {"title":"PG-9","headRefName":"x","state":"CLOSED","url":"u9"}
        ]"#;
        let seen = prs_observed(json).unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            (seen[0].signal, seen[1].signal),
            (Signal::PrOpened, Signal::PrMerged)
        );
    }

    #[test]
    fn tickets_move_only_forward_to_the_furthest_signal() {
        let p = project(&[
            ("PG-1", TicketStatus::Todo),
            ("PG-2", TicketStatus::Todo),
            ("PG-3", TicketStatus::InReview),
            ("PG-4", TicketStatus::Dropped),
        ]);
        let seen = [
            work_observed("pg-1-schema\nmain\nWIP on PG-2\n"),
            prs_observed(
                r#"[{"title":"PG-1 schema","headRefName":"pg-1-schema","state":"MERGED","url":"u1"},
                    {"title":"PG-3","headRefName":"b","state":"OPEN","url":"u3"},
                    {"title":"PG-4","headRefName":"b","state":"MERGED","url":"u4"}]"#,
            )
            .unwrap(),
        ]
        .concat();
        let got = moves(&p, &seen);
        let summary: Vec<(&str, TicketStatus, usize)> = got
            .iter()
            .map(|m| (m.ticket.as_str(), m.to, m.prs.len()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("PG-1", TicketStatus::Done, 1),
                ("PG-2", TicketStatus::InProgress, 0),
                // Already in review: no move, but its pull request is recorded.
                ("PG-3", TicketStatus::InReview, 1),
            ]
        );
    }

    #[test]
    fn a_move_is_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let p = project(&[("PG-1", TicketStatus::Todo)]);
        repo::write_project_file(dir, &p.file).unwrap();
        repo::write_ticket(dir, &p.tickets[0]).unwrap();
        let seen =
            prs_observed(r#"[{"title":"PG-1","headRefName":"b","state":"OPEN","url":"u1"}]"#)
                .unwrap();
        apply(dir, &p, &moves(&p, &seen)).unwrap();
        let mut again = p.clone();
        again.tickets[0] = repo::load(dir).unwrap().tickets.remove(0);
        assert_eq!(again.tickets[0].meta.status, TicketStatus::InReview);
        assert_eq!(again.tickets[0].meta.prs, vec!["u1".to_string()]);
        assert!(moves(&again, &seen).is_empty());
    }

    #[test]
    fn a_sync_is_due_once_per_passed_time() {
        let times = vec!["12:30".to_string(), "17:30".to_string()];
        assert_eq!(sync_due(&times, None, "2026-11-04 13:00"), None);
        assert_eq!(
            sync_due(&times, Some("2026-11-04 09:00"), "2026-11-04 13:00").as_deref(),
            Some("2026-11-04 12:30")
        );
        assert_eq!(
            sync_due(&times, Some("2026-11-04 12:30"), "2026-11-04 13:00"),
            None
        );
        assert_eq!(
            sync_due(&times, Some("2026-11-04 12:30"), "2026-11-04 18:05").as_deref(),
            Some("2026-11-04 17:30")
        );
        // Closed all morning: only the latest passed time runs, once.
        assert_eq!(
            sync_due(&times, Some("2026-11-03 17:30"), "2026-11-04 19:00").as_deref(),
            Some("2026-11-04 17:30")
        );
    }

    #[test]
    fn a_write_up_says_what_moved_and_a_second_sync_adds_to_the_day() {
        let text = write_up(
            "12:30",
            &[(
                "PG-1".into(),
                "Schema".into(),
                TicketStatus::Todo,
                TicketStatus::InProgress,
            )],
            &["Decision D-2 updated".into()],
        );
        assert_eq!(
            text,
            "## 12:30\n\n**Moved**\n\n- PG-1 Schema: Todo → In progress\n\n**Also changed**\n\n- Decision D-2 updated\n"
        );
        let tmp = tempfile::tempdir().unwrap();
        add_progress(tmp.path(), "aza", "2026-11-04", &text).unwrap();
        add_progress(
            tmp.path(),
            "aza",
            "2026-11-04",
            "## 17:30\n\nNothing changed.\n",
        )
        .unwrap();
        let file = std::fs::read_to_string(tmp.path().join("progress/aza/2026-11-04.md")).unwrap();
        assert!(file.starts_with("# aza, 2026-11-04\n\n## 12:30"));
        assert!(file.contains("In progress\n\n**Also changed**"));
        assert!(file.trim_end().ends_with("## 17:30\n\nNothing changed."));
    }
}
