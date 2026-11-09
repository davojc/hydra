//! The Projects sessions' tools (Projects spec §14): a planning session writes its project's
//! spec and proposes milestones and tickets for the lead to review; a work session reads its
//! ticket and reports the pull requests it opened. Each sees only its own project or ticket.

use std::path::{Path, PathBuf};

use hydraui_projects::{
    Decision, DecisionMeta, Milestone, MilestoneStatus, Project, Review, Ticket, TicketKind,
    TicketMeta, TicketStatus, next_id, progress, repo,
};
use serde_json::{Value, json};

use super::registry::Registry;
use super::tools::Outcome;
use crate::projects::Point;

/// A session hydraui started for a project, by what it may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// Plans project folder `dir` for `login` (the lead, who asked).
    Plan { dir: PathBuf, login: String },
    /// Works on ticket `ticket` of project folder `dir`.
    Work { dir: PathBuf, ticket: String },
    /// Checks ticket `ticket`'s pull requests with the testing skill (spec §14.5).
    Check { dir: PathBuf, ticket: String },
}

/// The planning tools, in the order `tools/list` shows them.
pub const PLAN: [&str; 13] = [
    "plan_show",
    "plan_set_spec",
    "plan_propose_milestone",
    "plan_propose_ticket",
    "plan_suggest_repo",
    "plan_revise",
    "plan_propose_change",
    "plan_propose_spec",
    "plan_withdraw",
    "plan_add_question",
    "plan_add_link",
    "plan_read_resource",
    "plan_done",
];

/// The work tools.
pub const WORK: [&str; 4] = [
    "ticket_show",
    "ticket_ask",
    "ticket_report",
    "ticket_checked",
];

/// Who asked a question a Claude session raised: its answer goes back to the session.
pub const ASKED_BY_CLAUDE: &str = "claude";

/// Every tool this module answers.
pub fn names() -> impl Iterator<Item = &'static str> {
    PLAN.into_iter().chain(WORK)
}

/// The `tools/list` entries for `names()`.
pub fn schemas() -> Vec<Value> {
    let s = |what: &str| json!({ "type": "string", "description": what });
    let points = |what: &str| {
        json!({
            "type": "array",
            "description": what,
            "items": {
                "type": "object",
                "properties": {
                    "point": {"type": "string", "description": "The point, in plain words; at most 80 characters."},
                    "detail": {"type": "string", "description": "Its full explanation, shown when the lead clicks it: what it means, why, and what done looks like; at most 1000 characters."},
                },
                "required": ["point", "detail"],
            },
        })
    };
    let tool = |name: &str, description: &str, properties: Value, required: Vec<&str>| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object", "properties": properties, "required": required},
        })
    };
    vec![
        tool(
            "plan_show",
            "Planning: the project as it stands: spec, milestones and tickets with their review \
             state (to review, changes asked with the lead's note, approved, rejected with the \
             reason), open questions and resources. Call it first, and after the lead writes.",
            json!({}),
            vec![],
        ),
        tool(
            "plan_set_spec",
            "Planning: replace the project's spec (Markdown): why, goals, non-goals, decisions \
             and their reasons, in short numbered sections in plain words.",
            json!({"text": s("The whole spec, Markdown.")}),
            vec!["text"],
        ),
        tool(
            "plan_propose_milestone",
            "Planning: propose a milestone, added at the end for the lead to review. Returns \
             its id (M1, M2, ...). Write it for anyone on the team, not only engineers: an \
             outcome people care about, in plain words, with no technical terms, components or \
             implementation detail (those go in its tickets). Title at most 60 characters, e.g. \
             \"Partners can see shared groups\"; then 1 to 4 short points (at most 80 \
             characters each) on what it's for and how we'll know it's done, e.g. \"Partners \
             stop emailing us for group lists\", each with a detail: its full explanation in \
             plain words, shown when the lead clicks the point. Don't propose again what was \
             rejected.",
            json!({
                "title": s("The outcome, in plain words; at most 60 characters."),
                "target": s("Target date, YYYY-MM-DD (optional)."),
                "points": points("1 to 4 points: what it's for and how we'll know it's done, each with its full explanation."),
            }),
            vec!["title", "points"],
        ),
        tool(
            "plan_propose_ticket",
            "Planning: propose a ticket in an approved milestone, for the lead to review. Each \
             should be finishable in a few days, in one code repo, with how to tell it's done. \
             Returns its id.",
            json!({
                "milestone": s("The approved milestone's id, e.g. M1."),
                "title": s("The ticket, in one line."),
                "repo": s("The code repo the work lands in, owner/name, e.g. iov42/version2-frontend."),
                "kind": s("feature, bug or chore (default feature)."),
                "description": s("What to do and how to tell it's done (Markdown)."),
            }),
            vec!["milestone", "title", "repo", "description"],
        ),
        tool(
            "plan_suggest_repo",
            "Planning: tell the lead a code repo this project will probably change, and why, as \
             soon as you can tell: while writing the spec or milestones, before any ticket names \
             it. It shows in hydraui for the lead to add or dismiss. A ticket's repo is \
             suggested on its own; don't suggest again what plan_show lists.",
            json!({
                "repo": s("owner/name, e.g. iov42/api-designs."),
                "why": s("Why the project changes it, in one short line."),
            }),
            vec!["repo", "why"],
        ),
        tool(
            "plan_revise",
            "Planning: revise a milestone (M..) or ticket (KEY-..) that is still to review or \
             had changes asked (a milestone keeps to plain words: title at most 60 characters \
             and 1 to 4 points of at most 80); it goes back to the lead to review. A ticket's \
             repo alone can be set on any ticket, approved ones too, and it stays approved. Only the fields \
             given change. Use the lead's note (plan_show shows it).",
            json!({
                "id": s("What to revise."),
                "title": s("New title."),
                "target": s("A milestone's target date, YYYY-MM-DD."),
                "description": s("A ticket's new description."),
                "repo": s("A ticket's code repo, owner/name."),
                "points": points("A milestone's new points, 1 to 4, each with its full explanation."),
                "milestone": s("A ticket's milestone id."),
                "kind": s("A ticket's kind: feature, bug or chore."),
            }),
            vec!["id"],
        ),
        tool(
            "plan_propose_change",
            "Planning: propose a change to an approved milestone (M..) or ticket (KEY-..) after \
             new requirements. The approved version stays in force until the lead accepts the \
             change in hydraui. Only the fields given change; say why, from the requirements. \
             Only tickets still to do can change: started or done work is never rewritten, so \
             propose a follow-up ticket for it instead (plan_propose_ticket, saying which ticket \
             it follows). A milestone keeps to plain words (title at most 60 characters, 1 to 4 \
             points of at most 80). drop proposes dropping it. A new change replaces one \
             already waiting.",
            json!({
                "id": s("The approved milestone or ticket."),
                "why": s("Why, in a line or two: what in the new requirements asks for it."),
                "from": s("The requirements' file name, as plan_show lists it under Resources."),
                "title": s("New title."),
                "target": s("A milestone's new target date, YYYY-MM-DD."),
                "points": points("A milestone's new points, 1 to 4, each with its full explanation."),
                "description": s("A ticket's new description."),
                "repo": s("A ticket's code repo, owner/name."),
                "milestone": s("A ticket's new milestone id (approved)."),
                "drop": {"type": "boolean", "description": "Propose dropping it: the requirements no longer need it. Not for a milestone with started work."},
            }),
            vec!["id", "why"],
        ),
        tool(
            "plan_propose_spec",
            "Planning: propose a new version of the agreed spec after new requirements, for the \
             lead to accept in hydraui; the current spec stays until then. The whole spec, \
             Markdown, with why. (Before any milestone is approved, plan_set_spec writes it \
             directly.)",
            json!({
                "text": s("The whole new spec, Markdown."),
                "why": s("What changed and why, in a line or two."),
                "from": s("The requirements' file name, as plan_show lists it under Resources."),
            }),
            vec!["text", "why"],
        ),
        tool(
            "plan_withdraw",
            "Planning: withdraw a proposal that hasn't been approved (e.g. after splitting it \
             into two). Approved and rejected items stay.",
            json!({"id": s("The milestone or ticket.")}),
            vec!["id"],
        ),
        tool(
            "plan_add_question",
            "Planning: ask a question the lead (or someone else) must decide. This is how you \
             ask anything: never ask in the terminal. It shows in hydraui as a decision to make, \
             with your options as buttons; the answer is recorded and sent back to you here. \
             Returns its id.",
            json!({
                "question": s("The question, in one line."),
                "context": s("What the person deciding needs to know: why it matters, what each option means (optional)."),
                "options": {"type": "array", "items": {"type": "string"},
                    "description": "The likely answers, as short labels (give them whenever you can)."},
                "assigned_to": s("GitHub login of who decides (optional; the lead by default)."),
            }),
            vec!["question"],
        ),
        tool(
            "plan_add_link",
            "Planning: attach a link to the project's resources.",
            json!({"title": s("What it is."), "url": s("The link.")}),
            vec!["title", "url"],
        ),
        tool(
            "plan_read_resource",
            "Planning: read an attached file by name (as plan_show lists it): its text, or for \
             a file that isn't text its path, to open with your Read tool.",
            json!({"name": s("The file's name under resources/.")}),
            vec!["name"],
        ),
        tool(
            "plan_done",
            "Planning: call when the job you were given is finished (or can't go further until \
             a decision is made), after reviewing your work. hydraui closes this session and \
             shows the lead your summary; answering a decision or the next job starts a fresh \
             session, which reads the project as it stands.",
            json!({
                "summary": s("For the lead, two or three short lines: what you did, your review's score, and what waits on them."),
            }),
            vec!["summary"],
        ),
        tool(
            "ticket_show",
            "Work: your ticket: its description, milestone, the project's spec and its open \
             questions about it.",
            json!({}),
            vec![],
        ),
        tool(
            "ticket_ask",
            "Work: ask a question someone must decide before you can go on. This is how you \
             ask anything: never ask in the terminal. It shows in hydraui as a decision on your \
             ticket; the answer is recorded and sent back to you here.",
            json!({
                "question": s("The question, in one line."),
                "context": s("What the person deciding needs to know (optional)."),
                "options": {"type": "array", "items": {"type": "string"},
                    "description": "The likely answers, as short labels."},
                "assigned_to": s("GitHub login of who decides (optional; the lead by default)."),
            }),
            vec!["question"],
        ),
        tool(
            "ticket_report",
            "Work: finish the ticket: the pull requests you opened for it and what you did. The \
             ticket moves to in review and the people on the project see the links. A ticket \
             whose work changes no code (an investigation, a check, a write-up) finishes with \
             no pull requests: the summary is its result, and it's done.",
            json!({
                "prs": {"type": "array", "items": {"type": "string"},
                    "description": "The pull request URLs; none when the work changed no code."},
                "summary": s("What you did, and anything a reviewer should know."),
                "for_a_person": {"type": "array", "items": {"type": "object", "properties": {
                        "task": {"type": "string", "description": "What they have to do, as a short title: \"Create the review-received-data-stepper flag in GrowthBook\"."},
                        "why": {"type": "string", "description": "Why, and anything they need to do it."}},
                    "required": ["task"]},
                    "description": "What's left that only a person can do (create a flag, set a secret, ask another team, run something by hand). Each becomes a task for a person in the milestone; Claude never works those."},
            }),
            vec!["prs", "summary"],
        ),
        tool(
            "ticket_checked",
            "Testing: report your check of the ticket's pull requests, done with the testing \
             skill. A pass lets the milestone run move on; a fail pauses it for the lead.",
            json!({
                "passed": {"type": "boolean", "description": "Whether the work passes."},
                "summary": s("What you checked and, if it failed, what's wrong."),
            }),
            vec!["passed", "summary"],
        ),
    ]
}

