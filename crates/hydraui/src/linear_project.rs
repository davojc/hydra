//! What a project's requirements are in Linear (spec §14.7): Linear projects, or issues
//! with their sub-issues and comments. Read them whole, keep them as a snapshot in the
//! project folder (`linear.json`), and say what changed since the last snapshot, as
//! requirements for the planning session. Requests are built and replies read by pure
//! functions; [`fetch`] sends them, blocking, so call it off the UI thread.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::linear::{Http, LinearError, data};

/// The snapshot's file in a project folder.
pub const FILE: &str = "linear.json";

/// The most issue pages (of 100) read from one project.
const MAX_PAGES: usize = 20;

/// A Linear project as hydraui last read it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    pub url: String,
    /// When it was read, `YYYY-MM-DD HH:MM`.
    pub fetched_at: String,
    #[serde(default)]
    pub description: String,
    /// The project's overview text.
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub documents: Vec<Doc>,
    #[serde(default)]
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// `MAP-12`.
    pub identifier: String,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub description: String,
    /// Its comments, oldest first, as "Name: text" (read for linked issues and their
    /// sub-issues).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<String>,
}

/// A Linear link hydraui can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// A project, by its slug id.
    Project(String),
    /// An issue, by its identifier (`PRE-123`).
    Issue(String),
}

/// What a Linear link points at, or None when it's neither a project nor an issue.
pub fn link_of(url: &str) -> Option<Link> {
    if let Some(slug) = slug_of(url) {
        return Some(Link::Project(slug));
    }
    let rest = url.trim().split("linear.app/").nth(1)?;
    let mut parts = rest.split('/');
    let _workspace = parts.next()?;
    if parts.next()? != "issue" {
        return None;
    }
    let id = parts.next()?.split(['?', '#']).next()?.to_ascii_uppercase();
    let (team, n) = id.split_once('-')?;
    (!team.is_empty()
        && team.chars().all(|c| c.is_ascii_alphanumeric())
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit()))
    .then_some(Link::Issue(id))
}

/// The links in `text`: one or more, separated by spaces, commas or new lines. An error names
/// the first that isn't a Linear project or issue.
pub fn links_in(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.split(|c: char| c.is_whitespace() || c == ',') {
        if l.is_empty() {
            continue;
        }
        if link_of(l).is_none() {
            return Err(format!(
                "{l} isn't a Linear project or issue link: open it in Linear and copy its address."
            ));
        }
        if !out.iter().any(|o| o == l) {
            out.push(l.to_string());
        }
    }
    Ok(out)
}

/// The slug id in a Linear project URL (`…/project/mapping-ux-3b1f2c4d5e6f/overview` →
/// `3b1f2c4d5e6f`), or None when it isn't one.
pub fn slug_of(url: &str) -> Option<String> {
    let rest = url.trim().split("linear.app/").nth(1)?;
    let mut parts = rest.split('/');
    let _workspace = parts.next()?;
    if parts.next()? != "project" {
        return None;
    }
    let name = parts.next()?.split(['?', '#']).next()?;
    let slug = name.rsplit('-').next()?;
    (!slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric())).then(|| slug.to_string())
}

pub fn project_request(slug: &str) -> Value {
    json!({
        "query": "query($slug: String!) { projects(filter: {slugId: {eq: $slug}}, first: 1) \
                  { nodes { id name url description content \
                  documents(first: 50) { nodes { title url content } } } } }",
        "variables": {"slug": slug},
    })
}

const ISSUE_FIELDS: &str = "identifier title url description state { name } \
     comments(first: 50) { nodes { body createdAt user { name } } }";

/// An issue with its sub-issues, each with their comments.
pub fn issue_request(identifier: &str) -> Value {
    json!({
        "query": format!(
            "query($id: String!) {{ issue(id: $id) {{ {ISSUE_FIELDS} \
             children(first: 100) {{ nodes {{ {ISSUE_FIELDS} }} }} }} }}"
        ),
        "variables": {"id": identifier},
    })
}

