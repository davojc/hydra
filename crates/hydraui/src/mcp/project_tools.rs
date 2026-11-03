//! The draft tools (Projects spec §10): a New project session writes the spec, milestones,
//! tickets, questions and links of its own draft, and nothing else.

use std::path::{Path, PathBuf};

use hydraui_projects::{
    Decision, DecisionMeta, Link, Milestone, MilestoneStatus, Ticket, TicketKind, TicketMeta,
    next_id, progress, repo,
};
use serde_json::{Value, json};

use super::registry::Registry;
use super::tools::Outcome;

/// A New project session's draft: its folder and who is drafting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub dir: PathBuf,
    /// The drafter's GitHub login: the lead, and who asked the draft's questions.
    pub login: String,
}

/// The tools this module answers, in the order `tools/list` shows them.
pub const NAMES: [&str; 9] = [
    "draft_show",
    "draft_set_spec",
    "draft_add_milestone",
    "draft_add_ticket",
    "draft_add_question",
    "draft_add_link",
    "draft_read_resource",
    "draft_edit",
    "draft_remove",
];

/// The `tools/list` entries for `NAMES`.
pub fn schemas() -> Vec<Value> {
    let s = |what: &str| json!({ "type": "string", "description": what });
    let tool = |name: &str, description: &str, properties: Value, required: Vec<&str>| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object", "properties": properties, "required": required},
        })
    };
    vec![
        tool(
            "draft_show",
            "New project: show the draft project as it stands: its name and key, the spec, \
             milestones, tickets, open questions and resources (attached files and links). \
             The user sees the same draft beside you, updated as you write.",
            json!({}),
            vec![],
        ),
        tool(
            "draft_set_spec",
            "New project: replace the draft's spec (Markdown). Write it with the user: why, \
             goals, non-goals, decisions and their reasons, in short numbered sections.",
            json!({"text": s("The whole spec, Markdown.")}),
            vec!["text"],
        ),
        tool(
            "draft_add_milestone",
            "New project: add a milestone at the end. Returns its id (M1, M2, ...).",
            json!({
                "title": s("The milestone, in a few words."),
                "target": s("Target date, YYYY-MM-DD (optional)."),
                "description": s("What is done when it is done (optional)."),
            }),
            vec!["title"],
        ),
        tool(
            "draft_add_ticket",
            "New project: add a ticket. Returns its id (the project key and a number).",
            json!({
                "title": s("The ticket, in one line."),
                "milestone": s("The milestone id it belongs to, e.g. M1."),
                "kind": s("feature, bug or chore (default feature)."),
                "description": s("What to do and how to tell it's done (Markdown, optional)."),
            }),
            vec!["title", "milestone"],
        ),
        tool(
            "draft_add_question",
            "New project: add an open question (a decision still to be made). Returns its id \
             (D1, D2, ...). Assign it to the GitHub login who should decide; the user by default.",
            json!({
                "question": s("The question."),
                "assigned_to": s("GitHub login of who decides (optional)."),
                "options": {"type": "array", "items": {"type": "string"},
                    "description": "Possible answers, if there are a few (optional)."},
            }),
            vec!["question"],
        ),
        tool(
            "draft_add_link",
            "New project: attach a link as a resource (a Figma file, a doc, a thread).",
            json!({"title": s("What it is."), "url": s("The link.")}),
            vec!["title", "url"],
        ),
        tool(
            "draft_read_resource",
            "New project: read an attached file by name (as draft_show lists it). Returns its \
             text, or for a file that isn't text (a PDF, an image) its path, to open with your \
             Read tool.",
            json!({"name": s("The file's name under resources/.")}),
            vec!["name"],
        ),
        tool(
            "draft_edit",
            "New project: change a milestone (M..), ticket (KEY-..) or question (D..). Only the \
             fields given change.",
            json!({
                "id": s("What to change."),
                "title": s("Milestone or ticket title."),
                "target": s("Milestone target date, YYYY-MM-DD."),
                "description": s("Milestone or ticket description."),
                "milestone": s("A ticket's milestone id."),
                "kind": s("A ticket's kind: feature, bug or chore."),
                "question": s("A question's text."),
                "assigned_to": s("A question's GitHub login."),
            }),
            vec!["id"],
        ),
        tool(
            "draft_remove",
            "New project: remove a milestone (only once no ticket is in it), ticket, question \
             or link (by its url).",
            json!({"id": s("The id, or a link's url.")}),
            vec!["id"],
        ),
    ]
}

