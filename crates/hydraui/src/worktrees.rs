//! A git worktree per task card (task board spec §8): "Take in new worktree" makes a branch
//! `task/<slug>` checked out beside the repository in `<repo>-worktrees/<slug>`, and moving the
//! card to Done offers to merge that branch back and remove the worktree. Which card has which
//! worktree is kept in `<hydra home>/hydraui/worktrees.toml`, out of the repository. No UI;
//! the git calls block, so run them off the UI thread.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A card's worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// The repository's main work tree.
    pub repo: PathBuf,
    /// The card's text when its worktree was made.
    pub card: String,
    pub path: PathBuf,
    pub branch: String,
    /// The branch the repository was on, which the work merges back into.
    pub base: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Records {
    #[serde(default, rename = "worktree")]
    worktrees: Vec<Worktree>,
}

/// Up to five words of `text`, lowercase ASCII letters and digits joined by `-`, at most 40
/// characters; `task` when nothing is left.
pub fn slug(text: &str) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .map(str::to_ascii_lowercase)
        .collect();
    let mut s = words.join("-");
    if s.len() > 40 {
        s.truncate(40);
        s = s.trim_end_matches('-').to_string();
    }
    if s.is_empty() { "task".into() } else { s }
}

/// Runs git in `dir` without a console window: its output, or its error text.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    let out = hydra_platform::process::hide_window(&mut cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git didn't run: {e}"))?;
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

/// Makes a worktree for the card `card` of the repository whose main work tree is `repo`: a new
/// branch `task/<slug>` from what `repo` has checked out, in `<repo>-worktrees/<slug>` (`-2`,
/// `-3`, ... when the folder or branch is taken).
pub fn create(repo: &Path, card: &str) -> Result<Worktree, String> {
    let base = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("the repository has no folder name")?;
    let parent = repo.parent().ok_or("the repository has no parent folder")?;
    let home = parent.join(format!("{name}-worktrees"));
    let stem = slug(card);
    let (path, branch) = (1..)
        .map(|n| {
            let s = if n == 1 {
                stem.clone()
            } else {
                format!("{stem}-{n}")
            };
            (home.join(&s), format!("task/{s}"))
        })
        .find(|(path, branch)| {
            let head = format!("refs/heads/{branch}");
            !path.exists() && git(repo, &["rev-parse", "--verify", "--quiet", &head]).is_err()
        })
        .expect("an unbounded range has a free name");
    let target = path.to_string_lossy().into_owned();
    git(repo, &["worktree", "add", "-b", &branch, &target, "HEAD"])?;
    Ok(Worktree {
        repo: repo.to_path_buf(),
        card: card.to_string(),
        path,
        branch,
        base,
    })
}

fn load(file: &Path) -> Records {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(file: &Path, records: &Records) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(records).map_err(std::io::Error::other)?;
    std::fs::write(file, text)
}

/// Remembers `wt` in the records `file`.
pub fn remember(file: &Path, wt: &Worktree) -> std::io::Result<()> {
    let mut records = load(file);
    records
        .worktrees
        .retain(|w| !(w.repo == wt.repo && w.card == wt.card));
    records.worktrees.push(wt.clone());
    save(file, &records)
}

/// The worktree recorded for the card `card` of `repo`, if any.
pub fn find(file: &Path, repo: &Path, card: &str) -> Option<Worktree> {
    load(file)
        .worktrees
        .into_iter()
        .find(|w| w.repo == repo && w.card == card)
}

/// Forgets `wt` in the records `file`.
pub fn forget(file: &Path, wt: &Worktree) -> std::io::Result<()> {
    let mut records = load(file);
    records.worktrees.retain(|w| w != wt);
    save(file, &records)
}

