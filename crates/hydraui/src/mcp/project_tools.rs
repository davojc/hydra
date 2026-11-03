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

/// A session hydraui started for a project, by what it may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// Plans project folder `dir` for `login` (the lead, who asked).
    Plan { dir: PathBuf, login: String },
    /// Works on ticket `ticket` of project folder `dir`.
    Work { dir: PathBuf, ticket: String },
}

/// The planning tools, in the order `tools/list` shows them.
pub const PLAN: [&str; 9] = [
    "plan_show",
    "plan_set_spec",
    "plan_propose_milestone",
    "plan_propose_ticket",
    "plan_revise",
    "plan_withdraw",
    "plan_add_question",
    "plan_add_link",
    "plan_read_resource",
];

/// The work tools.
pub const WORK: [&str; 3] = ["ticket_show", "ticket_ask", "ticket_report"];

/// Who asked a question a Claude session raised: its answer goes back to the session.
pub const ASKED_BY_CLAUDE: &str = "claude";

/// Every tool this module answers.
pub fn names() -> impl Iterator<Item = &'static str> {
    PLAN.into_iter().chain(WORK)
}

/// The `tools/list` entries for `names()`.
pub fn schemas() -> Vec<Value> {
    let s = |what: &str| json!({ "type": "string", "description": what });
    let points =
        |what: &str| json!({ "type": "array", "items": {"type": "string"}, "description": what });
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
             stop emailing us for group lists\". Don't propose again what was rejected.",
            json!({
                "title": s("The outcome, in plain words; at most 60 characters."),
                "target": s("Target date, YYYY-MM-DD (optional)."),
                "points": points("1 to 4 short points, at most 80 characters each: what it's for and how we'll know it's done."),
            }),
            vec!["title", "points"],
        ),
        tool(
            "plan_propose_ticket",
            "Planning: propose a ticket in an approved milestone, for the lead to review. Each \
             should be finishable in a few days, with how to tell it's done. Returns its id.",
            json!({
                "milestone": s("The approved milestone's id, e.g. M1."),
                "title": s("The ticket, in one line."),
                "kind": s("feature, bug or chore (default feature)."),
                "description": s("What to do and how to tell it's done (Markdown)."),
            }),
            vec!["milestone", "title", "description"],
        ),
        tool(
            "plan_revise",
            "Planning: revise a milestone (M..) or ticket (KEY-..) that is still to review or \
             had changes asked (a milestone keeps to plain words: title at most 60 characters \
             and 1 to 4 points of at most 80); it goes back to the lead to review. Only the fields \
             given change. Use the lead's note (plan_show shows it).",
            json!({
                "id": s("What to revise."),
                "title": s("New title."),
                "target": s("A milestone's target date, YYYY-MM-DD."),
                "description": s("A ticket's new description."),
                "points": points("A milestone's new points, 1 to 4, at most 80 characters each."),
                "milestone": s("A ticket's milestone id."),
                "kind": s("A ticket's kind: feature, bug or chore."),
            }),
            vec!["id"],
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
             ticket moves to in review and the people on the project see the links.",
            json!({
                "prs": {"type": "array", "items": {"type": "string"},
                    "description": "The pull request URLs."},
                "summary": s("What you did, and anything a reviewer should know."),
            }),
            vec!["prs", "summary"],
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
        (Some(Session::Plan { dir, login }), true) => plan(dir, login, name, args),
        (Some(Session::Work { dir, ticket }), false) => work(dir, ticket, name, args),
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
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            repo::write_spec(dir, text).map_err(e)?;
            Ok("Spec saved.".into())
        }
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
/// and easy for anyone to read, a few bullet points rather than a paragraph; detail belongs
/// in tickets).
pub const MILESTONE_TITLE_MAX: usize = 60;
pub const MILESTONE_POINTS_MAX: usize = 4;
pub const MILESTONE_POINT_MAX: usize = 80;

