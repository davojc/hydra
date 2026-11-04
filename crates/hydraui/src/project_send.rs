//! Sending a project's changes to the projects repo, and the lead approving other people's
//! (spec §5).
//!
//! The checkout's working tree holds each engineer's pending change (plan, slice 1). A send
//! rebuilds the project's part of it on the latest default branch, file by file: a file nobody
//! else changed goes as it is, a text file both sides changed is merged three ways, and
//! anything else is a clash for the engineer to settle (§5.3). The result is one commit on the
//! engineer's branch for the project, behind one open pull request. The lead's own merges at
//! once; anyone else's waits for the lead's Approve or Send back (§5.2).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hydraui_projects::front_matter;
use serde::Deserialize;

/// How long a call to GitHub may take.
const NETWORK_LIMIT: Duration = Duration::from_secs(120);

/// What the README of a new projects repo says.
/// What a new projects repo starts with: its README, and the merge gate that lets anyone start a
/// project but only its lead change it (spec §14.4). `{admins}` in CODEOWNERS becomes the
/// person who starts the repo. The gate's own tests stay in hydra.
pub const REPO_FILES: [(&str, &str); 5] = [
    ("README.md", include_str!("projects_repo/README.md")),
    (
        ".github/CODEOWNERS",
        include_str!("projects_repo/.github/CODEOWNERS"),
    ),
    (
        ".github/workflows/projects-gate.yml",
        include_str!("projects_repo/.github/workflows/projects-gate.yml"),
    ),
    (
        ".github/scripts/projects_gate.py",
        include_str!("projects_repo/.github/scripts/projects_gate.py"),
    ),
    (
        ".github/projects-ruleset.json",
        include_str!("projects_repo/.github/projects-ruleset.json"),
    ),
];

/// Runs git and gh for a send. The real one runs local git directly and anything that talks
/// to GitHub as the environment; tests use a fake GitHub.
pub trait Runner {
    /// git in `dir`, on this machine only; `index` points git at another index file.
    fn git(&self, dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String>;
    /// git in `dir` that talks to the remote, as the environment.
    fn remote_git(&self, dir: &Path, args: &[&str]) -> Result<String, String>;
    /// gh, as the environment.
    fn gh(&self, args: &[&str]) -> Result<String, String>;
}

/// The real [`Runner`]: `hydra run <env> --` for the network, run from the checkout `cwd`.
pub struct EnvRunner<'a> {
    pub hydra: &'a Path,
    pub env: &'a str,
    pub cwd: &'a Path,
}

impl Runner for EnvRunner<'_> {
    fn git(&self, dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null());
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        let out = hydra_platform::process::hide_window(&mut cmd)
            .output()
            .map_err(|e| format!("git didn't run: {e}"))?;
        finish(out, "git", args)
    }

    fn remote_git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let mut cmd = std::process::Command::new(self.hydra);
        cmd.args(["run", self.env, "--", "git", "-C"])
            .arg(dir)
            .args(args)
            .current_dir(self.cwd);
        hydra_platform::process::hide_window(&mut cmd);
        finish(
            crate::branch_rule::output_within(cmd, NETWORK_LIMIT)?,
            "git",
            args,
        )
    }

    fn gh(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = std::process::Command::new(self.hydra);
        cmd.args(["run", self.env, "--", "gh"])
            .args(args)
            .current_dir(self.cwd);
        hydra_platform::process::hide_window(&mut cmd);
        finish(
            crate::branch_rule::output_within(cmd, NETWORK_LIMIT)?,
            "gh",
            args,
        )
    }
}

/// A [`Runner`] for reading the checkout only, with no environment: nothing reaches GitHub.
pub struct Local;

impl Runner for Local {
    fn git(&self, dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
        EnvRunner {
            hydra: Path::new("hydra"),
            env: "",
            cwd: dir,
        }
        .git(dir, args, index)
    }

    fn remote_git(&self, _: &Path, _: &[&str]) -> Result<String, String> {
        Err("reading only".into())
    }

    fn gh(&self, _: &[&str]) -> Result<String, String> {
        Err("reading only".into())
    }
}

/// `out`'s stdout as it is, or its stderr as the error.
fn finish(out: std::process::Output, program: &str, args: &[&str]) -> Result<String, String> {
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!(
            "{program} {} failed",
            args.first().copied().unwrap_or_default()
        )
    } else {
        err
    })
}

/// An open pull request from hydraui (its branch is `hydra/<login>/<slug>/<date>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPr {
    pub url: String,
    pub title: String,
    pub body: String,
    pub author: String,
    pub branch: String,
    /// The project it changes.
    pub slug: String,
    /// The lead's note when they sent it back, until it's approved.
    pub sent_back: Option<String>,
}

/// The `--json` fields [`parse_prs`] reads.
const PR_FIELDS: &str = "url,title,body,author,headRefName,reviewDecision,latestReviews";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    url: String,
    title: String,
    #[serde(default)]
    body: String,
    author: Login,
    head_ref_name: String,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    latest_reviews: Vec<RawReview>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct RawReview {
    state: String,
    #[serde(default)]
    body: String,
}

/// hydraui's pull requests in `gh pr list --json` output; others are left out.
pub fn parse_prs(json: &str) -> Result<Vec<OpenPr>, String> {
    let raw: Vec<RawPr> = serde_json::from_str(json)
        .map_err(|e| format!("gh gave pull requests hydraui can't read: {e}"))?;
    Ok(raw
        .into_iter()
        .filter_map(|p| {
            let mut parts = p.head_ref_name.split('/');
            let slug = (parts.next() == Some("hydra"))
                .then(|| parts.nth(1))
                .flatten()?
                .to_string();
            let sent_back =
                (p.review_decision.as_deref() == Some("CHANGES_REQUESTED")).then(|| {
                    p.latest_reviews
                        .iter()
                        .rev()
                        .find(|r| r.state == "CHANGES_REQUESTED")
                        .map(|r| r.body.trim().to_string())
                        .unwrap_or_default()
                });
            Some(OpenPr {
                url: p.url,
                title: p.title,
                body: p.body,
                author: p.author.login,
                branch: p.head_ref_name,
                slug,
                sent_back,
            })
        })
        .collect())
}