/// Merges `wt`'s branch into its base in the main work tree, then removes the worktree and
/// deletes the branch. Refused, with nothing changed, when the worktree or the main work tree
/// has uncommitted changes or the main work tree isn't on the base branch; a merge that
/// conflicts is aborted. On success, what was done.
pub fn merge_and_remove(wt: &Worktree) -> Result<String, String> {
    let dirty = |dir: &Path| {
        git(dir, &["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty())
    };
    if wt.path.exists() && dirty(&wt.path)? {
        return Err(format!(
            "{} has uncommitted changes: commit them first",
            wt.path.display()
        ));
    }
    if dirty(&wt.repo)? {
        return Err(format!(
            "{} has uncommitted changes: commit or stash them first",
            wt.repo.display()
        ));
    }
    let on = git(&wt.repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if on != wt.base {
        return Err(format!(
            "{} is on {on}, not {}: switch back to {} first",
            wt.repo.display(),
            wt.base,
            wt.base
        ));
    }
    if let Err(e) = git(&wt.repo, &["merge", "--no-ff", "--no-edit", &wt.branch]) {
        let _ = git(&wt.repo, &["merge", "--abort"]);
        return Err(format!(
            "merging {} failed, nothing changed: {e}",
            wt.branch
        ));
    }
    let mut done = format!("Merged {} into {}", wt.branch, wt.base);
    if wt.path.exists() {
        let target = wt.path.to_string_lossy().into_owned();
        match git(&wt.repo, &["worktree", "remove", &target]) {
            Ok(_) => done.push_str(", removed its worktree"),
            Err(e) => return Ok(format!("{done}; its worktree stays: {e}")),
        }
    }
    match git(&wt.repo, &["branch", "-d", &wt.branch]) {
        Ok(_) => done.push_str(" and deleted the branch"),
        Err(e) => done.push_str(&format!("; the branch stays: {e}")),
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_short_and_safe() {
        assert_eq!(
            slug("Worktree per card: \"Take in new worktree\" creates"),
            "worktree-per-card-take-in"
        );
        assert_eq!(slug("Fix the ✓ icon!"), "fix-the-icon");
        assert_eq!(slug("✓✓"), "task");
        assert!(slug(&"abcdefghij ".repeat(5)).len() <= 40);
        assert!(!slug(&"abcdefghij ".repeat(5)).ends_with('-'));
    }

    /// A repository with one commit on `main` in `<tmp>/repo`; `None` without git.
    fn repo() -> Option<(tempfile::TempDir, PathBuf)> {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        if git(&repo, &["init", "-q", "-b", "main"]).is_err() {
            eprintln!("skipped: git isn't installed");
            return None;
        }
        for (k, v) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            git(&repo, &["config", k, v]).unwrap();
        }
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "a.txt"]).unwrap();
        git(&repo, &["commit", "-q", "-m", "first"]).unwrap();
        Some((d, repo))
    }

    #[test]
    fn a_cards_worktree_is_made_merged_and_removed() {
        let Some((d, repo)) = repo() else {
            return;
        };
        let wt = create(&repo, "Add the orders page").unwrap();
        assert_eq!(wt.branch, "task/add-the-orders-page");
        assert_eq!(wt.base, "main");
        assert_eq!(
            wt.path,
            d.path().join("repo-worktrees").join("add-the-orders-page")
        );
        assert!(wt.path.join("a.txt").is_file());
        // The same card again gets the next free name.
        let again = create(&repo, "Add the orders page").unwrap();
        assert_eq!(again.branch, "task/add-the-orders-page-2");

        // Uncommitted work in the worktree is refused.
        std::fs::write(wt.path.join("a.txt"), "two\n").unwrap();
        let refused = merge_and_remove(&wt).unwrap_err();
        assert!(refused.contains("uncommitted"), "{refused}");
        git(&wt.path, &["commit", "-q", "-am", "orders"]).unwrap();

        let done = merge_and_remove(&wt).unwrap();
        assert!(
            done.contains("Merged task/add-the-orders-page into main"),
            "{done}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(!wt.path.exists());
        assert!(
            git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/heads/task/add-the-orders-page"
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn a_conflicting_merge_changes_nothing() {
        let Some((_d, repo)) = repo() else {
            return;
        };
        let wt = create(&repo, "x").unwrap();
        std::fs::write(wt.path.join("a.txt"), "theirs\n").unwrap();
        git(&wt.path, &["commit", "-q", "-am", "theirs"]).unwrap();
        std::fs::write(repo.join("a.txt"), "ours\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "ours"]).unwrap();
        let err = merge_and_remove(&wt).unwrap_err();
        assert!(err.contains("nothing changed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "ours\n"
        );
        assert!(wt.path.exists());
        assert!(git(&repo, &["status", "--porcelain"]).unwrap().is_empty());
    }

    #[test]
    fn records_remember_find_and_forget() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("hydraui").join("worktrees.toml");
        let wt = Worktree {
            repo: "C:/r".into(),
            card: "do it".into(),
            path: "C:/r-worktrees/do-it".into(),
            branch: "task/do-it".into(),
            base: "main".into(),
        };
        assert_eq!(find(&file, Path::new("C:/r"), "do it"), None);
        remember(&file, &wt).unwrap();
        assert_eq!(find(&file, Path::new("C:/r"), "do it"), Some(wt.clone()));
        assert_eq!(find(&file, Path::new("C:/other"), "do it"), None);
        forget(&file, &wt).unwrap();
        assert_eq!(find(&file, Path::new("C:/r"), "do it"), None);
    }
}
