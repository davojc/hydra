//! Projects (docs/superpowers/specs/2026-11-03-hydraui-projects-design.md), apart from the
//! view: hydra's checkout of an environment's projects repo, the edits the tab makes, and what
//! waits on whom. Slice 1 leaves edits uncommitted in the checkout: the pending change is the
//! working tree's diff (plan, slice 1).

use std::collections::BTreeSet;
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
        return Ok(());
    }
    // Nothing on the remote yet (a new repo): no branch to pull.
    let heads = env_git(hydra, env, &["-C", &path, "ls-remote", "--heads", "origin"])?;
    if heads.is_empty() {
        return Ok(());
    }
    env_git(
        hydra,
        env,
        &["-C", &path, "pull", "--ff-only", "--autostash"],
    )
    .map(|_| ())
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
fn parse_status(text: &str) -> BTreeSet<String> {
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
        return Err("Write the question first.".into());
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
        sync: Default::default(),
        extra: Default::default(),
    };
    repo::write_project_file(&checkout.join(&slug), &file).map_err(|e| e.to_string())?;
    Ok(slug)
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
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Give the ticket a title.".into());
    }
    let mut t = ticket.clone();
    t.meta.title = title.to_string();
    t.body = description.trim().to_string();
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

/// Makes a worktree of `clone` for work on `ticket`: branch [`branch_for`] from the remote's
/// default branch (fetched first, as `env`), in `<clone>-worktrees/<branch>`. An existing
/// worktree on that branch is reused. Returns its folder and branch. Blocks.
pub fn ticket_worktree(
    hydra: &Path,
    env: &str,
    clone: &Path,
    ticket: &Ticket,
) -> Result<(PathBuf, String), String> {
    let branch = branch_for(ticket);
    let name = clone
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("the clone has no folder name")?;
    let parent = clone.parent().ok_or("the clone has no parent folder")?;
    let path = parent.join(format!("{name}-worktrees")).join(&branch);
    if path.join(".git").exists() {
        return Ok((path, branch));
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
    let base =
        local(&["rev-parse", "--abbrev-ref", "origin/HEAD"]).unwrap_or_else(|_| "HEAD".into());
    let target = path.display().to_string();
    let head = format!("refs/heads/{branch}");
    if local(&["rev-parse", "--verify", "--quiet", &head]).is_ok() {
        local(&["worktree", "add", &target, &branch])?;
    } else {
        local(&["worktree", "add", "-b", &branch, &target, &base])?;
    }
    Ok((path, branch))
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
                sync: Default::default(),
                extra: Default::default(),
            },
        )
        .unwrap();
        repo::load(dir).unwrap()
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
        edit_ticket(&dir, &p.tickets[0], "Schema v2", "the tables").unwrap();
        let p = repo::load(&dir).unwrap();
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
}
