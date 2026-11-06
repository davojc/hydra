//! Projects (docs/superpowers/specs/2026-11-03-hydraui-projects-design.md), apart from the
//! view: hydra's checkout of an environment's projects repo, the edits the tab makes, and what
//! waits on whom. Slice 1 leaves edits uncommitted in the checkout: the pending change is the
//! working tree's diff (plan, slice 1).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hydraui_projects::{
    Decision, DecisionMeta, Project, ProjectFile, Review, Ticket, TicketKind, TicketMeta,
    TicketStatus, next_id, progress, repo,
};

/// How long a clone or pull may take before it's given up on.
const NETWORK_LIMIT: Duration = Duration::from_secs(120);

/// Where each environment's projects repo is set: `<HYDRA_HOME>/hydraui/projects.toml`,
/// `[repos] <env> = "owner/name"`. The spec puts it in env.toml (§3); the prototype keeps it
/// here, because a hydra that doesn't know `[projects]` refuses an env.toml that has it.
pub fn config_file(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("projects.toml")
}

/// `env`'s projects repo, if one is set.
pub fn configured_repo(hydra_root: &Path, env: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_file(hydra_root)).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    table
        .get("repos")?
        .get(env)?
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
}

/// Sets `env`'s projects repo, or with `None` removes it, keeping the rest of the file.
pub fn set_configured_repo(hydra_root: &Path, env: &str, repo: Option<&str>) -> Result<(), String> {
    let path = config_file(hydra_root);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !doc.contains_key("repos") {
        doc.insert("repos", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let repos = doc
        .get_mut("repos")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or("repos in projects.toml isn't a table")?;
    match repo.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => {
            repos.insert(env, toml_edit::value(r));
        }
        None => {
            repos.remove(env);
        }
    }
    hydraui_sets::layout::write_atomic(&path, &doc.to_string()).map_err(|e| e.to_string())
}

/// Whether `repo` names a GitHub repo (`owner/name`) rather than a local path.
pub fn is_github(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && !p.starts_with('.')
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// Where hydra keeps its checkout of `repo` for an environment whose state folder is `state`:
/// `state/<env>/projects/<owner>-<name>` (spec §3), or the folder's name for a local path.
pub fn checkout_dir(state: &Path, repo: &str) -> PathBuf {
    let name = if is_github(repo) {
        repo.replace('/', "-")
    } else {
        Path::new(repo.trim_end_matches(['/', '\\']))
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("projects")
            .to_string()
    };
    state.join("projects").join(name)
}

/// Where New project keeps its drafts, outside the checkout so nothing is sent before Create.
pub fn drafts_dir(state: &Path) -> PathBuf {
    state.join("project-drafts")
}

fn clone_source(repo: &str) -> String {
    if is_github(repo) {
        format!("https://github.com/{repo}.git")
    } else {
        repo.to_string()
    }
}

/// Runs `git args` as environment `env` (`hydra run <env> -- git`), so GitHub sees its login.
fn env_git(hydra: &Path, env: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new(hydra);
    cmd.args(["run", env, "--", "git"]).args(args);
    hydra_platform::process::hide_window(&mut cmd);
    let out = crate::branch_rule::output_within(cmd, NETWORK_LIMIT)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            err
        })
    }
}

/// Clones `repo` into `dir` if it isn't there yet, else brings it up to date, keeping
/// uncommitted edits. An empty repo is fine: there's nothing to pull yet. Blocks.
pub fn sync_checkout(hydra: &Path, env: &str, repo: &str, dir: &Path) -> Result<(), String> {
    let path = dir.display().to_string();
    if !dir.join(".git").exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        env_git(hydra, env, &["clone", &clone_source(repo), &path])?;
        ignore_os_files(dir);
        return Ok(());
    }
    ignore_os_files(dir);
    let r = crate::project_send::EnvRunner {
        hydra,
        env,
        cwd: dir,
    };
    // Nothing on the remote yet (a new repo): no branch to come up to.
    let Some(default) = crate::project_send::default_branch(&r, dir)? else {
        return Ok(());
    };
    env_git(hydra, env, &["-C", &path, "fetch", "origin", "--prune"])?;
    // Not a plain pull: a file sent and merged is pending here too, and would clash with
    // the same file coming from the repo.
    crate::project_send::settle(&r, dir, &default)
}

/// Keeps files the OS drops in folders (Finder's `.DS_Store`, Windows' `Thumbs.db`) out of
/// the checkout's changes, in its local exclude list: never in the repo's own `.gitignore`.
fn ignore_os_files(dir: &Path) {
    let exclude = dir.join(".git").join("info").join("exclude");
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    let missing: Vec<&str> = [".DS_Store", "Thumbs.db", "desktop.ini"]
        .into_iter()
        .filter(|name| !text.lines().any(|l| l.trim() == *name))
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for name in missing {
        text.push_str(name);
        text.push('\n');
    }
    if let Some(parent) = exclude.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(exclude, text);
}

/// Project `slug`'s folder on GitHub, for a GitHub repo (`HEAD` is its default branch).
pub fn github_url(repo: &str, slug: &str) -> Option<String> {
    is_github(repo).then(|| format!("https://github.com/{repo}/tree/HEAD/{slug}"))
}

/// The projects in checkout `dir` that are in the repo already, not only on this machine: their
/// `project.toml` is tracked. Blocks.
pub fn sent_projects(dir: &Path) -> BTreeSet<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["ls-files", "-z", "--", "*/project.toml"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let Ok(out) = hydra_platform::process::hide_window(&mut cmd).output() else {
        return BTreeSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter_map(|p| p.strip_suffix("/project.toml"))
        .filter(|slug| !slug.contains('/'))
        .map(str::to_string)
        .collect()
}

/// The files changed in `dir` and not sent yet, relative to it with `/`. Blocks.
pub fn pending(dir: &Path) -> BTreeSet<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["status", "--porcelain", "-uall", "-z"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let Ok(out) = hydra_platform::process::hide_window(&mut cmd).output() else {
        return BTreeSet::new();
    };
    parse_status(&String::from_utf8_lossy(&out.stdout))
}

/// The paths in `git status --porcelain -z` output (a rename's new path).
pub(crate) fn parse_status(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut entries = text.split('\0');
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
        out.insert(entry[3..].to_string());
    }
    out
}

/// Whether `ticket` in project `slug` has a change not sent yet.
pub fn ticket_unsent(pending: &BTreeSet<String>, slug: &str, ticket: &str) -> bool {
    pending.contains(&format!("{slug}/tickets/{ticket}.md"))
}

/// Whether anything in project `slug` waits to be sent.
pub fn project_unsent(pending: &BTreeSet<String>, slug: &str) -> usize {
    let prefix = format!("{slug}/");
    pending.iter().filter(|p| p.starts_with(&prefix)).count()
}

/// The open questions across `projects` assigned to `login`, oldest first (spec §8.4).
pub fn questions_for<'a>(login: &str, projects: &'a [Project]) -> Vec<(&'a Project, &'a Decision)> {
    let mut out: Vec<_> = projects
        .iter()
        .flat_map(|p| p.decisions.iter().map(move |d| (p, d)))
        .filter(|(_, d)| d.is_open() && d.meta.assigned_to.eq_ignore_ascii_case(login))
        .collect();
    out.sort_by_key(|(_, d)| d.meta.asked_at.map(|t| t.to_string()).unwrap_or_default());
    out
}

/// Records `answer` to `question` in project folder `dir`, by `login`, now.
pub fn answer(dir: &Path, question: &Decision, answer: &str, login: &str) -> Result<(), String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Err("Write an answer first.".into());
    }
    let mut d = question.clone();
    d.meta.answer = answer.to_string();
    d.meta.decided_by = login.to_string();
    d.meta.decided_at = Some(progress::now());
    repo::write_decision(dir, &d).map_err(|e| e.to_string())
}

/// Asks `question` in project `p` (folder `dir`), for `assigned_to`; returns its id.
pub fn ask(
    dir: &Path,
    p: &Project,
    question: &str,
    assigned_to: &str,
    login: &str,
) -> Result<String, String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("Say what needs deciding first.".into());
    }
    let id = next_id("D", p.decisions.iter().map(|d| d.id.as_str()));
    let d = Decision {
        id: id.clone(),
        meta: DecisionMeta {
            question: question.to_string(),
            asked_by: login.to_string(),
            asked_at: Some(progress::now()),
            assigned_to: match assigned_to.trim() {
                "" => p.file.lead.clone(),
                who => who.trim_start_matches('@').to_string(),
            },
            options: Vec::new(),
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
    repo::write_decision(dir, &d).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Adds a ticket to milestone `milestone` of project `p`; returns its id.
pub fn new_ticket(
    dir: &Path,
    p: &Project,
    title: &str,
    milestone: &str,
    kind: TicketKind,
    login: &str,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Give the ticket a title.".into());
    }
    let id = next_id(&p.file.key, p.tickets.iter().map(|t| t.id.as_str()));
    let t = Ticket {
        id: id.clone(),
        meta: TicketMeta {
            title: title.to_string(),
            kind,
            status: TicketStatus::Todo,
            milestone: milestone.to_string(),
            assignee: String::new(),
            created_by: login.to_string(),
            created: Some(progress::now()),
            ..Default::default()
        },
        body: String::new(),
    };
    repo::write_ticket(dir, &t).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Attaches link `url` to the project or draft in `dir`, titled `title` (the link itself when
/// empty). Only web links.
pub fn add_link(dir: &Path, url: &str, title: &str) -> Result<(), String> {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Give a web link, starting https://".into());
    }
    let title = match title.trim() {
        "" => url,
        t => t,
    };
    let mut links = repo::load(dir).map_err(|e| e.to_string())?.links;
    links.push(hydraui_projects::Link {
        title: title.to_string(),
        url: url.to_string(),
    });
    repo::write_links(dir, &links).map_err(|e| e.to_string())
}

/// Moves `ticket` to `status`.
pub fn move_ticket(dir: &Path, ticket: &Ticket, status: TicketStatus) -> Result<(), String> {
    let mut t = ticket.clone();
    t.meta.status = status;
    repo::write_ticket(dir, &t).map_err(|e| e.to_string())
}

/// Sets who works on `ticket` (empty for nobody).
pub fn assign_ticket(dir: &Path, ticket: &Ticket, assignee: &str) -> Result<(), String> {
    let mut t = ticket.clone();
    t.meta.assignee = assignee.trim().trim_start_matches('@').to_string();
    repo::write_ticket(dir, &t).map_err(|e| e.to_string())
}

/// Makes project `name` in checkout `checkout`, led by `login` (spec §14): just its
/// `project.toml`, for resources to be attached before planning starts. Returns its slug.
pub fn new_project(checkout: &Path, name: &str, login: &str) -> Result<String, String> {
    let name = name.trim();
    let slug = repo::slug_for(name);
    if slug.is_empty() {
        return Err("Give the project a name with letters or numbers in it.".into());
    }
    if checkout.join(&slug).exists() {
        return Err(format!("The repo already has a project folder {slug}."));
    }
    let key = repo::key_for(name);
    let mut key_used = key.clone();
    let taken: Vec<String> = repo::load_all(checkout)
        .into_iter()
        .filter_map(|(_, p)| p.ok().map(|p| p.file.key.to_ascii_uppercase()))
        .collect();
    let mut n = 2;
    while taken.contains(&key_used) {
        key_used = format!("{key}{n}");
        n += 1;
    }
    let file = ProjectFile {
        name: name.to_string(),
        key: key_used,
        lead: login.to_string(),
        members: vec![login.to_string()],
        status: Default::default(),
        repos: Vec::new(),
        suggested_repos: Vec::new(),
        dismissed_repos: Vec::new(),
        spec_change: None,
        spec_note: String::new(),
        linear: Vec::new(),
        sync: Default::default(),
        extra: Default::default(),
    };
    repo::write_project_file(&checkout.join(&slug), &file).map_err(|e| e.to_string())?;
    Ok(slug)
}

/// How a milestone is shown to people: `M3` is "Milestone 3". Files and Claude keep `M3`.
pub fn milestone_name(id: &str) -> String {
    match id.strip_prefix('M') {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
            format!("Milestone {n}")
        }
        _ => id.to_string(),
    }
}

/// How a decision is shown to people: `D-7` is "Decision 7". Files and Claude keep `D-7`.
pub fn decision_name(id: &str) -> String {
    match id.strip_prefix("D-") {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
            format!("Decision {n}")
        }
        _ => id.to_string(),
    }
}

