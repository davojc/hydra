//! Swarm-relative paths, the allow-list, conflict-copy names and the document keys that
//! files and signed changes are stored under.
use std::path::{Component, Path};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::roster::SignedChange;

pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const LOG_PREFIX: &[u8] = b"log/";
pub const FILES_PREFIX: &[u8] = b"files/";

/// A swarm-relative path with `/` separators, checked safe on every OS.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Characters no Windows file name can hold, which a swarm path must not rely on anywhere.
fn forbidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
}

fn safe_segment(seg: &str) -> bool {
    if seg.is_empty() || seg == "." || seg == ".." || seg.ends_with([' ', '.']) {
        return false;
    }
    if seg.chars().any(|c| c != '/' && forbidden_char(c)) {
        return false;
    }
    // Windows reserves these whatever follows the first dot, and ignores trailing spaces before it.
    let stem = seg
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_lowercase();
    !RESERVED.contains(&stem.as_str())
}

/// Checks a path received from a peer (or read from disk) is safe to write under a swarm root.
pub fn safe_rel_path(s: &str) -> Option<RelPath> {
    if s.split('/').all(safe_segment) {
        Some(RelPath(s.to_string()))
    } else {
        None
    }
}

/// `file` relative to `root`, as a checked swarm path.
pub fn rel_from_disk(root: &Path, file: &Path) -> Option<RelPath> {
    let rest = file.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rest.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str()?),
            _ => return None,
        }
    }
    safe_rel_path(&parts.join("/"))
}

/// The file name of `p` split into stem and extension (with its dot; empty when there is none).
fn split_name(p: &str) -> (&str, &str, &str) {
    let (dir, name) = match p.rfind('/') {
        Some(i) => p.split_at(i + 1),
        None => ("", p),
    };
    match name.rfind('.') {
        // A ')' after the dot means the dot is inside a conflict marker, not before an extension.
        Some(i) if i > 0 && !name[i..].contains(')') => (dir, &name[..i], &name[i..]),
        _ => (dir, name, ""),
    }
}

/// Whether `p` may be synced: a listed extension (case-insensitive, e.g. `.md`) and a size within the cap.
pub fn allowed(p: &RelPath, size: u64, types: &[String]) -> bool {
    if size > MAX_FILE_BYTES {
        return false;
    }
    let (_, _, ext) = split_name(&p.0);
    !ext.is_empty() && types.iter().any(|t| t.eq_ignore_ascii_case(ext))
}

/// The name a losing copy of `p` is kept under: `stem (conflict, machine, 27 Oct 13.05).ext`.
pub fn conflict_name(p: &RelPath, machine: &str, at: Timestamp, tz: &TimeZone) -> RelPath {
    let machine: String = machine
        .chars()
        .map(|c| if forbidden_char(c) { '_' } else { c })
        .collect();
    let when = at.to_zoned(tz.clone()).strftime("%-d %b %H.%M");
    let (dir, stem, ext) = split_name(&p.0);
    RelPath(format!("{dir}{stem} (conflict, {machine}, {when}){ext}"))
}

pub fn file_key(p: &RelPath) -> Vec<u8> {
    let mut k = FILES_PREFIX.to_vec();
    k.extend_from_slice(p.0.as_bytes());
    k.push(0);
    k
}

pub fn path_of_key(key: &[u8]) -> Option<RelPath> {
    let rest = key.strip_prefix(FILES_PREFIX)?.strip_suffix(&[0])?;
    safe_rel_path(std::str::from_utf8(rest).ok()?)
}

/// Sorts by time, then signature, so the log reads in the order the roster folds it.
pub fn log_key(c: &SignedChange) -> Vec<u8> {
    let sig = c.sig.get(..16).unwrap_or(&c.sig);
    let mut k = LOG_PREFIX.to_vec();
    k.extend_from_slice(format!("{:020}-{}", c.at, sig).as_bytes());
    k
}

/// Whether `p` is named like a copy made by `conflict_name`.
pub fn is_conflict_copy(p: &RelPath) -> bool {
    let (_, stem, _) = split_name(&p.0);
    let Some(inner) = stem.strip_suffix(')') else {
        return false;
    };
    let Some(i) = inner.rfind(" (conflict, ") else {
        return false;
    };
    let Some((_machine, when)) = inner[i + " (conflict, ".len()..].rsplit_once(", ") else {
        return false;
    };
    is_conflict_time(when)
}