/// Runs tool `name` for the terminal holding `token`; `None` for a name this module doesn't
/// answer.
pub fn call(reg: &Registry, token: &str, name: &str, args: &Value) -> Option<Outcome> {
    if !names().any(|n| n == name) {
        return None;
    }
    let session = reg.project_sessions.get(token);
    let out = match (session, PLAN.contains(&name)) {
        (Some(Session::Plan { .. }), true) if name == "plan_done" => done(reg, token, args),
        (Some(Session::Plan { dir, login }), true) => plan(dir, login, name, args),
        (Some(Session::Work { .. }), false) if name == "ticket_checked" => Err(
            "Only a testing session reports a check: this session does the work and reports it \
             with ticket_report."
                .into(),
        ),
        (Some(Session::Check { .. }), false) if name == "ticket_report" => Err(
            "A testing session reports with ticket_checked; ticket_report is the work \
             session's."
                .into(),
        ),
        (Some(Session::Work { dir, ticket } | Session::Check { dir, ticket }), false) => {
            work(dir, ticket, name, args)
        }
        _ if PLAN.contains(&name) => Err("Only a project's planning session can use the plan \
             tools: start one from the project in hydraui's Projects tab."
            .into()),
        _ => Err(
            "Only a ticket's work session can use the ticket tools: start one with Start \
             work on the ticket in hydraui's Projects tab."
                .into(),
        ),
    };
    Some(match out {
        Ok(text) => Outcome::ok(text),
        Err(e) => Outcome::err(e),
    })
}

/// `plan_done`: the session's job is finished; hydraui closes it and shows the lead `summary`.
fn done(reg: &Registry, token: &str, args: &Value) -> Result<String, String> {
    let summary = need(args, "summary")?;
    reg.planning_done
        .lock()
        .map_err(|_| "hydraui's registry is broken".to_string())?
        .push((token.to_string(), summary));
    Ok("Done: hydraui closes this session and shows your summary to the lead.".into())
}

