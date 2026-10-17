//! The git repository a pane is in, for the status bar: branch, how it stands against its
//! upstream (as of the last fetch: no network), and how many files changed.
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// The checked-out branch; `None` on a detached HEAD.
    pub branch: Option<String>,
    /// The short commit a detached HEAD is at.
    pub detached: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Files with staged changes.
    pub staged: u32,
    /// Files changed in the working tree but not staged.
    pub modified: u32,
    pub untracked: u32,
    /// Files with unresolved merge conflicts.
    pub conflicts: u32,
}

/// Reads `git status --porcelain=v2 --branch` output.
pub fn parse(text: &str) -> GitStatus {
    let mut s = GitStatus::default();
    let mut oid = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("# branch.oid ") {
            oid = v.to_string();
        } else if let Some(v) = line.strip_prefix("# branch.head ") {
            if v != "(detached)" {
                s.branch = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("# branch.ab ") {
            let mut parts = v.split_whitespace();
            s.ahead = parts
                .next()
                .and_then(|a| a.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
            s.behind = parts
                .next()
                .and_then(|b| b.trim_start_matches('-').parse().ok())
                .unwrap_or(0);
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // "<kind> <XY> ...": X is the index (staged) side, Y the working tree; '.' = unchanged.
            let xy: Vec<char> = line.chars().skip(2).take(2).collect();
            if xy.first().is_some_and(|c| *c != '.') {
                s.staged += 1;
            }
            if xy.get(1).is_some_and(|c| *c != '.') {
                s.modified += 1;
            }
        } else if line.starts_with("u ") {
            s.conflicts += 1;
        } else if line.starts_with("? ") {
            s.untracked += 1;
        }
    }
    if s.branch.is_none() && !oid.is_empty() && oid != "(initial)" {
        s.detached = Some(oid.chars().take(7).collect());
    }
    s
}

/// The status bar text: `⎇ main ↑2 ↓1 · +3 ~5 ?2 !1`, showing only what isn't zero, or
/// `⎇ main ✓` when nothing changed.
pub fn summary(s: &GitStatus) -> String {
    let mut head = match (&s.branch, &s.detached) {
        (Some(b), _) => format!("⎇ {b}"),
        (None, Some(c)) => format!("⎇ detached {c}"),
        (None, None) => "⎇".to_string(),
    };
    if s.ahead > 0 {
        head.push_str(&format!(" ↑{}", s.ahead));
    }
    if s.behind > 0 {
        head.push_str(&format!(" ↓{}", s.behind));
    }
    let counts: Vec<String> = [
        ('+', s.staged),
        ('~', s.modified),
        ('?', s.untracked),
        ('!', s.conflicts),
    ]
    .iter()
    .filter(|(_, n)| *n > 0)
    .map(|(mark, n)| format!("{mark}{n}"))
    .collect();
    if counts.is_empty() {
        format!("{head} ✓")
    } else {
        format!("{head} · {}", counts.join(" "))
    }
}

/// The repository `dir` is in, or `None` when it isn't in one (or git isn't installed). Runs
/// git without a console window; call it off the UI thread.
pub fn read(dir: &Path) -> Option<GitStatus> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["status", "--porcelain=v2", "--branch"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let out = hydra_platform::process::hide_window(&mut cmd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AHEAD: &str = "# branch.oid 1234567890abcdef1234567890abcdef12345678\n\
# branch.head main\n\
# branch.upstream origin/main\n\
# branch.ab +2 -1\n\
1 M. N... 100644 100644 100644 aaaa bbbb src/a.rs\n\
1 .M N... 100644 100644 100644 aaaa bbbb src/b.rs\n\
1 MM N... 100644 100644 100644 aaaa bbbb src/c.rs\n\
2 R. N... 100644 100644 100644 aaaa bbbb R100 new.rs\told.rs\n\
u UU N... 100644 100644 100644 100644 aaaa bbbb cccc both.rs\n\
? notes.txt\n\
? tmp/\n";

    #[test]
    fn reads_branch_upstream_and_every_kind_of_change() {
        let s = parse(AHEAD);
        assert_eq!(
            s,
            GitStatus {
                branch: Some("main".into()),
                detached: None,
                ahead: 2,
                behind: 1,
                staged: 3,
                modified: 2,
                untracked: 2,
                conflicts: 1,
            }
        );
        assert_eq!(summary(&s), "⎇ main ↑2 ↓1 · +3 ~2 ?2 !1");
    }

    #[test]
    fn a_clean_repo_in_sync_is_just_the_branch() {
        let s = parse(
            "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
        );
        assert_eq!(summary(&s), "⎇ main ✓");
    }

    #[test]
    fn no_upstream_and_a_new_repo() {
        let s = parse("# branch.oid (initial)\n# branch.head main\n? a.txt\n");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!((s.ahead, s.behind), (0, 0));
        assert_eq!(summary(&s), "⎇ main · ?1");
    }

    #[test]
    fn a_detached_head_shows_its_short_commit() {
        let s = parse(
            "# branch.oid a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0\n# branch.head (detached)\n",
        );
        assert_eq!(s.branch, None);
        assert_eq!(s.detached.as_deref(), Some("a1b2c3d"));
        assert_eq!(summary(&s), "⎇ detached a1b2c3d ✓");
    }

    #[test]
    fn a_real_repository_is_read_and_a_plain_folder_is_not() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(d.path())
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            eprintln!("skipped: git isn't installed");
            return;
        }
        std::fs::write(d.path().join("a.txt"), "x").unwrap();
        let s = read(d.path()).unwrap();
        assert!(s.branch.is_some(), "{s:?}");
        assert_eq!(s.untracked, 1);
        assert!(git(&["add", "a.txt"]));
        assert_eq!(read(d.path()).unwrap().staged, 1);
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(read(plain.path()), None);
    }
}