/// A code repo a project changes, with the live tickets in it and, for one the planning session
/// suggested, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoUse {
    pub repo: String,
    pub tickets: Vec<String>,
    pub why: String,
}

/// `text` as an `owner/name` repo, from a name or a GitHub URL.
pub fn repo_name(text: &str) -> Result<String, String> {
    let t = text.trim().trim_end_matches('/');
    let t = t
        .strip_prefix("https://github.com/")
        .or_else(|| t.strip_prefix("git@github.com:"))
        .unwrap_or(t);
    let t = t.strip_suffix(".git").unwrap_or(t);
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match t.split_once('/') {
        Some((owner, name)) if ok(owner) && ok(name) => Ok(format!("{owner}/{name}")),
        _ => Err(format!(
            "\"{}\" isn't a repo: write it as owner/name.",
            text.trim()
        )),
    }
}

/// The repos project `p` changes: those the lead confirmed, and those suggested (named by a live
/// ticket, or by the planning session and not dismissed) but not confirmed yet. Each with the
/// live tickets in it, in id order.
pub fn repos_of(p: &Project) -> (Vec<RepoUse>, Vec<RepoUse>) {
    let live = |t: &&Ticket| t.meta.review != Review::Rejected && !t.meta.repo.is_empty();
    let tickets_in = |repo: &str| -> Vec<String> {
        let mut ids: Vec<String> = p
            .tickets
            .iter()
            .filter(live)
            .filter(|t| t.meta.repo.eq_ignore_ascii_case(repo))
            .map(|t| t.id.clone())
            .collect();
        ids.sort_by_key(|id| crate_number(id));
        ids
    };
    let confirmed: Vec<RepoUse> = p
        .file
        .repos
        .iter()
        .map(|r| RepoUse {
            repo: r.clone(),
            tickets: tickets_in(r),
            why: String::new(),
        })
        .collect();
    let known = |r: &str| p.file.repos.iter().any(|c| c.eq_ignore_ascii_case(r));
    let mut suggested: BTreeMap<String, RepoUse> = BTreeMap::new();
    for s in &p.file.suggested_repos {
        if known(&s.repo)
            || p.file
                .dismissed_repos
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&s.repo))
        {
            continue;
        }
        suggested.insert(
            s.repo.to_ascii_lowercase(),
            RepoUse {
                repo: s.repo.clone(),
                tickets: Vec::new(),
                why: s.why.clone(),
            },
        );
    }
    for t in p.tickets.iter().filter(live) {
        if known(&t.meta.repo) {
            continue;
        }
        suggested
            .entry(t.meta.repo.to_ascii_lowercase())
            .or_insert_with(|| RepoUse {
                repo: t.meta.repo.clone(),
                tickets: Vec::new(),
                why: String::new(),
            });
    }
    let suggested = suggested
        .into_values()
        .map(|mut r| {
            r.tickets = tickets_in(&r.repo);
            r
        })
        .collect();
    (confirmed, suggested)
}

fn crate_number(id: &str) -> u32 {
    hydraui_projects::number_of(id).unwrap_or(u32::MAX)
}

/// The repos the progress check watches for project `p` (spec §6.1): the confirmed ones and
/// every repo a live ticket names, sorted, once.
pub fn check_repos(p: &Project) -> Vec<String> {
    let mut out: BTreeSet<String> = p.file.repos.iter().cloned().collect();
    for t in &p.tickets {
        if t.meta.review != Review::Rejected && !t.meta.repo.is_empty() {
            out.insert(t.meta.repo.clone());
        }
    }
    out.into_iter().collect()
}

/// Confirms `repo` (a name or URL) as one project `p` changes.
pub fn add_repo(dir: &Path, p: &Project, repo: &str) -> Result<(), String> {
    let repo = repo_name(repo)?;
    let mut file = p.file.clone();
    if !file.repos.iter().any(|r| r.eq_ignore_ascii_case(&repo)) {
        file.repos.push(repo.clone());
    }
    file.suggested_repos
        .retain(|s| !s.repo.eq_ignore_ascii_case(&repo));
    file.dismissed_repos
        .retain(|d| !d.eq_ignore_ascii_case(&repo));
    repo::write_project_file(dir, &file).map_err(|e| e.to_string())
}

/// Dismisses the planning session's suggestion of `repo`, so it isn't suggested again. A repo a
/// live ticket names stays suggested: the ticket says the work is there.
pub fn dismiss_repo(dir: &Path, p: &Project, repo: &str) -> Result<(), String> {
    let mut file = p.file.clone();
    file.suggested_repos
        .retain(|s| !s.repo.eq_ignore_ascii_case(repo));
    if !file
        .dismissed_repos
        .iter()
        .any(|d| d.eq_ignore_ascii_case(repo))
    {
        file.dismissed_repos.push(repo.to_string());
    }
    repo::write_project_file(dir, &file).map_err(|e| e.to_string())
}

/// A milestone or ticket, by id (spec §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Milestone(String),
    Ticket(String),
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::Milestone(id) | Item::Ticket(id) => id,
        }
    }

    /// How it's shown to people: "Milestone 3", or the ticket's key.
    pub fn name(&self) -> String {
        match self {
            Item::Milestone(id) => milestone_name(id),
            Item::Ticket(id) => id.clone(),
        }
    }
}

/// Records the lead's review of `item` in project folder `dir`: approved, changes asked with
/// `note`, or rejected with `note` as the reason.
pub fn review(dir: &Path, item: &Item, to: Review, note: &str, login: &str) -> Result<(), String> {
    let note = note.trim();
    if matches!(to, Review::Changes | Review::Rejected) && note.is_empty() {
        return Err(match to {
            Review::Rejected => "Say why, so Claude doesn't propose it again.".into(),
            _ => "Write what should change.".into(),
        });
    }
    let p = repo::load(dir).map_err(|e| e.to_string())?;
    let stamp = |r: &mut Review,
                 n: &mut String,
                 by: &mut String,
                 at: &mut Option<toml::value::Datetime>| {
        *r = to;
        *n = note.to_string();
        *by = login.to_string();
        *at = Some(progress::now());
    };
    match item {
        Item::Milestone(id) => {
            let mut ms = p.milestones.clone();
            let m = ms
                .iter_mut()
                .find(|m| &m.id == id)
                .ok_or_else(|| format!("There's no milestone {id}."))?;
            stamp(
                &mut m.review,
                &mut m.review_note,
                &mut m.reviewed_by,
                &mut m.reviewed_at,
            );
            repo::write_milestones(dir, &ms).map_err(|e| e.to_string())
        }
        Item::Ticket(id) => {
            let mut t = p
                .tickets
                .iter()
                .find(|t| &t.id == id)
                .cloned()
                .ok_or_else(|| format!("There's no ticket {id}."))?;
            let m = &mut t.meta;
            stamp(
                &mut m.review,
                &mut m.review_note,
                &mut m.reviewed_by,
                &mut m.reviewed_at,
            );
            repo::write_ticket(dir, &t).map_err(|e| e.to_string())
        }
    }
}

/// What a proposed change is to: the spec, a milestone or a ticket (spec §14.6).
pub fn change_target(id: &str) -> &'static str {
    if id == SPEC {
        "spec"
    } else if id.starts_with('M') && id[1..].chars().all(|c| c.is_ascii_digit()) {
        "milestone"
    } else {
        "ticket"
    }
}

/// The id a spec change goes by.
pub const SPEC: &str = "spec";

/// Every change waiting for the lead, as (id, change), spec first, then milestones and
/// tickets in order.
pub fn pending_changes_of(p: &Project) -> Vec<(String, hydraui_projects::Change)> {
    let mut out = Vec::new();
    if let Some(c) = &p.file.spec_change {
        out.push((SPEC.to_string(), c.clone()));
    }
    for m in &p.milestones {
        if let Some(c) = &m.change {
            out.push((m.id.clone(), c.clone()));
        }
    }
    for t in &p.tickets {
        if let Some(c) = &t.meta.change {
            out.push((t.id.clone(), c.clone()));
        }
    }
    out
}

/// Accepts the change proposed to `id` (the spec, a milestone or a ticket) in project folder
/// `dir`: it replaces the approved version (spec §14.6). A ticket must still be to do: started
/// work only ever gets a follow-up ticket.
pub fn accept_change(dir: &Path, id: &str, login: &str) -> Result<(), String> {
    let e = |e: hydraui_projects::Error| e.to_string();
    let p = repo::load(dir).map_err(e)?;
    let gone = || format!("There's no change waiting for {id}.");
    if id == SPEC {
        let mut file = p.file.clone();
        let c = file.spec_change.take().ok_or_else(gone)?;
        repo::write_spec(dir, c.description.as_deref().unwrap_or(&p.spec)).map_err(e)?;
        file.spec_note.clear();
        return repo::write_project_file(dir, &file).map_err(e);
    }
    if let Some(i) = p.milestones.iter().position(|m| m.id == id) {
        let mut ms = p.milestones.clone();
        let c = ms[i].change.take().ok_or_else(gone)?;
        let m = &mut ms[i];
        if c.drop {
            if let Some(t) = p
                .tickets
                .iter()
                .find(|t| t.meta.milestone == id && started(t.meta.status))
            {
                return Err(format!(
                    "{} has started work ({}), so it can't be dropped. Reject this change.",
                    milestone_name(id),
                    t.id
                ));
            }
            m.review = Review::Rejected;
            m.review_note = format!("Dropped: {}", c.why);
            for t in p.tickets.iter().filter(|t| t.meta.milestone == id) {
                let mut t = t.clone();
                if t.meta.status == TicketStatus::Todo {
                    t.meta.status = TicketStatus::Dropped;
                    t.meta.change = None;
                    repo::write_ticket(dir, &t).map_err(e)?;
                }
            }
        } else {
            if let Some(v) = c.title {
                m.title = v;
            }
            if let Some(v) = c.target {
                m.target = Some(v);
            }
            if let Some(v) = c.description {
                m.description = v;
            }
            m.review_note.clear();
        }
        m.reviewed_by = login.to_string();
        m.reviewed_at = Some(progress::now());
        return repo::write_milestones(dir, &ms).map_err(e);
    }
    let mut t = p
        .tickets
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .ok_or_else(|| format!("There's no {id}."))?;
    let c = t.meta.change.take().ok_or_else(gone)?;
    if t.meta.status != TicketStatus::Todo {
        return Err(format!(
            "{id} is {} now, so it isn't changed. Reject this and ask Claude for a follow-up \
             ticket instead.",
            t.meta.status.label().to_lowercase()
        ));
    }
    if c.drop {
        t.meta.status = TicketStatus::Dropped;
    } else {
        if let Some(v) = c.title {
            t.meta.title = v;
        }
        if let Some(v) = c.description {
            t.body = v;
        }
        if let Some(v) = c.repo {
            t.meta.repo = v;
        }
        if let Some(v) = c.milestone {
            if !p
                .milestones
                .iter()
                .any(|m| m.id == v && m.review == Review::Approved)
            {
                return Err(format!(
                    "{} isn't an approved milestone.",
                    milestone_name(&v)
                ));
            }
            t.meta.milestone = v;
        }
    }
    t.meta.review_note.clear();
    t.meta.reviewed_by = login.to_string();
    t.meta.reviewed_at = Some(progress::now());
    repo::write_ticket(dir, &t).map_err(e)
}

