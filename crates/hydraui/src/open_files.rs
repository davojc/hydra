//! The file pane's file tabs (spec §7.8). No UI.
use std::path::{Path, PathBuf};

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

/// The file tabs that closing all but `keep` (or all, for `None`) closes, last first so each
/// index stays good as the ones after it go.
pub fn to_close(len: usize, keep: Option<usize>) -> Vec<usize> {
    (0..len).rev().filter(|&i| Some(i) != keep).collect()
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
    fn closing_others_or_all_goes_from_the_last_tab() {
        assert_eq!(to_close(4, Some(1)), vec![3, 2, 0]);
        assert_eq!(to_close(3, None), vec![2, 1, 0]);
        assert_eq!(to_close(1, Some(0)), Vec::<usize>::new());
        assert_eq!(to_close(0, None), Vec::<usize>::new());
    }
}
