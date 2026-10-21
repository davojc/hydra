//! The git repository a pane is in, for the status bar: branch, how it stands against its
//! upstream (as of the last fetch: no network), and how many files changed; and each changed
//! file, for the file tree (spec §7.3).
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// How a file stands in git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Modified,
    Added,
    Untracked,
    Deleted,
    Renamed,
    Ignored,
}

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
    /// Each changed file (relative to the repository's root); an untracked or ignored folder
    /// is one entry.
    pub files: BTreeMap<PathBuf, Change>,
    /// The repository's root folder.
    pub root: Option<PathBuf>,
    /// Each renamed file's old path.
    pub renamed_from: BTreeMap<PathBuf, PathBuf>,
}

/// What a file's diff compares it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// The last commit.
    Head,
    /// Nothing: the whole file is new.
    Nothing,
    /// The last commit's version under its old name.
    Renamed(PathBuf),
}

/// A tracked file's change from its `XY` letters.
fn change_from(xy: &str) -> Change {
    if xy.contains('R') {
        Change::Renamed
    } else if xy.contains('D') {
        Change::Deleted
    } else if xy.starts_with('A') {
        Change::Added
    } else {
        Change::Modified
    }
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
            // "1 XY sub mH mI mW hH hI path" / "2 XY sub mH mI mW hH hI Xscore path\torig".
            let (path, orig) = if line.starts_with("1 ") {
                (line.splitn(9, ' ').nth(8), None)
            } else {
                match line.splitn(10, ' ').nth(9) {
                    Some(p) => match p.split_once('\t') {
                        Some((new, old)) => (Some(new), Some(old)),
                        None => (Some(p), None),
                    },
                    None => (None, None),
                }
            };
            if let (Some(path), Some(orig)) = (path, orig) {
                s.renamed_from
                    .insert(PathBuf::from(path), PathBuf::from(orig));
            }
            if let Some(path) = path {
                s.files
                    .insert(PathBuf::from(path), change_from(&line[2..4]));
            }
        } else if line.starts_with("u ") {
            s.conflicts += 1;
            if let Some(path) = line.splitn(11, ' ').nth(10) {
                s.files.insert(PathBuf::from(path), Change::Modified);
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            s.untracked += 1;
            s.files.insert(PathBuf::from(path), Change::Untracked);
        } else if let Some(path) = line.strip_prefix("! ") {
            s.files.insert(PathBuf::from(path), Change::Ignored);
        }
    }
    if s.branch.is_none() && !oid.is_empty() && oid != "(initial)" {
        s.detached = Some(oid.chars().take(7).collect());
    }
    s
}

/// The status bar's git summary in pieces, each with what it says.
pub type Summary = Vec<(String, GitMark)>;

/// What a piece of the status bar's git summary says, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    Branch,
    Ahead,
    Behind,
    Separator,
    Staged,
    Modified,
    Untracked,
    Conflicts,
    Clean,
}

/// The status bar's git summary as pieces, each with what it says (for its colour): the
/// branch, ahead/behind, then the counts that aren't zero after a `·`, or `✓` when nothing
/// changed.
pub fn summary_parts(s: &GitStatus) -> Summary {
    let head = match (&s.branch, &s.detached) {
        (Some(b), _) => format!("⎇ {b}"),
        (None, Some(c)) => format!("⎇ detached {c}"),
        (None, None) => "⎇".to_string(),
    };
    let mut parts = vec![(head, GitMark::Branch)];
    if s.ahead > 0 {
        parts.push((format!("↑{}", s.ahead), GitMark::Ahead));
    }
    if s.behind > 0 {
        parts.push((format!("↓{}", s.behind), GitMark::Behind));
    }
    let counts: Vec<(String, GitMark)> = [
        ('+', s.staged, GitMark::Staged),
        ('~', s.modified, GitMark::Modified),
        ('?', s.untracked, GitMark::Untracked),
        ('!', s.conflicts, GitMark::Conflicts),
    ]
    .into_iter()
    .filter(|(_, n, _)| *n > 0)
    .map(|(sign, n, mark)| (format!("{sign}{n}"), mark))
    .collect();
    if counts.is_empty() {
        parts.push(("✓".into(), GitMark::Clean));
    } else {
        parts.push(("·".into(), GitMark::Separator));
        parts.extend(counts);
    }
    parts
}