/// Turns down the change proposed to `id`, keeping `reason` so Claude doesn't propose it again.
pub fn reject_change(dir: &Path, id: &str, reason: &str, login: &str) -> Result<(), String> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err("Say why, so Claude doesn't propose it again.".into());
    }
    let e = |e: hydraui_projects::Error| e.to_string();
    let p = repo::load(dir).map_err(e)?;
    let gone = || format!("There's no change waiting for {id}.");
    let note = format!("Change turned down: {reason}");
    if id == SPEC {
        let mut file = p.file.clone();
        file.spec_change.take().ok_or_else(gone)?;
        file.spec_note = note;
        return repo::write_project_file(dir, &file).map_err(e);
    }
    if let Some(i) = p.milestones.iter().position(|m| m.id == id) {
        let mut ms = p.milestones.clone();
        let m = &mut ms[i];
        m.change.take().ok_or_else(gone)?;
        m.review_note = note;
        m.reviewed_by = login.to_string();
        m.reviewed_at = Some(progress::now());
        return repo::write_milestones(dir, &ms).map_err(e);
    }
    let mut t = p
        .tickets
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .ok_or_else(|| format!("There's no {id}."))?;
    t.meta.change.take().ok_or_else(gone)?;
    t.meta.review_note = note;
    t.meta.reviewed_by = login.to_string();
    t.meta.reviewed_at = Some(progress::now());
    repo::write_ticket(dir, &t).map_err(e)
}

/// The resource name new requirements are saved under: dated, and who they came from.
pub fn requirements_name(from: &str) -> String {
    let date = progress::today();
    match from.trim() {
        "" => format!("Requirements {date}"),
        who => format!("Requirements from {who} {date}"),
    }
}

/// One line of [`line_diff`]: kept, removed or added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diff<'a> {
    Same(&'a str),
    Gone(&'a str),
    Added(&'a str),
}

/// The lines of `old` and `new`, marked kept, removed or added (a longest-common-subsequence
/// diff), to show a proposed change against what's agreed.
pub fn line_diff<'a>(old: &'a str, new: &'a str) -> Vec<Diff<'a>> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // lcs[i][j]: the common lines of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(Diff::Same(a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(Diff::Gone(a[i]));
            i += 1;
        } else {
            out.push(Diff::Added(b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| Diff::Gone(l)));
    out.extend(b[j..].iter().map(|l| Diff::Added(l)));
    out
}

/// Takes `snap`, a fresh read of the project's Linear project, into project folder `dir`
/// (spec §14.7): the first read as the whole project, later ones as what changed since the
/// last, saved as a requirements resource. Returns its file name, or None when Linear hasn't
/// changed. The snapshot is kept either way.
pub fn take_linear(
    dir: &Path,
    snap: &crate::linear_project::Snapshot,
) -> Result<Option<String>, String> {
    use crate::linear_project as lp;
    let text = match lp::load(dir) {
        None => Some(lp::render_all(snap)),
        Some(old) => lp::changes(&old, snap),
    };
    lp::save(dir, snap)?;
    let Some(text) = text else {
        return Ok(None);
    };
    let date = snap.fetched_at.get(..10).unwrap_or(&snap.fetched_at);
    save_text_resource(dir, &format!("Linear {date}"), &text).map(Some)
}

/// Whether work on a ticket has begun: started tickets are never rewritten (spec §14.6).
pub fn started(s: TicketStatus) -> bool {
    matches!(
        s,
        TicketStatus::InProgress | TicketStatus::InReview | TicketStatus::Done
    )
}

/// Saves new requirements as a dated resource in project folder `dir`, named after who gave
/// them, and returns its file name (spec §14.6).
pub fn add_requirements(dir: &Path, from: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Paste the requirements, or attach them as files.".into());
    }
    let who = from.trim();
    let date = progress::today();
    let name = requirements_name(who);
    let heading = if who.is_empty() {
        format!("# Requirements, {date}\n\n")
    } else {
        format!("# Requirements from {who}, {date}\n\n")
    };
    save_text_resource(dir, &name, &format!("{heading}{text}\n"))
}

/// Saves `text` as a Markdown resource named `name` (never over another), returning the
/// file name it got.
fn save_text_resource(dir: &Path, name: &str, text: &str) -> Result<String, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let staged = std::env::temp_dir().join(format!(
        "hydraui-requirements-{}-{nanos}.md",
        std::process::id()
    ));
    std::fs::write(&staged, text).map_err(|e| e.to_string())?;
    let saved = repo::add_resource_as(dir, &staged, name).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&staged);
    saved
}

/// Edits milestone `id` in place: its title, target date (`YYYY-MM-DD`, empty for none) and
/// description. Its review stays as it is.
pub fn edit_milestone(
    dir: &Path,
    id: &str,
    title: &str,
    target: &str,
    description: &str,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Give the milestone a title.".into());
    }
    let target = match target.trim() {
        "" => None,
        t => Some(date(t)?),
    };
    let mut ms = repo::load(dir).map_err(|e| e.to_string())?.milestones;
    let m = ms
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("There's no milestone {id}."))?;
    m.title = title.to_string();
    m.target = target;
    m.description = description.trim().to_string();
    repo::write_milestones(dir, &ms).map_err(|e| e.to_string())
}

/// Edits ticket `ticket`'s title and description in place.
pub fn edit_ticket(
    dir: &Path,
    ticket: &Ticket,
    title: &str,
    description: &str,
    code_repo: &str,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Give the ticket a title.".into());
    }
    let mut t = ticket.clone();
    t.meta.title = title.to_string();
    t.body = description.trim().to_string();
    t.meta.repo = if code_repo.trim().is_empty() {
        String::new()
    } else {
        repo_name(code_repo)?
    };
    repo::write_ticket(dir, &t).map_err(|e| e.to_string())
}

/// A `YYYY-MM-DD` date.
pub fn date(text: &str) -> Result<toml::value::Datetime, String> {
    let bad = || format!("{text} isn't a date like 2026-12-01");
    let d: toml::value::Datetime = text.trim().parse().map_err(|_| bad())?;
    if d.date.is_none() || d.time.is_some() {
        return Err(bad());
    }
    Ok(d)
}

/// What needs the lead's review in `p`: milestones and tickets proposed and not yet decided.
pub fn to_review(p: &Project) -> (usize, usize) {
    (
        p.milestones.iter().filter(|m| m.review.waiting()).count(),
        p.tickets.iter().filter(|t| t.meta.review.waiting()).count(),
    )
}

/// Whether work can start on `ticket`: it and its milestone are approved, and it isn't done.
pub fn can_start(p: &Project, ticket: &Ticket) -> bool {
    ticket.meta.review == Review::Approved
        && !matches!(
            ticket.meta.status,
            TicketStatus::Done | TicketStatus::Dropped
        )
        && p.milestones
            .iter()
            .any(|m| m.id == ticket.meta.milestone && m.review == Review::Approved)
}

/// Where `repo` is cloned on this machine, as remembered (`[clones]`, spec §14).
pub fn clone_of(hydra_root: &Path, repo: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_file(hydra_root)).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    let path = PathBuf::from(table.get("clones")?.get(repo)?.as_str()?);
    path.join(".git").exists().then_some(path)
}