/// hydraui's open pull requests in `repo`.
pub fn open_prs(r: &dyn Runner, repo: &str) -> Result<Vec<OpenPr>, String> {
    let json = r.gh(&[
        "pr", "list", "--repo", repo, "--state", "open", "--limit", "100", "--json", PR_FIELDS,
    ])?;
    parse_prs(&json)
}

/// What a send did.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    /// Nothing differs from the repo.
    Nothing,
    /// Sent; the pull request waits for the lead.
    Waiting(String),
    /// Sent and merged: the lead's own change.
    Merged(String),
    /// The lead's own change, sent and set to merge as soon as the repo's gate check passes.
    Queued(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// Files changed here and in the repo in ways that can't be merged: the engineer picks.
    Clash(Vec<String>),
    Failed(String),
}

impl From<String> for SendError {
    fn from(e: String) -> Self {
        SendError::Failed(e)
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Clash(paths) => write!(
                f,
                "{} changed here and in the repo: pick which to keep",
                paths.join(", ")
            ),
            SendError::Failed(e) => f.write_str(e),
        }
    }
}

/// One project's send.
pub struct Send<'a> {
    /// `owner/name`.
    pub repo: &'a str,
    /// The checkout.
    pub dir: &'a Path,
    pub slug: &'a str,
    pub name: &'a str,
    pub lead: &'a str,
    /// Who sends.
    pub login: &'a str,
    /// `YYYY-MM-DD`, for a new branch's name.
    pub today: &'a str,
    /// Clashing files the engineer chose to keep their own version of.
    pub mine: &'a BTreeSet<String>,
}

/// Sends project `s.slug`'s pending change (spec §5.1): rebuilt on the latest default branch,
/// pushed to the sender's branch for it, with its pull request opened or updated, and merged
/// at once when the sender leads the project. Blocks.
pub fn send(r: &dyn Runner, s: &Send) -> Result<Sent, SendError> {
    let dir = s.dir;
    r.remote_git(dir, &["fetch", "origin", "--prune"])?;
    let default = match default_branch(r, dir)? {
        Some(b) => b,
        None => start_repo(r, dir, s.login)?,
    };
    let upstream = format!("origin/{default}");
    let has_head = rev(r, dir, "HEAD").is_some();
    let prefix = format!("{}/", s.slug);
    let paths: Vec<String> = pending_in(r, dir)?
        .into_iter()
        .filter(|p| p.starts_with(&prefix))
        .collect();

    let index = git_dir(dir).join("hydra-send.index");
    let _ = std::fs::remove_file(&index);
    r.git(dir, &["read-tree", &upstream], Some(&index))?;
    let mut clashes = Vec::new();
    for path in &paths {
        let theirs = rev(r, dir, &format!("{upstream}:{path}"));
        let base = has_head
            .then(|| rev(r, dir, &format!("HEAD:{path}")))
            .flatten();
        let mut ours = work_blob(r, dir, path, true)?;
        if theirs != base && theirs != ours && !s.mine.contains(path) {
            match merge3(r, dir, path, base.as_deref(), theirs.as_deref()) {
                Some(merged) => ours = Some(merged),
                None => {
                    clashes.push(path.clone());
                    continue;
                }
            }
        }
        match &ours {
            Some(sha) => r.git(
                dir,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{sha},{path}"),
                ],
                Some(&index),
            )?,
            None => r.git(
                dir,
                &["update-index", "--force-remove", "--", path],
                Some(&index),
            )?,
        };
    }
    if !clashes.is_empty() {
        let _ = std::fs::remove_file(&index);
        return Err(SendError::Clash(clashes));
    }
    let tree = line(r.git(dir, &["write-tree"], Some(&index))?);
    let _ = std::fs::remove_file(&index);
    let up_tree = line(r.git(dir, &["rev-parse", &format!("{upstream}^{{tree}}")], None)?);
    if tree == up_tree {
        settle(r, dir, &default)?;
        forget_sent(dir, s.slug);
        return Ok(Sent::Nothing);
    }

    let changes = r.git(
        dir,
        &["diff-tree", "-r", "--name-status", &up_tree, &tree],
        None,
    )?;
    let (title, body) = describe(s.name, s.login, &changes, Some(dir));
    let commit = line(r.git(
        dir,
        &[
            "commit-tree",
            &tree,
            "-p",
            &upstream,
            "-m",
            &title,
            "-m",
            &body,
        ],
        None,
    )?);
    let open = open_prs(r, s.repo)?
        .into_iter()
        .find(|p| p.slug == s.slug && p.author.eq_ignore_ascii_case(s.login));
    let branch = open
        .as_ref()
        .map(|p| p.branch.clone())
        .unwrap_or_else(|| format!("hydra/{}/{}/{}", s.login, s.slug, s.today));
    push(r, dir, &format!("{commit}:refs/heads/{branch}"), true)?;
    let url = match &open {
        Some(p) => {
            r.gh(&["pr", "edit", &p.url, "--title", &title, "--body", &body])?;
            p.url.clone()
        }
        None => {
            let out = r.gh(&[
                "pr", "create", "--repo", s.repo, "--base", &default, "--head", &branch, "--title",
                &title, "--body", &body,
            ])?;
            out.lines()
                .map(str::trim)
                .rfind(|l| l.starts_with("http"))
                .ok_or_else(|| format!("gh opened no pull request: {}", out.trim()))?
                .to_string()
        }
    };
    if lead_on(r, dir, &upstream, s.slug)
        .as_deref()
        .unwrap_or(s.lead)
        .eq_ignore_ascii_case(s.login)
    {
        if !merge(r, dir, &url, &branch)? {
            remember_sent(r, dir, s.slug);
            return Ok(Sent::Queued(url));
        }
        settle(r, dir, &default)?;
        forget_sent(dir, s.slug);
        return Ok(Sent::Merged(url));
    }
    remember_sent(r, dir, s.slug);
    Ok(Sent::Waiting(url))
}