/// The planning sessions that called `plan_done` since the last call, as (token, summary).
pub fn take_finished(reg: &Registry) -> Vec<(String, String)> {
    reg.planning_done
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

fn get<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn need(args: &Value, key: &str) -> Result<String, String> {
    get(args, key)
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is required"))
}

fn plan(dir: &Path, login: &str, name: &str, args: &Value) -> Result<String, String> {
    let p = repo::load(dir).map_err(|e| e.to_string())?;
    let e = |e: hydraui_projects::Error| e.to_string();
    match name {
        "plan_show" => Ok(show(dir, &p)),
        "plan_set_spec" => {
            if agreed(&p) {
                return Err(
                    "The plan is agreed (a milestone is approved), so the spec changes \
                            only by proposal: use plan_propose_spec, with why."
                        .into(),
                );
            }
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            repo::write_spec(dir, text).map_err(e)?;
            Ok("Spec saved.".into())
        }
        "plan_propose_spec" => {
            if !agreed(&p) {
                return Err("Nothing is approved yet: write the spec with plan_set_spec.".into());
            }
            let text = need(args, "text")?;
            let mut file = p.file.clone();
            file.spec_change = Some(hydraui_projects::Change {
                why: need(args, "why")?,
                from: get(args, "from").unwrap_or_default().to_string(),
                description: Some(text),
                ..Default::default()
            });
            repo::write_project_file(dir, &file).map_err(e)?;
            Ok("Proposed the new spec; the lead accepts or turns it down in hydraui.".into())
        }
        "plan_propose_change" => propose_change(dir, &p, &need(args, "id")?, args),
        "plan_propose_milestone" => {
            let id = format!(
                "M{}",
                p.milestones
                    .iter()
                    .filter_map(|m| m.id.strip_prefix('M')?.parse::<u32>().ok())
                    .max()
                    .unwrap_or(0)
                    + 1
            );
            let mut ms = p.milestones.clone();
            let title = need(args, "title")?;
            let description = milestone_points(args)?.ok_or(
                "Give the milestone's points: 1 to 4 short bullet points, what it's for and \
                 how we'll know it's done.",
            )?;
            milestone_fits(&title, &description)?;
            ms.push(Milestone {
                id: id.clone(),
                title,
                target: get(args, "target").map(crate::projects::date).transpose()?,
                status: MilestoneStatus::Planned,
                description,
                review: Review::Proposed,
                ..Default::default()
            });
            repo::write_milestones(dir, &ms).map_err(e)?;
            Ok(format!("Proposed {id}; the lead reviews it in hydraui."))
        }
        "plan_propose_ticket" => {
            let milestone = need(args, "milestone")?;
            match p.milestones.iter().find(|m| m.id == milestone) {
                None => return Err(format!("There's no milestone {milestone}.")),
                Some(m) if m.review != Review::Approved => {
                    return Err(format!(
                        "{milestone} isn't approved yet ({}); tickets come once it is.",
                        m.review.label()
                    ));
                }
                Some(_) => {}
            }
            let id = next_id(&p.file.key, p.tickets.iter().map(|t| t.id.as_str()));
            let t = Ticket {
                id: id.clone(),
                meta: TicketMeta {
                    title: need(args, "title")?,
                    kind: kind(get(args, "kind"))?,
                    status: TicketStatus::Todo,
                    milestone,
                    repo: crate::projects::repo_name(&need(args, "repo").map_err(|_| {
                        "Say which code repo the ticket lands in (repo: owner/name).".to_string()
                    })?)?,
                    created_by: login.to_string(),
                    created: Some(progress::now()),
                    review: Review::Proposed,
                    ..Default::default()
                },
                body: need(args, "description")?,
            };
            repo::write_ticket(dir, &t).map_err(e)?;
            Ok(format!("Proposed {id}; the lead reviews it in hydraui."))
        }
        "plan_suggest_repo" => {
            let repo = crate::projects::repo_name(&need(args, "repo")?)?;
            let why = need(args, "why")?;
            if p.file.repos.iter().any(|r| r.eq_ignore_ascii_case(&repo)) {
                return Ok(format!("{repo} is already one of the project's repos."));
            }
            if p.file
                .dismissed_repos
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&repo))
            {
                return Err(format!(
                    "The lead dismissed {repo}; ask a question if you think it's needed after all."
                ));
            }
            let mut file = p.file.clone();
            match file
                .suggested_repos
                .iter_mut()
                .find(|s| s.repo.eq_ignore_ascii_case(&repo))
            {
                Some(s) => s.why = why,
                None => file.suggested_repos.push(hydraui_projects::SuggestedRepo {
                    repo: repo.clone(),
                    why,
                }),
            }
            repo::write_project_file(dir, &file).map_err(e)?;
            Ok(format!(
                "Suggested {repo}; the lead adds or dismisses it in hydraui."
            ))
        }
        "plan_revise" => revise(dir, &p, &need(args, "id")?, args),
        "plan_withdraw" => {
            let id = need(args, "id")?;
            let open = |r: Review| matches!(r, Review::Proposed | Review::Changes);
            if let Some(m) = p.milestones.iter().find(|m| m.id == id) {
                if !open(m.review) {
                    return Err(format!("{id} is {}; it stays.", m.review.label()));
                }
                if p.tickets.iter().any(|t| t.meta.milestone == id) {
                    return Err(format!("{id} has tickets; it stays."));
                }
                let ms: Vec<_> = p
                    .milestones
                    .iter()
                    .filter(|m| m.id != id)
                    .cloned()
                    .collect();
                repo::write_milestones(dir, &ms).map_err(e)?;
            } else if let Some(t) = p.tickets.iter().find(|t| t.id == id) {
                if !open(t.meta.review) {
                    return Err(format!("{id} is {}; it stays.", t.meta.review.label()));
                }
                std::fs::remove_file(dir.join("tickets").join(format!("{id}.md")))
                    .map_err(|e| e.to_string())?;
            } else {
                return Err(format!("There's no {id}."));
            }
            Ok(format!("Withdrew {id}."))
        }
        "plan_add_question" => ask(dir, &p, args, ""),
        "plan_add_link" => {
            crate::projects::add_link(dir, &need(args, "url")?, &need(args, "title")?)?;
            Ok("Link added.".into())
        }
        "plan_read_resource" => read_resource(dir, &need(args, "name")?),
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Asks a question in hydraui for whoever should decide (spec §14): Claude asks here, not in
/// its terminal. The answer is recorded there and sent back to the session.
fn ask(dir: &Path, p: &Project, args: &Value, ticket: &str) -> Result<String, String> {
    let id = next_id("D", p.decisions.iter().map(|d| d.id.as_str()));
    let options = args
        .get("options")
        .and_then(Value::as_array)
        .map(|o| {
            o.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let d = Decision {
        id: id.clone(),
        meta: DecisionMeta {
            question: need(args, "question")?,
            asked_by: ASKED_BY_CLAUDE.to_string(),
            asked_at: Some(progress::now()),
            assigned_to: get(args, "assigned_to")
                .map(|w| w.trim_start_matches('@').to_string())
                .unwrap_or_else(|| p.file.lead.clone()),
            options,
            ticket: ticket.to_string(),
            ..Default::default()
        },
        body: get(args, "context").unwrap_or_default().to_string(),
    };
    repo::write_decision(dir, &d).map_err(|e| e.to_string())?;
    Ok(format!(
        "Asked {id} in hydraui, for {}. Wait for the answer: hydraui sends it to you here \
         when it's given. Carry on with anything that doesn't depend on it.",
        d.meta.assigned_to
    ))
}

/// How long a milestone may be (Jordan, 2026-11-03: milestones are short, purpose-driven
/// and easy for anyone to read, a few bullet points rather than a paragraph, and any point
/// can be clicked for its full explanation; implementation belongs in tickets).
pub const MILESTONE_TITLE_MAX: usize = 60;
pub const MILESTONE_POINTS_MAX: usize = 4;
pub const MILESTONE_POINT_MAX: usize = 80;
pub const MILESTONE_DETAIL_MAX: usize = 1000;

/// The `points` argument as a milestone description (see [`crate::projects::points_text`]),
/// if given.
fn milestone_points(args: &Value) -> Result<Option<String>, String> {
    const SHAPE: &str = "points is a list of {point, detail}: a short line, and its full \
                         explanation.";
    let Some(v) = args.get("points") else {
        return Ok(None);
    };
    let mut points = Vec::new();
    for item in v.as_array().ok_or(SHAPE)? {
        let field = |k: &str| item.get(k).and_then(Value::as_str).map(str::trim);
        let text = field("point").ok_or(SHAPE)?;
        points.push(Point {
            text: crate::projects::strip_point(text).to_string(),
            detail: field("detail").unwrap_or_default().to_string(),
        });
    }
    if points.iter().any(|p| p.text.is_empty()) {
        return Err("A point is empty.".into());
    }
    Ok(Some(crate::projects::points_text(&points)))
}

/// Refuses a milestone that's too long to take in at a glance, or whose points don't
/// explain themselves when clicked, saying how to fix it.
fn milestone_fits(title: &str, description: &str) -> Result<(), String> {
    let t = title.chars().count();
    if t > MILESTONE_TITLE_MAX {
        return Err(format!(
            "The title is {t} characters; keep it to {MILESTONE_TITLE_MAX}: the outcome, in \
             plain words, with no technical terms."
        ));
    }
    let Some(points) = crate::projects::points_of(description) else {
        return Err(
            "Give the milestone as points (1 to 4 short bullet points: what it's for and how \
             we'll know it's done, each with its full explanation), not a paragraph."
                .into(),
        );
    };
    if points.len() > MILESTONE_POINTS_MAX {
        return Err(format!(
            "That's {} points; keep it to at most {MILESTONE_POINTS_MAX}.",
            points.len()
        ));
    }
    for p in &points {
        if p.text.chars().count() > MILESTONE_POINT_MAX {
            return Err(format!(
                "\"{}\" is too long; keep each point to {MILESTONE_POINT_MAX} characters, in \
                 plain words, and put the rest in its detail.",
                p.text
            ));
        }
        if p.detail.is_empty() {
            return Err(format!(
                "\"{}\" has no detail; give each point its full explanation (what it means, \
                 why, and what done looks like), shown when the lead clicks it.",
                p.text
            ));
        }
        if p.detail.chars().count() > MILESTONE_DETAIL_MAX {
            return Err(format!(
                "The detail of \"{}\" is over {MILESTONE_DETAIL_MAX} characters; keep it to a \
                 short paragraph or a few lines. Implementation goes in its tickets.",
                p.text
            ));
        }
    }
    Ok(())
}

fn revise(dir: &Path, p: &Project, id: &str, args: &Value) -> Result<String, String> {
    let e = |e: hydraui_projects::Error| e.to_string();
    let open = |r: Review| matches!(r, Review::Proposed | Review::Changes);
    if let Some(i) = p.milestones.iter().position(|m| m.id == id) {
        let mut ms = p.milestones.clone();
        let m = &mut ms[i];
        if !open(m.review) {
            return Err(format!(
                "{id} is {}; only proposals are revised. To change an approved one, use \
                 plan_propose_change.",
                m.review.label()
            ));
        }
        if let Some(v) = get(args, "title") {
            m.title = v.to_string();
        }
        if let Some(v) = get(args, "target") {
            m.target = Some(crate::projects::date(v)?);
        }
        if let Some(v) = get(args, "description") {
            m.description = v.to_string();
        }
        if let Some(v) = milestone_points(args)? {
            m.description = v;
        }
        milestone_fits(&m.title, &m.description)?;
        m.review = Review::Proposed;
        repo::write_milestones(dir, &ms).map_err(e)?;
    } else if let Some(t) = p.tickets.iter().find(|t| t.id == id) {
        let mut t = t.clone();
        // A ticket's repo says where the work lands, not what was approved: it can be set on
        // any live ticket, alone, and the ticket keeps its review.
        let only_repo = args
            .as_object()
            .is_some_and(|o| o.keys().all(|k| k == "id" || k == "repo") && o.contains_key("repo"));
        if only_repo && t.meta.review != Review::Rejected {
            t.meta.repo = crate::projects::repo_name(&need(args, "repo")?)?;
            repo::write_ticket(dir, &t).map_err(e)?;
            return Ok(format!("{id} is in {} now.", t.meta.repo));
        }
        if !open(t.meta.review) {
            return Err(format!(
                "{id} is {}; only proposals are revised. To change an approved one, use \
                 plan_propose_change.",
                t.meta.review.label()
            ));
        }
        if let Some(v) = get(args, "title") {
            t.meta.title = v.to_string();
        }
        if let Some(v) = get(args, "description") {
            t.body = v.to_string();
        }
        if let Some(v) = get(args, "repo") {
            t.meta.repo = crate::projects::repo_name(v)?;
        }
        if let Some(v) = get(args, "milestone") {
            if !p
                .milestones
                .iter()
                .any(|m| m.id == v && m.review == Review::Approved)
            {
                return Err(format!("{v} isn't an approved milestone."));
            }
            t.meta.milestone = v.to_string();
        }
        if let Some(v) = get(args, "kind") {
            t.meta.kind = kind(Some(v))?;
        }
        t.meta.review = Review::Proposed;
        repo::write_ticket(dir, &t).map_err(e)?;
    } else {
        return Err(format!("There's no {id}."));
    }
    Ok(format!("Revised {id}; it's back with the lead to review."))
}

/// Whether the plan is agreed: a milestone is approved, so changes go through the lead
/// (spec §14.6).
fn agreed(p: &Project) -> bool {
    p.milestones.iter().any(|m| m.review == Review::Approved)
}

/// Proposes a change to an approved milestone or ticket (spec §14.6).
fn propose_change(dir: &Path, p: &Project, id: &str, args: &Value) -> Result<String, String> {
    let e = |e: hydraui_projects::Error| e.to_string();
    let drop = args.get("drop").and_then(Value::as_bool).unwrap_or(false);
    let mut c = hydraui_projects::Change {
        why: need(args, "why")?,
        from: get(args, "from").unwrap_or_default().to_string(),
        title: get(args, "title").map(str::to_string),
        drop,
        ..Default::default()
    };
    let not_approved = |r: Review| match r {
        Review::Approved => Ok(()),
        Review::Rejected => Err(format!("{id} was rejected; it stays that way.")),
        _ => Err(format!(
            "{id} isn't approved yet: revise it with plan_revise instead."
        )),
    };
    if let Some(i) = p.milestones.iter().position(|m| m.id == id) {
        let m = &p.milestones[i];
        not_approved(m.review)?;
        if let Some(v) = get(args, "target") {
            c.target = Some(crate::projects::date(v)?);
        }
        c.description = milestone_points(args)?;
        if drop {
            if let Some(t) = p
                .tickets
                .iter()
                .find(|t| t.meta.milestone == id && crate::projects::started(t.meta.status))
            {
                return Err(format!(
                    "{id} has started work ({}), so it can't be dropped; propose changes to its \
                     tickets still to do instead.",
                    t.id
                ));
            }
        } else {
            milestone_fits(
                c.title.as_deref().unwrap_or(&m.title),
                c.description.as_deref().unwrap_or(&m.description),
            )?;
        }
        if !drop && c.title.is_none() && c.target.is_none() && c.description.is_none() {
            return Err("Give what changes: title, target or points (or drop).".into());
        }
        let mut ms = p.milestones.clone();
        ms[i].change = Some(c);
        repo::write_milestones(dir, &ms).map_err(e)?;
    } else if let Some(t) = p.tickets.iter().find(|t| t.id == id) {
        not_approved(t.meta.review)?;
        match t.meta.status {
            TicketStatus::Todo => {}
            TicketStatus::Dropped => return Err(format!("{id} is dropped.")),
            s => {
                return Err(format!(
                    "{id} is {}: started work is never rewritten. Propose a follow-up ticket \
                     for what the new requirements add (plan_propose_ticket in {}, its \
                     description starting \"Follows {id}:\" and saying why).",
                    s.label().to_lowercase(),
                    t.meta.milestone
                ));
            }
        }
        c.description = get(args, "description").map(str::to_string);
        c.repo = get(args, "repo")
            .map(crate::projects::repo_name)
            .transpose()?;
        if let Some(v) = get(args, "milestone") {
            if !p
                .milestones
                .iter()
                .any(|m| m.id == v && m.review == Review::Approved)
            {
                return Err(format!("{v} isn't an approved milestone."));
            }
            c.milestone = Some(v.to_string());
        }
        if !drop
            && c.title.is_none()
            && c.description.is_none()
            && c.repo.is_none()
            && c.milestone.is_none()
        {
            return Err(
                "Give what changes: title, description, repo or milestone (or drop).".into(),
            );
        }
        let mut t = t.clone();
        t.meta.change = Some(c);
        repo::write_ticket(dir, &t).map_err(e)?;
    } else {
        return Err(format!("There's no {id}."));
    }
    Ok(format!(
        "Proposed a change to {id}; the approved version stands until the lead accepts it."
    ))
}

fn work(dir: &Path, ticket: &str, name: &str, args: &Value) -> Result<String, String> {
    let p = repo::load(dir).map_err(|e| e.to_string())?;
    let t = p
        .tickets
        .iter()
        .find(|t| t.id == ticket)
        .ok_or_else(|| format!("Ticket {ticket} isn't in the project any more."))?;
    match name {
        "ticket_show" => {
            let m = p.milestones.iter().find(|m| m.id == t.meta.milestone);
            let mut out = format!(
                "{} · {}\nProject: {} ({})\nMilestone: {}\nBranch: {}\n\n{}\n",
                t.id,
                t.meta.title,
                p.file.name,
                dir.display(),
                m.map(|m| format!("{} {}: {}", m.id, m.title, m.description))
                    .unwrap_or_default(),
                t.meta.branch,
                t.body.trim(),
            );
            let questions: Vec<_> = p
                .decisions
                .iter()
                .filter(|d| d.meta.ticket == t.id)
                .collect();
            if !questions.is_empty() {
                out.push_str("\nQuestions about it:\n");
                for d in questions {
                    let answer = if d.is_open() {
                        "(open)".to_string()
                    } else {
                        d.meta.answer.clone()
                    };
                    out.push_str(&format!("- {}: {answer}\n", d.meta.question));
                }
            }
            out.push_str(&format!(
                "\nThe project's spec is {}.\n",
                dir.join("spec.md").display()
            ));
            Ok(out)
        }
        "ticket_ask" => ask(dir, &p, args, ticket),
        "ticket_report" => {
            let prs: Vec<String> = args
                .get("prs")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|u| u.trim().to_string())
                        .filter(|u| !u.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(bad) = prs.iter().find(|u| !u.starts_with("https://")) {
                return Err(format!("{bad} isn't a pull request URL."));
            }
            let summary = need(args, "summary")?;
            let mut t = t.clone();
            // With no pull request there's nothing to review or merge: the summary is the result.
            let done = prs.is_empty();
            t.meta.prs = prs;
            t.meta.status = if done {
                TicketStatus::Done
            } else {
                TicketStatus::InReview
            };
            t.meta.finished_at = Some(progress::now());
            t.body = format!("{}\n\n## Work\n\n{summary}\n", t.body.trim_end());
            repo::write_ticket(dir, &t).map_err(|e| e.to_string())?;
            let mut filed = Vec::new();
            for item in args
                .get("for_a_person")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let field = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default();
                if field("task").trim().is_empty() {
                    continue;
                }
                let p = repo::load(dir).map_err(|e| e.to_string())?;
                if let Some(id) = crate::projects::new_person_task(
                    dir,
                    &p,
                    &t.meta.milestone,
                    &t.id,
                    field("task"),
                    field("why"),
                    ASKED_BY_CLAUDE,
                )? {
                    filed.push(id);
                }
            }
            let filed = if filed.is_empty() {
                String::new()
            } else {
                format!(" Filed for a person: {}.", filed.join(", "))
            };
            Ok(if done {
                format!(
                    "Reported: {} is done, with your summary as its result.{filed}",
                    t.id
                )
            } else {
                format!(
                    "Reported: {} is in review with its pull requests.{filed}",
                    t.id
                )
            })
        }
        "ticket_checked" => {
            let passed = args
                .get("passed")
                .and_then(Value::as_bool)
                .ok_or("Say whether the check passed (passed: true or false).")?;
            let summary = need(args, "summary")?;
            let mut t = t.clone();
            t.meta.check = if passed { "passed" } else { "failed" }.into();
            t.meta.check_note = summary.clone();
            t.body = format!(
                "{}\n\n## Check\n\n{}: {summary}\n",
                t.body.trim_end(),
                if passed { "Passed" } else { "Failed" }
            );
            repo::write_ticket(dir, &t).map_err(|e| e.to_string())?;
            Ok(format!(
                "Recorded: {}'s check {}.",
                t.id,
                if passed { "passed" } else { "failed" }
            ))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn read_resource(dir: &Path, name: &str) -> Result<String, String> {
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return Err("Give the file's name as plan_show lists it.".into());
    }
    let path = dir.join("resources").join(name);
    let bytes = std::fs::read(&path).map_err(|_| format!("There's no file {name}."))?;
    match String::from_utf8(bytes) {
        Ok(text) if text.len() <= 200_000 => Ok(text),
        _ => Ok(format!(
            "{name} isn't plain text; open it with your Read tool: {}",
            path.display()
        )),
    }
}

/// The project as `plan_show` tells it.
fn show(dir: &Path, p: &Project) -> String {
    let mut out = format!(
        "Project \"{}\" (key {}, lead {}), in {}\n\n## Spec\n{}\n\n## Milestones\n",
        p.file.name,
        p.file.key,
        p.file.lead,
        dir.display(),
        if p.spec.trim().is_empty() {
            "(empty)"
        } else {
            p.spec.trim()
        },
    );
    if let Some(c) = &p.file.spec_change {
        out = out.replace(
            "\n\n## Milestones\n",
            &format!(
                "\n\n(A new spec is waiting for the lead: {})\n\n## Milestones\n",
                c.why
            ),
        );
    } else if !p.file.spec_note.is_empty() {
        out = out.replace(
            "\n\n## Milestones\n",
            &format!("\n\n(Spec: {})\n\n## Milestones\n", p.file.spec_note),
        );
    }
    let note = |r: Review, n: &str, c: &Option<hydraui_projects::Change>| {
        let mut s = match r {
            Review::Changes => format!(" [changes asked: {n}]"),
            Review::Rejected => format!(" [rejected: {n}]"),
            Review::Approved if !n.is_empty() => format!(" [approved; {n}]"),
            other => format!(" [{}]", other.label()),
        };
        if let Some(c) = c {
            let what = if c.drop { "drop it" } else { "change" };
            s.push_str(&format!(
                " [proposed {what}, waiting for the lead: {}]",
                c.why
            ));
        }
        s
    };
    for m in &p.milestones {
        let target = m
            .target
            .map(|t| format!(", target {t}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- {}: {}{target}{}\n  {}\n",
            m.id,
            m.title,
            note(m.review, &m.review_note, &m.change),
            m.description
        ));
        for t in p.tickets.iter().filter(|t| t.meta.milestone == m.id) {
            let kind = match t.meta.kind {
                TicketKind::Feature => "",
                TicketKind::Bug => " (bug)",
                TicketKind::Chore => " (chore)",
            };
            let repo = if t.meta.repo.is_empty() {
                String::new()
            } else {
                format!(" · in {}", t.meta.repo)
            };
            out.push_str(&format!(
                "  - {}: {}{kind}{} · {}{repo}\n",
                t.id,
                t.meta.title,
                note(t.meta.review, &t.meta.review_note, &t.meta.change),
                t.meta.status.label()
            ));
        }
    }
    out.push_str("\n## Repos\n");
    let (confirmed, suggested) = crate::projects::repos_of(p);
    for r in &confirmed {
        out.push_str(&format!("- {}\n", r.repo));
    }
    for r in &suggested {
        let why = if !r.why.is_empty() {
            r.why.clone()
        } else {
            format!("tickets {}", r.tickets.join(", "))
        };
        out.push_str(&format!("- {} (suggested: {why})\n", r.repo));
    }
    if !p.file.dismissed_repos.is_empty() {
        out.push_str(&format!(
            "- dismissed by the lead: {}\n",
            p.file.dismissed_repos.join(", ")
        ));
    }
    out.push_str("\n## Open questions\n");
    for d in p.decisions.iter().filter(|d| d.is_open()) {
        out.push_str(&format!(
            "- {}: {} (for {})\n",
            d.id, d.meta.question, d.meta.assigned_to
        ));
    }
    out.push_str("\n## Decided\n");
    for d in p.decisions.iter().filter(|d| !d.is_open()) {
        out.push_str(&format!(
            "- {}: {} → {} (by {})\n",
            d.id, d.meta.question, d.meta.answer, d.meta.decided_by
        ));
    }
    out.push_str("\n## Resources\n");
    for f in &p.files {
        out.push_str(&format!("- file: {f}\n"));
    }
    for l in &p.links {
        out.push_str(&format!("- link: {} <{}>\n", l.title, l.url));
    }
    out
}

fn kind(text: Option<&str>) -> Result<TicketKind, String> {
    match text.map(str::to_ascii_lowercase).as_deref() {
        None | Some("feature") => Ok(TicketKind::Feature),
        Some("bug") => Ok(TicketKind::Bug),
        Some("chore") => Ok(TicketKind::Chore),
        Some(other) => Err(format!("{other} isn't a kind: feature, bug or chore")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{Item, review};

    fn setup() -> (tempfile::TempDir, Registry, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("product-groups");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.toml"),
            "name = \"Product Groups\"\nkey = \"PG\"\nlead = \"lead\"\n",
        )
        .unwrap();
        let mut reg = Registry::default();
        reg.project_sessions.insert(
            "plan".into(),
            Session::Plan {
                dir: dir.clone(),
                login: "lead".into(),
            },
        );
        (tmp, reg, dir)
    }

    fn run(reg: &Registry, token: &str, name: &str, args: Value) -> Outcome {
        call(reg, token, name, &args).expect("a project tool")
    }

    fn approve(dir: &Path, id: &str) {
        let item = if id.starts_with('M') {
            Item::Milestone(id.into())
        } else {
            Item::Ticket(id.into())
        };
        review(dir, &item, Review::Approved, "", "lead").unwrap();
    }

    /// An agreed plan: spec, M1 approved, PG-1 to do and PG-2 in progress, both approved.
    fn agreed_plan(reg: &Registry, dir: &Path) {
        run(reg, "plan", "plan_set_spec", json!({"text": "# PG"}));
        run(
            reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "tables", "detail": "All of them."}]}),
        );
        approve(dir, "M1");
        for title in ["Schema", "Share"] {
            run(
                reg,
                "plan",
                "plan_propose_ticket",
                json!({"milestone": "M1", "title": title, "description": "x", "repo": "iov42/be"}),
            );
        }
        approve(dir, "PG-1");
        approve(dir, "PG-2");
        let mut t = repo::load(dir).unwrap().tickets.remove(1);
        t.meta.status = TicketStatus::InProgress;
        repo::write_ticket(dir, &t).unwrap();
    }

    #[test]
    fn new_requirements_change_an_agreed_plan_only_by_proposal() {
        let (_tmp, reg, dir) = setup();
        agreed_plan(&reg, &dir);
        let from = "Requirements from Joshua 2026-11-05.md";
        // The agreed spec isn't overwritten; a new one is proposed.
        assert!(run(&reg, "plan", "plan_set_spec", json!({"text": "# New"})).is_error);
        let out = run(
            &reg,
            "plan",
            "plan_propose_spec",
            json!({"text": "# New", "why": "CSV export", "from": from}),
        );
        assert!(!out.is_error, "{}", out.text);
        // A ticket still to do gets a change; the approved version stands meanwhile.
        let out = run(
            &reg,
            "plan",
            "plan_propose_change",
            json!({"id": "PG-1", "why": "CSV too", "from": from, "title": "Schema with CSV"}),
        );
        assert!(!out.is_error, "{}", out.text);
        // Started work is never rewritten: Claude is pointed at a follow-up ticket.
        let out = run(
            &reg,
            "plan",
            "plan_propose_change",
            json!({"id": "PG-2", "why": "CSV too", "title": "Share as CSV"}),
        );
        assert!(
            out.is_error && out.text.contains("follow-up"),
            "{}",
            out.text
        );
        // A milestone with started work can't be dropped; its points keep the rules.
        let drop = json!({"id": "M1", "why": "cut", "drop": true});
        assert!(run(&reg, "plan", "plan_propose_change", drop).is_error);
        let long = json!({"id": "M1", "why": "x", "title": "x".repeat(61)});
        assert!(run(&reg, "plan", "plan_propose_change", long).is_error);
        let empty = json!({"id": "M1", "why": "x"});
        assert!(run(&reg, "plan", "plan_propose_change", empty).is_error);
        // Approved items aren't revised in place.
        let out = run(
            &reg,
            "plan",
            "plan_revise",
            json!({"id": "PG-1", "title": "y"}),
        );
        assert!(out.is_error && out.text.contains("plan_propose_change"));

        let p = repo::load(&dir).unwrap();
        assert_eq!(p.spec.trim(), "# PG");
        assert_eq!(p.tickets[0].meta.title, "Schema");
        assert_eq!(
            p.tickets[0].meta.change.as_ref().unwrap().title.as_deref(),
            Some("Schema with CSV")
        );
        let shown = run(&reg, "plan", "plan_show", json!({})).text;
        assert!(shown.contains("A new spec is waiting") && shown.contains("proposed change"));
        // A proposal still to review is revised, not changed.
        run(
            &reg,
            "plan",
            "plan_propose_ticket",
            json!({"milestone": "M1", "title": "Export", "description": "x", "repo": "iov42/be"}),
        );
        let out = run(
            &reg,
            "plan",
            "plan_propose_change",
            json!({"id": "PG-3", "why": "x", "title": "y"}),
        );
        assert!(out.is_error && out.text.contains("plan_revise"));
    }

    #[test]
    fn a_plan_is_proposed_then_reviewed() {
        let (_tmp, reg, dir) = setup();
        assert!(!run(&reg, "plan", "plan_set_spec", json!({"text": "# PG"})).is_error);
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "tables", "detail": "All of them."}], "target": "2026-12-01"}),
        );
        assert!(out.text.starts_with("Proposed M1"), "{}", out.text);
        let ticket = json!({"milestone": "M1", "title": "Schema", "description": "x", "repo": "iov42/version-2-backend"});
        let early = run(&reg, "plan", "plan_propose_ticket", ticket.clone());
        assert!(
            early.is_error && early.text.contains("isn't approved"),
            "{}",
            early.text
        );
        approve(&dir, "M1");
        let ok = run(&reg, "plan", "plan_propose_ticket", ticket);
        assert!(ok.text.starts_with("Proposed PG-1"), "{}", ok.text);
        let shown = run(&reg, "plan", "plan_show", json!({})).text;
        assert!(
            shown.contains("- M1: Model, target 2026-12-01 [approved]"),
            "{shown}"
        );
        assert!(
            shown.contains("  - PG-1: Schema [to review] · Todo"),
            "{shown}"
        );
    }

    #[test]
    fn revising_answers_a_note_and_goes_back_for_review() {
        let (_tmp, reg, dir) = setup();
        run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "x", "detail": "y"}]}),
        );
        let m1 = Item::Milestone("M1".into());
        review(&dir, &m1, Review::Changes, "give it a date", "lead").unwrap();
        let shown = run(&reg, "plan", "plan_show", json!({})).text;
        assert!(shown.contains("[changes asked: give it a date]"), "{shown}");
        let revise = json!({"id": "M1", "target": "2026-12-01"});
        assert!(!run(&reg, "plan", "plan_revise", revise).is_error);
        let m = repo::load(&dir).unwrap().milestones.remove(0);
        assert_eq!(m.review, Review::Proposed);
        assert_eq!(m.target.unwrap().to_string(), "2026-12-01");
        approve(&dir, "M1");
        let late = json!({"id": "M1", "title": "x"});
        assert!(
            run(&reg, "plan", "plan_revise", late).is_error,
            "approved stays"
        );
        assert!(run(&reg, "plan", "plan_withdraw", json!({"id": "M1"})).is_error);
    }

    #[test]
    fn a_rejection_is_shown_with_its_reason_and_stays() {
        let (_tmp, reg, dir) = setup();
        run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Mobile", "points": [{"point": "x", "detail": "y"}]}),
        );
        let m1 = Item::Milestone("M1".into());
        review(&dir, &m1, Review::Rejected, "out of scope", "lead").unwrap();
        let shown = run(&reg, "plan", "plan_show", json!({})).text;
        assert!(shown.contains("[rejected: out of scope]"), "{shown}");
        assert!(run(&reg, "plan", "plan_withdraw", json!({"id": "M1"})).is_error);
    }

    #[test]
    fn a_work_session_reports_its_pull_requests() {
        let (_tmp, mut reg, dir) = setup();
        run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "x", "detail": "y"}]}),
        );
        approve(&dir, "M1");
        let ticket = json!({"milestone": "M1", "title": "Schema", "description": "Make the tables.", "repo": "iov42/version-2-backend"});
        run(&reg, "plan", "plan_propose_ticket", ticket);
        reg.project_sessions.insert(
            "work".into(),
            Session::Work {
                dir: dir.clone(),
                ticket: "PG-1".into(),
            },
        );
        assert!(
            run(&reg, "work", "ticket_show", json!({}))
                .text
                .contains("Make the tables.")
        );
        let no_summary = json!({"prs": []});
        assert!(run(&reg, "work", "ticket_report", no_summary).is_error);
        let junk = json!({"prs": ["not a url"], "summary": "x"});
        assert!(run(&reg, "work", "ticket_report", junk).is_error);
        let pr = "https://github.com/iov42/fe/pull/1";
        let out = run(
            &reg,
            "work",
            "ticket_report",
            json!({"prs": [pr], "summary": "Added the tables."}),
        );
        assert!(!out.is_error, "{}", out.text);
        let t = repo::load(&dir).unwrap().tickets.remove(0);
        assert_eq!(t.meta.status, TicketStatus::InReview);
        assert_eq!(t.meta.prs, [pr]);
        assert!(t.body.contains("## Work\n\nAdded the tables."));
        // Work that changes no code, such as an investigation, finishes with its findings.
        let found = json!({"prs": [], "summary": "newRecords misses location types."});
        let out = run(&reg, "work", "ticket_report", found);
        assert!(!out.is_error && out.text.contains("done"), "{}", out.text);
        let t = repo::load(&dir).unwrap().tickets.remove(0);
        assert_eq!(t.meta.status, TicketStatus::Done);
        assert!(t.meta.prs.is_empty());
        assert!(t.meta.finished_at.is_some());
        // What only a person can do becomes their task in the milestone, once.
        let with_tasks = json!({"prs": [], "summary": "Done.", "for_a_person": [
            {"task": "Create the flag in GrowthBook", "why": "QA turns it on"},
            {"task": "create the flag in growthbook"},
            {"task": " "}]});
        let out = run(&reg, "work", "ticket_report", with_tasks);
        assert!(
            out.text.contains("Filed for a person: PG-2."),
            "{}",
            out.text
        );
        let p = repo::load(&dir).unwrap();
        let task = p.tickets.iter().find(|t| t.id == "PG-2").unwrap();
        assert!(task.meta.person && task.meta.milestone == "M1");
        assert_eq!(p.tickets.iter().filter(|t| t.meta.person).count(), 1);
        assert!(
            t.body
                .contains("## Work\n\nnewRecords misses location types.")
        );
    }

    #[test]
    fn claude_asks_through_hydraui_and_sees_the_answer() {
        let (_tmp, mut reg, dir) = setup();
        let q = json!({"question": "Postgres or Mongo?", "context": "We store groups.", "options": ["Postgres", " ", "Mongo"]});
        assert!(
            run(&reg, "plan", "plan_add_question", q)
                .text
                .starts_with("Asked D-1 in hydraui, for lead")
        );
        let d = repo::load(&dir).unwrap().decisions.remove(0);
        assert_eq!(
            (d.meta.asked_by.as_str(), d.meta.ticket.as_str()),
            (ASKED_BY_CLAUDE, "")
        );
        assert_eq!(d.meta.options, ["Postgres", "Mongo"]);
        assert_eq!(d.body.trim(), "We store groups.");
        crate::projects::answer(&dir, &d, "Postgres", "lead").unwrap();
        let shown = run(&reg, "plan", "plan_show", json!({})).text;
        assert!(
            shown.contains("- D-1: Postgres or Mongo? → Postgres (by lead)"),
            "{shown}"
        );
        reg.project_sessions.insert(
            "work".into(),
            Session::Work {
                dir: dir.clone(),
                ticket: "PG-1".into(),
            },
        );
        std::fs::create_dir_all(dir.join("tickets")).unwrap();
        std::fs::write(
            dir.join("tickets/PG-1.md"),
            "+++\ntitle = \"Schema\"\n+++\n",
        )
        .unwrap();
        let asked = run(
            &reg,
            "work",
            "ticket_ask",
            json!({"question": "Which table?", "assigned_to": "@aza"}),
        );
        assert!(!asked.is_error, "{}", asked.text);
        let d = repo::load(&dir).unwrap().decisions.remove(1);
        assert_eq!(
            (d.meta.ticket.as_str(), d.meta.assigned_to.as_str()),
            ("PG-1", "aza")
        );
    }

    #[test]
    fn tickets_name_their_repo_and_claude_suggests_repos_for_the_lead() {
        let (_tmp, reg, dir) = setup();
        run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "x", "detail": "y"}]}),
        );
        let _ = review(
            &dir,
            &Item::Milestone("M1".into()),
            Review::Approved,
            "",
            "lead",
        );
        let no_repo = run(
            &reg,
            "plan",
            "plan_propose_ticket",
            json!({"milestone": "M1", "title": "Schema", "description": "x"}),
        );
        assert!(
            no_repo.is_error && no_repo.text.contains("repo"),
            "{}",
            no_repo.text
        );
        let bad = run(
            &reg,
            "plan",
            "plan_propose_ticket",
            json!({"milestone": "M1", "title": "Schema", "description": "x", "repo": "the backend"}),
        );
        assert!(bad.is_error, "{}", bad.text);
        let ok = run(
            &reg,
            "plan",
            "plan_propose_ticket",
            json!({"milestone": "M1", "title": "Schema", "description": "x", "repo": "https://github.com/iov42/version-2-backend"}),
        );
        assert!(!ok.is_error, "{}", ok.text);
        let t = repo::load(&dir).unwrap().tickets.remove(0);
        assert_eq!(t.meta.repo, "iov42/version-2-backend");

        let s = run(
            &reg,
            "plan",
            "plan_suggest_repo",
            json!({"repo": "iov42/api-designs", "why": "a new endpoint"}),
        );
        assert!(!s.is_error, "{}", s.text);
        let p = repo::load(&dir).unwrap();
        assert_eq!(p.file.suggested_repos[0].why, "a new endpoint");
        let shown = run(&reg, "plan", "plan_show", json!({})).text;
        assert!(shown.contains("## Repos"), "{shown}");
        assert!(
            shown.contains("iov42/api-designs (suggested: a new endpoint)"),
            "{shown}"
        );
        assert!(shown.contains("in iov42/version-2-backend"), "{shown}");
    }

    #[test]
    fn an_approved_tickets_repo_can_be_set_and_it_stays_approved() {
        let (_tmp, reg, dir) = setup();
        run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "x", "detail": "y"}]}),
        );
        let _ = review(
            &dir,
            &Item::Milestone("M1".into()),
            Review::Approved,
            "",
            "lead",
        );
        run(
            &reg,
            "plan",
            "plan_propose_ticket",
            json!({"milestone": "M1", "title": "Schema", "description": "x", "repo": "iov42/a"}),
        );
        let _ = review(
            &dir,
            &Item::Ticket("PG-1".into()),
            Review::Approved,
            "",
            "lead",
        );
        // Only the repo: allowed, and still approved.
        let ok = run(
            &reg,
            "plan",
            "plan_revise",
            json!({"id": "PG-1", "repo": "iov42/b"}),
        );
        assert!(!ok.is_error, "{}", ok.text);
        let t = repo::load(&dir).unwrap().tickets.remove(0);
        assert_eq!(
            (t.meta.repo.as_str(), t.meta.review),
            ("iov42/b", Review::Approved)
        );
        // Anything more on an approved ticket still isn't Claude's to change.
        let no = run(
            &reg,
            "plan",
            "plan_revise",
            json!({"id": "PG-1", "repo": "iov42/c", "title": "New"}),
        );
        assert!(no.is_error, "{}", no.text);
    }

    #[test]
    fn a_planning_session_says_when_its_done() {
        let (_tmp, reg, _dir) = setup();
        let none = run(&reg, "plan", "plan_done", json!({}));
        assert!(none.is_error, "{}", none.text);
        let done = run(
            &reg,
            "plan",
            "plan_done",
            json!({"summary": "Set the repos on MUE-1 to MUE-8."}),
        );
        assert!(!done.is_error, "{}", done.text);
        assert_eq!(
            take_finished(&reg),
            vec![(
                "plan".to_string(),
                "Set the repos on MUE-1 to MUE-8.".to_string()
            )]
        );
        assert!(take_finished(&reg).is_empty());
    }

    #[test]
    fn milestones_stay_short() {
        let (_tmp, reg, dir) = setup();
        let long_title = "x".repeat(MILESTONE_TITLE_MAX + 1);
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": long_title, "points": [{"point": "x", "detail": "y"}]}),
        );
        assert!(
            out.is_error && out.text.contains("keep it to 60"),
            "{}",
            out.text
        );
        let long_point = "y".repeat(MILESTONE_POINT_MAX + 1);
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Partners see groups", "points": [{"point": long_point, "detail": "d"}]}),
        );
        assert!(
            out.is_error && out.text.contains("put the rest in its detail"),
            "{}",
            out.text
        );
        let five = vec![json!({"point": "a", "detail": "d"}); MILESTONE_POINTS_MAX + 1];
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Partners see groups", "points": five}),
        );
        assert!(
            out.is_error && out.text.contains("at most 4"),
            "{}",
            out.text
        );
        // A wall of text isn't points.
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Partners see groups", "description": "So they stop asking."}),
        );
        assert!(out.is_error, "{}", out.text);
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Partners see groups", "points": [{"point": "So they stop asking"}]}),
        );
        assert!(
            out.is_error && out.text.contains("no detail"),
            "{}",
            out.text
        );
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Partners see groups", "points": [
                {"point": "- So they stop asking", "detail": "Today they email us.\nEvery week."},
                {"point": "Done when a partner opens one", "detail": "In hydraui."},
            ]}),
        );
        assert!(!out.is_error, "{}", out.text);
        let m = repo::load(&dir).unwrap().milestones.remove(0);
        assert_eq!(
            m.description,
            "- So they stop asking\n  Today they email us.\n  Every week.\n- Done when a partner opens one\n  In hydraui."
        );
        let long_title = "z".repeat(61);
        assert!(
            run(
                &reg,
                "plan",
                "plan_revise",
                json!({"id": "M1", "title": long_title})
            )
            .is_error
        );
    }

    #[test]
    fn revising_an_old_milestone_needs_points() {
        let (_tmp, reg, dir) = setup();
        let mut p = repo::load(&dir).unwrap();
        p.milestones.push(Milestone {
            id: "M1".into(),
            title: "Old".into(),
            description: "A long paragraph from before points.".into(),
            review: Review::Proposed,
            ..Default::default()
        });
        repo::write_milestones(&dir, &p.milestones).unwrap();
        let out = run(
            &reg,
            "plan",
            "plan_revise",
            json!({"id": "M1", "title": "New"}),
        );
        assert!(out.is_error && out.text.contains("points"), "{}", out.text);
        let out = run(
            &reg,
            "plan",
            "plan_revise",
            json!({"id": "M1", "title": "New", "points": [
                {"point": "Why", "detail": "Because."},
                {"point": "Done when", "detail": "It is."},
            ]}),
        );
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            repo::load(&dir).unwrap().milestones[0].description,
            "- Why\n  Because.\n- Done when\n  It is."
        );
    }

    #[test]
    fn each_session_uses_only_its_own_tools() {
        let (_tmp, mut reg, dir) = setup();
        reg.project_sessions.insert(
            "work".into(),
            Session::Work {
                dir,
                ticket: "PG-1".into(),
            },
        );
        assert!(run(&reg, "work", "plan_show", json!({})).is_error);
        assert!(run(&reg, "plan", "ticket_show", json!({})).is_error);
        assert!(run(&reg, "stranger", "plan_show", json!({})).is_error);
        assert!(call(&reg, "plan", "tasks_read", &json!({})).is_none());
    }

    #[test]
    fn a_testing_session_checks_a_ticket_and_only_that() {
        let (_tmp, mut reg, dir) = setup();
        run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": [{"point": "x", "detail": "y"}]}),
        );
        approve(&dir, "M1");
        run(
            &reg,
            "plan",
            "plan_propose_ticket",
            json!({"milestone": "M1", "title": "Schema", "description": "x", "repo": "iov42/a"}),
        );
        approve(&dir, "PG-1");
        reg.project_sessions.insert(
            "check".into(),
            Session::Check {
                dir: dir.clone(),
                ticket: "PG-1".into(),
            },
        );
        reg.project_sessions.insert(
            "work".into(),
            Session::Work {
                dir: dir.clone(),
                ticket: "PG-1".into(),
            },
        );
        assert!(!run(&reg, "check", "ticket_show", json!({})).is_error);
        let report = json!({"prs": ["https://github.com/iov42/a/pull/1"], "summary": "x"});
        assert!(run(&reg, "check", "ticket_report", report).is_error);
        let checked = json!({"passed": false, "summary": "Two tests fail in the stepper."});
        assert!(run(&reg, "work", "ticket_checked", checked.clone()).is_error);
        let out = run(&reg, "check", "ticket_checked", checked);
        assert!(!out.is_error, "{}", out.text);
        let t = repo::load(&dir).unwrap().tickets.remove(0);
        assert_eq!(
            (t.meta.check.as_str(), t.meta.check_note.as_str()),
            ("failed", "Two tests fail in the stepper.")
        );
        assert!(
            t.body
                .contains("## Check\n\nFailed: Two tests fail in the stepper.")
        );
        assert!(
            run(
                &reg,
                "check",
                "ticket_checked",
                json!({"summary": "no verdict"})
            )
            .is_error
        );
    }
}