/// `d Mon HH.MM`.
fn is_conflict_time(s: &str) -> bool {
    let digits = |t: &str, n: std::ops::RangeInclusive<usize>| {
        n.contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit())
    };
    let mut parts = s.split(' ');
    let (Some(d), Some(m), Some(hm), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Some((h, min)) = hm.split_once('.') else {
        return false;
    };
    digits(d, 1..=2) && MONTHS.contains(&m) && digits(h, 2..=2) && digits(min, 2..=2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rel(s: &str) -> RelPath {
        safe_rel_path(s).unwrap()
    }

    #[test]
    fn remote_paths_that_escape_or_are_reserved_are_refused() {
        for bad in [
            "../x.md",
            "a/../x.md",
            "/x.md",
            "C:/x.md",
            "a\\b.md",
            "a//b.md",
            "con.md",
            "Aux.txt",
            "x.md.",
            "a /b.md",
            "x\0.md",
            "",
            "./x.md",
            "a/NUL",
            "lpt9.x.md",
            "a:b.md",
        ] {
            assert!(safe_rel_path(bad).is_none(), "{bad:?} should be refused");
        }
        assert_eq!(
            safe_rel_path("skills/review/SKILL.md").unwrap().as_str(),
            "skills/review/SKILL.md"
        );
        assert!(safe_rel_path("console.md").is_some());
    }

    #[test]
    fn disk_paths_become_rel_paths() {
        let root = PathBuf::from("base").join("swarm");
        let file = root.join("skills").join("a.md");
        assert_eq!(rel_from_disk(&root, &file).unwrap().as_str(), "skills/a.md");
        assert!(rel_from_disk(&root, &PathBuf::from("elsewhere").join("a.md")).is_none());
        assert!(rel_from_disk(&root, &root).is_none());
    }

    #[test]
    fn allowed_checks_extension_case_insensitively_and_size() {
        let types = vec![".md".to_string()];
        assert!(allowed(&rel("A.MD"), 10, &types));
        assert!(!allowed(&rel("a.py"), 10, &types));
        assert!(!allowed(&rel("a"), 10, &types));
        assert!(!allowed(&rel("a.md"), MAX_FILE_BYTES + 1, &types));
        assert!(allowed(&rel("a.md"), MAX_FILE_BYTES, &types));
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn conflict_names_follow_the_spec() {
        let tz = TimeZone::get("Europe/London").unwrap();
        // 13:05 BST is 12:05 UTC.
        let at = ts("2026-10-27T13:05:00Z");
        assert_eq!(
            conflict_name(&rel("skills/review/SKILL.md"), "Davo's desktop", at, &tz).as_str(),
            "skills/review/SKILL (conflict, Davo's desktop, 27 Oct 13.05).md"
        );
        assert_eq!(
            conflict_name(&rel("a.md"), "a/b", at, &tz).as_str(),
            "a (conflict, a_b, 27 Oct 13.05).md"
        );
        let early = ts("2026-11-03T09:07:00Z");
        assert_eq!(
            conflict_name(&rel("a"), "m", early, &tz).as_str(),
            "a (conflict, m, 3 Nov 09.07)"
        );
    }

    #[test]
    fn file_keys_round_trip_and_never_prefix_each_other() {
        let a = rel("a.md");
        let b = rel("a.md.md");
        assert_eq!(file_key(&a), b"files/a.md\0");
        assert!(!file_key(&b).starts_with(&file_key(&a)));
        assert_eq!(path_of_key(&file_key(&b)).unwrap(), b);
        assert!(path_of_key(b"log/x").is_none());
        assert!(path_of_key(b"files/a.md").is_none());
        assert!(path_of_key(b"files/../x\0").is_none());
    }

    #[test]
    fn log_keys_sort_by_time() {
        use crate::ids::PersonKey;
        use crate::roster::Change;
        let key = PersonKey::generate();
        let make = |at: u64| SignedChange {
            change: Change::Leave { person: key.id() },
            by: key.id(),
            at,
            sig: "0123456789abcdef0123".to_string(),
        };
        assert_eq!(
            log_key(&make(42)),
            b"log/00000000000000000042-0123456789abcdef"
        );
        assert!(log_key(&make(9)) < log_key(&make(10)));
        assert!(log_key(&make(1)).starts_with(LOG_PREFIX));
    }

    #[test]
    fn conflict_copies_are_recognised() {
        let tz = TimeZone::get("Europe/London").unwrap();
        let at = ts("2026-10-27T13:05:00Z");
        for p in ["a.md", "dir/b.tar.gz", "noext"] {
            let c = conflict_name(&rel(p), "Davo's desktop, 2", at, &tz);
            assert!(is_conflict_copy(&c), "{c:?}");
        }
        assert!(!is_conflict_copy(&rel("a.md")));
        assert!(!is_conflict_copy(&rel("a (conflict, x).md")));
        // Not a `conflict_name` result: the marker is not just before the extension.
        let trailing = RelPath("a (conflict, x, 27 Oct 13.05) b.md".to_string());
        assert!(!is_conflict_copy(&trailing));
    }

    #[test]
    fn conflict_names_are_safe_paths_and_round_trip_as_keys() {
        let tz = TimeZone::get("Europe/London").unwrap();
        let at = ts("2026-10-27T13:05:00Z");
        for p in ["a.md", "skills/review/SKILL.md", "noext"] {
            for machine in ["Davo's desktop", "a/b:c*d", "x, y"] {
                let c = conflict_name(&rel(p), machine, at, &tz);
                let safe = safe_rel_path(c.as_str()).expect("conflict name must be a safe path");
                assert_eq!(safe, c);
                assert_eq!(path_of_key(&file_key(&c)).unwrap(), c);
            }
        }
    }
}