/// Whether a push failed on GitHub's side (a server error), so trying again may work.
fn is_transient(err: &str) -> bool {
    [
        "Internal Server Error",
        "HTTP 500",
        "HTTP 502",
        "HTTP 503",
        "HTTP 504",
        "Bad Gateway",
        "Service Unavailable",
    ]
    .iter()
    .any(|s| err.contains(s))
}

/// Pushes `refspec` to origin (`force` replaces the branch), trying again twice when GitHub
/// fails on its side: a new repo answered its first pushes with Internal Server Error.
fn push(r: &dyn Runner, dir: &Path, refspec: &str, force: bool) -> Result<(), String> {
    let mut args = vec!["push"];
    if force {
        args.push("--force");
    }
    args.extend(["origin", refspec]);
    let mut tries = 0;
    loop {
        match r.remote_git(dir, &args) {
            Ok(_) => return Ok(()),
            Err(e) if tries < 2 && is_transient(&e) => {
                tries += 1;
                std::thread::sleep(Duration::from_secs(tries));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Project `slug`'s lead as `upstream` has it, or None for a project the repo doesn't have yet
/// (a new project: its creator leads it). Who may merge is decided from the repo, never from
/// the sender's own copy, where `lead` could be anyone.
fn lead_on(r: &dyn Runner, dir: &Path, upstream: &str, slug: &str) -> Option<String> {
    let text = r
        .git(
            dir,
            &["show", &format!("{upstream}:{slug}/project.toml")],
            None,
        )
        .ok()?;
    // In the repo but unreadable: nobody counts as its lead, rather than whoever sends.
    Some(
        toml::from_str::<hydraui_projects::ProjectFile>(&text)
            .map(|f| f.lead)
            .unwrap_or_default(),
    )
}

/// Squash-merges pull request `url` and deletes its `branch`.
/// Whether gh refused a merge because the repo's rules aren't met yet (the gate check still
/// running), rather than for good.
fn merge_waits(err: &str) -> bool {
    [
        "base branch policy",
        "required status check",
        "Required status check",
        "is not mergeable",
        "Merging is blocked",
    ]
    .iter()
    .any(|s| err.contains(s))
}

/// Squash-merges pull request `url` and deletes its `branch`: true when it merged now. When
/// the repo's gate check hasn't passed yet, it's queued to merge as soon as it does (GitHub's
/// auto-merge) and this returns false.
fn merge(r: &dyn Runner, dir: &Path, url: &str, branch: &str) -> Result<bool, String> {
    if let Err(e) = r.gh(&["pr", "merge", url, "--squash"]) {
        if !merge_waits(&e) {
            return Err(e);
        }
        r.gh(&["pr", "merge", url, "--squash", "--auto"])
            .map_err(|auto| format!("{e}; and it can't merge when ready: {auto}"))?;
        return Ok(false);
    }
    // Best effort: a leftover branch is harmless; the next send of the day reuses the name.
    let _ = r.remote_git(dir, &["push", "origin", "--delete", branch]);
    r.remote_git(dir, &["fetch", "origin", "--prune"])?;
    Ok(true)
}

/// The lead approves `pr` and merges it (spec §5.2). Blocks.
/// Returns true when it merged now, false when it's queued for the repo's gate check.
pub fn approve(r: &dyn Runner, dir: &Path, pr: &OpenPr) -> Result<bool, String> {
    r.gh(&["pr", "review", &pr.url, "--approve"])?;
    if !merge(r, dir, &pr.url, &pr.branch)? {
        return Ok(false);
    }
    let default = default_branch(r, dir)?.ok_or("the repo has no default branch")?;
    settle(r, dir, &default)?;
    Ok(true)
}

/// The lead sends `pr` back to its author with `note` (spec §5.2). Blocks.
pub fn send_back(r: &dyn Runner, pr: &OpenPr, note: &str) -> Result<(), String> {
    r.gh(&["pr", "review", &pr.url, "--request-changes", "--body", note])
        .map(|_| ())
}

/// Brings the checkout up to the default branch, keeping what isn't in it yet: a pending file
/// that's now the same in the repo (sent and merged) is dropped first, so it comes back from
/// the repo rather than clashing with it. Blocks.
pub fn settle(r: &dyn Runner, dir: &Path, default: &str) -> Result<(), String> {
    let upstream = format!("origin/{default}");
    if rev(r, dir, &upstream).is_none() {
        return Ok(());
    }
    let has_head = rev(r, dir, "HEAD").is_some();
    for path in pending_in(r, dir)? {
        let theirs = rev(r, dir, &format!("{upstream}:{path}"));
        if theirs != work_blob(r, dir, &path, false)? {
            continue;
        }
        if has_head && rev(r, dir, &format!("HEAD:{path}")).is_some() {
            r.git(dir, &["checkout", "HEAD", "--", &path], None)?;
        } else if dir.join(&path).is_file() {
            std::fs::remove_file(dir.join(&path)).map_err(|e| e.to_string())?;
        }
    }
    if has_head {
        r.git(
            dir,
            &["merge", "--ff-only", "--autostash", "-q", &upstream],
            None,
        )?;
    } else {
        r.git(
            dir,
            &["checkout", "-q", "-B", default, "--track", &upstream],
            None,
        )?;
    }
    Ok(())
}

/// Settles a clash on `path` the repo's way: the repo's version replaces this one. Blocks.
pub fn take_theirs(r: &dyn Runner, dir: &Path, path: &str) -> Result<(), String> {
    let default = default_branch(r, dir)?.ok_or("the repo is empty")?;
    let upstream = format!("origin/{default}");
    if rev(r, dir, &format!("{upstream}:{path}")).is_some() {
        r.git(dir, &["checkout", &upstream, "--", path], None)?;
    } else if dir.join(path).is_file() {
        std::fs::remove_file(dir.join(path)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The remote's default branch, or None when the repo is still empty.
pub fn default_branch(r: &dyn Runner, dir: &Path) -> Result<Option<String>, String> {
    let out = r.remote_git(dir, &["ls-remote", "--symref", "origin", "HEAD"])?;
    Ok(out.lines().find_map(|l| {
        l.strip_prefix("ref: refs/heads/")
            .and_then(|rest| rest.split('\t').next())
            .map(str::to_string)
    }))
}

/// Gives an empty repo its first commit on `main` ([`REPO_FILES`]: README and merge gate),
/// with `login` as the admin who owns the gate, and returns `main`.
fn start_repo(r: &dyn Runner, dir: &Path, login: &str) -> Result<String, String> {
    let index = git_dir(dir).join("hydra-send.index");
    let _ = std::fs::remove_file(&index);
    let tmp = git_dir(dir).join("hydra-repo-file");
    for (path, text) in REPO_FILES {
        std::fs::write(&tmp, text.replace("{admins}", &format!("@{login}")))
            .map_err(|e| e.to_string())?;
        let sha = line(r.git(
            dir,
            &["hash-object", "-w", &tmp.display().to_string()],
            None,
        )?);
        r.git(
            dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{sha},{path}"),
            ],
            Some(&index),
        )?;
    }
    let _ = std::fs::remove_file(&tmp);
    let tree = line(r.git(dir, &["write-tree"], Some(&index))?);
    let _ = std::fs::remove_file(&index);
    let commit = line(r.git(
        dir,
        &["commit-tree", &tree, "-m", "Start the projects repo"],
        None,
    )?);
    push(r, dir, &format!("{commit}:refs/heads/main"), false)?;
    r.remote_git(dir, &["fetch", "origin"])?;
    Ok("main".into())
}

/// Merges `path` three ways (base, the repo's, ours in the working tree), writes the result to
/// the working tree and returns its blob; None when it can't: a side is missing, it isn't
/// text, or the same lines changed both ways.
fn merge3(
    r: &dyn Runner,
    dir: &Path,
    path: &str,
    base: Option<&str>,
    theirs: Option<&str>,
) -> Option<String> {
    let (base, theirs) = (base?, theirs?);
    let ours = std::fs::read(dir.join(path)).ok()?;
    let tmp = git_dir(dir).join("hydra-merge");
    std::fs::create_dir_all(&tmp).ok()?;
    let (o, b, t) = (tmp.join("ours"), tmp.join("base"), tmp.join("theirs"));
    let text = |sha: &str| r.git(dir, &["cat-file", "blob", sha], None).ok();
    let (base_text, their_text) = (text(base)?, text(theirs)?);
    if [ours.as_slice(), base_text.as_bytes(), their_text.as_bytes()]
        .iter()
        .any(|b| b.contains(&0))
    {
        return None;
    }
    std::fs::write(&o, &ours).ok()?;
    std::fs::write(&b, base_text).ok()?;
    std::fs::write(&t, their_text).ok()?;
    let show = |p: &PathBuf| p.display().to_string();
    let merged = r
        .git(
            dir,
            &["merge-file", "-q", &show(&o), &show(&b), &show(&t)],
            None,
        )
        .ok()
        .and_then(|_| std::fs::read(&o).ok());
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::write(dir.join(path), merged?).ok()?;
    work_blob(r, dir, path, true).ok().flatten()
}

/// The files changed in the working tree (a rename's new path).
fn pending_in(r: &dyn Runner, dir: &Path) -> Result<BTreeSet<String>, String> {
    let out = r.git(dir, &["status", "--porcelain", "-uall", "-z"], None)?;
    Ok(crate::projects::parse_status(&out))
}

/// The object `spec` names, if any.
fn rev(r: &dyn Runner, dir: &Path, spec: &str) -> Option<String> {
    r.git(dir, &["rev-parse", "-q", "--verify", spec], None)
        .ok()
        .map(line)
}

/// The blob of `path` as it is in the working tree (stored when `write`), or None if it's gone.
fn work_blob(
    r: &dyn Runner,
    dir: &Path,
    path: &str,
    write: bool,
) -> Result<Option<String>, String> {
    if !dir.join(path).is_file() {
        return Ok(None);
    }
    let mut args = vec!["hash-object"];
    if write {
        args.push("-w");
    }
    args.extend(["--", path]);
    r.git(dir, &args, None).map(|o| Some(line(o)))
}

fn git_dir(dir: &Path) -> PathBuf {
    dir.join(".git")
}

fn line(s: String) -> String {
    s.trim().to_string()
}

// ---- What was sent, so the page can say "waiting for the lead" (spec §8.2) ----

fn sent_file(dir: &Path, slug: &str) -> PathBuf {
    git_dir(dir).join("hydra-sent").join(slug)
}

/// The project's pending files with their contents' hashes, one per line.
fn fingerprint(r: &dyn Runner, dir: &Path, slug: &str) -> Option<String> {
    let prefix = format!("{slug}/");
    let mut out = String::new();
    for path in pending_in(r, dir).ok()? {
        if path.starts_with(&prefix) {
            let blob = work_blob(r, dir, &path, false).ok()?;
            out.push_str(&format!("{path} {}\n", blob.as_deref().unwrap_or("-")));
        }
    }
    Some(out)
}

fn remember_sent(r: &dyn Runner, dir: &Path, slug: &str) {
    if let Some(print) = fingerprint(r, dir, slug) {
        let file = sent_file(dir, slug);
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(file, print);
    }
}

fn forget_sent(dir: &Path, slug: &str) {
    let _ = std::fs::remove_file(sent_file(dir, slug));
}

/// Whether project `slug`'s pending change is exactly what was last sent for the lead.
pub fn is_sent(r: &dyn Runner, dir: &Path, slug: &str) -> bool {
    let Ok(sent) = std::fs::read_to_string(sent_file(dir, slug)) else {
        return false;
    };
    fingerprint(r, dir, slug).is_some_and(|now| !now.is_empty() && now == sent)
}

// ---- The pull request's words ----

/// One changed file of a project in words: `status` is git's letter (`A`, `M` or `D`), and
/// `path` starts with the project's folder. With the checkout `dir`, a new decision that's
/// already answered says so.
fn change_words(status: &str, path: &str, dir: Option<&Path>) -> String {
    let rest = path.split_once('/').map(|(_, r)| r).unwrap_or(path);
    let (added, removed) = (status.starts_with('A'), status.starts_with('D'));
    let stem = |p: &str| {
        Path::new(p)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let verb = if added {
        "added"
    } else if removed {
        "removed"
    } else {
        "updated"
    };
    match rest {
        "project.toml" if added => "Project created".to_string(),
        "project.toml" => "Project settings changed".to_string(),
        "spec.md" if added => "Spec written".to_string(),
        "spec.md" => "Spec updated".to_string(),
        "milestones.toml" if added => "Milestones proposed".to_string(),
        "milestones.toml" => "Milestones updated".to_string(),
        "resources/links.toml" => "Links updated".to_string(),
        r if r.starts_with("tickets/") => format!("Ticket {} {verb}", stem(r)),
        r if r.starts_with("decisions/") && added && answered(dir, path) => {
            format!("{} made", crate::projects::decision_name(&stem(r)))
        }
        r if r.starts_with("decisions/") && added => {
            format!("{} to make", crate::projects::decision_name(&stem(r)))
        }
        r if r.starts_with("decisions/") => {
            format!("{} {verb}", crate::projects::decision_name(&stem(r)))
        }
        r if r.starts_with("resources/") => {
            format!("Resource {} {verb}", r.trim_start_matches("resources/"))
        }
        r if r.starts_with("meetings/") => format!("Meeting notes {} {verb}", stem(r)),
        r if r.starts_with("progress/") => "Progress written".to_string(),
        r => format!("{r} {verb}"),
    }
}

/// Whether the decision at `path` in checkout `dir` has an answer.
fn answered(dir: Option<&Path>, path: &str) -> bool {
    dir.and_then(|d| std::fs::read_to_string(d.join(path)).ok())
        .and_then(|text| {
            front_matter::parse::<hydraui_projects::DecisionMeta>(&text)
                .ok()
                .map(|(meta, _)| !meta.answer.is_empty())
        })
        .unwrap_or(false)
}

/// Project `slug`'s changes not sent yet, from `git status --porcelain -z` output: each in
/// words, with its file. `dir` is the checkout, for reading what a new file says.
pub fn pending_changes(status: &str, slug: &str, dir: Option<&Path>) -> Vec<(String, String)> {
    let prefix = format!("{slug}/");
    let mut out = Vec::new();
    let mut entries = status.split('\0');
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (x, y) = (&entry[..1], &entry[1..2]);
        if x == "R" || x == "C" {
            entries.next();
        }
        let path = &entry[3..];
        if !path.starts_with(&prefix) {
            continue;
        }
        let letter = if entry.starts_with("??") || matches!(x, "A" | "R" | "C") {
            "A"
        } else if x == "D" || y == "D" {
            "D"
        } else {
            "M"
        };
        out.push((change_words(letter, path, dir), path.to_string()));
    }
    out
}

/// The checkout's `git status --porcelain -z` output, for [`pending_changes`].
pub fn status_text(r: &dyn Runner, dir: &Path) -> Result<String, String> {
    r.git(dir, &["status", "--porcelain", "-uall", "-z"], None)
}

/// A pull request's title and body from `git diff-tree --name-status` output: what the change
/// does, in words, not a diff (spec §5.2).
pub fn describe(
    name: &str,
    login: &str,
    name_status: &str,
    dir: Option<&Path>,
) -> (String, String) {
    let mut items: Vec<String> = Vec::new();
    let mut created = false;
    for l in name_status.lines() {
        let mut parts = l.split('\t');
        let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        let rest = path.split_once('/').map(|(_, r)| r).unwrap_or(path);
        created |= rest == "project.toml" && status.starts_with('A');
        let item = change_words(status, path, dir);
        if !items.contains(&item) {
            items.push(item);
        }
    }
    let title = if created {
        format!("{name}: new project")
    } else {
        match items.as_slice() {
            [] => format!("{name}: changes"),
            [one] => format!("{name}: {one}"),
            [first, more @ ..] => format!("{name}: {first} and {} more", more.len()),
        }
    };
    let mut body = format!("Sent from hydraui by {login}.\n");
    if !items.is_empty() {
        body.push('\n');
        for i in &items {
            body.push_str(&format!("- {i}\n"));
        }
    }
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::process::Command;

    /// Real git against a local bare repo, and a fake GitHub for gh.
    struct Fake {
        bare: PathBuf,
        login: String,
        prs: RefCell<Vec<OpenPr>>,
        calls: RefCell<Vec<String>>,
        /// Pushes GitHub fails with a server error before taking one.
        failing_pushes: RefCell<u32>,
        /// The repo's gate check hasn't passed: a plain merge is refused, auto-merge is queued.
        gated: RefCell<bool>,
        /// Pull requests queued to merge when the check passes.
        queued: RefCell<Vec<String>>,
    }

    fn git_cmd(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(dir).args(args);
        for (k, v) in [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ] {
            cmd.env(k, v);
        }
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        finish(cmd.output().map_err(|e| e.to_string())?, "git", args)
    }

    impl Runner for Fake {
        fn git(&self, dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
            git_cmd(dir, args, index)
        }

        fn remote_git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
            if args[0] == "push" && *self.failing_pushes.borrow() > 0 {
                *self.failing_pushes.borrow_mut() -= 1;
                return Err(" ! [remote rejected] x -> y (Internal Server Error)".into());
            }
            git_cmd(dir, args, None)
        }

        fn gh(&self, args: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push(args[..2].join(" "));
            let flag = |name: &str| {
                args.iter()
                    .position(|a| *a == name)
                    .map(|i| args[i + 1].to_string())
                    .unwrap_or_default()
            };
            match &args[..2] {
                ["pr", "list"] => {
                    let prs: Vec<serde_json::Value> = self
                        .prs
                        .borrow()
                        .iter()
                        .map(|p| {
                            serde_json::json!({
                                "url": p.url, "title": p.title, "body": p.body,
                                "author": {"login": p.author}, "headRefName": p.branch,
                                "reviewDecision": if p.sent_back.is_some() { "CHANGES_REQUESTED" } else { "" },
                                "latestReviews": [{"state": "CHANGES_REQUESTED", "body": p.sent_back.clone().unwrap_or_default()}],
                            })
                        })
                        .collect();
                    Ok(serde_json::to_string(&prs).unwrap())
                }
                ["pr", "create"] => {
                    let n = self.prs.borrow().len() + 1;
                    let url = format!("https://github.com/o/r/pull/{n}");
                    self.prs.borrow_mut().push(OpenPr {
                        url: url.clone(),
                        title: flag("--title"),
                        body: flag("--body"),
                        author: self.login.clone(),
                        branch: flag("--head"),
                        slug: flag("--head").split('/').nth(2).unwrap().into(),
                        sent_back: None,
                    });
                    Ok(format!("Creating pull request\n{url}\n"))
                }
                ["pr", "edit"] => {
                    let mut prs = self.prs.borrow_mut();
                    let p = prs.iter_mut().find(|p| p.url == args[2]).unwrap();
                    p.body = flag("--body");
                    Ok(String::new())
                }
                ["pr", "merge"] if *self.gated.borrow() => {
                    if args.contains(&"--auto") {
                        self.queued.borrow_mut().push(args[2].to_string());
                        return Ok(String::new());
                    }
                    Err("X Pull request o/r#1 is not mergeable: the base branch policy prohibits the merge.".into())
                }
                ["pr", "merge"] => {
                    let mut prs = self.prs.borrow_mut();
                    let i = prs.iter().position(|p| p.url == args[2]).unwrap();
                    let p = prs.remove(i);
                    // One commit on main's tip: a fast-forward is what a squash gives.
                    let head = format!("refs/heads/{}", p.branch);
                    git_cmd(&self.bare, &["update-ref", "refs/heads/main", &head], None)?;
                    Ok(String::new())
                }
                ["pr", "review"] => Ok(String::new()),
                other => Err(format!("unexpected gh {other:?}")),
            }
        }
    }

    /// A bare repo and a clone of it at `checkout`, as `login`.
    fn setup(login: &str) -> (tempfile::TempDir, Fake, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("repo.git");
        let checkout = tmp.path().join("checkout");
        git_cmd(
            tmp.path(),
            &["init", "-q", "--bare", "-b", "main", "repo.git"],
            None,
        )
        .unwrap();
        git_cmd(
            tmp.path(),
            &["clone", "-q", &bare.display().to_string(), "checkout"],
            None,
        )
        .unwrap();
        let fake = Fake {
            bare,
            login: login.into(),
            prs: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
            failing_pushes: RefCell::new(0),
            gated: RefCell::new(false),
            queued: RefCell::new(Vec::new()),
        };
        (tmp, fake, checkout)
    }

    fn write(dir: &Path, path: &str, text: &str) {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn on_main(fake: &Fake, path: &str) -> Option<String> {
        git_cmd(&fake.bare, &["show", &format!("main:{path}")], None).ok()
    }

    fn sending<'a>(dir: &'a Path, login: &'a str, mine: &'a BTreeSet<String>) -> Send<'a> {
        Send {
            repo: "o/r",
            dir,
            slug: "pg",
            name: "Product Groups",
            lead: "lead",
            login,
            today: "2026-11-03",
            mine,
        }
    }

    /// The lead creates project `pg` with a spec, sent and merged.
    fn lead_creates(fake: &Fake, dir: &Path) {
        write(
            dir,
            "pg/project.toml",
            "name = \"Product Groups\"\nkey = \"PG\"\nlead = \"lead\"\n",
        );
        write(dir, "pg/spec.md", "one\ntwo\nthree\n");
        let none = BTreeSet::new();
        assert!(matches!(
            send(fake, &sending(dir, "lead", &none)),
            Ok(Sent::Merged(_))
        ));
    }

    #[test]
    fn the_leads_first_send_starts_the_repo_and_merges_at_once() {
        let (_tmp, fake, dir) = setup("lead");
        write(&dir, "other/project.toml", "name = \"Other\"\n");
        lead_creates(&fake, &dir);
        assert!(on_main(&fake, "README.md").unwrap().contains("# Projects"));
        // The repo starts with its merge gate, its creator as the admin who owns it.
        assert!(on_main(&fake, ".github/workflows/projects-gate.yml").is_some());
        assert!(on_main(&fake, ".github/scripts/projects_gate.py").is_some());
        assert!(on_main(&fake, ".github/projects-ruleset.json").is_some());
        assert!(
            on_main(&fake, ".github/CODEOWNERS")
                .unwrap()
                .contains("/.github/ @lead")
        );
        assert!(on_main(&fake, ".github/scripts/test_projects_gate.py").is_none());
        assert_eq!(on_main(&fake, "pg/spec.md").unwrap(), "one\ntwo\nthree\n");
        // Only the project sent: another project's files stay pending, here only.
        assert_eq!(on_main(&fake, "other/project.toml"), None);
        let pending = pending_in(&fake, &dir).unwrap();
        assert_eq!(pending, BTreeSet::from(["other/project.toml".to_string()]));
        assert_eq!(
            fake.calls.borrow().as_slice(),
            ["pr list", "pr create", "pr merge"]
        );
        assert!(fake.prs.borrow().is_empty());
    }

    #[test]
    fn a_members_change_waits_for_the_lead_and_resending_updates_its_pull_request() {
        let (tmp, mut fake, dir) = setup("lead");
        lead_creates(&fake, &dir);
        fake.login = "aza".into();
        let member = tmp.path().join("member");
        git_cmd(
            tmp.path(),
            &["clone", "-q", &fake.bare.display().to_string(), "member"],
            None,
        )
        .unwrap();
        write(&member, "pg/tickets/PG-1.md", "+++\ntitle = \"x\"\n+++\n");
        let none = BTreeSet::new();
        let sent = send(&fake, &sending(&member, "aza", &none)).unwrap();
        assert_eq!(sent, Sent::Waiting("https://github.com/o/r/pull/1".into()));
        assert_eq!(on_main(&fake, "pg/tickets/PG-1.md"), None);
        assert!(is_sent(&fake, &member, "pg"));
        assert_eq!(fake.prs.borrow()[0].branch, "hydra/aza/pg/2026-11-03");
        assert!(fake.prs.borrow()[0].body.contains("- Ticket PG-1 added"));

        write(&member, "pg/tickets/PG-2.md", "+++\ntitle = \"y\"\n+++\n");
        assert!(!is_sent(&fake, &member, "pg"));
        let again = send(&fake, &sending(&member, "aza", &none)).unwrap();
        assert_eq!(again, Sent::Waiting("https://github.com/o/r/pull/1".into()));
        assert_eq!(fake.prs.borrow().len(), 1);
        assert!(fake.calls.borrow().contains(&"pr edit".to_string()));

        // The lead approves; the member's next sync settles cleanly onto it.
        let pr = fake.prs.borrow()[0].clone();
        approve(&fake, &dir, &pr).unwrap();
        assert!(on_main(&fake, "pg/tickets/PG-2.md").is_some());
        git_cmd(&member, &["fetch", "-q", "origin"], None).unwrap();
        settle(&fake, &member, "main").unwrap();
        assert!(pending_in(&fake, &member).unwrap().is_empty());
        assert!(dir.join("pg/tickets/PG-2.md").is_file());
    }

    #[test]
    fn who_leads_comes_from_the_repo_not_the_senders_copy() {
        let (tmp, mut fake, dir) = setup("lead");
        lead_creates(&fake, &dir);
        fake.login = "aza".into();
        let member = tmp.path().join("member");
        git_cmd(
            tmp.path(),
            &["clone", "-q", &fake.bare.display().to_string(), "member"],
            None,
        )
        .unwrap();
        // aza makes themself lead in their own copy, then sends.
        write(
            &member,
            "pg/project.toml",
            "name = \"Product Groups\"\nkey = \"PG\"\nlead = \"aza\"\n",
        );
        let none = BTreeSet::new();
        let mut s = sending(&member, "aza", &none);
        s.lead = "aza";
        let sent = send(&fake, &s).unwrap();
        assert!(matches!(sent, Sent::Waiting(_)), "{sent:?}");
        assert!(
            on_main(&fake, "pg/project.toml")
                .unwrap()
                .contains("Product Groups")
        );
        assert!(!on_main(&fake, "pg/project.toml").unwrap().contains("aza"));
    }

    #[test]
    fn text_changed_on_both_sides_merges_and_the_same_lines_clash() {
        let (tmp, fake, dir) = setup("lead");
        lead_creates(&fake, &dir);
        // Someone else's change lands first: line one.
        let other = tmp.path().join("other");
        git_cmd(
            tmp.path(),
            &["clone", "-q", &fake.bare.display().to_string(), "other"],
            None,
        )
        .unwrap();
        write(&other, "pg/spec.md", "ONE\ntwo\nthree\n");
        git_cmd(&other, &["commit", "-qam", "x"], None).unwrap();
        git_cmd(&other, &["push", "-q", "origin", "main"], None).unwrap();

        write(&dir, "pg/spec.md", "one\ntwo\nTHREE\n");
        let none = BTreeSet::new();
        assert!(matches!(
            send(&fake, &sending(&dir, "lead", &none)),
            Ok(Sent::Merged(_))
        ));
        assert_eq!(on_main(&fake, "pg/spec.md").unwrap(), "ONE\ntwo\nTHREE\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("pg/spec.md")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );

        git_cmd(&other, &["pull", "-q"], None).unwrap();
        write(&other, "pg/spec.md", "ONE\nTWO, theirs\nTHREE\n");
        git_cmd(&other, &["commit", "-qam", "y"], None).unwrap();
        git_cmd(&other, &["push", "-q", "origin", "main"], None).unwrap();
        write(&dir, "pg/spec.md", "ONE\nTWO, mine\nTHREE\n");
        assert_eq!(
            send(&fake, &sending(&dir, "lead", &none)),
            Err(SendError::Clash(vec!["pg/spec.md".into()]))
        );
        // Use theirs: the repo's version wins, so there's nothing left to send.
        take_theirs(&fake, &dir, "pg/spec.md").unwrap();
        assert_eq!(
            send(&fake, &sending(&dir, "lead", &none)),
            Ok(Sent::Nothing)
        );
        assert!(pending_in(&fake, &dir).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("pg/spec.md")).unwrap(),
            "ONE\nTWO, theirs\nTHREE\n"
        );
        // Keep mine: this side's version goes.
        write(&dir, "pg/spec.md", "ONE\nTWO, mine\nTHREE\n");
        git_cmd(&other, &["pull", "-q"], None).unwrap();
        write(&other, "pg/spec.md", "ONE\nTWO, theirs again\nTHREE\n");
        git_cmd(&other, &["commit", "-qam", "z"], None).unwrap();
        git_cmd(&other, &["push", "-q", "origin", "main"], None).unwrap();
        assert!(matches!(
            send(&fake, &sending(&dir, "lead", &none)),
            Err(SendError::Clash(_))
        ));
        let mine = BTreeSet::from(["pg/spec.md".to_string()]);
        assert!(matches!(
            send(&fake, &sending(&dir, "lead", &mine)),
            Ok(Sent::Merged(_))
        ));
        assert_eq!(
            on_main(&fake, "pg/spec.md").unwrap(),
            "ONE\nTWO, mine\nTHREE\n"
        );
    }

    #[test]
    fn a_server_error_on_push_is_tried_again() {
        let (_tmp, fake, dir) = setup("lead");
        lead_creates(&fake, &dir);
        write(&dir, "pg/spec.md", "one\ntwo\nthree, again\n");
        *fake.failing_pushes.borrow_mut() = 1;
        let none = BTreeSet::new();
        assert!(matches!(
            send(&fake, &sending(&dir, "lead", &none)),
            Ok(Sent::Merged(_))
        ));
        // A refusal that isn't GitHub's fault isn't retried.
        assert!(!is_transient(
            "! [remote rejected] main (protected branch hook declined)"
        ));
        assert!(is_transient("error: RPC failed; HTTP 502 curl 22"));
    }

    #[test]
    fn with_the_gate_on_a_merge_waits_for_the_check() {
        let (_tmp, fake, dir) = setup("lead");
        lead_creates(&fake, &dir);
        *fake.gated.borrow_mut() = true;
        write(&dir, "pg/spec.md", "one\ntwo\nthree, again\n");
        let none = BTreeSet::new();
        let sent = send(&fake, &sending(&dir, "lead", &none)).unwrap();
        assert!(matches!(sent, Sent::Queued(_)), "{sent:?}");
        assert_eq!(fake.queued.borrow().len(), 1);
        // A refusal that isn't the check waiting fails as before.
        assert!(!merge_waits("could not resolve to a PullRequest"));
    }

    #[test]
    fn nothing_to_send_is_nothing() {
        let (_tmp, fake, dir) = setup("lead");
        lead_creates(&fake, &dir);
        let none = BTreeSet::new();
        assert_eq!(
            send(&fake, &sending(&dir, "lead", &none)),
            Ok(Sent::Nothing)
        );
    }

    #[test]
    fn only_hydra_pull_requests_are_read_with_the_leads_note() {
        let json = r#"[
          {"url":"u1","title":"t","body":"b","author":{"login":"aza"},"headRefName":"hydra/aza/pg/2026-11-03","reviewDecision":"CHANGES_REQUESTED","latestReviews":[{"state":"APPROVED","body":"old"},{"state":"CHANGES_REQUESTED","body":" Split PG-2 "}]},
          {"url":"u2","title":"t","body":"","author":{"login":"bo"},"headRefName":"feature/x","reviewDecision":null,"latestReviews":[]},
          {"url":"u3","title":"t","author":{"login":"bo"},"headRefName":"hydra/bo/mx/2026-11-02","reviewDecision":"REVIEW_REQUIRED","latestReviews":[]}
        ]"#;
        let prs = parse_prs(json).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(
            (prs[0].slug.as_str(), prs[0].sent_back.as_deref()),
            ("pg", Some("Split PG-2"))
        );
        assert_eq!(
            (prs[1].slug.as_str(), prs[1].sent_back.as_deref()),
            ("mx", None)
        );
    }

    #[test]
    fn pending_changes_read_in_words_with_their_files() {
        let status = "?? pg/tickets/PG-3.md\0 M pg/milestones.toml\0 D pg/resources/old.pdf\0R  pg/a.md\0pg/b.md\0?? mx/spec.md\0";
        assert_eq!(
            pending_changes(status, "pg", None),
            vec![
                (
                    "Ticket PG-3 added".to_string(),
                    "pg/tickets/PG-3.md".to_string()
                ),
                ("Milestones updated".into(), "pg/milestones.toml".into()),
                (
                    "Resource old.pdf removed".into(),
                    "pg/resources/old.pdf".into()
                ),
                ("a.md added".into(), "pg/a.md".into()),
            ]
        );
    }

    #[test]
    fn a_pull_request_says_what_changed_in_words() {
        let (title, body) = describe(
            "Product Groups",
            "aza",
            "M\tpg/milestones.toml\nA\tpg/tickets/PG-3.md\nA\tpg/decisions/D-2.md\nM\tpg/decisions/D-1.md\n",
            None,
        );
        assert_eq!(title, "Product Groups: Milestones updated and 3 more");
        assert_eq!(
            body,
            "Sent from hydraui by aza.\n\n- Milestones updated\n- Ticket PG-3 added\n- Decision 2 to make\n- Decision 1 updated\n"
        );
        let (title, _) = describe(
            "Product Groups",
            "aza",
            "A\tpg/project.toml\nA\tpg/spec.md\n",
            None,
        );
        assert_eq!(title, "Product Groups: new project");
        // A question answered before it was ever sent goes as a decision made.
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "pg/decisions/D-1.md",
            "+++\nquestion = \"q\"\nanswer = \"Yes\"\n+++\n",
        );
        write(
            tmp.path(),
            "pg/decisions/D-2.md",
            "+++\nquestion = \"q\"\n+++\n",
        );
        let (_, body) = describe(
            "Product Groups",
            "aza",
            "A\tpg/decisions/D-1.md\nA\tpg/decisions/D-2.md\n",
            Some(tmp.path()),
        );
        assert!(
            body.contains("- Decision 1 made\n- Decision 2 to make"),
            "{body}"
        );
        assert_eq!(
            pending_changes("?? pg/decisions/D-1.md\0", "pg", Some(tmp.path()))[0].0,
            "Decision 1 made"
        );
    }
}