/// The `points` argument as a milestone description ("- a\n- b"), if given.
fn milestone_points(args: &Value) -> Result<Option<String>, String> {
    let Some(v) = args.get("points") else {
        return Ok(None);
    };
    let items = v
        .as_array()
        .ok_or("points is a list of short strings.")?
        .iter()
        .map(|p| p.as_str().map(crate::projects::strip_point))
        .collect::<Option<Vec<_>>>()
        .ok_or("points is a list of short strings.")?;
    if items.iter().any(|p| p.is_empty()) {
        return Err("A point is empty.".into());
    }
    Ok(Some(
        items
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

/// Refuses a milestone that's too long to take in at a glance, saying how to fix it.
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
            "Give the milestone as points (1 to 4 short bullet points: what it's \
                    for and how we'll know it's done), not a paragraph."
                .into(),
        );
    };
    if points.len() > MILESTONE_POINTS_MAX {
        return Err(format!(
            "That's {} points; keep it to at most {MILESTONE_POINTS_MAX}.",
            points.len()
        ));
    }
    if let Some(long) = points
        .iter()
        .find(|p| p.chars().count() > MILESTONE_POINT_MAX)
    {
        return Err(format!(
            "\"{long}\" is too long; keep each point to {MILESTONE_POINT_MAX} characters, in \
             plain words. Technical detail goes in its tickets."
        ));
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
                "{id} is {}; only proposals are revised.",
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
        if !open(t.meta.review) {
            return Err(format!(
                "{id} is {}; only proposals are revised.",
                t.meta.review.label()
            ));
        }
        if let Some(v) = get(args, "title") {
            t.meta.title = v.to_string();
        }
        if let Some(v) = get(args, "description") {
            t.body = v.to_string();
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
            if prs.is_empty() {
                return Err("Give the pull request URLs you opened for the ticket.".into());
            }
            if let Some(bad) = prs.iter().find(|u| !u.starts_with("https://")) {
                return Err(format!("{bad} isn't a pull request URL."));
            }
            let summary = need(args, "summary")?;
            let mut t = t.clone();
            t.meta.prs = prs;
            t.meta.status = TicketStatus::InReview;
            t.body = format!("{}\n\n## Work\n\n{summary}\n", t.body.trim_end());
            repo::write_ticket(dir, &t).map_err(|e| e.to_string())?;
            Ok(format!(
                "Reported: {} is in review with its pull requests.",
                t.id
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
    let note = |r: Review, n: &str| match r {
        Review::Changes => format!(" [changes asked: {n}]"),
        Review::Rejected => format!(" [rejected: {n}]"),
        other => format!(" [{}]", other.label()),
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
            note(m.review, &m.review_note),
            m.description
        ));
        for t in p.tickets.iter().filter(|t| t.meta.milestone == m.id) {
            let kind = match t.meta.kind {
                TicketKind::Feature => "",
                TicketKind::Bug => " (bug)",
                TicketKind::Chore => " (chore)",
            };
            out.push_str(&format!(
                "  - {}: {}{kind}{} · {}\n",
                t.id,
                t.meta.title,
                note(t.meta.review, &t.meta.review_note),
                t.meta.status.label()
            ));
        }
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

    #[test]
    fn a_plan_is_proposed_then_reviewed() {
        let (_tmp, reg, dir) = setup();
        assert!(!run(&reg, "plan", "plan_set_spec", json!({"text": "# PG"})).is_error);
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": "Model", "points": ["tables"], "target": "2026-12-01"}),
        );
        assert!(out.text.starts_with("Proposed M1"), "{}", out.text);
        let ticket = json!({"milestone": "M1", "title": "Schema", "description": "x"});
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
            json!({"title": "Model", "points": ["x"]}),
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
            json!({"title": "Mobile", "points": ["x"]}),
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
            json!({"title": "Model", "points": ["x"]}),
        );
        approve(&dir, "M1");
        let ticket =
            json!({"milestone": "M1", "title": "Schema", "description": "Make the tables."});
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
        let empty = json!({"prs": [], "summary": "x"});
        assert!(run(&reg, "work", "ticket_report", empty).is_error);
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
    fn milestones_stay_short() {
        let (_tmp, reg, dir) = setup();
        let long_title = "x".repeat(MILESTONE_TITLE_MAX + 1);
        let out = run(
            &reg,
            "plan",
            "plan_propose_milestone",
            json!({"title": long_title, "points": ["x"]}),
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
            json!({"title": "Partners see groups", "points": [long_point]}),
        );
        assert!(
            out.is_error && out.text.contains("goes in its tickets"),
            "{}",
            out.text
        );
        let five = vec!["a"; MILESTONE_POINTS_MAX + 1];
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
            json!({"title": "Partners see groups", "points": ["- So they stop asking", "Done when a partner opens one"]}),
        );
        assert!(!out.is_error, "{}", out.text);
        let m = repo::load(&dir).unwrap().milestones.remove(0);
        assert_eq!(
            m.description,
            "- So they stop asking\n- Done when a partner opens one"
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
            json!({"id": "M1", "title": "New", "points": ["Why", "Done when"]}),
        );
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            repo::load(&dir).unwrap().milestones[0].description,
            "- Why\n- Done when"
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
}
