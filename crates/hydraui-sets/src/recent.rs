//! `<HYDRA_HOME>/hydraui/recent.toml` (spec §7.12): recently opened layouts, newest first,
//! shared by every hydraui. Each change rereads the file, so windows don't lose each
//! other's entries.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};

use crate::layout::write_atomic;

pub const MAX_RECENT: usize = 12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    /// When it was last opened, in Unix seconds.
    pub opened: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    layout: Vec<RecentEntry>,
}

fn recent_file(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("recent.toml")
}

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// The same file, ignoring case and `/` against `\`.
pub fn same_path(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// The list, newest first; empty when the file is missing or unreadable.
pub fn read_recent(paths: &HydraPaths) -> Vec<RecentEntry> {
    std::fs::read_to_string(recent_file(paths))
        .ok()
        .and_then(|t| toml::from_str::<RecentFile>(&t).ok())
        .map(|f| f.layout)
        .unwrap_or_default()
}

/// Rereads the list, edits it, keeps the newest `MAX_RECENT`, and replaces the file.
fn change(paths: &HydraPaths, edit: impl FnOnce(&mut Vec<RecentEntry>)) -> std::io::Result<()> {
    let mut list = read_recent(paths);
    edit(&mut list);
    list.truncate(MAX_RECENT);
    let text = toml::to_string(&RecentFile { layout: list }).map_err(std::io::Error::other)?;
    write_atomic(&recent_file(paths), &text)
}

/// Puts `path` at the top, opened `now`.
pub fn touch_recent(paths: &HydraPaths, path: &Path, now: u64) -> std::io::Result<()> {
    change(paths, |list| {
        list.retain(|e| !same_path(&e.path, path));
        list.insert(
            0,
            RecentEntry {
                path: path.to_path_buf(),
                opened: now,
            },
        );
    })
}

pub fn forget_recent(paths: &HydraPaths, path: &Path) -> std::io::Result<()> {
    change(paths, |list| list.retain(|e| !same_path(&e.path, path)))
}

/// A converted file: `from`'s entry becomes `to`, at the top.
pub fn rename_recent(paths: &HydraPaths, from: &Path, to: &Path, now: u64) -> std::io::Result<()> {
    change(paths, |list| {
        list.retain(|e| !same_path(&e.path, from) && !same_path(&e.path, to));
        list.insert(
            0,
            RecentEntry {
                path: to.to_path_buf(),
                opened: now,
            },
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, HydraPaths) {
        let d = tempfile::tempdir().unwrap();
        let p = HydraPaths::new(d.path());
        (d, p)
    }

    #[test]
    fn newest_first_without_duplicates() {
        let (_d, paths) = home();
        touch_recent(&paths, Path::new(r"C:\l\a.hydraui"), 10).unwrap();
        touch_recent(&paths, Path::new(r"C:\l\b.hydraui"), 20).unwrap();
        touch_recent(&paths, Path::new(r"C:\l\a.hydraui"), 30).unwrap();
        let got: Vec<(String, u64)> = read_recent(&paths)
            .into_iter()
            .map(|e| (e.path.display().to_string(), e.opened))
            .collect();
        assert_eq!(
            got,
            [
                (r"C:\l\a.hydraui".into(), 30),
                (r"C:\l\b.hydraui".into(), 20)
            ]
        );
    }

    #[test]
    fn the_list_keeps_twelve() {
        let (_d, paths) = home();
        for i in 0..15u64 {
            touch_recent(&paths, &PathBuf::from(format!(r"C:\l\{i}.hydraui")), i).unwrap();
        }
        let list = read_recent(&paths);
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0].path, PathBuf::from(r"C:\l\14.hydraui"));
    }

    #[test]
    fn paths_match_ignoring_case_and_slashes() {
        assert!(same_path(
            Path::new("C:/Work/A.hydraui"),
            Path::new(r"c:\work\a.hydraui")
        ));
        assert!(!same_path(
            Path::new(r"C:\a.hydraui"),
            Path::new(r"C:\b.hydraui")
        ));
        let (_d, paths) = home();
        touch_recent(&paths, Path::new("C:/Work/A.hydraui"), 1).unwrap();
        touch_recent(&paths, Path::new(r"c:\work\a.hydraui"), 2).unwrap();
        assert_eq!(read_recent(&paths).len(), 1);
    }

    #[test]
    fn each_change_rereads_the_list() {
        // Two windows: each touch reads what the other wrote.
        let (_d, paths) = home();
        touch_recent(&paths, Path::new(r"C:\l\a.hydraui"), 1).unwrap();
        let other = HydraPaths::new(paths.root());
        touch_recent(&other, Path::new(r"C:\l\b.hydraui"), 2).unwrap();
        assert_eq!(read_recent(&paths).len(), 2);
    }

    #[test]
    fn forget_and_rename() {
        let (_d, paths) = home();
        touch_recent(&paths, Path::new(r"C:\l\a.hydraui.toml"), 1).unwrap();
        touch_recent(&paths, Path::new(r"C:\l\b.hydraui"), 2).unwrap();
        rename_recent(
            &paths,
            Path::new(r"C:\l\a.hydraui.toml"),
            Path::new(r"C:\l\a.hydraui"),
            3,
        )
        .unwrap();
        forget_recent(&paths, Path::new(r"C:\l\b.hydraui")).unwrap();
        let list = read_recent(&paths);
        assert_eq!(
            list,
            [RecentEntry {
                path: r"C:\l\a.hydraui".into(),
                opened: 3
            }]
        );
    }

    #[test]
    fn an_unreadable_list_counts_as_empty() {
        let (_d, paths) = home();
        std::fs::create_dir_all(paths.root().join("hydraui")).unwrap();
        std::fs::write(
            paths.root().join("hydraui").join("recent.toml"),
            "\u{0}junk",
        )
        .unwrap();
        assert!(read_recent(&paths).is_empty());
        touch_recent(&paths, Path::new(r"C:\l\a.hydraui"), 1).unwrap();
        assert_eq!(read_recent(&paths).len(), 1);
    }
}