/// Remembers that `repo` is cloned at `path`, after checking it's a git repo.
pub fn set_clone(hydra_root: &Path, repo: &str, path: &Path) -> Result<(), String> {
    if !path.join(".git").exists() {
        return Err(format!("{} isn't a git repository.", path.display()));
    }
    let file = config_file(hydra_root);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if !doc.contains_key("clones") {
        doc.insert("clones", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    doc.get_mut("clones")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or("clones in projects.toml isn't a table")?
        .insert(repo, toml_edit::value(path.display().to_string()));
    hydraui_sets::layout::write_atomic(&file, &doc.to_string()).map_err(|e| e.to_string())
}

/// The branch for work on `ticket`: its id and a few words of its title, lower case
/// (`pg-12-share-dialog`), so the local check sees the key (§6.1).
pub fn branch_for(ticket: &Ticket) -> String {
    let words = crate::worktrees::slug(&ticket.meta.title);
    format!("{}-{words}", ticket.id.to_ascii_lowercase())
}

/// Makes a worktree of `clone` for work on `ticket`: branch [`branch_for`] from `base` (an
/// earlier ticket's branch it builds on, spec §14.5) or else the remote's default branch, both
/// fetched first as `env`, in `<clone>-worktrees/<branch>`. An existing worktree on that branch
/// is reused. Returns its folder and branch. Blocks.
pub fn ticket_worktree(
    hydra: &Path,
    env: &str,
    clone: &Path,
    ticket: &Ticket,
    base: Option<&str>,
) -> Result<(PathBuf, String), String> {
    let branch = branch_for(ticket);
    let name = folder_name(clone)?;
    let parent = clone.parent().ok_or("the clone has no parent folder")?;
    let path = parent.join(format!("{name}-worktrees")).join(&branch);
    worktree_at(hydra, env, clone, &branch, base, &path)?;
    Ok((path, branch))
}

/// One code repo of a ticket that names none: its clone here and the branch to build on.
pub struct RepoClone {
    pub clone: PathBuf,
    pub base: Option<String>,
}

/// Makes (or reuses) the folder for a ticket that doesn't name its repo: a worktree of each of
/// the project's code repos, side by side, all on the ticket's branch, so its session works in
/// whichever the ticket turns out to need. The folder sits beside the first clone, as
/// `<project>-tickets/<branch>`, each worktree named after its clone's folder.
pub fn ticket_workspace(
    hydra: &Path,
    env: &str,
    project: &str,
    repos: &[RepoClone],
    ticket: &Ticket,
) -> Result<(PathBuf, String), String> {
    let branch = branch_for(ticket);
    let first = repos.first().ok_or("the project has no code repos")?;
    let parent = first
        .clone
        .parent()
        .ok_or("the clone has no parent folder")?;
    let folder = parent.join(format!("{project}-tickets")).join(&branch);
    for r in repos {
        let path = folder.join(folder_name(&r.clone)?);
        worktree_at(hydra, env, &r.clone, &branch, r.base.as_deref(), &path)?;
    }
    Ok((folder, branch))
}

fn folder_name(clone: &Path) -> Result<String, String> {
    clone
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "the clone has no folder name".to_string())
}

/// A worktree of `clone` at `path` on `branch`, made from `base` (else the default branch)
/// when the branch is new; an existing worktree there is reused.
fn worktree_at(
    hydra: &Path,
    env: &str,
    clone: &Path,
    branch: &str,
    base: Option<&str>,
    path: &Path,
) -> Result<(), String> {
    if path.join(".git").exists() {
        return Ok(());
    }
    let c = clone.display().to_string();
    // Best effort: without the network the local default branch still works.
    let _ = env_git(hydra, env, &["-C", &c, "fetch", "origin"]);
    let local = |args: &[&str]| -> Result<String, String> {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(clone)
            .args(args)
            .stdin(std::process::Stdio::null());
        let out = hydra_platform::process::hide_window(&mut cmd)
            .output()
            .map_err(|e| format!("git didn't run: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    };
    let base = base
        .map(|b| format!("origin/{b}"))
        .filter(|b| local(&["rev-parse", "--verify", "--quiet", b]).is_ok())
        .unwrap_or_else(|| {
            local(&["rev-parse", "--abbrev-ref", "origin/HEAD"]).unwrap_or_else(|_| "HEAD".into())
        });
    let target = path.display().to_string();
    let head = format!("refs/heads/{branch}");
    if local(&["rev-parse", "--verify", "--quiet", &head]).is_ok() {
        local(&["worktree", "add", &target, branch])?;
    } else {
        local(&["worktree", "add", "-b", branch, &target, &base])?;
    }
    Ok(())
}

/// A ticket's body split into what was asked and what came of it: the brief, then the work
/// sessions' reports (`## Work`) and the checks (`## Check`) appended after it, in order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TicketSections {
    pub brief: String,
    pub work: Vec<String>,
    pub check: Vec<String>,
}

pub fn ticket_sections(body: &str) -> TicketSections {
    enum At {
        Brief,
        Work,
        Check,
    }
    let mut out = TicketSections::default();
    let mut at = At::Brief;
    let mut text = String::new();
    let flush = |at: &At, text: &mut String, out: &mut TicketSections| {
        let t = std::mem::take(text).trim().to_string();
        match at {
            At::Brief => out.brief = t,
            At::Work if !t.is_empty() => out.work.push(t),
            At::Check if !t.is_empty() => out.check.push(t),
            _ => {}
        }
    };
    for line in body.lines() {
        let next = match line.trim_end() {
            "## Work" => Some(At::Work),
            "## Check" => Some(At::Check),
            _ => None,
        };
        match next {
            Some(n) => {
                flush(&at, &mut text, &mut out);
                at = n;
            }
            None => {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    flush(&at, &mut text, &mut out);
    out
}

/// The first line of a report, for a one-line "latest" entry.
pub fn headline(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The headings a ticket's outcome summary is written under, in order.
pub const SUMMARY_HEADINGS: [&str; 4] = [
    "What it did",
    "What worked or passed",
    "What it missed or failed",
    "Gaps",
];

/// Everything a ticket's sessions reported, for summarising: their reports, its checks and
/// its pull requests. Empty when nothing was reported.
pub fn outcome_text(t: &Ticket) -> String {
    let s = ticket_sections(&t.body);
    let mut out: Vec<String> = s.work.iter().map(|w| format!("Report:\n{w}")).collect();
    out.extend(s.check.iter().map(|c| format!("Check:\n{c}")));
    if !t.meta.prs.is_empty() {
        out.push(format!("Pull requests: {}", t.meta.prs.join(", ")));
    }
    out.join("\n\n")
}

/// The prompt that turns a ticket's outcome into a short summary under [`SUMMARY_HEADINGS`].
pub fn summary_prompt(t: &Ticket, outcome: &str) -> String {
    let headings = SUMMARY_HEADINGS
        .iter()
        .map(|h| format!("## {h}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Summarise what came of ticket {id} \"{title}\" for its project lead, who wants to see \
         at a glance what happened. Use only the report below; don't add anything it doesn't \
         say. Write exactly these four headings, in this order, each followed by 1 to 5 short \
         bullet points (one line each, plain words, no code blocks):\n{headings}\n\
         Under a heading with nothing to say, write one bullet: \"- Nothing\". Reply with \
         just the summary.\n\n---\n{outcome}",
        id = t.id,
        title = t.meta.title,
    )
}

/// Where this Mac keeps ticket `ticket`'s outcome summary: a local cache, never sent.
pub fn summary_file(hydra_root: &Path, env: &str, slug: &str, ticket: &str) -> PathBuf {
    hydra_root
        .join("hydraui")
        .join("summaries")
        .join(env)
        .join(slug)
        .join(format!("{ticket}.md"))
}

/// A stable fingerprint of an outcome, so a summary is only reused for the report it summed up.
fn outcome_key(outcome: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in outcome.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The saved summary of `outcome`; None when there's none, or it summed up an older report.
pub fn cached_summary(file: &Path, outcome: &str) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let (first, rest) = text.split_once('\n')?;
    (first == format!("<!-- outcome {} -->", outcome_key(outcome))).then(|| rest.to_string())
}

pub fn save_summary(file: &Path, outcome: &str, summary: &str) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = format!(
        "<!-- outcome {} -->\n{}",
        outcome_key(outcome),
        summary.trim()
    );
    std::fs::write(file, text).map_err(|e| e.to_string())
}

/// Asks Claude, as `env` and with no tools, to summarise ticket `t`'s outcome. Blocks for up
/// to a few minutes.
pub fn summarise(
    hydra: &Path,
    env: &str,
    cwd: &Path,
    t: &Ticket,
    outcome: &str,
) -> Result<String, String> {
    std::fs::create_dir_all(cwd).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(hydra);
    cmd.args(["run", env, "--", "claude", "-p"])
        .arg(summary_prompt(t, outcome))
        .args(["--tools", "", "--model", "sonnet"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null());
    hydra_platform::process::hide_window(&mut cmd);
    let out = crate::branch_rule::output_within(cmd, Duration::from_secs(240))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !text.is_empty() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            "Claude gave no summary".to_string()
        } else {
            err
        })
    }
}

/// Marks `folder` as trusted in an environment's Claude state (`.claude.json` in its
/// CLAUDE_CONFIG_DIR), as accepting Claude Code's "trust this folder?" prompt would. hydraui
/// does this only for the folders it makes for a milestone run's sessions, from the user's own
/// clones: otherwise each new ticket's session sits at that prompt. Without that file (Claude
/// never ran there) it does nothing; the prompt then shows as usual.
pub fn trust_folder(claude_json: &Path, folder: &Path) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(claude_json) else {
        return Ok(());
    };
    let mut state: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} doesn't read: {e}", claude_json.display()))?;
    let root = state
        .as_object_mut()
        .ok_or_else(|| format!("{} isn't a JSON object", claude_json.display()))?;
    let projects = root
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("its projects entry isn't an object")?;
    let entry = projects
        .entry(folder.display().to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("its entry for the folder isn't an object")?;
    if entry.get("hasTrustDialogAccepted") == Some(&serde_json::Value::Bool(true)) {
        return Ok(());
    }
    entry.insert("hasTrustDialogAccepted".into(), true.into());
    let out = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    // Written whole and renamed into place, so Claude never reads half a file.
    let tmp = claude_json.with_extension("json.hydraui-tmp");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, claude_json).map_err(|e| e.to_string())
}

/// Records that work on `ticket` started: in progress, on `branch`, with how it was started.
pub fn start_work(
    dir: &Path,
    ticket: &Ticket,
    branch: &str,
    login: &str,
    how: (&str, &str, &str),
) -> Result<(), String> {
    let mut t = ticket.clone();
    let (skill, model, effort) = how;
    t.meta.status = TicketStatus::InProgress;
    if t.meta.assignee.is_empty() {
        t.meta.assignee = login.to_string();
    }
    t.meta.branch = branch.to_string();
    t.meta.started_by = login.to_string();
    t.meta.started_at = Some(progress::now());
    t.meta.skill = skill.to_string();
    t.meta.model = model.to_string();
    t.meta.effort = effort.to_string();
    repo::write_ticket(dir, &t).map_err(|e| e.to_string())
}

/// hydraui's built-in planning guide, a Claude Code skill: what a milestone is and isn't, the
/// layer-by-layer breakdown, the self-review and the product. A planning session follows it
/// when its engineer hasn't picked a skill of their own (spec §14.2).
pub const BUILT_IN_GUIDE: [(&str, &str); 4] = [
    ("SKILL.md", include_str!("planning_skill/SKILL.md")),
    (
        "references/format.md",
        include_str!("planning_skill/references/format.md"),
    ),
    (
        "references/product.md",
        include_str!("planning_skill/references/product.md"),
    ),
    (
        "checklists/devils-advocate.md",
        include_str!("planning_skill/checklists/devils-advocate.md"),
    ),
];

/// What a planning session follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanningGuide {
    /// A skill the engineer installed and picked for the project, by name.
    Skill(String),
    /// A guide read from this SKILL.md: a skill folder picked in Settings, or hydraui's
    /// built-in guide written out.
    BuiltIn(PathBuf),
}

impl PlanningGuide {
    /// The line a planning prompt starts with.
    pub fn instruction(&self) -> String {
        match self {
            PlanningGuide::Skill(name) => format!(
                "Use the /{name} skill throughout: read it, and the files it points to, before \
                 anything else. It says how we plan: what a milestone is and isn't, how to break \
                 one into tickets, how to check your own work, and what we build where."
            ),
            PlanningGuide::BuiltIn(path) => format!(
                "First read the planning guide, {}, and the files it points to: what a milestone \
                 is and isn't, how to break one into tickets, how to check your own work, and \
                 what we build where. Follow it throughout.",
                path.display()
            ),
        }
    }
}

/// The skill this engineer picked for planning project `slug` of `repo`, if any (`[planning]`
/// in projects.toml: local, so each engineer plugs in their own).
pub fn planning_skill_of(hydra_root: &Path, repo: &str, slug: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_file(hydra_root)).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    table
        .get("planning")?
        .get(format!("{repo}/{slug}"))?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Picks `skill` for planning project `slug` of `repo`, or with `None` goes back to the
/// built-in guide, keeping the rest of the file.
pub fn set_planning_skill(
    hydra_root: &Path,
    repo: &str,
    slug: &str,
    skill: Option<&str>,
) -> Result<(), String> {
    let file = config_file(hydra_root);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if !doc.contains_key("planning") {
        doc.insert("planning", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let table = doc
        .get_mut("planning")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or("planning in projects.toml isn't a table")?;
    let key = format!("{repo}/{slug}");
    match skill.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            table.insert(&key, toml_edit::value(s));
        }
        None => {
            table.remove(&key);
        }
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    hydraui_sets::layout::write_atomic(&file, &doc.to_string()).map_err(|e| e.to_string())
}

/// How this engineer's hydraui works a project's tickets (spec §14.5): the skill for each code
/// repo, the testing skill, and the model and effort of work sessions. Local, in
/// `[work."<repo>/<slug>"]` of projects.toml.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkSetup {
    /// Skill folders by code repo (`owner/name`).
    pub skills: BTreeMap<String, PathBuf>,
    /// How each pull request is checked before the run moves on; `None`: it isn't.
    pub testing: Option<Testing>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// How a milestone run checks each pull request (spec §14.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Testing {
    /// With this skill folder.
    Skill(PathBuf),
    /// With no skill named: the repo's own and those the team's skill router asks for.
    Team,
}

/// How projects.toml writes `Testing::Team`; a chosen folder is always a full path.
pub const TEAM_TESTING: &str = "team";

/// One of a project's work settings.
#[derive(Debug, Clone, Copy)]
pub enum WorkSetting<'a> {
    /// The skill for this code repo.
    Skill(&'a str),
    Testing,
    Model,
    Effort,
}

/// Project `slug` of `repo`'s work settings on this machine.
pub fn work_setup(hydra_root: &Path, repo: &str, slug: &str) -> WorkSetup {
    let Some(table) = std::fs::read_to_string(config_file(hydra_root))
        .ok()
        .and_then(|text| toml::from_str::<toml::Table>(&text).ok())
    else {
        return WorkSetup::default();
    };
    let Some(work) = table
        .get("work")
        .and_then(|w| w.get(format!("{repo}/{slug}")))
    else {
        return WorkSetup::default();
    };
    let text = |k: &str| {
        work.get(k)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    WorkSetup {
        skills: work
            .get("skills")
            .and_then(|s| s.as_table())
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| Some((k.clone(), PathBuf::from(v.as_str()?))))
                    .collect()
            })
            .unwrap_or_default(),
        testing: text("testing").map(|t| match t.as_str() {
            TEAM_TESTING => Testing::Team,
            _ => Testing::Skill(PathBuf::from(t)),
        }),
        model: text("model"),
        effort: text("effort"),
    }
}

/// Sets one of project `slug` of `repo`'s work settings, or with `None` clears it.
pub fn set_work(
    hydra_root: &Path,
    repo: &str,
    slug: &str,
    setting: WorkSetting,
    value: Option<&str>,
) -> Result<(), String> {
    let file = config_file(hydra_root);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let table_in = |parent: &mut toml_edit::Item, key: &str| -> Result<(), String> {
        let t = parent
            .as_table_like_mut()
            .ok_or("projects.toml isn't laid out as hydraui wrote it")?;
        if t.get(key).is_none() {
            let mut new = toml_edit::Table::new();
            new.set_implicit(true);
            t.insert(key, toml_edit::Item::Table(new));
        }
        Ok(())
    };
    let project = format!("{repo}/{slug}");
    table_in(doc.as_item_mut(), "work")?;
    table_in(&mut doc["work"], &project)?;
    let at = &mut doc["work"][project.as_str()];
    let (holder, key) = match setting {
        WorkSetting::Skill(code_repo) => {
            table_in(at, "skills")?;
            (&mut at["skills"], code_repo)
        }
        WorkSetting::Testing => (at, "testing"),
        WorkSetting::Model => (at, "model"),
        WorkSetting::Effort => (at, "effort"),
    };
    let t = holder
        .as_table_like_mut()
        .ok_or("projects.toml isn't laid out as hydraui wrote it")?;
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            t.insert(key, toml_edit::value(v));
        }
        None => {
            t.remove(key);
        }
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    hydraui_sets::layout::write_atomic(&file, &doc.to_string()).map_err(|e| e.to_string())
}

