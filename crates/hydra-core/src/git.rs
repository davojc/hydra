//! What counts as a git repository.

use std::path::Path;

/// Whether `dir` is the root of a git repository: its `.git` is a directory holding `HEAD`, or a
/// file starting `gitdir:` (a linked worktree or submodule). A `.git` that merely exists is not
/// enough: a home folder can carry a `.git` of config files that no git command treats as a repo.
pub fn is_repo_root(dir: &Path) -> bool {
    let git = dir.join(".git");
    if git.is_dir() {
        git.join("HEAD").exists()
    } else {
        std::fs::read_to_string(&git).is_ok_and(|t| t.starts_with("gitdir:"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_dir_with_head_is_a_root() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(is_repo_root(d.path()));
    }

    #[test]
    fn a_git_dir_of_config_files_is_not() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git/info")).unwrap();
        std::fs::write(d.path().join(".git/.gitconfig"), "[user]\n").unwrap();
        assert!(!is_repo_root(d.path()));
    }

    #[test]
    fn a_gitdir_file_is_a_root() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".git"), "gitdir: ../x/.git/worktrees/a\n").unwrap();
        assert!(is_repo_root(d.path()));
    }

    #[test]
    fn a_git_file_with_other_text_is_not() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".git"), "hello\n").unwrap();
        assert!(!is_repo_root(d.path()));
    }

    #[test]
    fn no_git_is_not() {
        let d = tempfile::tempdir().unwrap();
        assert!(!is_repo_root(d.path()));
    }
}