/// Runs tool `name` for the terminal holding `token`; `None` for a name not in `NAMES`.
pub fn call(reg: &Registry, token: &str, name: &str, args: &Value) -> Option<Outcome> {
    if !NAMES.contains(&name) {
        return None;
    }
    let Some(draft) = reg.drafts.get(token) else {
        return Some(Outcome::err(
            "Only a New project session can use the draft tools: start one from hydraui's \
             Projects tab.",
        ));
    };
    Some(match apply(draft, name, args) {
        Ok(text) => Outcome::ok(text),
        Err(e) => Outcome::err(e),
    })
}

fn apply(draft: &Draft, name: &str, args: &Value) -> Result<String, String> {
    let arg = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let need = |key: &str| arg(key).ok_or_else(|| format!("{key} is required"));
    let dir = &draft.dir;
    let p = repo::load(dir).map_err(|e| e.to_string())?;
    match name {
        "draft_show" => Ok(show(dir, &p)),
        "draft_set_spec" => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            repo::write_spec(dir, text).map_err(|e| e.to_string())?;
            Ok("Spec saved.".into())
        }
        "draft_add_milestone" => {
            let id = format!(
                "M{}",
                p.milestones
                    .iter()
                    .filter_map(|m| m.id.strip_prefix('M')?.parse::<u32>().ok())
                    .max()
                    .unwrap_or(0)
                    + 1
            );
            let mut milestones = p.milestones.clone();
            milestones.push(Milestone {
                id: id.clone(),
                title: need("title")?,
                target: arg("target").map(|t| date(&t)).transpose()?,
                status: if milestones.is_empty() {
                    MilestoneStatus::Active
                } else {
                    MilestoneStatus::Planned
                },
                description: arg("description").unwrap_or_default(),
            });
            repo::write_milestones(dir, &milestones).map_err(|e| e.to_string())?;
            Ok(format!("Added {id}."))
        }
        "draft_add_ticket" => {
            let milestone = need("milestone")?;
            if !p.milestones.iter().any(|m| m.id == milestone) {
                return Err(format!("There's no milestone {milestone}; add it first."));
            }
            let id = next_id(&p.file.key, p.tickets.iter().map(|t| t.id.as_str()));
            let ticket = Ticket {
                id: id.clone(),
                meta: TicketMeta {
                    title: need("title")?,
                    kind: kind(arg("kind").as_deref())?,
                    status: Default::default(),
                    milestone,
                    assignee: String::new(),
                    created_by: draft.login.clone(),
                    created: Some(progress::now()),
                    extra: Default::default(),
                },
                body: arg("description").unwrap_or_default(),
            };
            repo::write_ticket(dir, &ticket).map_err(|e| e.to_string())?;
            Ok(format!("Added {id}."))
        }
        "draft_add_question" => {
            let id = next_id("D", p.decisions.iter().map(|d| d.id.as_str()));
            let options = args
                .get("options")
                .and_then(Value::as_array)
                .map(|o| {
                    o.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let decision = Decision {
                id: id.clone(),
                meta: DecisionMeta {
                    question: need("question")?,
                    asked_by: draft.login.clone(),
                    asked_at: Some(progress::now()),
                    assigned_to: arg("assigned_to").unwrap_or_else(|| draft.login.clone()),
                    options,
                    ticket: String::new(),
                    milestone: String::new(),
                    answer: String::new(),
                    decided_by: String::new(),
                    decided_at: None,
                    replaces: String::new(),
                    extra: Default::default(),
                },
                body: String::new(),
            };
            repo::write_decision(dir, &decision).map_err(|e| e.to_string())?;
            Ok(format!("Added {id}."))
        }
        "draft_add_link" => {
            let mut links = p.links.clone();
            links.push(Link {
                title: need("title")?,
                url: need("url")?,
            });
            repo::write_links(dir, &links).map_err(|e| e.to_string())?;
            Ok("Link added.".into())
        }
        "draft_read_resource" => read_resource(dir, &need("name")?),
        "draft_edit" => edit(dir, &p, &need("id")?, &arg),
        "draft_remove" => remove(dir, &p, &need("id")?),
        _ => Err(format!("unknown tool {name}")),
    }
}

fn edit(
    dir: &Path,
    p: &hydraui_projects::Project,
    id: &str,
    arg: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    if let Some(i) = p.milestones.iter().position(|m| m.id == id) {
        let mut milestones = p.milestones.clone();
        let m = &mut milestones[i];
        if let Some(v) = arg("title") {
            m.title = v;
        }
        if let Some(v) = arg("target") {
            m.target = Some(date(&v)?);
        }
        if let Some(v) = arg("description") {
            m.description = v;
        }
        repo::write_milestones(dir, &milestones).map_err(|e| e.to_string())?;
    } else if let Some(t) = p.tickets.iter().find(|t| t.id == id) {
        let mut t = t.clone();
        if let Some(v) = arg("title") {
            t.meta.title = v;
        }
        if let Some(v) = arg("description") {
            t.body = v;
        }
        if let Some(v) = arg("milestone") {
            if !p.milestones.iter().any(|m| m.id == v) {
                return Err(format!("There's no milestone {v}."));
            }
            t.meta.milestone = v;
        }
        if let Some(v) = arg("kind") {
            t.meta.kind = kind(Some(&v))?;
        }
        repo::write_ticket(dir, &t).map_err(|e| e.to_string())?;
    } else if let Some(d) = p.decisions.iter().find(|d| d.id == id) {
        let mut d = d.clone();
        if let Some(v) = arg("question") {
            d.meta.question = v;
        }
        if let Some(v) = arg("assigned_to") {
            d.meta.assigned_to = v;
        }
        repo::write_decision(dir, &d).map_err(|e| e.to_string())?;
    } else {
        return Err(format!("There's no {id} in the draft."));
    }
    Ok(format!("Changed {id}."))
}

fn remove(dir: &Path, p: &hydraui_projects::Project, id: &str) -> Result<String, String> {
    let gone = |path: PathBuf| std::fs::remove_file(path).map_err(|e| e.to_string());
    if p.milestones.iter().any(|m| m.id == id) {
        if p.tickets.iter().any(|t| t.meta.milestone == id) {
            return Err(format!(
                "{id} still has tickets; move or remove them first."
            ));
        }
        let milestones: Vec<_> = p
            .milestones
            .iter()
            .filter(|m| m.id != id)
            .cloned()
            .collect();
        repo::write_milestones(dir, &milestones).map_err(|e| e.to_string())?;
    } else if p.tickets.iter().any(|t| t.id == id) {
        gone(dir.join("tickets").join(format!("{id}.md")))?;
    } else if p.decisions.iter().any(|d| d.id == id) {
        gone(dir.join("decisions").join(format!("{id}.md")))?;
    } else if p.links.iter().any(|l| l.url == id) {
        let links: Vec<_> = p.links.iter().filter(|l| l.url != id).cloned().collect();
        repo::write_links(dir, &links).map_err(|e| e.to_string())?;
    } else {
        return Err(format!("There's no {id} in the draft."));
    }
    Ok(format!("Removed {id}."))
}

fn read_resource(dir: &Path, name: &str) -> Result<String, String> {
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return Err("Give the file's name as draft_show lists it.".into());
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

/// The draft as `draft_show` tells it.
fn show(dir: &Path, p: &hydraui_projects::Project) -> String {
    let mut out = format!(
        "Draft project \"{}\" (key {}, lead {}), in {}\n\n## Spec\n{}\n\n## Milestones\n",
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
    for m in &p.milestones {
        let target = m
            .target
            .map(|t| format!(", target {t}"))
            .unwrap_or_default();
        out.push_str(&format!("- {}: {}{target}\n", m.id, m.title));
        for t in p.tickets.iter().filter(|t| t.meta.milestone == m.id) {
            let kind = match t.meta.kind {
                TicketKind::Feature => "",
                TicketKind::Bug => " [bug]",
                TicketKind::Chore => " [chore]",
            };
            out.push_str(&format!("  - {}: {}{kind}\n", t.id, t.meta.title));
        }
    }
    out.push_str("\n## Open questions\n");
    for d in &p.decisions {
        out.push_str(&format!(
            "- {}: {} (for {})\n",
            d.id, d.meta.question, d.meta.assigned_to
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

fn date(text: &str) -> Result<toml::value::Datetime, String> {
    let d: toml::value::Datetime = text
        .parse()
        .map_err(|_| format!("{text} isn't a date like 2026-12-01"))?;
    if d.date.is_none() || d.time.is_some() {
        return Err(format!("{text} isn't a date like 2026-12-01"));
    }
    Ok(d)
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

    fn draft() -> (tempfile::TempDir, Registry) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("product-groups");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.toml"),
            "name = \"Product Groups\"\nkey = \"PG\"\nlead = \"me\"\n",
        )
        .unwrap();
        let mut reg = Registry::default();
        reg.drafts.insert(
            "tok".into(),
            Draft {
                dir,
                login: "me".into(),
            },
        );
        (tmp, reg)
    }

    fn run(reg: &Registry, name: &str, args: Value) -> Outcome {
        call(reg, "tok", name, &args).expect("a draft tool")
    }

    #[test]
    fn a_session_builds_a_draft() {
        let (_tmp, reg) = draft();
        assert!(!run(&reg, "draft_set_spec", json!({"text": "# PG\n"})).is_error);
        assert_eq!(
            run(
                &reg,
                "draft_add_milestone",
                json!({"title": "Model", "target": "2026-12-01"})
            )
            .text,
            "Added M1."
        );
        assert_eq!(
            run(
                &reg,
                "draft_add_ticket",
                json!({"title": "Schema", "milestone": "M1"})
            )
            .text,
            "Added PG-1."
        );
        assert_eq!(
            run(
                &reg,
                "draft_add_ticket",
                json!({"title": "Crash", "milestone": "M1", "kind": "bug"})
            )
            .text,
            "Added PG-2."
        );
        assert_eq!(
            run(
                &reg,
                "draft_add_question",
                json!({"question": "Postgres?", "options": ["yes", "no"]})
            )
            .text,
            "Added D-1."
        );
        assert!(
            !run(
                &reg,
                "draft_add_link",
                json!({"title": "Figma", "url": "https://figma.com/x"})
            )
            .is_error
        );
        let shown = run(&reg, "draft_show", json!({})).text;
        assert!(shown.contains("- M1: Model, target 2026-12-01"), "{shown}");
        assert!(shown.contains("  - PG-2: Crash [bug]"), "{shown}");
        assert!(shown.contains("- D-1: Postgres? (for me)"), "{shown}");
        assert!(
            shown.contains("- link: Figma <https://figma.com/x>"),
            "{shown}"
        );
    }

    #[test]
    fn edits_and_removals_check_what_they_name() {
        let (_tmp, reg) = draft();
        run(&reg, "draft_add_milestone", json!({"title": "Model"}));
        run(
            &reg,
            "draft_add_ticket",
            json!({"title": "Schema", "milestone": "M1"}),
        );
        assert!(
            run(
                &reg,
                "draft_add_ticket",
                json!({"title": "x", "milestone": "M9"})
            )
            .is_error
        );
        assert!(
            run(&reg, "draft_remove", json!({"id": "M1"})).is_error,
            "M1 has a ticket"
        );
        assert!(
            !run(
                &reg,
                "draft_edit",
                json!({"id": "PG-1", "title": "Schema v2"})
            )
            .is_error
        );
        assert!(run(&reg, "draft_edit", json!({"id": "M1", "target": "soon"})).is_error);
        assert!(!run(&reg, "draft_remove", json!({"id": "PG-1"})).is_error);
        assert!(!run(&reg, "draft_remove", json!({"id": "M1"})).is_error);
        assert!(
            run(&reg, "draft_show", json!({}))
                .text
                .contains("## Milestones\n\n")
        );
    }

    #[test]
    fn only_a_draft_session_may_call_them() {
        let (_tmp, reg) = draft();
        let out = call(&reg, "someone-else", "draft_show", &json!({})).unwrap();
        assert!(out.is_error);
        assert!(call(&reg, "tok", "tasks_read", &json!({})).is_none());
    }

    #[test]
    fn resources_are_read_by_name_only() {
        let (tmp, reg) = draft();
        let res = tmp.path().join("product-groups/resources");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("notes.md"), "kickoff").unwrap();
        std::fs::write(res.join("flow.png"), [0xff, 0xd8, 0x00]).unwrap();
        assert_eq!(
            run(&reg, "draft_read_resource", json!({"name": "notes.md"})).text,
            "kickoff"
        );
        assert!(
            run(&reg, "draft_read_resource", json!({"name": "flow.png"}))
                .text
                .contains("Read tool")
        );
        assert!(
            run(
                &reg,
                "draft_read_resource",
                json!({"name": "../project.toml"})
            )
            .is_error
        );
    }
}