/// The skill folder for `picked`: a folder holding a `SKILL.md`, or a `SKILL.md` itself.
pub fn skill_folder(picked: &Path) -> Result<PathBuf, String> {
    let dir = if picked.file_name().is_some_and(|n| n == "SKILL.md") {
        picked.parent().unwrap_or(picked)
    } else {
        picked
    };
    if dir.join("SKILL.md").is_file() {
        Ok(dir.to_path_buf())
    } else {
        Err(format!(
            "{} isn't a skill: pick the folder that holds its SKILL.md.",
            dir.display()
        ))
    }
}

/// The name Claude's Skill tool knows the skill in `folder` by (spec §14.5): its SKILL.md's
/// `name`, else the folder's; `plugin:skill` for a plugin's skill, the form the team's skill
/// router counts as loaded.
pub fn skill_name(folder: &Path) -> Option<String> {
    let own = std::fs::read_to_string(folder.join("SKILL.md"))
        .ok()
        .and_then(|text| frontmatter_name(&text))
        .or_else(|| folder.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    let plugin = folder
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "skills"))
        .and_then(Path::parent)
        .and_then(|root| std::fs::read_to_string(root.join(".claude-plugin/plugin.json")).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("name")?.as_str().map(str::to_string));
    Some(match plugin {
        Some(plugin) => format!("{plugin}:{own}"),
        None => own,
    })
}

/// The `name:` in a SKILL.md's front matter.
fn frontmatter_name(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().trim_matches(['"', '\'']).to_string())
        .filter(|n| !n.is_empty())
}

/// Where a work or testing session's skill comes from (spec §14.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// None chosen: the session follows the repo's own CLAUDE.md and `.claude/skills`.
    RepoOwn,
    /// A skill inside the code repo, by its path in the repo: read from the ticket's
    /// worktree, so it matches the code being changed.
    InRepo(PathBuf),
    /// The engineer's own skill, outside the repo: read where it is.
    Yours(PathBuf),
}

impl SkillSource {
    /// Where `chosen` comes from, given where the ticket's repo is cloned (if known).
    pub fn of(chosen: Option<&Path>, clone: Option<&Path>) -> Self {
        let Some(chosen) = chosen else {
            return Self::RepoOwn;
        };
        let inside = |c: &Path, s: &Path| s.strip_prefix(c).ok().map(Path::to_path_buf);
        let rel = clone.and_then(|c| {
            inside(c, chosen).or_else(|| {
                // Through symlinks too (e.g. /tmp vs /private/tmp on macOS).
                inside(&c.canonicalize().ok()?, &chosen.canonicalize().ok()?)
            })
        });
        match rel {
            Some(rel) => Self::InRepo(rel),
            None => Self::Yours(chosen.to_path_buf()),
        }
    }

    /// The skill folder a session in `worktree` reads; None for the repo's own.
    pub fn folder_in(&self, worktree: &Path) -> Option<PathBuf> {
        match self {
            Self::RepoOwn => None,
            Self::InRepo(rel) => Some(worktree.join(rel)),
            Self::Yours(path) => Some(path.clone()),
        }
    }

    /// How Settings shows it.
    pub fn label(&self) -> String {
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        match self {
            Self::RepoOwn => "The repo's own (CLAUDE.md and .claude/skills)".to_string(),
            Self::InRepo(rel) => format!("{}, from the repo ({})", name(rel), rel.display()),
            Self::Yours(path) => format!("{}, yours ({})", name(path), path.display()),
        }
    }
}

/// The last sync slot run for project `slug` of `repo` on this machine (`[synced]` in
/// projects.toml, local `YYYY-MM-DD HH:MM`), spec §7.
pub fn synced_at(hydra_root: &Path, repo: &str, slug: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_file(hydra_root)).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    table
        .get("synced")?
        .get(format!("{repo}/{slug}"))?
        .as_str()
        .map(str::to_string)
}

/// Records `slot` as the last sync slot run for project `slug` of `repo`.
pub fn set_synced(hydra_root: &Path, repo: &str, slug: &str, slot: &str) -> Result<(), String> {
    let file = config_file(hydra_root);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if !doc.contains_key("synced") {
        doc.insert("synced", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    doc.get_mut("synced")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or("synced in projects.toml isn't a table")?
        .insert(&format!("{repo}/{slug}"), toml_edit::value(slot));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    hydraui_sets::layout::write_atomic(&file, &doc.to_string()).map_err(|e| e.to_string())
}

/// What a planning session for project `slug` of `repo` follows: the engineer's skill, or the
/// built-in guide written out under `state` (refreshed each time, so it's this build's).
pub fn planning_guide(
    hydra_root: &Path,
    state: &Path,
    repo: &str,
    slug: &str,
) -> Result<PlanningGuide, String> {
    if let Some(skill) = planning_skill_of(hydra_root, repo, slug) {
        // A folder picked in Settings, or a skill name from before the folder picker.
        return Ok(if skill.contains('/') || skill.contains('\\') {
            PlanningGuide::BuiltIn(PathBuf::from(skill).join("SKILL.md"))
        } else {
            PlanningGuide::Skill(skill)
        });
    }
    let dir = state.join("planning-guide");
    for (name, text) in BUILT_IN_GUIDE {
        let file = dir.join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, text).map_err(|e| e.to_string())?;
    }
    Ok(PlanningGuide::BuiltIn(dir.join("SKILL.md")))
}

/// `text` without a leading bullet ("- ", "* ", "• ") and surrounding space.
pub fn strip_point(text: &str) -> &str {
    let t = text.trim();
    ["- ", "* ", "• ", "-", "•"]
        .iter()
        .find_map(|b| t.strip_prefix(b))
        .unwrap_or(t)
        .trim()
}

/// One of a milestone's points: a short line, and the full explanation shown when it's
/// clicked (empty when there's none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub text: String,
    pub detail: String,
}

/// A milestone description's points, or None when it isn't points (an older paragraph).
/// Each point is a bullet line; its explanation is the indented lines under it.
pub fn points_of(text: &str) -> Option<Vec<Point>> {
    let bullet = |l: &str| ["- ", "* ", "• "].iter().any(|b| l.starts_with(b));
    let mut points: Vec<(Point, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        let indented = line.starts_with(char::is_whitespace);
        match points.last_mut() {
            Some((_, detail)) if line.trim().is_empty() => detail.push(""),
            _ if line.trim().is_empty() => {}
            _ if !indented && bullet(line) => points.push((
                Point {
                    text: strip_point(line).to_string(),
                    detail: String::new(),
                },
                Vec::new(),
            )),
            Some((_, detail)) if indented => {
                detail.push(line.strip_prefix("  ").unwrap_or(line.trim_start()))
            }
            _ => return None,
        }
    }
    (!points.is_empty()).then(|| {
        points
            .into_iter()
            .map(|(mut p, detail)| {
                p.detail = detail.join("\n").trim().to_string();
                p
            })
            .collect()
    })
}