fn read_one_issue(i: &Value) -> Issue {
    let mut comments: Vec<(String, String)> = i["comments"]["nodes"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    let who = c["user"]["name"].as_str().unwrap_or("Someone");
                    (
                        text(c, "createdAt"),
                        format!("{who}: {}", text(c, "body").trim()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    comments.sort();
    Issue {
        identifier: text(i, "identifier"),
        title: text(i, "title"),
        url: text(i, "url"),
        state: i["state"]["name"].as_str().unwrap_or_default().to_string(),
        description: text(i, "description"),
        comments: comments.into_iter().map(|(_, c)| c).collect(),
    }
}

/// The issue and its sub-issues; None when Linear has no such issue.
pub fn read_issue_tree(reply: &Value) -> Result<Option<Vec<Issue>>, LinearError> {
    let i = &data(reply)?["issue"];
    if i.is_null() {
        return Ok(None);
    }
    let mut out = vec![read_one_issue(i)];
    if let Some(children) = i["children"]["nodes"].as_array() {
        out.extend(children.iter().map(read_one_issue));
    }
    Ok(Some(out))
}

pub fn issues_request(project_id: &str, after: Option<&str>) -> Value {
    json!({
        "query": "query($id: ID!, $after: String) { issues(filter: {project: {id: {eq: $id}}}, \
                  first: 100, after: $after) { nodes { identifier title url description \
                  state { name } } pageInfo { hasNextPage endCursor } } }",
        "variables": {"id": project_id, "after": after},
    })
}

fn text(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The project's id and its snapshot without issues; None when Linear has no such project.
pub fn read_project(reply: &Value) -> Result<Option<(String, Snapshot)>, LinearError> {
    let Some(p) = data(reply)?["projects"]["nodes"]
        .as_array()
        .and_then(|n| n.first())
    else {
        return Ok(None);
    };
    let documents = p["documents"]["nodes"]
        .as_array()
        .map(|ds| {
            ds.iter()
                .map(|d| Doc {
                    title: text(d, "title"),
                    url: text(d, "url"),
                    content: text(d, "content"),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some((
        text(p, "id"),
        Snapshot {
            name: text(p, "name"),
            url: text(p, "url"),
            description: text(p, "description"),
            content: text(p, "content"),
            documents,
            ..Default::default()
        },
    )))
}

/// One page of issues, and the cursor of the next page when there is one.
pub fn read_issues(reply: &Value) -> Result<(Vec<Issue>, Option<String>), LinearError> {
    let issues = &data(reply)?["issues"];
    let nodes = issues["nodes"]
        .as_array()
        .ok_or_else(|| LinearError::Failed("Linear's reply had no issues".into()))?;
    let page = nodes.iter().map(read_one_issue).collect();
    let next = (issues["pageInfo"]["hasNextPage"].as_bool() == Some(true))
        .then(|| text(&issues["pageInfo"], "endCursor"))
        .filter(|c| !c.is_empty());
    Ok((page, next))
}

/// Reads every link in `links` whole, as of `now` (`YYYY-MM-DD HH:MM`), into one snapshot.
pub fn fetch(api: &Http, links: &[String], now: &str) -> Result<Snapshot, LinearError> {
    let mut all = Snapshot {
        fetched_at: now.to_string(),
        ..Default::default()
    };
    for url in links {
        let one = match link_of(url) {
            Some(Link::Project(slug)) => fetch_project(api, &slug)?,
            Some(Link::Issue(id)) => {
                let issues =
                    read_issue_tree(&api.post(&issue_request(&id))?)?.ok_or_else(|| {
                        LinearError::Failed(format!(
                            "Linear has no issue {id}, or this key can't see it"
                        ))
                    })?;
                Snapshot {
                    name: format!("{id} {}", issues[0].title),
                    url: url.clone(),
                    issues,
                    ..Default::default()
                }
            }
            None => {
                return Err(LinearError::Failed(format!(
                    "{url} isn't a Linear project or issue link"
                )));
            }
        };
        all.merge(one);
    }
    Ok(all.sorted())
}

fn fetch_project(api: &Http, slug: &str) -> Result<Snapshot, LinearError> {
    let (id, mut snap) = read_project(&api.post(&project_request(slug))?)?.ok_or_else(|| {
        LinearError::Failed(format!(
            "Linear has no project {slug}, or this key can't see it"
        ))
    })?;
    let mut after = None;
    for _ in 0..MAX_PAGES {
        let (page, next) = read_issues(&api.post(&issues_request(&id, after.as_deref()))?)?;
        snap.issues.extend(page);
        match next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    Ok(snap)
}

impl Snapshot {
    /// Adds another link's read: names and links joined, documents and issues added once.
    pub fn merge(&mut self, other: Snapshot) {
        let join = |a: &mut String, b: &str| {
            if !b.is_empty() {
                if !a.is_empty() {
                    a.push_str(" + ");
                }
                a.push_str(b);
            }
        };
        join(&mut self.name, &other.name);
        join(&mut self.url, &other.url);
        let para = |a: &mut String, b: &str| {
            if !b.trim().is_empty() {
                if !a.is_empty() {
                    a.push_str("\n\n");
                }
                a.push_str(b.trim());
            }
        };
        para(&mut self.description, &other.description);
        para(&mut self.content, &other.content);
        for d in other.documents {
            if !self.documents.iter().any(|o| o.url == d.url) {
                self.documents.push(d);
            }
        }
        for i in other.issues {
            if !self.issues.iter().any(|o| o.identifier == i.identifier) {
                self.issues.push(i);
            }
        }
    }

    /// Issues by number and documents by title, so two reads compare line for line.
    pub fn sorted(mut self) -> Self {
        let number = |i: &Issue| {
            i.identifier
                .rsplit('-')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        };
        self.issues.sort_by(|a, b| {
            number(a)
                .cmp(&number(b))
                .then(a.identifier.cmp(&b.identifier))
        });
        self.documents.sort_by(|a, b| a.title.cmp(&b.title));
        self
    }
}

/// The snapshot in project folder `dir`, if there is one.
pub fn load(dir: &std::path::Path) -> Option<Snapshot> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(FILE)).ok()?).ok()
}

pub fn save(dir: &std::path::Path, snap: &Snapshot) -> Result<(), String> {
    let text = serde_json::to_string_pretty(snap).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(FILE), format!("{text}\n")).map_err(|e| e.to_string())
}

fn issue_block(i: &Issue) -> String {
    let body = i.description.trim();
    let mut out = format!(
        "### {} · {} ({})\n{}\n\n{}\n",
        i.identifier,
        i.title,
        i.state,
        i.url,
        if body.is_empty() {
            "(no description)"
        } else {
            body
        }
    );
    if !i.comments.is_empty() {
        out.push_str("\nComments:\n");
        for c in &i.comments {
            out.push_str(&format!("- {}\n", c.replace('\n', "\n  ")));
        }
    }
    out
}

/// The whole project as requirements, for the first read: the plan is compared against it.
pub fn render_all(s: &Snapshot) -> String {
    let mut out = format!(
        "# Linear: \"{}\", as of {}\n{}\n\n\
         What Linear has now. Compare it with the plan: what does it ask for that \
         the plan doesn't have, or differently?\n",
        s.name, s.fetched_at, s.url
    );
    if !s.description.trim().is_empty() || !s.content.trim().is_empty() {
        out.push_str(&format!(
            "\n## Overview\n{}\n\n{}\n",
            s.description.trim(),
            s.content.trim()
        ));
    }
    for d in &s.documents {
        out.push_str(&format!(
            "\n## Document: {}\n{}\n\n{}\n",
            d.title,
            d.url,
            d.content.trim()
        ));
    }
    out.push_str("\n## Issues\n");
    for i in &s.issues {
        out.push('\n');
        out.push_str(&issue_block(i));
    }
    out
}

/// A text's change as `-`/`+` lines in a fenced block, unchanged lines kept for context.
fn text_diff(old: &str, new: &str) -> String {
    let mut out = String::from("```diff\n");
    for d in crate::projects::line_diff(old.trim(), new.trim()) {
        let (mark, line) = match d {
            crate::projects::Diff::Same(l) => (' ', l),
            crate::projects::Diff::Gone(l) => ('-', l),
            crate::projects::Diff::Added(l) => ('+', l),
        };
        out.push(mark);
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

/// What changed in Linear between two reads, as requirements; None when nothing did.
pub fn changes(old: &Snapshot, new: &Snapshot) -> Option<String> {
    let mut sections = Vec::new();
    if old.description.trim() != new.description.trim() || old.content.trim() != new.content.trim()
    {
        sections.push(format!(
            "## Overview changed\n{}",
            text_diff(
                &format!("{}\n\n{}", old.description.trim(), old.content.trim()),
                &format!("{}\n\n{}", new.description.trim(), new.content.trim()),
            )
        ));
    }
    for d in &new.documents {
        match old
            .documents
            .iter()
            .find(|o| o.url == d.url || o.title == d.title)
        {
            None => sections.push(format!(
                "## New document: {}\n{}\n\n{}\n",
                d.title,
                d.url,
                d.content.trim()
            )),
            Some(o) if o.content.trim() != d.content.trim() => sections.push(format!(
                "## Document changed: {}\n{}\n\n{}",
                d.title,
                d.url,
                text_diff(&o.content, &d.content)
            )),
            Some(_) => {}
        }
    }
    for i in &new.issues {
        let Some(o) = old.issues.iter().find(|o| o.identifier == i.identifier) else {
            sections.push(format!("## New issue\n{}", issue_block(i)));
            continue;
        };
        let mut what = Vec::new();
        if o.title != i.title {
            what.push(format!("Title: \"{}\" → \"{}\"\n", o.title, i.title));
        }
        if o.state != i.state {
            what.push(format!("State: {} → {}\n", o.state, i.state));
        }
        if o.description.trim() != i.description.trim() {
            what.push(format!(
                "Description:\n{}",
                text_diff(&o.description, &i.description)
            ));
        }
        let new_comments: Vec<&String> = i
            .comments
            .iter()
            .filter(|c| !o.comments.contains(c))
            .collect();
        if !new_comments.is_empty() {
            what.push(format!(
                "New comments:\n{}",
                new_comments
                    .iter()
                    .map(|c| format!("- {}\n", c.replace('\n', "\n  ")))
                    .collect::<String>()
            ));
        }
        if !what.is_empty() {
            sections.push(format!(
                "## Changed: {} · {}\n{}\n\n{}",
                i.identifier,
                i.title,
                i.url,
                what.join("\n")
            ));
        }
    }
    for o in &old.issues {
        if !new.issues.iter().any(|i| i.identifier == o.identifier) {
            sections.push(format!(
                "## No longer there: {} · {}\n{}\n",
                o.identifier, o.title, o.url
            ));
        }
    }
    if sections.is_empty() {
        return None;
    }
    Some(format!(
        "# What changed in Linear: \"{}\", {} to {}\n{}\n\n\
         Removed lines start with -, added ones with +.\n\n{}",
        new.name,
        old.fetched_at,
        new.fetched_at,
        new.url,
        sections.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(id: &str, title: &str, state: &str, desc: &str) -> Issue {
        Issue {
            identifier: id.into(),
            title: title.into(),
            url: format!("https://linear.app/iov42/issue/{id}"),
            state: state.into(),
            description: desc.into(),
            comments: vec![],
        }
    }

    fn snap(at: &str, issues: Vec<Issue>) -> Snapshot {
        Snapshot {
            name: "Mapping UX".into(),
            url: "https://linear.app/iov42/project/mapping-ux-3b1f2c4d5e6f".into(),
            fetched_at: at.into(),
            issues,
            ..Default::default()
        }
    }

    #[test]
    fn a_project_link_gives_its_slug_id() {
        for url in [
            "https://linear.app/iov42/project/mapping-ux-enhancements-3b1f2c4d5e6f/overview",
            "https://linear.app/iov42/project/mapping-ux-enhancements-3b1f2c4d5e6f",
            " linear.app/iov42/project/mapping-ux-enhancements-3b1f2c4d5e6f?x=1 ",
        ] {
            assert_eq!(slug_of(url).as_deref(), Some("3b1f2c4d5e6f"), "{url}");
        }
        assert_eq!(slug_of("https://linear.app/iov42/issue/MAP-1/x"), None);
        assert_eq!(slug_of("https://example.com/project/x-1"), None);
    }

    #[test]
    fn replies_are_read_into_a_snapshot() {
        let reply = json!({"data": {"projects": {"nodes": [{
            "id": "p1", "name": "Mapping UX", "url": "u", "description": "d", "content": "c",
            "documents": {"nodes": [{"title": "Spec", "url": "du", "content": "spec text"}]},
        }]}}});
        let (id, s) = read_project(&reply).unwrap().unwrap();
        assert_eq!(
            (
                id.as_str(),
                s.name.as_str(),
                s.documents[0].content.as_str()
            ),
            ("p1", "Mapping UX", "spec text")
        );
        assert_eq!(
            read_project(&json!({"data": {"projects": {"nodes": []}}})).unwrap(),
            None
        );
        let page = json!({"data": {"issues": {
            "nodes": [{"identifier": "MAP-2", "title": "t", "url": "u", "description": null, "state": {"name": "Todo"}}],
            "pageInfo": {"hasNextPage": true, "endCursor": "c2"},
        }}});
        let (issues, next) = read_issues(&page).unwrap();
        assert_eq!(
            (issues[0].state.as_str(), issues[0].description.as_str()),
            ("Todo", "")
        );
        assert_eq!(next.as_deref(), Some("c2"));
        let last = json!({"data": {"issues": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": "c3"}}}});
        assert_eq!(read_issues(&last).unwrap().1, None);
        assert!(read_issues(&json!({"errors": [{"message": "boom"}]})).is_err());
        // The issues request pages on.
        assert_eq!(issues_request("p1", Some("c2"))["variables"]["after"], "c2");
    }

    #[test]
    fn issue_links_are_read_too() {
        assert_eq!(
            link_of("https://linear.app/iov42/issue/pre-1840/mapping-ux-tweaks"),
            Some(Link::Issue("PRE-1840".into()))
        );
        assert_eq!(
            link_of("https://linear.app/iov42/project/mapping-3b1f2c4d5e6f/overview"),
            Some(Link::Project("3b1f2c4d5e6f".into()))
        );
        assert_eq!(link_of("https://linear.app/iov42/issue/nope/x"), None);
        let two = links_in(
            "https://linear.app/iov42/issue/PRE-1/a,\nhttps://linear.app/iov42/issue/PRE-2 \
             https://linear.app/iov42/issue/PRE-1/a",
        )
        .unwrap();
        assert_eq!(two.len(), 2);
        assert!(links_in("https://linear.app/iov42/document/spec-aa6b5b292db0").is_err());
        let reply = json!({"data": {"issue": {
            "identifier": "PRE-1840", "title": "Mapping tweaks", "url": "u", "description": "d",
            "state": {"name": "Todo"},
            "comments": {"nodes": [
                {"body": "Later", "createdAt": "2026-11-05T10:00:00Z", "user": {"name": "Josh"}},
                {"body": "First", "createdAt": "2026-11-04T10:00:00Z", "user": {"name": "Jordan"}},
            ]},
            "children": {"nodes": [{"identifier": "PRE-1841", "title": "Sub", "url": "u2",
                "description": null, "state": {"name": "Done"}, "comments": {"nodes": []}}]},
        }}});
        let issues = read_issue_tree(&reply).unwrap().unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].comments, ["Jordan: First", "Josh: Later"]);
        assert_eq!(
            read_issue_tree(&json!({"data": {"issue": null}})).unwrap(),
            None
        );
    }

    #[test]
    fn a_new_comment_is_a_change() {
        let mut a = issue("PRE-1", "t", "Todo", "d");
        let old = snap("t1", vec![a.clone()]);
        a.comments.push("Josh: also export to PDF".into());
        let c = changes(&old, &snap("t2", vec![a])).unwrap();
        assert!(
            c.contains("New comments:\n- Josh: also export to PDF"),
            "{c}"
        );
    }

    #[test]
    fn several_reads_merge_into_one() {
        let mut a = snap("t", vec![issue("PRE-1", "", "", "")]);
        a.name = "A".into();
        let mut b = snap(
            "t",
            vec![issue("PRE-1", "", "", ""), issue("PRE-2", "", "", "")],
        );
        b.name = "B".into();
        a.merge(b);
        assert_eq!((a.name.as_str(), a.issues.len()), ("A + B", 2));
    }

    #[test]
    fn issues_sort_by_number_so_reads_compare() {
        let s = snap(
            "t",
            vec![issue("MAP-10", "", "", ""), issue("MAP-9", "", "", "")],
        )
        .sorted();
        assert_eq!(s.issues[0].identifier, "MAP-9");
    }

    #[test]
    fn changes_name_new_edited_and_removed_issues() {
        let old = snap(
            "2026-11-04 12:30",
            vec![
                issue("MAP-1", "Upload", "Todo", "Upload a CSV.\nShow errors."),
                issue("MAP-2", "Map", "Todo", "Map columns."),
                issue("MAP-3", "Old", "Todo", ""),
            ],
        );
        assert_eq!(changes(&old, &old), None);
        let new = snap(
            "2026-11-05 09:00",
            vec![
                issue(
                    "MAP-1",
                    "Upload",
                    "Todo",
                    "Upload a CSV or XLSX.\nShow errors.",
                ),
                issue("MAP-2", "Map fields", "In Progress", "Map columns."),
                issue("MAP-4", "Bulk edit", "Todo", "Edit many rows."),
            ],
        );
        let c = changes(&old, &new).unwrap();
        assert!(c.contains("## Changed: MAP-1"), "{c}");
        assert!(
            c.contains("-Upload a CSV.\n+Upload a CSV or XLSX.\n Show errors."),
            "{c}"
        );
        assert!(
            c.contains("Title: \"Map\" → \"Map fields\"")
                && c.contains("State: Todo → In Progress")
        );
        assert!(c.contains("## New issue\n### MAP-4 · Bulk edit"));
        assert!(c.contains("## No longer there: MAP-3"));
        assert!(c.contains("2026-11-04 12:30 to 2026-11-05 09:00"));
    }

    #[test]
    fn the_first_read_is_the_whole_project() {
        let mut s = snap(
            "2026-11-05 09:00",
            vec![issue("MAP-1", "Upload", "Todo", "")],
        );
        s.documents.push(Doc {
            title: "Spec".into(),
            url: "du".into(),
            content: "The spec.".into(),
        });
        let all = render_all(&s);
        assert!(all.contains("### MAP-1 · Upload (Todo)") && all.contains("(no description)"));
        assert!(all.contains("## Document: Spec") && all.contains("Compare it with the plan"));
    }

    #[test]
    fn a_snapshot_is_kept_in_the_project_folder() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()), None);
        let s = snap("t", vec![issue("MAP-1", "a", "b", "c")]);
        save(tmp.path(), &s).unwrap();
        assert_eq!(load(tmp.path()), Some(s));
    }
}
