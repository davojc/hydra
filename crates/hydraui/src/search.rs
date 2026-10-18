//! Search in files (spec §7.10): plain text or a regex, case-sensitive or not, under a folder
//! with `.gitignore` respected; capped, cancellable. No UI.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::file_doc;
use crate::quick_open::list_files;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case: bool,
}

/// The regex a query searches with; plain text is matched literally.
pub fn matcher(q: &Query) -> Result<regex::Regex, String> {
    let pattern = if q.regex {
        q.text.clone()
    } else {
        regex::escape(&q.text)
    };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!q.case)
        .build()
        .map_err(|e| e.to_string())
}

/// The lines under `root` that match, at most `cap` (`true` when cut off); binary and
/// too-large files are skipped, and a cancelled search returns nothing.
pub fn search(
    root: &Path,
    q: &Query,
    cap: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<Hit>, bool), String> {
    let re = matcher(q)?;
    let mut hits = Vec::new();
    let (files, _) = list_files(root, 200_000);
    for rel in files {
        if cancelled() {
            return Ok((Vec::new(), false));
        }
        let path = root.join(&rel);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > file_doc::MAX_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(doc) = file_doc::decode(&bytes) else {
            continue;
        };
        for (i, line) in doc.text.lines().enumerate() {
            if let Some(m) = re.find(line) {
                if hits.len() == cap {
                    return Ok((hits, true));
                }
                hits.push(Hit {
                    path: path.clone(),
                    line: i + 1,
                    column: line[..m.start()].chars().count() + 1,
                    text: line.trim_end().to_string(),
                });
            }
        }
    }
    Ok((hits, false))
}

/// Tells whether a search is still the newest one.
#[derive(Debug, Clone, Default)]
pub struct Generation(Arc<AtomicU64>);

impl Generation {
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.0.load(Ordering::SeqCst) == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn main() {}\nlet Main = 1;\n").unwrap();
        std::fs::write(d.path().join("b.bin"), b"main\0\0").unwrap();
        d
    }

    fn q(text: &str, regex: bool, case: bool) -> Query {
        Query {
            text: text.into(),
            regex,
            case,
        }
    }

    #[test]
    fn plain_text_ignores_case_unless_asked() {
        let d = tree();
        let (hits, _) = search(d.path(), &q("main", false, false), 100, &|| false).unwrap();
        assert_eq!(hits.len(), 2, "both lines, binary file skipped");
        assert_eq!((hits[0].line, hits[0].column), (1, 4));
        let (hits, _) = search(d.path(), &q("Main", false, true), 100, &|| false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "let Main = 1;");
    }

    #[test]
    fn regex_and_its_errors() {
        let d = tree();
        let (hits, _) = search(d.path(), &q(r"fn \w+\(", true, false), 100, &|| false).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(search(d.path(), &q("(", true, false), 100, &|| false).is_err());
        let (hits, _) = search(d.path(), &q("(", false, false), 100, &|| false).unwrap();
        assert_eq!(hits.len(), 1, "plain text: '(' is literal");
    }

    #[test]
    fn results_stop_at_the_cap() {
        let d = tree();
        let (hits, capped) = search(d.path(), &q("main", false, false), 1, &|| false).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(capped);
    }

    #[test]
    fn a_newer_search_supersedes_an_older_one() {
        let g = Generation::default();
        let first = g.next();
        let second = g.next();
        assert!(!g.is_current(first));
        assert!(g.is_current(second));
        let d = tree();
        let (hits, _) = search(d.path(), &q("main", false, false), 100, &|| true).unwrap();
        assert!(hits.is_empty(), "a cancelled search stops");
    }
}