/// Points as a milestone description: each a bullet line with its explanation indented
/// under it, which [`points_of`] reads back.
pub fn points_text(points: &[Point]) -> String {
    points
        .iter()
        .map(|p| {
            let mut out = format!("- {}", p.text);
            for line in p.detail.lines() {
                out.push_str("\n  ");
                out.push_str(line);
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An older paragraph description as one point per sentence, so it never shows as a wall
/// of text: split after `.`, `!` or `?` followed by white space, and at line breaks.
pub fn sentences_of(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut start = 0;
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let ends = matches!(c, '.' | '!' | '?')
                && chars.peek().is_some_and(|(_, n)| n.is_whitespace());
            if ends {
                out.push(line[start..=i].trim());
                start = i + 1;
            }
        }
        out.push(line[start..].trim());
    }
    out.retain(|s| !s.is_empty());
    out
}

/// Parses a comma- or space-separated list (members, repos), dropping blanks and `@`s.
pub fn list_of(text: &str) -> Vec<String> {
    text.split([',', ' ', '\n'])
        .map(|s| s.trim().trim_start_matches('@'))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Sync times from `text`: one or two `HH:MM`, sorted (spec §2).
pub fn sync_times(text: &str) -> Result<Vec<String>, String> {
    let mut times = Vec::new();
    for t in list_of(text) {
        let ok = t
            .split_once(':')
            .and_then(|(h, m)| Some((h.parse::<u8>().ok()?, m.parse::<u8>().ok()?, m.len())))
            .is_some_and(|(h, m, len)| h < 24 && m < 60 && len == 2);
        if !ok {
            return Err(format!("{t} isn't a time like 12:30"));
        }
        let (h, m) = t.split_once(':').unwrap_or_default();
        times.push(format!("{:02}:{m}", h.parse::<u8>().unwrap_or(0)));
    }
    times.sort();
    times.dedup();
    match times.len() {
        1 | 2 => Ok(times),
        _ => Err("Give one or two times a day, like 12:30, 17:30.".into()),
    }
}

/// How long ago `then` was, shortly: `5m`, `3h`, `2d`. `now` and `then` are TOML date-times.
pub fn age(then: &toml::value::Datetime, now_secs: u64) -> String {
    let Some(then) = unix_of(then) else {
        return String::new();
    };
    let secs = now_secs.saturating_sub(then);
    match secs {
        s if s < 3600 => format!("{}m", (s / 60).max(1)),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// Seconds since the epoch of a date or date-time (UTC when it has no offset).
fn unix_of(t: &toml::value::Datetime) -> Option<u64> {
    let d = t.date?;
    let (y, m, day) = (i64::from(d.year), i64::from(d.month), i64::from(d.day));
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut secs = days * 86_400;
    if let Some(time) = t.time {
        secs += i64::from(time.hour) * 3600 + i64::from(time.minute) * 60 + i64::from(time.second);
    }
    if let Some(toml::value::Offset::Custom { minutes }) = t.offset {
        secs -= i64::from(minutes) * 60;
    }
    u64::try_from(secs).ok()
}

/// Seconds since the epoch, now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_settings_are_kept_per_project_on_this_machine() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(work_setup(root, "o/r", "pg"), WorkSetup::default());
        set_work(
            root,
            "o/r",
            "pg",
            WorkSetting::Skill("iov42/version2-frontend"),
            Some("/s/fe"),
        )
        .unwrap();
        set_work(
            root,
            "o/r",
            "pg",
            WorkSetting::Skill("iov42/version-2-backend"),
            Some("/s/be"),
        )
        .unwrap();
        set_work(root, "o/r", "pg", WorkSetting::Testing, Some("/s/qa")).unwrap();
        set_work(root, "o/r", "pg", WorkSetting::Model, Some("opus")).unwrap();
        set_work(root, "o/r", "pg", WorkSetting::Effort, Some("high")).unwrap();
        set_planning_skill(root, "o/r", "pg", Some("/s/plan")).unwrap();
        let w = work_setup(root, "o/r", "pg");
        assert_eq!(w.skills["iov42/version2-frontend"], PathBuf::from("/s/fe"));
        assert_eq!(w.skills["iov42/version-2-backend"], PathBuf::from("/s/be"));
        assert_eq!(w.testing, Some(Testing::Skill("/s/qa".into())));
        assert_eq!(
            (w.model.as_deref(), w.effort.as_deref()),
            (Some("opus"), Some("high"))
        );
        // Another project has its own; clearing one leaves the rest.
        assert_eq!(work_setup(root, "o/r", "mx"), WorkSetup::default());
        set_work(
            root,
            "o/r",
            "pg",
            WorkSetting::Skill("iov42/version2-frontend"),
            None,
        )
        .unwrap();
        set_work(root, "o/r", "pg", WorkSetting::Model, None).unwrap();
        let w = work_setup(root, "o/r", "pg");
        assert!(!w.skills.contains_key("iov42/version2-frontend"));
        assert_eq!(
            (w.model, w.testing),
            (None, Some(Testing::Skill("/s/qa".into())))
        );
        assert_eq!(
            planning_skill_of(root, "o/r", "pg").as_deref(),
            Some("/s/plan")
        );
    }

    #[test]
    fn pull_requests_can_be_checked_with_the_team_skills_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        set_work(root, "o/r", "pg", WorkSetting::Testing, Some(TEAM_TESTING)).unwrap();
        assert_eq!(work_setup(root, "o/r", "pg").testing, Some(Testing::Team));
        set_work(root, "o/r", "pg", WorkSetting::Testing, None).unwrap();
        assert_eq!(work_setup(root, "o/r", "pg").testing, None);
    }

    #[test]
    fn a_skill_folder_must_hold_a_skill() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(skill_folder(tmp.path()).is_err());
        std::fs::write(tmp.path().join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        assert_eq!(skill_folder(tmp.path()).unwrap(), tmp.path());
        // Picking the SKILL.md itself means its folder.
        assert_eq!(
            skill_folder(&tmp.path().join("SKILL.md")).unwrap(),
            tmp.path()
        );
    }

    #[test]
    fn a_chosen_skill_is_loaded_by_the_name_claude_knows_it_by() {
        let tmp = tempfile::tempdir().unwrap();
        // A skill of your own: its SKILL.md's name, else its folder's.
        let own = tmp.path().join("mine");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(
            own.join("SKILL.md"),
            "---\nname: rust\ndescription: x\n---\n",
        )
        .unwrap();
        assert_eq!(skill_name(&own).as_deref(), Some("rust"));
        let bare = tmp.path().join("angular");
        assert_eq!(skill_name(&bare).as_deref(), Some("angular"));
        // A plugin's skill goes by plugin:skill, which is what the skill router counts.
        let root = tmp.path().join("cache/iov42/qa-engineer/1.2.0");
        let qa = root.join("skills/qa-engineer");
        std::fs::create_dir_all(&qa).unwrap();
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name":"qa-engineer","version":"1.2.0"}"#,
        )
        .unwrap();
        std::fs::write(qa.join("SKILL.md"), "---\nname: qa-engineer\n---\n").unwrap();
        assert_eq!(skill_name(&qa).as_deref(), Some("qa-engineer:qa-engineer"));
    }

    #[test]
    fn a_skill_inside_the_repo_is_read_from_the_worktree() {
        let clone = Path::new("/code/fe");
        let wt = Path::new("/wt/pg-3");
        assert_eq!(SkillSource::of(None, Some(clone)), SkillSource::RepoOwn);
        assert_eq!(SkillSource::RepoOwn.folder_in(wt), None);
        let mine = SkillSource::of(Some(Path::new("/code/fe/.claude/skills/ng")), Some(clone));
        assert_eq!(mine, SkillSource::InRepo(".claude/skills/ng".into()));
        assert_eq!(
            mine.folder_in(wt),
            Some(PathBuf::from("/wt/pg-3/.claude/skills/ng"))
        );
        assert!(mine.label().starts_with("ng, from the repo"));
        // A skill of your own stays where it is, clone known or not.
        let yours = SkillSource::of(Some(Path::new("/me/skills/rust")), Some(clone));
        assert_eq!(yours, SkillSource::Yours("/me/skills/rust".into()));
        assert_eq!(yours.folder_in(wt), Some(PathBuf::from("/me/skills/rust")));
        assert_eq!(
            SkillSource::of(Some(Path::new("/code/fe/x")), None),
            SkillSource::Yours("/code/fe/x".into())
        );
        // A sibling folder that only shares a prefix isn't inside the repo.
        assert!(matches!(
            SkillSource::of(Some(Path::new("/code/fe-old/s")), Some(clone)),
            SkillSource::Yours(_)
        ));
    }

    #[test]
    fn a_skill_reached_through_a_symlink_is_still_in_the_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join(".claude/skills/ng")).unwrap();
        let link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();
        assert_eq!(
            SkillSource::of(Some(&link.join(".claude/skills/ng")), Some(&real)),
            SkillSource::InRepo(".claude/skills/ng".into())
        );
    }

    #[test]
    fn a_milestone_is_shown_by_its_number() {
        assert_eq!(milestone_name("M3"), "Milestone 3");
        assert_eq!(Item::Milestone("M12".into()).name(), "Milestone 12");
        assert_eq!(Item::Ticket("MUE-3".into()).name(), "MUE-3");
        assert_eq!(milestone_name("Mx"), "Mx");
    }

    #[test]
    fn a_decision_is_shown_by_its_number() {
        assert_eq!(decision_name("D-7"), "Decision 7");
        assert_eq!(decision_name("odd"), "odd");
    }

    #[test]
    fn os_litter_stays_out_of_the_checkouts_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let info = tmp.path().join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), "# git's own\n.DS_Store").unwrap();
        ignore_os_files(tmp.path());
        ignore_os_files(tmp.path());
        assert_eq!(
            std::fs::read_to_string(info.join("exclude")).unwrap(),
            "# git's own\n.DS_Store\nThumbs.db\ndesktop.ini\n"
        );
    }

    #[test]
    fn a_projects_repos_are_confirmed_or_suggested_by_its_tickets_and_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut file: hydraui_projects::ProjectFile =
            toml::from_str("name = \"P\"\nkey = \"PG\"\nlead = \"me\"").unwrap();
        file.repos = vec!["iov42/version2-frontend".into()];
        file.suggested_repos = vec![
            hydraui_projects::SuggestedRepo {
                repo: "iov42/api-designs".into(),
                why: "a new endpoint".into(),
            },
            hydraui_projects::SuggestedRepo {
                repo: "iov42/svc-usage".into(),
                why: String::new(),
            },
        ];
        file.dismissed_repos = vec!["iov42/svc-usage".into()];
        repo::write_project_file(dir, &file).unwrap();
        let ticket = |id: &str, on: &str, review: Review| Ticket {
            id: id.into(),
            meta: hydraui_projects::TicketMeta {
                title: id.into(),
                repo: on.into(),
                review,
                ..Default::default()
            },
            body: String::new(),
        };
        for t in [
            ticket("PG-1", "iov42/version-2-backend", Review::Approved),
            ticket("PG-2", "iov42/version2-frontend", Review::Proposed),
            ticket("PG-3", "iov42/version-2-backend", Review::Proposed),
            ticket("PG-4", "iov42/identity-platform", Review::Rejected),
        ] {
            repo::write_ticket(dir, &t).unwrap();
        }
        let p = repo::load(dir).unwrap();
        let (confirmed, suggested) = repos_of(&p);
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].tickets, vec!["PG-2".to_string()]);
        let names: Vec<(&str, Vec<String>, &str)> = suggested
            .iter()
            .map(|r| (r.repo.as_str(), r.tickets.clone(), r.why.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("iov42/api-designs", vec![], "a new endpoint"),
                (
                    "iov42/version-2-backend",
                    vec!["PG-1".into(), "PG-3".into()],
                    ""
                ),
            ]
        );
        // The check watches the confirmed repos and every repo a live ticket names.
        assert_eq!(
            check_repos(&p),
            vec![
                "iov42/version-2-backend".to_string(),
                "iov42/version2-frontend".into()
            ]
        );

        add_repo(dir, &p, "https://github.com/iov42/api-designs").unwrap();
        let p = repo::load(dir).unwrap();
        assert!(p.file.repos.contains(&"iov42/api-designs".to_string()));
        assert!(
            p.file
                .suggested_repos
                .iter()
                .all(|s| s.repo != "iov42/api-designs")
        );
        dismiss_repo(dir, &p, "iov42/version-2-backend").unwrap();
        let p = repo::load(dir).unwrap();
        // A ticket still names it, so it's still suggested.
        assert!(
            repos_of(&p)
                .1
                .iter()
                .any(|r| r.repo == "iov42/version-2-backend")
        );
        assert_eq!(repo_name(" iov42/x.git "), Ok("iov42/x".to_string()));
        assert!(repo_name("not a repo").is_err());
    }

    #[test]
    fn the_last_sync_is_remembered_per_project() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(synced_at(tmp.path(), "o/r", "pg"), None);
        set_synced(tmp.path(), "o/r", "pg", "2026-11-04 12:30").unwrap();
        set_planning_skill(tmp.path(), "o/r", "pg", Some("x")).unwrap();
        assert_eq!(
            synced_at(tmp.path(), "o/r", "pg").as_deref(),
            Some("2026-11-04 12:30")
        );
        assert_eq!(synced_at(tmp.path(), "o/r", "mx"), None);
    }

    #[test]
    fn each_engineer_picks_a_planning_skill_per_project_or_gets_the_built_in_guide() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let state = root.join("state");
        let guide = planning_guide(root, &state, "o/r", "pg").unwrap();
        let PlanningGuide::BuiltIn(path) = &guide else {
            panic!("{guide:?}")
        };
        assert_eq!(path, &state.join("planning-guide/SKILL.md"));
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("name: project-planning")
        );
        assert!(
            state
                .join("planning-guide/checklists/devils-advocate.md")
                .is_file()
        );
        assert!(guide.instruction().contains(&path.display().to_string()));

        set_planning_skill(root, "o/r", "pg", Some("our-planning")).unwrap();
        let guide = planning_guide(root, &state, "o/r", "pg").unwrap();
        assert_eq!(guide, PlanningGuide::Skill("our-planning".into()));
        assert!(guide.instruction().contains("/our-planning skill"));
        // Only that project.
        assert!(matches!(
            planning_guide(root, &state, "o/r", "mx").unwrap(),
            PlanningGuide::BuiltIn(_)
        ));
        set_planning_skill(root, "o/r", "pg", Some("/skills/ours")).unwrap();
        assert_eq!(
            planning_guide(root, &state, "o/r", "pg").unwrap(),
            PlanningGuide::BuiltIn(PathBuf::from("/skills/ours/SKILL.md"))
        );
        set_planning_skill(root, "o/r", "pg", None).unwrap();
        assert_eq!(planning_skill_of(root, "o/r", "pg"), None);
    }

    #[test]
    fn points_are_bullet_lines_only() {
        let texts = |text| points_of(text).map(|ps| ps.into_iter().map(|p| p.text).collect());
        assert_eq!(
            texts("- Why\n\n* Done when\n• Third"),
            Some(vec!["Why".to_string(), "Done when".into(), "Third".into()])
        );
        assert_eq!(texts("A paragraph.\n- then a point"), None);
        assert_eq!(texts(""), None);
        assert_eq!(strip_point("  - x "), "x");
        assert_eq!(strip_point("plain"), "plain");
    }

    #[test]
    fn a_point_keeps_its_explanation_indented_under_it() {
        let points = vec![
            Point {
                text: "Partners see groups".into(),
                detail: "Today they email us.\n\nThis ends that.".into(),
            },
            Point {
                text: "Done when one opens".into(),
                detail: String::new(),
            },
        ];
        let text = points_text(&points);
        assert_eq!(
            text,
            "- Partners see groups\n  Today they email us.\n  \n  This ends that.\n- Done when one opens"
        );
        assert_eq!(points_of(&text), Some(points));
    }

    #[test]
    fn a_paragraph_reads_as_one_point_per_sentence() {
        assert_eq!(
            sentences_of("We know it works. The old page (v1.2) stays!  Done?\nYes"),
            vec![
                "We know it works.",
                "The old page (v1.2) stays!",
                "Done?",
                "Yes"
            ]
        );
    }

    fn project(dir: &Path) -> Project {
        repo::write_project_file(
            dir,
            &ProjectFile {
                name: "Product Groups".into(),
                key: "PG".into(),
                lead: "lead".into(),
                members: vec!["me".into()],
                status: Default::default(),
                repos: vec![],
                suggested_repos: vec![],
                dismissed_repos: vec![],
                spec_change: None,
                spec_note: String::new(),
                linear: Vec::new(),
                sync: Default::default(),
                extra: Default::default(),
            },
        )
        .unwrap();
        repo::load(dir).unwrap()
    }

    fn change(why: &str) -> hydraui_projects::Change {
        hydraui_projects::Change {
            why: why.into(),
            from: "Requirements from Joshua 2026-11-05.md".into(),
            ..Default::default()
        }
    }

    fn ticket_in(dir: &Path, id: &str, status: TicketStatus) -> Ticket {
        let t = Ticket {
            id: id.into(),
            meta: TicketMeta {
                title: format!("{id} title"),
                milestone: "M1".into(),
                status,
                ..Default::default()
            },
            body: "old body".into(),
        };
        repo::write_ticket(dir, &t).unwrap();
        t
    }

    fn milestone_m1(dir: &Path) {
        repo::write_milestones(
            dir,
            &[hydraui_projects::Milestone {
                id: "M1".into(),
                title: "Old".into(),
                description: "- old point".into(),
                ..Default::default()
            }],
        )
        .unwrap();
    }

    #[test]
    fn an_accepted_change_replaces_the_approved_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        project(dir);
        milestone_m1(dir);
        let mut t = ticket_in(dir, "PG-1", TicketStatus::Todo);
        t.meta.change = Some(hydraui_projects::Change {
            title: Some("New title".into()),
            description: Some("new body".into()),
            repo: Some("iov42/fe".into()),
            ..change("Joshua asked for it")
        });
        repo::write_ticket(dir, &t).unwrap();
        let mut ms = repo::load(dir).unwrap().milestones;
        ms[0].change = Some(hydraui_projects::Change {
            title: Some("New".into()),
            ..change("clearer")
        });
        repo::write_milestones(dir, &ms).unwrap();
        let mut f = repo::load(dir).unwrap().file;
        f.spec_change = Some(hydraui_projects::Change {
            description: Some("# New spec".into()),
            ..change("more scope")
        });
        repo::write_project_file(dir, &f).unwrap();
        let p = repo::load(dir).unwrap();
        assert_eq!(
            pending_changes_of(&p)
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["spec", "M1", "PG-1"]
        );

        for id in ["spec", "M1", "PG-1"] {
            accept_change(dir, id, "lead").unwrap();
        }
        let p = repo::load(dir).unwrap();
        assert!(pending_changes_of(&p).is_empty());
        assert_eq!(p.spec.trim(), "# New spec");
        assert_eq!(p.milestones[0].title, "New");
        assert_eq!(p.milestones[0].description, "- old point");
        let t = &p.tickets[0];
        assert_eq!(
            (t.meta.title.as_str(), t.body.trim(), t.meta.repo.as_str()),
            ("New title", "new body", "iov42/fe")
        );
        assert_eq!(t.meta.reviewed_by, "lead");
        assert!(
            accept_change(dir, "PG-1", "lead").is_err(),
            "nothing left to accept"
        );
    }

    #[test]
    fn started_work_is_never_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        project(dir);
        milestone_m1(dir);
        // The ticket started after Claude proposed the change.
        let mut t = ticket_in(dir, "PG-1", TicketStatus::InProgress);
        t.meta.change = Some(hydraui_projects::Change {
            title: Some("New".into()),
            ..change("x")
        });
        repo::write_ticket(dir, &t).unwrap();
        let err = accept_change(dir, "PG-1", "lead").unwrap_err();
        assert!(err.contains("follow-up"), "{err}");
        assert_eq!(repo::load(dir).unwrap().tickets[0].meta.title, "PG-1 title");
        // A milestone with started work can't be dropped either.
        let mut ms = repo::load(dir).unwrap().milestones;
        ms[0].change = Some(hydraui_projects::Change {
            drop: true,
            ..change("not needed")
        });
        repo::write_milestones(dir, &ms).unwrap();
        assert!(accept_change(dir, "M1", "lead").is_err());
    }

    #[test]
    fn dropping_a_milestone_drops_its_tickets_still_to_do() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        project(dir);
        milestone_m1(dir);
        ticket_in(dir, "PG-1", TicketStatus::Todo);
        ticket_in(dir, "PG-2", TicketStatus::Dropped);
        let mut ms = repo::load(dir).unwrap().milestones;
        ms[0].change = Some(hydraui_projects::Change {
            drop: true,
            ..change("Joshua cut it")
        });
        repo::write_milestones(dir, &ms).unwrap();
        accept_change(dir, "M1", "lead").unwrap();
        let p = repo::load(dir).unwrap();
        assert_eq!(p.milestones[0].review, Review::Rejected);
        assert!(p.milestones[0].review_note.contains("Joshua cut it"));
        assert_eq!(p.tickets[0].meta.status, TicketStatus::Dropped);
    }

    #[test]
    fn a_turned_down_change_keeps_its_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        project(dir);
        let mut t = ticket_in(dir, "PG-1", TicketStatus::Todo);
        t.meta.change = Some(change("x"));
        repo::write_ticket(dir, &t).unwrap();
        let mut f = repo::load(dir).unwrap().file;
        f.spec_change = Some(change("y"));
        repo::write_project_file(dir, &f).unwrap();
        assert!(reject_change(dir, "PG-1", " ", "lead").is_err());
        reject_change(dir, "PG-1", "out of scope", "lead").unwrap();
        reject_change(dir, "spec", "keep it short", "lead").unwrap();
        let p = repo::load(dir).unwrap();
        assert!(pending_changes_of(&p).is_empty());
        assert!(p.tickets[0].meta.review_note.contains("out of scope"));
        assert!(p.file.spec_note.contains("keep it short"));
        assert_eq!(p.tickets[0].meta.title, "PG-1 title");
    }

    #[test]
    fn a_diff_marks_kept_removed_and_added_lines() {
        use Diff::*;
        assert_eq!(
            line_diff("a\nb\nc", "a\nB\nc\nd"),
            [Same("a"), Gone("b"), Added("B"), Same("c"), Added("d")]
        );
        assert_eq!(line_diff("", "x"), [Added("x")]);
        assert_eq!(line_diff("x", ""), [Gone("x")]);
    }

    #[test]
    fn linear_is_taken_whole_then_as_changes() {
        use crate::linear_project::{Issue, Snapshot};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        project(dir);
        let issue = |desc: &str| Issue {
            identifier: "MAP-1".into(),
            title: "Upload".into(),
            url: "u".into(),
            state: "Todo".into(),
            description: desc.into(),
            comments: vec![],
        };
        let read = |at: &str, desc: &str| Snapshot {
            name: "Mapping".into(),
            url: "u".into(),
            fetched_at: at.into(),
            issues: vec![issue(desc)],
            ..Default::default()
        };
        let first = take_linear(dir, &read("2026-11-05 09:00", "CSV"))
            .unwrap()
            .unwrap();
        assert_eq!(first, "Linear 2026-11-05.md");
        let text = std::fs::read_to_string(dir.join("resources").join(&first)).unwrap();
        assert!(text.contains("MAP-1 · Upload") && text.contains("Compare it with the plan"));
        // Nothing changed: no requirements, but the read is kept.
        assert_eq!(
            take_linear(dir, &read("2026-11-05 12:30", "CSV")).unwrap(),
            None
        );
        assert_eq!(
            crate::linear_project::load(dir).unwrap().fetched_at,
            "2026-11-05 12:30"
        );
        let next = take_linear(dir, &read("2026-11-05 17:30", "CSV or XLSX"))
            .unwrap()
            .unwrap();
        assert_eq!(next, "Linear 2026-11-05 (2).md");
        let text = std::fs::read_to_string(dir.join("resources").join(&next)).unwrap();
        assert!(text.contains("-CSV\n+CSV or XLSX"), "{text}");
    }

    #[test]
    fn requirements_are_saved_as_a_dated_resource() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        project(dir);
        assert!(add_requirements(dir, "Joshua", "  ").is_err());
        let name = add_requirements(dir, "Joshua", "Export to CSV too.").unwrap();
        assert!(name.starts_with("Requirements from Joshua ") && name.ends_with(".md"));
        let text = std::fs::read_to_string(dir.join("resources").join(&name)).unwrap();
        assert!(text.starts_with("# Requirements from Joshua, ") && text.contains("CSV"));
        assert_eq!(repo::load(dir).unwrap().files, [name]);
    }

    #[test]
    fn the_repo_is_set_per_environment_keeping_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(configured_repo(root, "iov42"), None);
        set_configured_repo(root, "iov42", Some("iov42/iov-projects")).unwrap();
        set_configured_repo(root, "est", Some("/tmp/p")).unwrap();
        let file = config_file(root);
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("# mine\n{text}")).unwrap();
        set_configured_repo(root, "est", None).unwrap();
        assert_eq!(
            configured_repo(root, "iov42").as_deref(),
            Some("iov42/iov-projects")
        );
        assert_eq!(configured_repo(root, "est"), None);
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .starts_with("# mine")
        );
    }

    #[test]
    fn github_links_point_at_the_project_folder() {
        assert_eq!(
            github_url("iov42/iov-projects", "mapping-ux").as_deref(),
            Some("https://github.com/iov42/iov-projects/tree/HEAD/mapping-ux")
        );
        assert_eq!(github_url("/tmp/projects", "x"), None);
    }

    #[test]
    fn only_tracked_projects_count_as_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        for slug in ["sent", "local"] {
            std::fs::create_dir_all(dir.join(slug)).unwrap();
            std::fs::write(dir.join(slug).join("project.toml"), "name = \"x\"").unwrap();
        }
        git(&["add", "sent/project.toml"]);
        assert_eq!(sent_projects(dir).into_iter().collect::<Vec<_>>(), ["sent"]);
    }

    #[test]
    fn github_repos_and_local_paths() {
        assert!(is_github("iov42/iov-projects"));
        assert!(!is_github("/tmp/projects"));
        assert!(!is_github("~/projects"));
        assert!(!is_github("a/b/c"));
        assert!(!is_github("./x"));
        let state = Path::new("/h/state/iov42");
        assert_eq!(
            checkout_dir(state, "iov42/iov-projects"),
            Path::new("/h/state/iov42/projects/iov42-iov-projects")
        );
        assert_eq!(
            checkout_dir(state, "/tmp/sample-projects/"),
            Path::new("/h/state/iov42/projects/sample-projects")
        );
    }

    #[test]
    fn status_paths_include_renames_and_untracked() {
        let text = " M pg/tickets/PG-1.md\0?? pg/decisions/D-2.md\0R  pg/new.md\0pg/old.md\0";
        let paths = parse_status(text);
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            ["pg/decisions/D-2.md", "pg/new.md", "pg/tickets/PG-1.md"]
        );
    }

    #[test]
    fn questions_wait_on_whom_they_are_assigned_to() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        let p = project(&dir);
        ask(&dir, &p, "Postgres?", "", "me").unwrap();
        let p = repo::load(&dir).unwrap();
        ask(&dir, &p, "Name?", "@ME", "lead").unwrap();
        let all = vec![repo::load(&dir).unwrap()];
        let mine = questions_for("me", &all);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].1.meta.question, "Name?");
        assert_eq!(
            questions_for("lead", &all)[0].1.meta.asked_by,
            "me",
            "empty means the lead"
        );
    }

    #[test]
    fn answering_records_who_and_when() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        let p = project(&dir);
        let id = ask(&dir, &p, "Postgres?", "me", "lead").unwrap();
        let q = repo::load(&dir).unwrap().decisions.remove(0);
        assert!(answer(&dir, &q, "  ", "me").is_err());
        answer(&dir, &q, "Yes, Postgres", "me").unwrap();
        let d = repo::load(&dir).unwrap().decisions.remove(0);
        assert_eq!(
            (d.id.as_str(), d.meta.decided_by.as_str()),
            (id.as_str(), "me")
        );
        assert!(!d.is_open() && d.meta.decided_at.is_some());
    }

    #[test]
    fn links_are_web_links_titled_by_default_with_themselves() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        project(&dir);
        assert!(add_link(&dir, "figma.com/x", "Figma").is_err());
        add_link(&dir, " https://figma.com/x ", "Figma: share dialog").unwrap();
        add_link(&dir, "https://docs/y", "").unwrap();
        let links = repo::load(&dir).unwrap().links;
        assert_eq!(links.len(), 2);
        assert_eq!(
            (links[0].title.as_str(), links[0].url.as_str()),
            ("Figma: share dialog", "https://figma.com/x")
        );
        assert_eq!(links[1].title, "https://docs/y");
    }

    #[test]
    fn tickets_are_numbered_moved_and_assigned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        let p = project(&dir);
        assert_eq!(
            new_ticket(&dir, &p, "Schema", "M1", TicketKind::Feature, "me").unwrap(),
            "PG-1"
        );
        let p = repo::load(&dir).unwrap();
        assert_eq!(
            new_ticket(&dir, &p, "Crash", "M1", TicketKind::Bug, "me").unwrap(),
            "PG-2"
        );
        let t = repo::load(&dir).unwrap().tickets.remove(1);
        move_ticket(&dir, &t, TicketStatus::InReview).unwrap();
        let t = repo::load(&dir).unwrap().tickets.remove(1);
        assign_ticket(&dir, &t, "@aza").unwrap();
        let t = repo::load(&dir).unwrap().tickets.remove(1);
        assert_eq!(
            (t.meta.status, t.meta.assignee.as_str()),
            (TicketStatus::InReview, "aza")
        );
    }

    #[test]
    fn a_new_project_gets_a_free_key() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path();
        assert_eq!(
            new_project(checkout, "Product Groups", "me").unwrap(),
            "product-groups"
        );
        assert!(
            new_project(checkout, "product groups", "me").is_err(),
            "same folder"
        );
        assert_eq!(
            new_project(checkout, "Payment Gateway", "me").unwrap(),
            "payment-gateway"
        );
        let p = repo::load(&checkout.join("payment-gateway")).unwrap();
        assert_eq!((p.file.key.as_str(), p.file.lead.as_str()), ("PG2", "me"));
    }

    fn planned(dir: &Path) -> Project {
        let p = project(dir);
        repo::write_milestones(
            dir,
            &[hydraui_projects::Milestone {
                id: "M1".into(),
                title: "Model".into(),
                review: Review::Proposed,
                ..Default::default()
            }],
        )
        .unwrap();
        new_ticket(dir, &p, "Schema", "M1", TicketKind::Feature, "me").unwrap();
        repo::load(dir).unwrap()
    }

    #[test]
    fn reviews_record_who_and_need_a_reason_to_refuse() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        planned(&dir);
        let m1 = Item::Milestone("M1".into());
        assert!(review(&dir, &m1, Review::Rejected, " ", "lead").is_err());
        assert!(review(&dir, &m1, Review::Changes, "", "lead").is_err());
        review(&dir, &m1, Review::Changes, "split it", "lead").unwrap();
        let p = repo::load(&dir).unwrap();
        let m = &p.milestones[0];
        assert_eq!(
            (m.review, m.review_note.as_str(), m.reviewed_by.as_str()),
            (Review::Changes, "split it", "lead")
        );
        assert_eq!(
            to_review(&p),
            (0, 0),
            "changes asked isn't waiting on the lead"
        );
        review(
            &dir,
            &Item::Ticket("PG-1".into()),
            Review::Proposed,
            "",
            "lead",
        )
        .unwrap();
        assert_eq!(to_review(&repo::load(&dir).unwrap()), (0, 1));
    }

    #[test]
    fn work_starts_only_on_approved_tickets_of_approved_milestones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        let p = planned(&dir);
        assert!(
            !can_start(&p, &p.tickets[0]),
            "its milestone is only proposed"
        );
        review(
            &dir,
            &Item::Milestone("M1".into()),
            Review::Approved,
            "",
            "lead",
        )
        .unwrap();
        let p = repo::load(&dir).unwrap();
        assert!(can_start(&p, &p.tickets[0]));
        review(
            &dir,
            &Item::Ticket("PG-1".into()),
            Review::Proposed,
            "",
            "lead",
        )
        .unwrap();
        let p = repo::load(&dir).unwrap();
        assert!(
            !can_start(&p, &p.tickets[0]),
            "the ticket itself is only proposed"
        );
    }

    #[test]
    fn edits_check_their_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        let p = planned(&dir);
        assert!(edit_milestone(&dir, "M1", "Model", "soon", "").is_err());
        edit_milestone(&dir, "M1", "Data model", "2026-12-01", "tables").unwrap();
        assert!(
            edit_ticket(
                &dir,
                &p.tickets[0],
                "Schema v2",
                "the tables",
                "the backend"
            )
            .is_err()
        );
        edit_ticket(
            &dir,
            &p.tickets[0],
            "Schema v2",
            "the tables",
            "https://github.com/iov42/version-2-backend",
        )
        .unwrap();
        let p = repo::load(&dir).unwrap();
        assert_eq!(p.tickets[0].meta.repo, "iov42/version-2-backend");
        assert_eq!(p.milestones[0].target.unwrap().to_string(), "2026-12-01");
        assert_eq!(
            p.milestones[0].review,
            Review::Proposed,
            "an edit isn't an approval"
        );
        assert_eq!(
            (p.tickets[0].meta.title.as_str(), p.tickets[0].body.as_str()),
            ("Schema v2", "the tables\n")
        );
    }

    #[test]
    fn clones_are_remembered_only_for_git_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, clone) = (tmp.path().join("hydra"), tmp.path().join("frontend"));
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        assert!(set_clone(&root, "iov42/fe", tmp.path()).is_err());
        set_clone(&root, "iov42/fe", &clone).unwrap();
        set_configured_repo(&root, "iov42", Some("iov42/iov-projects")).unwrap();
        assert_eq!(clone_of(&root, "iov42/fe"), Some(clone));
        assert_eq!(
            configured_repo(&root, "iov42").as_deref(),
            Some("iov42/iov-projects")
        );
    }

    #[test]
    fn work_branches_carry_the_ticket_key() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        let p = planned(&dir);
        assert_eq!(branch_for(&p.tickets[0]), "pg-1-schema");
        start_work(
            &dir,
            &p.tickets[0],
            "pg-1-schema",
            "me",
            ("angular", "opus", "high"),
        )
        .unwrap();
        let t = repo::load(&dir).unwrap().tickets.remove(0);
        assert_eq!(t.meta.status, TicketStatus::InProgress);
        assert_eq!(
            (
                t.meta.assignee.as_str(),
                t.meta.skill.as_str(),
                t.meta.effort.as_str()
            ),
            ("me", "angular", "high")
        );
    }

    #[test]
    fn sync_times_are_one_or_two_valid_times() {
        assert_eq!(sync_times("17:30, 9:05").unwrap(), ["09:05", "17:30"]);
        assert!(sync_times("").is_err());
        assert!(sync_times("9:00 12:00 18:00").is_err());
        assert!(sync_times("25:00").is_err());
        assert!(sync_times("9:5").is_err());
        assert_eq!(list_of("@aza, me  you"), ["aza", "me", "you"]);
    }

    #[test]
    fn ages_read_shortly() {
        let t: toml::value::Datetime = "2026-11-03T10:00:00Z".parse().unwrap();
        let base = unix_of(&t).unwrap();
        assert_eq!(age(&t, base + 30), "1m");
        assert_eq!(age(&t, base + 3 * 3600), "3h");
        assert_eq!(age(&t, base + 2 * 86_400), "2d");
        let date: toml::value::Datetime = "2026-11-03".parse().unwrap();
        assert_eq!(unix_of(&date), Some(base - 10 * 3600));
    }

    #[test]
    fn a_ticket_with_no_repo_gets_a_worktree_of_every_repo_on_its_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        let repos: Vec<RepoClone> = ["backend", "frontend"]
            .iter()
            .map(|name| {
                let clone = tmp.path().join(name);
                std::fs::create_dir_all(&clone).unwrap();
                git(&clone, &["init", "-q"]);
                git(&clone, &["commit", "-q", "--allow-empty", "-m", "start"]);
                RepoClone { clone, base: None }
            })
            .collect();
        let t = Ticket {
            id: "PG-2".into(),
            meta: TicketMeta {
                title: "Group page".into(),
                ..Default::default()
            },
            body: String::new(),
        };
        let none = Path::new("/no/hydra");
        let (folder, branch) = ticket_workspace(none, "dev", "pg", &repos, &t).unwrap();
        assert_eq!(branch, "pg-2-group-page");
        assert_eq!(folder, tmp.path().join("pg-tickets").join(&branch));
        for name in ["backend", "frontend"] {
            let head = std::fs::read_to_string(folder.join(name).join(".git")).unwrap();
            assert!(head.starts_with("gitdir:"), "{name} is a worktree");
            git(
                &folder.join(name),
                &["rev-parse", "--verify", "refs/heads/pg-2-group-page"],
            );
        }
        // Run again (a restarted session), it reuses them.
        assert_eq!(
            ticket_workspace(none, "dev", "pg", &repos, &t).unwrap().0,
            folder
        );
    }

    #[test]
    fn a_run_folder_is_trusted_without_losing_claudes_other_state() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(".claude.json");
        let folder = Path::new("/Users/me/Projects/pg-tickets/pg-2-group-page");
        // No state file yet: nothing to do.
        trust_folder(&file, folder).unwrap();
        assert!(!file.exists());
        std::fs::write(
            &file,
            r#"{"oauthAccount":{"emailAddress":"a@b"},"projects":{"/x":{"hasTrustDialogAccepted":true},"/Users/me/Projects/pg-tickets/pg-2-group-page":{"allowedTools":["Bash"]}}}"#,
        )
        .unwrap();
        trust_folder(&file, folder).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let entry = &v["projects"][folder.display().to_string()];
        assert_eq!(entry["hasTrustDialogAccepted"], true);
        assert_eq!(entry["allowedTools"][0], "Bash");
        assert_eq!(v["projects"]["/x"]["hasTrustDialogAccepted"], true);
        assert_eq!(v["oauthAccount"]["emailAddress"], "a@b");
        // A file that doesn't read is left alone, with an error.
        std::fs::write(&file, "{nope").unwrap();
        assert!(trust_folder(&file, folder).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{nope");
    }

    #[test]
    fn a_tickets_outcome_is_read_apart_from_its_brief() {
        let body = "**Why:** x\n\n**What to do**\n- a\n\n## Work\n\nFound three gaps.\nMore.\n\n\
                    ## Check\n\nPassed: fine\n\n## Work\n\nFixed the gaps.\n";
        let s = ticket_sections(body);
        assert_eq!(s.brief, "**Why:** x\n\n**What to do**\n- a");
        assert_eq!(s.work, ["Found three gaps.\nMore.", "Fixed the gaps."]);
        assert_eq!(s.check, ["Passed: fine"]);
        assert_eq!(headline(&s.work[0]), "Found three gaps.");
        // A ticket nobody has worked yet is all brief.
        assert_eq!(
            ticket_sections("Just the ask."),
            TicketSections {
                brief: "Just the ask.".into(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn an_outcome_summary_asks_for_the_four_headings_and_is_kept_per_report() {
        let t = Ticket {
            id: "PG-1".into(),
            meta: TicketMeta {
                title: "Check the list".into(),
                prs: vec!["https://github.com/o/r/pull/1".into()],
                ..Default::default()
            },
            body: "Ask.\n\n## Work\n\nFound two gaps.\n\n## Check\n\nPassed: ok\n".into(),
        };
        let outcome = outcome_text(&t);
        assert!(
            outcome.contains("Found two gaps.")
                && outcome.contains("Passed: ok")
                && outcome.contains("pull/1")
                && !outcome.contains("Ask."),
            "{outcome}"
        );
        let prompt = summary_prompt(&t, &outcome);
        for h in SUMMARY_HEADINGS {
            assert!(prompt.contains(&format!("## {h}")), "{prompt}");
        }
        assert!(prompt.contains("Found two gaps.") && prompt.contains("PG-1"));

        let tmp = tempfile::tempdir().unwrap();
        let file = summary_file(tmp.path(), "dev", "pg", "PG-1");
        assert_eq!(cached_summary(&file, &outcome), None);
        save_summary(&file, &outcome, "## What it did\n- Read code\n").unwrap();
        assert_eq!(
            cached_summary(&file, &outcome).as_deref(),
            Some("## What it did\n- Read code")
        );
        // A new report (the session ran again) needs a new summary.
        assert_eq!(cached_summary(&file, &format!("{outcome}\nMore.")), None);
        // Nothing reported: nothing to summarise.
        let fresh = Ticket {
            body: "Ask.".into(),
            meta: TicketMeta::default(),
            ..t
        };
        assert_eq!(outcome_text(&fresh), "");
    }
}
