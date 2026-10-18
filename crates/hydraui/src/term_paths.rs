//! Paths in terminal output (spec §7.10): the word under a Ctrl+click as a file, with the
//! line and column tools print after it. No UI.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHit {
    pub path: PathBuf,
    /// 1-based; 0 when none was given.
    pub line: usize,
    pub column: usize,
}

/// Characters that end a word in terminal output.
const WORD_BREAK: &[char] = &[' ', '\t', '"', '\'', '`', '<', '>', '|', '[', ']', '{', '}'];

/// The path under `column` of `text`, with `:L`, `:L:C` or `(L,C)` after it, when it names a
/// file that exists (relative paths against `cwd`).
pub fn path_at(
    text: &str,
    column: usize,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathHit> {
    let chars: Vec<char> = text.chars().collect();
    if column >= chars.len() || WORD_BREAK.contains(&chars[column]) {
        return None;
    }
    let start = chars[..column]
        .iter()
        .rposition(|c| WORD_BREAK.contains(c))
        .map_or(0, |i| i + 1);
    let end = chars[column..]
        .iter()
        .position(|c| WORD_BREAK.contains(c))
        .map_or(chars.len(), |i| column + i);
    let word: String = chars[start..end].iter().collect();
    let word = word
        .trim_start_matches('(')
        .trim_end_matches([',', '.', ';', ':', ')']);
    let (path, line, column) = split_position(word);
    if path.is_empty() {
        return None;
    }
    let p = Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    exists(&full).then_some(PathHit {
        path: full,
        line,
        column,
    })
}

/// `p(L,C)` / `p(L` (its `)` already trimmed), `p:L:C`, `p:L` → `(p, L, C)`; else `(word, 0, 0)`.
fn split_position(word: &str) -> (&str, usize, usize) {
    if let Some(open) = word.rfind('(') {
        let inner = word[open + 1..].trim_end_matches(')');
        let mut nums = inner.split(',').map(|n| n.trim().parse::<usize>());
        if let Some(Ok(line)) = nums.next() {
            let column = nums.next().and_then(Result::ok).unwrap_or(0);
            return (&word[..open], line, column);
        }
    }
    match number_suffix(word) {
        Some((rest, last)) => match number_suffix(rest) {
            Some((path, line)) => (path, line, last),
            None => (rest, last, 0),
        },
        None => (word, 0, 0),
    }
}

/// `x:N` → `(x, N)`; a drive letter's colon (`C:`) isn't one.
fn number_suffix(s: &str) -> Option<(&str, usize)> {
    let (rest, n) = s.rsplit_once(':')?;
    if rest.len() <= 1 {
        return None;
    }
    n.parse().ok().map(|n| (rest, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists_in(files: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| {
            let s = p.to_string_lossy().replace('\\', "/");
            files.iter().any(|f| s.ends_with(f))
        }
    }

    fn hit(path: &str, line: usize, column: usize) -> Option<PathHit> {
        Some(PathHit {
            path: PathBuf::from(path),
            line,
            column,
        })
    }

    const CWD: &str = "C:/w";

    #[test]
    fn line_and_column_forms_are_understood() {
        let ex = exists_in(&["src/a.rs"]);
        let at = |t: &str, c| path_at(t, c, Path::new(CWD), &ex);
        assert_eq!(
            at("error at src/a.rs:12:5 here", 12),
            hit("C:/w/src/a.rs", 12, 5)
        );
        assert_eq!(at("src/a.rs:12", 3), hit("C:/w/src/a.rs", 12, 0));
        assert_eq!(at("src/a.rs", 0), hit("C:/w/src/a.rs", 0, 0));
        assert_eq!(
            at("src\\a.rs(12,5): error", 4),
            hit("C:/w/src\\a.rs", 12, 5)
        );
    }

    #[test]
    fn trailing_punctuation_and_quotes_are_trimmed() {
        let ex = exists_in(&["src/a.rs"]);
        let at = |t: &str, c| path_at(t, c, Path::new(CWD), &ex);
        assert_eq!(at("\"src/a.rs:12\",", 3), hit("C:/w/src/a.rs", 12, 0));
        assert_eq!(at("(src/a.rs:12)", 3), hit("C:/w/src/a.rs", 12, 0));
        assert_eq!(at("see src/a.rs.", 6), hit("C:/w/src/a.rs", 0, 0));
    }

    #[test]
    fn absolute_paths_and_misses() {
        let ex = exists_in(&["D:/x/b.rs"]);
        let at = |t: &str, c| path_at(t, c, Path::new(CWD), &ex);
        assert_eq!(at("D:/x/b.rs:3", 2), hit("D:/x/b.rs", 3, 0));
        assert_eq!(at("nothing here", 2), None, "not a file that exists");
        assert_eq!(at("D:/x/b.rs:3   ", 12), None, "click on the spaces");
    }
}
