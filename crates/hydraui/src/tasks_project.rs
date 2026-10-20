//! Where a terminal's task board lives and whether it is committed (spec:
//! docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md §2-§3): the project is the
//! terminal's git work tree, the board is `<project>/.hydra/tasks.md`, and a `.gitignore` line
//! of ours keeps it private.

use std::path::{Path, PathBuf};

use hydra_core::git::is_repo_root;

use crate::git_watch::ignore_rules;
use crate::tasks::{Board, Change, apply, parse, write};

/// The `.gitignore` line that keeps the board out of git.
pub const IGNORE_LINE: &str = ".hydra/tasks.md";

/// The git work tree containing `dir` (nearest ancestor-or-self that is a git repository root),
/// else `dir` itself.
pub fn project_root(dir: &Path) -> PathBuf {
    dir.ancestors()
        .find(|d| is_repo_root(d))
        .unwrap_or(dir)
        .to_path_buf()
}

/// Whether `project` is a git work tree (a repository root itself).
pub fn is_repo(project: &Path) -> bool {
    is_repo_root(project)
}

pub fn tasks_path(project: &Path) -> PathBuf {
    project.join(".hydra").join("tasks.md")
}

pub struct Committed {
    pub board: Board,
    /// The exact text now on disk.
    pub text: String,
    /// This write created the file.
    pub created: bool,
}

#[derive(Debug)]
pub enum CommitError {
    /// The card the change names isn't in the file any more.
    Missing,
    Io(std::io::Error),
}

impl From<std::io::Error> for CommitError {
    fn from(e: std::io::Error) -> Self {
        CommitError::Io(e)
    }
}

