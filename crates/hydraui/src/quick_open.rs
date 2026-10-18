//! Quick open (spec §7.10): the folder's files, fuzzy-matched. No UI.
use std::path::{Path, PathBuf};

/// How well `query` matches `path`: `None` unless its letters appear in order (ignoring
/// case). Consecutive letters, word starts and letters in the file name score higher, and
/// shorter paths win ties.
pub fn score(query: &str, path: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let name_start = path
        .rfind(['/', '\\'])
        .map_or(0, |i| path[..=i].chars().count());
    let mut score = 0i32;
    let mut from = 0;
    let mut last: Option<usize> = None;
    for qc in &q {
        let found = (from..p.len()).find(|&i| p[i] == *qc)?;
        score += 1;
        if found > 0 && last == Some(found - 1) {
            score += 5;
        }
        let prev = found.checked_sub(1).map(|i| p[i]);
        if prev.is_none_or(|c| matches!(c, '/' | '\\' | '_' | '-' | '.' | ' ')) {
            score += 8;
        }
        if found >= name_start {
            score += 3;
        }
        last = Some(found);
        from = found + 1;
    }
    Some(score * 100 - p.len() as i32)
}

/// `name:42` → (`name`, 42); otherwise (`query`, 0).
pub fn split_line(query: &str) -> (&str, usize) {
    match query.rsplit_once(':') {
        Some((name, n)) if !name.is_empty() => match n.parse::<usize>() {
            Ok(line) => (name, line),
            Err(_) => (query, 0),
        },
        _ => (query, 0),
    }
}

/// The files under `root` (relative), `.gitignore` respected, at most `cap`; `true` when cut
/// off.
pub fn list_files(root: &Path, cap: usize) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if out.len() == cap {
                return (out, true);
            }
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_must_appear_in_order() {
        assert!(score("wsrs", "src/workspace.rs").is_some());
        assert!(score("srw", "src/workspace.rs").is_some());
        assert!(score("zz", "src/workspace.rs").is_none());
        assert!(score("", "anything").is_some());
    }

    #[test]
    fn file_names_and_word_starts_rank_higher() {
        let file = score("main", "src/main.rs").unwrap();
        let deep = score("main", "domain/xmain/zzz.rs").unwrap();
        assert!(file > deep, "{file} vs {deep}");
        let starts = score("fp", "file_pane.rs").unwrap();
        let middle = score("fp", "xfxp.rs").unwrap();
        assert!(starts > middle);
    }

    #[test]
    fn a_trailing_line_number_is_split_off() {
        assert_eq!(split_line("main.rs:42"), ("main.rs", 42));
        assert_eq!(split_line("main.rs"), ("main.rs", 0));
        assert_eq!(split_line("C:"), ("C:", 0));
    }

    #[test]
    fn listing_respects_gitignore_and_the_cap() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::create_dir_all(d.path().join("target")).unwrap();
        std::fs::write(d.path().join(".gitignore"), "target/\n").unwrap();
        for f in ["src/a.rs", "src/b.rs", "target/x.bin", "README.md"] {
            std::fs::write(d.path().join(f), "").unwrap();
        }
        let (files, cut) = list_files(d.path(), 100);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert!(names.contains(&"src/a.rs".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("target")));
        assert!(!cut);
        let (two, cut) = list_files(d.path(), 2);
        assert_eq!(two.len(), 2);
        assert!(cut);
    }
}
