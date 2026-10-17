//! The file pane's file tabs (spec §7.8), and which terminal stands for a tab when its file
//! pane has the focus. No UI.
use std::path::{Path, PathBuf};

use crate::split::PaneId;

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_ascii_lowercase()
}

/// The file tab already showing `path`, if any (Windows paths: case and slashes don't matter).
pub fn find(paths: &[PathBuf], path: &Path) -> Option<usize> {
    let k = key(path);
    paths.iter().position(|p| key(p) == k)
}

/// The active index after closing `closed` from `len` file tabs; `None` when none are left.
/// The tab to the right takes the place of a closed active one, or the left one if it was
/// last; closing another tab keeps the same file active.
pub fn after_close(len: usize, active: usize, closed: usize) -> Option<usize> {
    let left = len.checked_sub(1)?;
    if left == 0 {
        return None;
    }
    Some(if closed < active {
        active - 1
    } else if closed == active {
        active.min(left - 1)
    } else {
        active
    })
}

/// The terminal a tab's status bar, git summary, tree root, insert, `cd` and Split use: the
/// focused pane if it's a terminal, else the last focused terminal still in the tab, else the
/// tab's first terminal.
pub fn terminal_for(
    focused: PaneId,
    last: Option<PaneId>,
    tree_panes: &[PaneId],
    is_terminal: &dyn Fn(PaneId) -> bool,
) -> Option<PaneId> {
    if is_terminal(focused) {
        return Some(focused);
    }
    last.filter(|l| tree_panes.contains(l) && is_terminal(*l))
        .or_else(|| tree_panes.iter().copied().find(|p| is_terminal(*p)))
}

/// Whether closing `closing` leaves the tab with no terminal (and so closes the tab).
pub fn last_terminal_closes_tab(
    tree_panes: &[PaneId],
    closing: PaneId,
    is_terminal: &dyn Fn(PaneId) -> bool,
) -> bool {
    is_terminal(closing) && !tree_panes.iter().any(|p| *p != closing && is_terminal(*p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_file_is_found_however_its_path_is_written() {
        let paths = vec![
            PathBuf::from("C:\\work\\README.md"),
            PathBuf::from("C:\\work\\a.rs"),
        ];
        assert_eq!(find(&paths, Path::new("c:/work/readme.md")), Some(0));
        assert_eq!(find(&paths, Path::new("C:\\work\\a.rs")), Some(1));
        assert_eq!(find(&paths, Path::new("C:\\work\\b.rs")), None);
    }

    #[test]
    fn closing_picks_the_next_file_tab() {
        assert_eq!(
            after_close(3, 1, 1),
            Some(1),
            "the right one moves into place"
        );
        assert_eq!(after_close(3, 2, 2), Some(1), "the last one: the left one");
        assert_eq!(
            after_close(3, 2, 0),
            Some(1),
            "another tab: the same file stays active"
        );
        assert_eq!(after_close(3, 0, 2), Some(0));
        assert_eq!(after_close(1, 0, 0), None);
    }

    #[test]
    fn the_terminal_is_the_focused_one_else_the_last_else_the_first() {
        let term = |id: PaneId| id != 9;
        assert_eq!(terminal_for(2, Some(1), &[1, 2, 9], &term), Some(2));
        assert_eq!(terminal_for(9, Some(1), &[1, 2, 9], &term), Some(1));
        assert_eq!(
            terminal_for(9, Some(5), &[1, 2, 9], &term),
            Some(1),
            "last is gone"
        );
        assert_eq!(terminal_for(9, None, &[9], &term), None);
    }

    #[test]
    fn closing_the_last_terminal_closes_the_tab() {
        let term = |id: PaneId| id != 9;
        assert!(last_terminal_closes_tab(&[1, 9], 1, &term));
        assert!(!last_terminal_closes_tab(&[1, 2, 9], 1, &term));
        assert!(
            !last_terminal_closes_tab(&[1, 9], 9, &term),
            "closing the file pane"
        );
    }
}