/// A file's text, `None` when it doesn't exist.
fn read_if_present(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Reads the board; an absent file is `(Board::default(), String::new())`.
pub fn load(project: &Path) -> std::io::Result<(Board, String)> {
    let text = read_if_present(&tasks_path(project))?.unwrap_or_default();
    Ok((parse(&text), text))
}

/// Re-reads the file (absent = empty board), applies `change` and writes atomically (creating
/// `.hydra`), unless the board comes out as it was read: then nothing is written, so an empty
/// Add on a missing file creates nothing. The board returned is the written text read back, so
/// it is what a reload of the file would show.
pub fn commit(project: &Path, change: &Change) -> Result<Committed, CommitError> {
    let existing = read_if_present(&tasks_path(project))?;
    let before = parse(existing.as_deref().unwrap_or_default());
    let mut board = before.clone();
    apply(&mut board, change).map_err(|_| CommitError::Missing)?;
    if board == before {
        return Ok(Committed {
            board,
            text: existing.unwrap_or_default(),
            created: false,
        });
    }
    let text = write(&board);
    hydraui_sets::layout::write_atomic(&tasks_path(project), &text)?;
    Ok(Committed {
        board: parse(&text),
        text,
        created: existing.is_none(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    Committed,
    PrivateByUs,
    IgnoredElsewhere,
}

fn gitignore_path(project: &Path) -> PathBuf {
    project.join(".gitignore")
}

fn read_gitignore(project: &Path) -> std::io::Result<String> {
    Ok(read_if_present(&gitignore_path(project))?.unwrap_or_default())
}

fn is_our_line(line: &str) -> bool {
    line.trim() == IGNORE_LINE
}

/// `PrivateByUs` when the project's `.gitignore` has `IGNORE_LINE` as a whole line;
/// `IgnoredElsewhere` when its other ignore rules cover the file anyway; else `Committed`.
pub fn privacy(project: &Path) -> Privacy {
    let ours = read_gitignore(project)
        .unwrap_or_default()
        .lines()
        .any(is_our_line);
    if ours {
        return Privacy::PrivateByUs;
    }
    let path = tasks_path(project);
    let rel = path.strip_prefix(project).unwrap_or(&path);
    if ignore_rules(project)
        .matched_path_or_any_parents(rel, false)
        .is_ignore()
    {
        Privacy::IgnoredElsewhere
    } else {
        Privacy::Committed
    }
}

/// Adds `IGNORE_LINE` to `.gitignore` (creating it) on a line of its own, if it isn't there.
pub fn keep_private(project: &Path) -> std::io::Result<()> {
    let mut text = read_gitignore(project)?;
    if text.lines().any(is_our_line) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(IGNORE_LINE);
    text.push('\n');
    std::fs::write(gitignore_path(project), text)
}

/// Removes our line from `.gitignore`, leaving every other line as it was.
pub fn commit_tasks(project: &Path) -> std::io::Result<()> {
    let text = read_gitignore(project)?;
    if !text.lines().any(is_our_line) {
        return Ok(());
    }
    let kept: String = text
        .split_inclusive('\n')
        .filter(|l| !is_our_line(l))
        .collect();
    std::fs::write(gitignore_path(project), kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{Card, Column};

    fn fake_repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git/HEAD"),
            "ref: refs/heads/main
",
        )
        .unwrap();
    }

    #[test]
    fn a_repo_subfolder_belongs_to_the_repo() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(d.path());
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        assert_eq!(project_root(&d.path().join("a/b")), d.path());
        assert!(is_repo(d.path()));
    }

    #[test]
    fn a_git_folder_of_config_files_is_not_a_repo() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git/info")).unwrap();
        std::fs::write(
            d.path().join(".git/.gitconfig"),
            "[user]
",
        )
        .unwrap();
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(project_root(&sub), sub);
        assert!(!is_repo(d.path()));
    }

    #[test]
    fn a_linked_worktree_is_its_own_project() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(d.path());
        let wt = d.path().join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();
        assert_eq!(project_root(&wt.join("src")), wt);
    }

    #[test]
    fn outside_a_repo_the_folder_is_the_project() {
        let d = tempfile::tempdir().unwrap();
        // The temp dir may itself sit inside someone's repo (a git-tracked home folder).
        let root = project_root(d.path());
        if d.path().ancestors().any(is_repo_root) {
            assert!(is_repo(&root));
        } else {
            assert_eq!(root, d.path());
        }
        assert!(!is_repo(d.path()));
    }

    #[test]
    fn the_first_card_creates_the_file() {
        let d = tempfile::tempdir().unwrap();
        let c = commit(d.path(), &Change::Add { text: "".into() }).unwrap();
        assert!(
            !c.created && !tasks_path(d.path()).exists(),
            "empty add creates nothing"
        );
        let c = commit(
            d.path(),
            &Change::Add {
                text: "idea".into(),
            },
        )
        .unwrap();
        assert!(c.created);
        assert_eq!(
            std::fs::read_to_string(tasks_path(d.path())).unwrap(),
            c.text
        );
        assert_eq!(c.board.cards(Column::Backlog), [Card::new("idea")]);
    }

    #[test]
    fn commit_rereads_the_file() {
        let d = tempfile::tempdir().unwrap();
        commit(
            d.path(),
            &Change::Add {
                text: "mine".into(),
            },
        )
        .unwrap();
        let (seen, _) = load(d.path()).unwrap();
        std::fs::write(
            tasks_path(d.path()),
            "## Backlog\n- mine\n## Pickup\n- claude's\n",
        )
        .unwrap();
        let card = seen.card_ref(Column::Backlog, 0).unwrap();
        let c = commit(
            d.path(),
            &Change::Edit {
                card,
                text: "mine, edited".into(),
            },
        )
        .unwrap();
        assert_eq!(c.board.cards(Column::Backlog), [Card::new("mine, edited")]);
        assert_eq!(c.board.cards(Column::Pickup), [Card::new("claude's")]);
    }

    #[test]
    fn commit_returns_the_board_as_the_file_reads() {
        let d = tempfile::tempdir().unwrap();
        let c = commit(
            d.path(),
            &Change::Add {
                text: "[x] foo".into(),
            },
        )
        .unwrap();
        let (loaded, text) = load(d.path()).unwrap();
        assert_eq!(c.text, text);
        assert_eq!(c.board, loaded);
        assert_eq!(c.board.cards(Column::Backlog), [Card::new("foo")]);
    }

    #[test]
    fn an_unchanged_board_is_not_rewritten() {
        let d = tempfile::tempdir().unwrap();
        let odd = "## Backlog\n* odd bullet\n";
        std::fs::create_dir_all(d.path().join(".hydra")).unwrap();
        std::fs::write(tasks_path(d.path()), odd).unwrap();
        let c = commit(d.path(), &Change::ClearDone).unwrap();
        assert!(!c.created);
        assert_eq!(std::fs::read_to_string(tasks_path(d.path())).unwrap(), odd);
    }

    #[test]
    fn a_vanished_card_is_missing() {
        let d = tempfile::tempdir().unwrap();
        let card = parse("## Done\n- gone\n")
            .card_ref(Column::Done, 0)
            .unwrap();
        assert!(matches!(
            commit(d.path(), &Change::Delete { card }),
            Err(CommitError::Missing)
        ));
    }

    #[test]
    fn keep_private_appends_on_its_own_line() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(d.path());
        std::fs::write(d.path().join(".gitignore"), "target").unwrap();
        keep_private(d.path()).unwrap();
        keep_private(d.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join(".gitignore")).unwrap(),
            "target\n.hydra/tasks.md\n"
        );
        assert_eq!(privacy(d.path()), Privacy::PrivateByUs);
    }

    #[test]
    fn keep_private_creates_gitignore() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(d.path());
        keep_private(d.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join(".gitignore")).unwrap(),
            ".hydra/tasks.md\n"
        );
    }

    #[test]
    fn commit_tasks_removes_only_our_line() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(d.path());
        std::fs::write(
            d.path().join(".gitignore"),
            "target\n.hydra/tasks.md\n# keep\n",
        )
        .unwrap();
        commit_tasks(d.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join(".gitignore")).unwrap(),
            "target\n# keep\n"
        );
        assert_eq!(privacy(d.path()), Privacy::Committed);
    }

    #[test]
    fn another_rule_counts_as_ignored_elsewhere() {
        let d = tempfile::tempdir().unwrap();
        fake_repo(d.path());
        std::fs::write(d.path().join(".gitignore"), ".hydra/\n").unwrap();
        assert_eq!(privacy(d.path()), Privacy::IgnoredElsewhere);
    }
}
