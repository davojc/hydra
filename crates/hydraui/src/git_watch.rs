//! Watches the focused terminal's repository so the file tree's git marks and the status bar
//! follow a save or a `git commit` within a moment, not the 15-second timer (spec §7.3).
use std::path::{Path, PathBuf};

use futures::channel::mpsc::UnboundedSender;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// The repository's top-level ignore rules (`.gitignore`, `.git/info/exclude`), to skip
/// changes in build output and the like. Nested `.gitignore`s aren't read: a change there only
/// costs an extra refresh.
pub fn ignore_rules(root: &Path) -> Gitignore {
    let mut b = GitignoreBuilder::new(root);
    let _ = b.add(root.join(".gitignore"));
    let _ = b.add(root.join(".git").join("info").join("exclude"));
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Whether a change to `path` can change what `git status` says.
pub fn relevant(root: &Path, path: &Path, rules: &Gitignore) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy());
    match parts.next().as_deref() {
        None => false,
        Some(".git") => match parts.next().as_deref() {
            Some("index") | Some("HEAD") => parts.next().is_none(),
            Some("refs") => true,
            _ => false,
        },
        Some(_) => !rules
            .matched_path_or_any_parents(rel, path.is_dir())
            .is_ignore(),
    }
}

/// A recursive watch on a repository that sends `()` whenever something relevant changes.
pub struct GitWatch {
    pub root: PathBuf,
    _watcher: RecommendedWatcher,
}

impl GitWatch {
    /// `None` when the folder can't be watched (the 15-second timer still covers it).
    pub fn new(root: PathBuf, tx: UnboundedSender<()>) -> Option<Self> {
        let rules = ignore_rules(&root);
        let base = root.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && event.paths.iter().any(|p| relevant(&base, p, &rules))
            {
                let _ = tx.unbounded_send(());
            }
        })
        .ok()?;
        watcher.watch(&root, RecursiveMode::Recursive).ok()?;
        Some(Self {
            root,
            _watcher: watcher,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_tree_changes_count_but_ignored_files_and_git_internals_dont() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        let rules = ignore_rules(root);
        let yes = |p: &str| relevant(root, &root.join(p), &rules);
        assert!(yes("src/main.rs"));
        assert!(yes("README.md"));
        assert!(!yes("target/debug/hydra.exe"), "gitignored folder");
        assert!(!yes("build.log"), "gitignored pattern");
        // git add / commit / checkout change these:
        assert!(yes(".git/index"));
        assert!(yes(".git/HEAD"));
        assert!(yes(".git/refs/heads/main"));
        // …but not these, which churn without changing what status says:
        assert!(!yes(".git/objects/ab/cdef"));
        assert!(!yes(".git/logs/HEAD"));
        assert!(!yes(".git/index.lock"));
        assert!(!yes(".git"), "the folder itself");
        assert!(!relevant(root, Path::new(r"C:\elsewhere\a.rs"), &rules));
    }
}
