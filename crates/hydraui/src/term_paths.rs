//! Paths in terminal output (spec §7.10): the word under a Ctrl+click as a file, with the
//! line and column tools print after it. No UI.
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::frame::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHit {
    pub path: PathBuf,
    /// 1-based; 0 when none was given.
    pub line: usize,
    pub column: usize,
}

/// Characters that end a word in terminal output.
const WORD_BREAK: &[char] = &[' ', '\t', '"', '\'', '`', '<', '>', '|', '[', ']', '{', '}'];

/// The whole line a visible row belongs to, when output wrapped over several rows, and
/// `column` within it. Each row says whether it carries on into the next.
pub fn join_wrapped(rows: &[(String, bool)], row: usize, column: usize) -> (String, usize) {
    let mut start = row;
    while start > 0 && rows[start - 1].1 {
        start -= 1;
    }
    let mut end = row;
    while end + 1 < rows.len() && rows[end].1 {
        end += 1;
    }
    let before: usize = rows[start..row]
        .iter()
        .map(|(t, _)| t.chars().count())
        .sum();
    let text = rows[start..=end].iter().map(|(t, _)| t.as_str()).collect();
    (text, before + column)
}

/// The rows a stretch of a joined line (from `join_wrapped` on `row`) covers.
pub fn wrapped_spans(rows: &[(String, bool)], row: usize, range: Range<usize>) -> Vec<Span> {
    let mut start = row;
    while start > 0 && rows[start - 1].1 {
        start -= 1;
    }
    let mut out = Vec::new();
    let mut offset = 0;
    for (r, (text, _)) in rows.iter().enumerate().skip(start) {
        let len = text.chars().count();
        let (from, to) = (range.start.max(offset), range.end.min(offset + len));
        if from < to {
            out.push(Span {
                row: r,
                start: from - offset,
                end: to - offset,
            });
        }
        offset += len;
        if offset >= range.end || !rows[r].1 {
            break;
        }
    }
    out
}

/// The path under `column` of `text`, with `:L`, `:L:C` or `(L,C)` after it, when it names a
/// file that exists (relative paths against `cwd`).
pub fn path_at(
    text: &str,
    column: usize,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathHit> {
    path_span_at(text, column, cwd, exists).map(|(hit, _)| hit)
}

/// `path_at`, with the characters of `text` the path and its position cover (for the
/// Ctrl-hover underline).
pub fn path_span_at(
    text: &str,
    column: usize,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<(PathHit, Range<usize>)> {
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
    let whole: String = chars[start..end].iter().collect();
    let word = whole.trim_start_matches('(');
    let lead = whole.chars().count() - word.chars().count();
    let word = word.trim_end_matches([',', '.', ';', ':', ')']);
    let (path, line, column, used) = split_position(word);
    let from = start + lead;
    let span = from..from + word[..used].chars().count();
    if path.is_empty() {
        return None;
    }
    let p = Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    exists(&full).then_some((
        PathHit {
            path: full,
            line,
            column,
        },
        span,
    ))
}

/// `p(L,C)` / `p(L` (its `)` already trimmed), `p:L:C`, `p:L` → `(p, L, C)`; else `(word, 0, 0)`;
/// and how many bytes of `word` that took.
fn split_position(word: &str) -> (&str, usize, usize, usize) {
    if let Some(open) = word.rfind('(') {
        let inner = word[open + 1..].trim_end_matches(')');
        let mut nums = inner.split(',').map(|n| n.trim().parse::<usize>());
        if let Some(Ok(line)) = nums.next() {
            let column = nums.next().and_then(Result::ok).unwrap_or(0);
            return (&word[..open], line, column, word.len());
        }
    }
    // The first `:L` (then `:C`) that ends the word or is followed by another `:`, as in
    // grep's `path:12:text`; a drive letter's colon (`C:`) isn't one.
    for (i, _) in word.match_indices(':').filter(|(i, _)| *i > 1) {
        let rest = &word[i + 1..];
        let (line, after) = leading_number(rest);
        let Some(line) = line else { continue };
        if !(after.is_empty() || after.starts_with(':')) {
            continue;
        }
        let (column, rest) = match after.strip_prefix(':').map(leading_number) {
            Some((Some(c), rest)) => (c, rest),
            _ => (0, after),
        };
        return (&word[..i], line, column, word.len() - rest.len());
    }
    (word, 0, 0, word.len())
}

/// The digits `s` starts with, as a number, and what follows them.
fn leading_number(s: &str) -> (Option<usize>, &str) {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (s[..n].parse().ok(), &s[n..])
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
    fn grep_output_with_the_line_glued_on() {
        let ex = exists_in(&["src/a.rs"]);
        let at = |t: &str, c| path_at(t, c, Path::new(CWD), &ex);
        assert_eq!(
            at("src/a.rs:12:fn main() {", 3),
            hit("C:/w/src/a.rs", 12, 0)
        );
        assert_eq!(at("src/a.rs:12:5:error: x", 3), hit("C:/w/src/a.rs", 12, 5));
        assert_eq!(at("src/a.rs:12:5", 3), hit("C:/w/src/a.rs", 12, 5));
    }

    #[test]
    fn a_path_wrapped_over_rows_is_joined() {
        let rows = vec![
            ("$ ls".to_string(), false),
            ("see C:/w/s".to_string(), true),
            ("rc/a.rs:3 ".to_string(), false),
        ];
        assert_eq!(
            join_wrapped(&rows, 2, 1),
            ("see C:/w/src/a.rs:3 ".to_string(), 11)
        );
        assert_eq!(
            join_wrapped(&rows, 1, 6),
            ("see C:/w/src/a.rs:3 ".to_string(), 6)
        );
        assert_eq!(join_wrapped(&rows, 0, 2), ("$ ls".to_string(), 2));
    }

    #[test]
    fn the_span_covers_the_path_and_its_position_but_not_punctuation() {
        let ex = exists_in(&["src/a.rs"]);
        let at = |t: &str, c| path_span_at(t, c, Path::new(CWD), &ex).map(|(_, s)| s);
        assert_eq!(at("error at src/a.rs:12:5 here", 12), Some(9..22));
        assert_eq!(at("(src/a.rs:12),", 3), Some(1..12));
        assert_eq!(at("src/a.rs:12:fn main", 2), Some(0..11));
        assert_eq!(at("nothing", 2), None);
    }

    #[test]
    fn a_span_in_a_wrapped_line_maps_back_to_its_rows() {
        let rows = vec![
            ("$ ls".to_string(), false),
            ("see C:/w/s".to_string(), true),
            ("rc/a.rs:3 ".to_string(), false),
        ];
        let span = |row, start, end| Span { row, start, end };
        assert_eq!(
            wrapped_spans(&rows, 2, 4..19),
            vec![span(1, 4, 10), span(2, 0, 9)]
        );
        assert_eq!(wrapped_spans(&rows, 0, 2..4), vec![span(0, 2, 4)]);
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
