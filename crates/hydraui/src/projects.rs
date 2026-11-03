//! Projects (docs/superpowers/specs/2026-11-03-hydraui-projects-design.md), apart from the
//! view: hydra's checkout of an environment's projects repo, the edits the tab makes, and what
//! waits on whom. Slice 1 leaves edits uncommitted in the checkout: the pending change is the
//! working tree's diff (plan, slice 1).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hydraui_projects::{
    Decision, DecisionMeta, Project, ProjectFile, Ticket, TicketKind, TicketMeta, TicketStatus,
    next_id, progress, repo,
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
            extra: Default::default(),
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

/// Starts a draft for a project called `name`, led by `login` (spec §9.1). Returns its folder.
pub fn start_draft(drafts: &Path, name: &str, login: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    let slug = repo::slug_for(name);
    if slug.is_empty() {
        return Err("Give the project a name with letters or numbers in it.".into());
    }
    let dir = drafts.join(&slug);
    if dir.exists() {
        return Err(format!(
            "There's already a draft called {slug}: open it, or discard it first."
        ));
    }
    let file = ProjectFile {
        name: name.to_string(),
        key: repo::key_for(name),
        lead: login.to_string(),
        members: vec![login.to_string()],
        status: Default::default(),
        repos: Vec::new(),
        sync: Default::default(),
        extra: Default::default(),
    };
    repo::write_project_file(&dir, &file).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Moves draft `draft` into the checkout `checkout` as a project (spec §9.1's Create). Returns
/// its slug. Refused if the checkout already has a project by that name or key.
pub fn create_from_draft(draft: &Path, checkout: &Path) -> Result<String, String> {
    let p = repo::load(draft).map_err(|e| e.to_string())?;
    let target = checkout.join(&p.slug);
    if target.exists() {
        return Err(format!("The repo already has a project folder {}.", p.slug));
    }
    for (slug, other) in repo::load_all(checkout) {
        if let Ok(other) = other
            && other.file.key.eq_ignore_ascii_case(&p.file.key)
        {
            return Err(format!(
                "{slug} already uses the key {}; change this project's key first.",
                p.file.key
            ));
        }
    }
    std::fs::create_dir_all(checkout).map_err(|e| e.to_string())?;
    if std::fs::rename(draft, &target).is_err() {
        copy_dir(draft, &target).map_err(|e| e.to_string())?;
        std::fs::remove_dir_all(draft).map_err(|e| e.to_string())?;
    }
    Ok(p.slug)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
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
    fn a_draft_becomes_a_project_once() {
        let tmp = tempfile::tempdir().unwrap();
        let drafts = tmp.path().join("drafts");
        let checkout = tmp.path().join("checkout");
        let draft = start_draft(&drafts, "Product Groups", "me").unwrap();
        assert!(
            start_draft(&drafts, "product groups", "me").is_err(),
            "same slug"
        );
        assert_eq!(
            create_from_draft(&draft, &checkout).unwrap(),
            "product-groups"
        );
        assert!(!draft.exists());
        let p = repo::load(&checkout.join("product-groups")).unwrap();
        assert_eq!((p.file.key.as_str(), p.file.lead.as_str()), ("PG", "me"));
        // Another project with the same key is refused.
        let again = start_draft(&drafts, "Payment Gateway", "me").unwrap();
        assert!(
            create_from_draft(&again, &checkout)
                .unwrap_err()
                .contains("key PG")
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
