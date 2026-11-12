//! What of a swarm folder syncs: the file types, the dot rule and the owner's `.swarmignore`.
//!
//! Kept pure (no disk, no network) so the scan, the watcher and the pull side all get the same
//! answer.
use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::paths::{RelPath, allowed};

/// The root file that carries the ignore patterns. It always syncs, so the owner's rules reach
/// everyone through the folder as well as the roster.
pub const SWARMIGNORE: &str = ".swarmignore";

pub struct SyncRules {
    types: Vec<String>,
    ignore: Gitignore,
}

impl SyncRules {
    pub fn new(types: &[String], ignore: Option<&str>) -> SyncRules {
        // Rooted at a dummy folder: patterns only ever see swarm-relative paths.
        let mut b = GitignoreBuilder::new("/swarm");
        for line in ignore.unwrap_or("").lines() {
            // A line that does not parse is skipped so the rest still apply.
            let _ = b.add_line(None, line);
        }
        let ignore = b.build().unwrap_or_else(|_| Gitignore::empty());
        SyncRules {
            types: types.to_vec(),
            ignore,
        }
    }

    /// Whether any folder in `p` starts with a dot (`.git`, `.github`, ...). The last part counts
    /// only when `p` is itself a folder, so `.notes.md` is an ordinary file.
    fn dotted(p: &RelPath, is_dir: bool) -> bool {
        let parts: Vec<&str> = p.as_str().split('/').collect();
        let folders = if is_dir { parts.len() } else { parts.len() - 1 };
        parts[..folders].iter().any(|c| c.starts_with('.'))
    }

    /// Whether the file `p` is inside a folder starting with a dot.
    pub fn under_dot_folder(p: &RelPath) -> bool {
        Self::dotted(p, false)
    }

    fn ignored(&self, p: &RelPath, is_dir: bool) -> bool {
        self.ignore
            .matched_path_or_any_parents(Path::new(p.as_str()), is_dir)
            .is_ignore()
    }

    pub fn file_syncs(&self, p: &RelPath) -> bool {
        if p.as_str() == SWARMIGNORE {
            return true;
        }
        !Self::under_dot_folder(p) && allowed(p, 0, &self.types) && !self.ignored(p, false)
    }

    /// Whether a folder may hold synced files, so the scan can skip whole trees.
    pub fn folder_may_sync(&self, p: &RelPath) -> bool {
        !Self::dotted(p, true) && !self.ignored(p, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::safe_rel_path;

    fn rel(s: &str) -> RelPath {
        safe_rel_path(s).unwrap()
    }

    fn types(t: &[&str]) -> Vec<String> {
        t.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn a_folder_starting_with_a_dot_never_syncs() {
        let r = SyncRules::new(&types(&[".md"]), None);
        assert!(!r.file_syncs(&rel(".github/a.md")));
        assert!(!r.folder_may_sync(&rel(".github")));
        assert!(!r.file_syncs(&rel("docs/.hidden/a.md")));
    }

    #[test]
    fn a_file_starting_with_a_dot_is_not_affected() {
        let r = SyncRules::new(&types(&[".md"]), None);
        assert!(r.file_syncs(&rel("docs/.notes.md")));
    }

    #[test]
    fn swarmignore_at_the_root_always_syncs() {
        let r = SyncRules::new(&types(&[".md"]), Some("*"));
        assert!(r.file_syncs(&rel(".swarmignore")));
        assert!(!r.file_syncs(&rel("docs/.swarmignore")));
    }

    #[test]
    fn swarmignore_patterns_take_files_and_folders_out() {
        let r = SyncRules::new(&types(&[".md"]), Some("build/\n*.tmp.md\n"));
        assert!(!r.file_syncs(&rel("build/a.md")));
        assert!(!r.folder_may_sync(&rel("build")));
        assert!(!r.file_syncs(&rel("a.tmp.md")));
        assert!(r.file_syncs(&rel("a.md")));
    }

    #[test]
    fn a_bang_line_brings_back_only_what_a_pattern_took_out() {
        let r = SyncRules::new(
            &types(&[".md"]),
            Some("drafts/**\n!drafts/keep.md\n.github/\n!.github/**"),
        );
        assert!(r.file_syncs(&rel("drafts/keep.md")));
        assert!(!r.file_syncs(&rel("drafts/x.md")));
        assert!(!r.file_syncs(&rel(".github/a.md")));
        let r = SyncRules::new(&types(&[".md"]), Some("!a.py"));
        assert!(!r.file_syncs(&rel("a.py")));
    }
}