/// The status bar text: `⎇ main ↑2 ↓1 · +3 ~5 ?2 !1`, showing only what isn't zero, or
/// `⎇ main ✓` when nothing changed.
pub fn summary(s: &GitStatus) -> String {
    summary_parts(s)
        .into_iter()
        .map(|(t, _)| t)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The folders that hold changes (ignored files don't count), relative to the root.
pub fn changed_folders(files: &BTreeMap<PathBuf, Change>) -> BTreeSet<PathBuf> {
    files
        .iter()
        .filter(|(_, c)| **c != Change::Ignored)
        .flat_map(|(p, _)| p.ancestors().skip(1))
        .filter(|a| !a.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect()
}

/// How `path` (relative to the repository's root) stands: its own entry, or that of the
/// untracked or ignored folder it is in.
pub fn change_of(files: &BTreeMap<PathBuf, Change>, path: &Path) -> Option<Change> {
    if let Some(c) = files.get(path) {
        return Some(*c);
    }
    path.ancestors()
        .skip(1)
        .filter_map(|a| files.get(a))
        .find(|c| matches!(c, Change::Untracked | Change::Ignored))
        .copied()
}

/// Runs git in `dir` without a console window; its output when it succeeds.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let out = hydra_platform::process::hide_window(&mut cmd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The repository `dir` is in, or `None` when it isn't in one (or git isn't installed). Runs
/// git without a console window; call it off the UI thread.
pub fn read(dir: &Path) -> Option<GitStatus> {
    let text = git(
        dir,
        &[
            // Status may refresh the index otherwise, which the repository watch would see.
            "--no-optional-locks",
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=v2",
            "--branch",
            "--ignored",
        ],
    )?;
    let mut s = parse(&text);
    // Not --show-toplevel: git resolves `subst` drives and junctions there, so the root would
    // be spelt unlike the paths the file tree matches against it.
    s.root = git(dir, &["rev-parse", "--show-cdup"]).map(|up| root_from_cdup(dir, &up));
    Some(s)
}

/// The repository's root, spelt as `dir` is: `dir` with git's `--show-cdup` ("../../") applied.
fn root_from_cdup(dir: &Path, cdup: &str) -> PathBuf {
    let mut root = PathBuf::new();
    for part in dir.components() {
        if part != std::path::Component::CurDir {
            root.push(part);
        }
    }
    for step in cdup.trim().split('/').filter(|s| !s.is_empty()) {
        if step == ".." {
            root.pop();
        }
    }
    root
}

/// `rev` as a commit's short hash in the repository at `repo_dir`, or `None` when it names no
/// commit there. Call it off the UI thread.
pub fn short_commit(repo_dir: &Path, rev: &str) -> Option<String> {
    if rev.starts_with('-') {
        return None;
    }
    let spec = format!("{rev}^{{commit}}");
    git(
        repo_dir,
        &["rev-parse", "--verify", "--quiet", "--short", &spec],
    )
    .map(|h| h.trim().to_string())
    .filter(|h| !h.is_empty())
}

/// What `commit` changed, as a patch of every file it touched (renames followed), without a
/// console window; call it off the UI thread.
pub fn commit_patch(repo_dir: &Path, commit: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(repo_dir)
        .args(["show", "--no-color", "--format=", "-M", commit, "--"]);
    let out = hydra_platform::process::hide_window(&mut cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `file`'s diff against `base` (with no commits yet, against nothing), without a console
/// window; call it off the UI thread.
pub fn diff_patch(repo_dir: &Path, file: &Path, base: &Base) -> Result<String, String> {
    let base = match base {
        Base::Head if git(repo_dir, &["rev-parse", "--verify", "-q", "HEAD"]).is_none() => {
            &Base::Nothing
        }
        b => b,
    };
    let untracked = *base == Base::Nothing;
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_dir).arg("diff").arg("--no-color");
    match base {
        Base::Nothing => {
            // The empty file to diff a new one against: the null device.
            let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
            cmd.args(["--no-index", "--", null]).arg(file);
        }
        Base::Head => {
            cmd.args(["HEAD", "--"]).arg(file);
        }
        Base::Renamed(old) => {
            cmd.args(["-M", "HEAD", "--"]).arg(old).arg(file);
        }
    }
    let out = hydra_platform::process::hide_window(&mut cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    // `--no-index` exits 1 when the files differ: that's a diff, not a failure.
    if out.status.success() || (untracked && out.status.code() == Some(1)) {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
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
                files: s.files.clone(),
                root: None,
                renamed_from: s.renamed_from.clone(),
            }
        );
        assert_eq!(summary(&s), "⎇ main ↑2 ↓1 · +3 ~2 ?2 !1");
    }

    #[test]
    fn the_summary_comes_in_marked_pieces() {
        use GitMark::*;
        let s = parse(
            "# branch.oid abc\n# branch.head main\n# branch.ab +2 -1\n\
             1 M. N... 100644 100644 100644 a a f1\n\
             1 .M N... 100644 100644 100644 a a f2\n\
             ? f3\n\
             u UU N... 100644 100644 100644 100644 a a a f4\n",
        );
        let parts = summary_parts(&s);
        let got: Vec<(&str, GitMark)> = parts.iter().map(|(t, m)| (t.as_str(), *m)).collect();
        assert_eq!(
            got,
            [
                ("⎇ main", Branch),
                ("↑2", Ahead),
                ("↓1", Behind),
                ("·", Separator),
                ("+1", Staged),
                ("~1", Modified),
                ("?1", Untracked),
                ("!1", Conflicts),
            ]
        );
        let clean = parse("# branch.oid abc\n# branch.head main\n");
        assert_eq!(
            summary_parts(&clean),
            [("⎇ main".to_string(), Branch), ("✓".to_string(), Clean)]
        );
        // The plain text is the pieces joined, as before.
        assert_eq!(
            summary(&s),
            parts
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
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
    fn files_get_their_change() {
        let text = "# branch.head main\n\
            1 .M N... 100644 100644 100644 a a src/a.rs\n\
            1 A. N... 000000 100644 100644 0 b src/new.rs\n\
            1 .D N... 100644 100644 000000 c c gone.rs\n\
            ? notes.txt\n\
            ! target/x\n";
        let s = parse(text);
        assert_eq!(s.files.get(Path::new("src/a.rs")), Some(&Change::Modified));
        assert_eq!(s.files.get(Path::new("src/new.rs")), Some(&Change::Added));
        assert_eq!(s.files.get(Path::new("gone.rs")), Some(&Change::Deleted));
        assert_eq!(
            s.files.get(Path::new("notes.txt")),
            Some(&Change::Untracked)
        );
        assert_eq!(s.files.get(Path::new("target/x")), Some(&Change::Ignored));
    }

    #[test]
    fn renames_mark_the_new_path() {
        let s = parse("2 R. N... 100644 100644 100644 a a R100 src/new.rs\tsrc/old.rs\n");
        assert_eq!(s.files.get(Path::new("src/new.rs")), Some(&Change::Renamed));
        assert_eq!(
            s.renamed_from.get(Path::new("src/new.rs")),
            Some(&PathBuf::from("src/old.rs"))
        );
        assert!(!s.files.contains_key(Path::new("src/old.rs")));
    }

    #[test]
    fn paths_with_spaces_are_kept_whole() {
        let s = parse("1 .M N... 100644 100644 100644 a a my notes/a b.md\n");
        assert_eq!(
            s.files.get(Path::new("my notes/a b.md")),
            Some(&Change::Modified)
        );
    }

    #[test]
    fn a_folder_holding_changes_is_marked() {
        let s = parse("1 .M N... 100644 100644 100644 a a src/ui/a.rs\n! target/x\n");
        let changed = changed_folders(&s.files);
        assert!(changed.contains(Path::new("src")));
        assert!(changed.contains(Path::new("src/ui")));
        assert!(!changed.contains(Path::new("docs")));
        assert!(
            !changed.contains(Path::new("src/ui/a.rs")),
            "a file isn't a folder"
        );
        assert!(
            !changed.contains(Path::new("target")),
            "ignored isn't a change"
        );
        assert!(!changed.contains(Path::new("")));
    }

    #[test]
    fn files_in_an_untracked_or_ignored_folder_take_its_change() {
        let s = parse("1 .M N... 100644 100644 100644 a a src/a.rs\n? tmp/\n! target/\n");
        assert_eq!(
            change_of(&s.files, Path::new("src/a.rs")),
            Some(Change::Modified)
        );
        assert_eq!(
            change_of(&s.files, Path::new("tmp/x/y.txt")),
            Some(Change::Untracked)
        );
        assert_eq!(
            change_of(&s.files, Path::new("target")),
            Some(Change::Ignored)
        );
        assert_eq!(
            change_of(&s.files, Path::new("target/debug/a")),
            Some(Change::Ignored)
        );
        assert_eq!(change_of(&s.files, Path::new("src/b.rs")), None);
        assert_eq!(
            change_of(&s.files, Path::new("src")),
            None,
            "a modified file doesn't spread up"
        );
    }

    #[test]
    fn a_commit_resolves_to_its_short_hash_and_patch() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(d.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            eprintln!("skipped: git isn't installed");
            return;
        }
        std::fs::write(
            d.path().join("a.txt"),
            "one
",
        )
        .unwrap();
        assert!(git(&["add", "a.txt"]));
        assert!(git(&["commit", "-q", "-m", "first"]));
        std::fs::write(
            d.path().join("a.txt"),
            "two
",
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.txt"),
            "new
",
        )
        .unwrap();
        assert!(git(&["add", "."]));
        assert!(git(&["commit", "-q", "-m", "second"]));
        let short = short_commit(d.path(), "HEAD").expect("HEAD is a commit");
        assert!(short.len() >= 4, "{short}");
        assert_eq!(
            short_commit(d.path(), &short).as_deref(),
            Some(short.as_str())
        );
        assert_eq!(short_commit(d.path(), "nope"), None);
        assert_eq!(short_commit(d.path(), "--all"), None);
        let patch = commit_patch(d.path(), &short).unwrap();
        assert!(patch.contains("+two") && patch.contains("b.txt"), "{patch}");
        // The first commit has no parent: still its whole patch.
        let root = commit_patch(d.path(), "HEAD~1").unwrap();
        assert!(root.contains("+one"), "{root}");
    }

    #[test]
    fn diffs_of_a_changed_and_an_untracked_file() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(d.path())
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            eprintln!("skipped: git isn't installed");
            return;
        }
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        assert!(git(&["add", "a.txt"]));
        assert!(git(&["commit", "-q", "-m", "x"]));
        std::fs::write(d.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(d.path().join("new.txt"), "fresh\n").unwrap();
        let changed = diff_patch(d.path(), Path::new("a.txt"), &Base::Head).unwrap();
        assert!(
            changed.contains("-one") && changed.contains("+two"),
            "{changed}"
        );
        let added = diff_patch(d.path(), Path::new("new.txt"), &Base::Nothing).unwrap();
        assert!(added.contains("+fresh"), "{added}");
    }

    /// A temp repo and a git runner that leaves the user's config alone; `None` without git.
    fn repo() -> Option<(tempfile::TempDir, impl Fn(&[&str]) -> bool)> {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let git = move |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        git(&["init", "-q"]).then_some((d, git))
    }

    #[test]
    fn a_renamed_file_diffs_against_its_old_name() {
        let Some((d, git)) = repo() else { return };
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(d.path().join("old.txt"), &body).unwrap();
        assert!(git(&["add", "."]) && git(&["commit", "-q", "-m", "x"]));
        assert!(git(&["mv", "old.txt", "new.txt"]));
        std::fs::write(d.path().join("new.txt"), body.replace("line 5\n", "five\n")).unwrap();
        let s = read(d.path()).unwrap();
        let from = s.renamed_from.get(Path::new("new.txt")).cloned();
        assert_eq!(from, Some(PathBuf::from("old.txt")), "{s:?}");
        let patch = diff_patch(
            d.path(),
            Path::new("new.txt"),
            &Base::Renamed(from.unwrap()),
        )
        .unwrap();
        assert!(
            patch.contains("-line 5") && patch.contains("+five"),
            "{patch}"
        );
        assert!(
            !patch.contains("+line 1\n"),
            "not the whole file as new: {patch}"
        );
    }

    #[test]
    fn a_repository_with_no_commits_shows_the_file_as_new() {
        let Some((d, git)) = repo() else { return };
        std::fs::write(d.path().join("a.txt"), "first\n").unwrap();
        assert!(git(&["add", "a.txt"]));
        let patch = diff_patch(d.path(), Path::new("a.txt"), &Base::Head).unwrap();
        assert!(patch.contains("+first"), "{patch}");
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
        let staged = read(d.path()).unwrap();
        assert_eq!(staged.staged, 1);
        assert_eq!(staged.files.get(Path::new("a.txt")), Some(&Change::Added));
        let root = staged.root.expect("the repository's root");
        assert_eq!(
            std::fs::canonicalize(root).unwrap(),
            std::fs::canonicalize(d.path()).unwrap()
        );
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(read(plain.path()), None);
    }

    #[test]
    fn the_root_keeps_the_spelling_of_the_folder_asked_about() {
        let api = Path::new(if cfg!(windows) {
            r"Q:\code\api"
        } else {
            "/code/api"
        });
        assert_eq!(root_from_cdup(api, ""), api);
        assert_eq!(
            root_from_cdup(&api.join("src").join("lib"), "../../\n"),
            api
        );
        assert_eq!(root_from_cdup(&api.join(".").join("src"), "../"), api);
    }

    /// A folder reached through a junction (as through a `subst` drive; a symlink on Unix):
    /// git names the real folder as its top, but the root must be spelt as the tree spells
    /// its paths.
    #[test]
    fn a_repository_through_a_junction_keeps_the_junction_path_as_its_root() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(&real)
            .args(["init", "-q"])
            .output();
        if !init.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: git isn't installed");
            return;
        }
        let link = d.path().join("link");
        #[cfg(windows)]
        {
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(&real)
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(made, "mklink /J");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::fs::write(real.join("sub").join("a.txt"), "x").unwrap();
        let s = read(&link.join("sub")).unwrap();
        assert_eq!(s.root.as_deref(), Some(link.as_path()));
        assert_eq!(s.untracked, 1, "{s:?}");
        // Leave the junction, not the folder it points at, to the temp dir's cleanup.
        let _ = std::fs::remove_dir(&link);
    }
}
