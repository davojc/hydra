# hydraui Finding Files, Git in the Tree, Environments from the Launcher — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ctrl+click paths in terminal output, quick open (Ctrl+Shift+O), search in files (Ctrl+Shift+F), git status markers and side-by-side diffs in the file tree, and deleting / setting the home of an environment from the launcher.

**Architecture:** A shared "open a file at a line" path in the file pane and workspace, used by three finders. Each finder's logic is a pure, tested module (`term_paths`, `quick_open`, `search`); their dialogs live in a new `workspace/finder.rs`. Git per-file status extends the existing `git_status` parser and feeds the tree; diffs come from `git diff` into gpui-component's diff view (Split mode) as a file-pane tab. Environment deletion and home reuse hydra-core's `envs::remove` and `bindedit`.

**Tech Stack:** Rust 2024, GPUI, gpui-component `=0.7.3` (`diff::{Diff, DiffState, DiffFile, DiffMode}`, `input`, `menu`), `ignore` (walking), new dependency `regex = "1"`, hydra-core.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` — §7.3 (git in the tree), §7.4 (launcher: open file, search, delete env, set home), §7.8 (diff tabs, opening at a line), §7.10 (finding files).

## Global Constraints

- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`; no employer names.
- Gate before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Tests use temp dirs and `MemoryStore`; never the real `~/.hydra` or Credential Manager.
- Finders use the focused terminal's folder (else the user's home) and respect `.gitignore` (§7.10).
- Quick open: at most 50,000 files, a note when cut off; `name:42` opens at line 42 (§7.10).
- Search: plain text by default, case-sensitive and regex toggles, stop at 1,000 results and say so, skip files over 5 MB and binary files, newer search cancels older, invalid regex shows its error (§7.10).
- Keys `open_file` = `ctrl-shift-o`, `search` = `ctrl-shift-f`, rebindable in `[keys]` (§7.10).
- Clickable paths: `p`, `p:42`, `p:42:7`, `p(42,7)`; relative to the terminal's folder; only existing files (§7.10).
- Git markers: M yellow, A and ? green, D red, ignored dimmed, folder dot; nothing outside a repo (§7.3).
- Diff: `git diff HEAD -- <file>`, `--no-index` for untracked, Split mode, read-only, Open file; git's error shown on failure (§7.8).
- Delete env like `hydra env rm`: refused with open terminals; separate saved-logins question; bindings listed (§7.4).
- Git and other child processes run with `hydra_platform::process::hide_window`, off the UI thread.
- Windows; write edit scripts with the Write tool.

## Review Focus

1. Ctrl+click on a path followed by punctuation or quotes (`"src/a.rs:12",` or `(src/a.rs:12)`) must open the file, not fail on the trailing characters. Pinned in Task 2 (`trailing_punctuation_and_quotes_are_trimmed`).
2. Typing fast in search must not show results from an older query after a newer one. Pinned in Task 4 (`a_newer_search_supersedes_an_older_one`).
3. A renamed file in git status (`2 R. ... new\told`) must mark the new path. Pinned in Task 5 (`renames_mark_the_new_path`).
4. Deleting the environment of a pane that is open in hydraui must be refused, naming it. Pinned in Task 6 (`delete_is_refused_while_hydraui_has_its_terminals_open`).
5. Opening at a line past the end of the file must land on the last line, not panic. Pinned in Task 1 (`a_line_past_the_end_goes_to_the_last_line`).

---

### Task 1: Open a file at a line

**Files:**
- Modify: `crates/hydraui/src/file_doc.rs` (pure `position_for`), `crates/hydraui/src/file_pane.rs` (`open_at`), `crates/hydraui/src/workspace.rs` (`show_file_at`)
- Test: `file_doc.rs`

**Interfaces:**
- Produces: `pub fn position_for(line: usize, column: usize, text: &str) -> (u32, u32)` (1-based in, 0-based clamped out); `FilePane::open_at(&mut self, path: PathBuf, line: usize, column: usize, window, cx)`; `Workspace::show_file_at(&mut self, path: PathBuf, line: usize, column: usize, window, cx)`.

- [ ] **Step 1: Write the failing tests** (file_doc.rs tests)

```rust
    #[test]
    fn a_line_and_column_become_an_editor_position() {
        assert_eq!(position_for(1, 1, "one\ntwo"), (0, 0));
        assert_eq!(position_for(2, 3, "one\ntwo"), (1, 2));
        assert_eq!(position_for(0, 0, "one"), (0, 0), "0 means the start");
    }

    #[test]
    fn a_line_past_the_end_goes_to_the_last_line() {
        assert_eq!(position_for(99, 1, "one\ntwo"), (1, 3));
        assert_eq!(position_for(2, 99, "one\ntwo"), (1, 3));
    }
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p hydraui --lib file_doc` → compile error (no `position_for`).

- [ ] **Step 3: Implement**

`file_doc.rs`:

```rust
/// The editor position for a 1-based line and column (as tools print them), clamped to the
/// text; 0 counts as 1.
pub fn position_for(line: usize, column: usize, text: &str) -> (u32, u32) {
    cursor_after_reload(
        line.saturating_sub(1) as u32,
        column.saturating_sub(1) as u32,
        text,
    )
}
```

(`cursor_after_reload` past the last line returns the end of the last line, which is what a too-large line should do.)

`file_pane.rs`, beside `open`:

```rust
    /// Shows `path` with the cursor at a 1-based line and column, scrolled into view (spec
    /// §7.8). Markdown switches to Raw, where a line means something.
    pub fn open_at(
        &mut self,
        path: PathBuf,
        line: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(path, window, cx);
        if let Some(FileTab {
            body: Body::Text { editor, view, .. },
            ..
        }) = self.tabs.get_mut(self.active)
        {
            *view = View::Raw;
            editor.update(cx, |e, cx| {
                let (line, character) = file_doc::position_for(line, column, e.value().as_ref());
                e.set_cursor_position(Position { line, character }, window, cx);
            });
        }
        self.focus_content(window, cx);
        cx.notify();
    }
```

`workspace.rs`: refactor `show_file(path)` into `show_file_at(path, line: usize, column: usize)` where `line == 0` means "just open" (call `f.open(..)`), else `f.open_at(..)`; `show_file(path)` becomes `self.show_file_at(path, 0, 0, window, cx)`. In the new-pane branch, after `create_files_pane`, call `open_at` on the new view when `line > 0`.

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib file_doc` → pass; `cargo build -p hydraui` ok.

- [ ] **Step 5: Gate and commit** — `git commit -m "feat: hydraui opens a file at a line"`.

---

### Task 2: Clickable paths in terminal output

**Files:**
- Create: `crates/hydraui/src/term_paths.rs`
- Modify: `crates/hydraui/src/frame.rs` (`row_text`), `crates/hydraui/src/pane_view.rs` (Ctrl+click fallback, `PaneEvent::OpenPath`), `crates/hydraui/src/workspace.rs` (handle it), `lib.rs`
- Test: `term_paths.rs`

**Interfaces:**
- Consumes: `Workspace::show_file_at` (Task 1).
- Produces: `pub struct PathHit { pub path: PathBuf, pub line: usize, pub column: usize }`; `pub fn path_at(text: &str, column: usize, cwd: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<PathHit>`; `frame::row_text<T>(term: &Term<T>, row: usize) -> String`; `PaneEvent::OpenPath(PathBuf, usize, usize)`.

- [ ] **Step 1: Write the failing tests** (`term_paths.rs`)

```rust
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
        Some(PathHit { path: PathBuf::from(path), line, column })
    }

    const CWD: &str = "C:/w";

    #[test]
    fn line_and_column_forms_are_understood() {
        let ex = exists_in(&["src/a.rs"]);
        let at = |t: &str, c| path_at(t, c, Path::new(CWD), &ex);
        assert_eq!(at("error at src/a.rs:12:5 here", 12), hit("C:/w/src/a.rs", 12, 5));
        assert_eq!(at("src/a.rs:12", 3), hit("C:/w/src/a.rs", 12, 0));
        assert_eq!(at("src/a.rs", 0), hit("C:/w/src/a.rs", 0, 0));
        assert_eq!(at("src\\a.rs(12,5): error", 4), hit("C:/w/src\\a.rs", 12, 5));
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
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p hydraui --lib term_paths` → compile error.

- [ ] **Step 3: Implement** (`term_paths.rs`)

```rust
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

const WORD_BREAK: &[char] = &[' ', '\t', '"', '\'', '`', '<', '>', '|', '[', ']', '{', '}'];

/// The path under `column` of `text`, with `:L`, `:L:C` or `(L,C)` after it, if it names a file
/// that exists (relative paths against `cwd`).
pub fn path_at(text: &str, column: usize, cwd: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<PathHit> {
    let chars: Vec<char> = text.chars().collect();
    if column >= chars.len() || WORD_BREAK.contains(&chars[column]) {
        return None;
    }
    let start = chars[..column].iter().rposition(|c| WORD_BREAK.contains(c)).map_or(0, |i| i + 1);
    let end = chars[column..].iter().position(|c| WORD_BREAK.contains(c)).map_or(chars.len(), |i| column + i);
    let word: String = chars[start..end].iter().collect();
    let word = word.trim_start_matches('(').trim_end_matches([',', '.', ';', ')', ':']);
    let (path, line, column) = split_position(word);
    if path.is_empty() {
        return None;
    }
    let p = Path::new(path);
    let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    exists(&full).then_some(PathHit { path: full, line, column })
}

/// `p:L:C`, `p:L`, `p(L,C)`, `p(L)` → `(p, L, C)`; a drive letter's colon isn't a position.
fn split_position(word: &str) -> (&str, usize, usize) {
    if let Some(open) = word.rfind('(') {
        if let Some(inner) = word[open + 1..].strip_suffix(')').or(Some(&word[open + 1..])) {
            let mut nums = inner.split(',').map(|n| n.trim().parse::<usize>());
            if let Some(Ok(line)) = nums.next() {
                let col = nums.next().and_then(Result::ok).unwrap_or(0);
                return (&word[..open], line, col);
            }
        }
    }
    let mut parts: Vec<&str> = word.rsplitn(3, ':').collect();
    parts.reverse();
    let num = |s: &str| s.parse::<usize>().ok();
    match parts.as_slice() {
        [p, l, c] if num(l).is_some() && num(c).is_some() && p.len() > 1 => {
            (p, num(l).unwrap(), num(c).unwrap())
        }
        [p, l] if num(l).is_some() && p.len() > 1 => (p, num(l).unwrap(), 0),
        _ => match word.rsplit_once(':') {
            Some((p, l)) if num(l).is_some() && p.len() > 1 => (p, num(l).unwrap(), 0),
            _ => (word, 0, 0),
        },
    }
}
```

Note for the implementer: `rsplitn(3, ':')` on `D:/x/b.rs:3` gives `["3", "/x/b.rs", "D"]` → reversed `["D", "/x/b.rs", "3"]`, which the `[p, l, c]` arm rejects (`/x/b.rs` isn't a number), and the fallback `rsplit_once` gives `("D:/x/b.rs", 3)`. Verify with the tests and adjust the matcher, not the tests.

`frame.rs`:

```rust
/// The text of a visible row, for finding paths under the mouse.
pub fn row_text<T>(term: &Term<T>, row: usize) -> String {
    if row >= term.screen_lines() {
        return String::new();
    }
    (0..term.columns())
        .map(|col| {
            let point = viewport_to_point(term.grid().display_offset(), Point::new(row, Column(col)));
            term.grid()[point].c
        })
        .collect()
}
```

`pane_view.rs`: add `OpenPath(PathBuf, usize, usize)` to `PaneEvent` (derive `Clone, PartialEq`); in `on_mouse_down`'s Ctrl branch, after the OSC 8 `link_at` check:

```rust
            } else if let Some(hit) = pane.with_term(|t| {
                term_paths::path_at(&row_text(t, row), col, self.cwd(), &|p| p.is_file())
            }) {
                cx.emit(PaneEvent::OpenPath(hit.path, hit.line, hit.column));
            }
```

`workspace.rs` (create_pane's subscription): `if let PaneEvent::OpenPath(p, l, c) = ev { ws.show_file_at(p.clone(), *l, *c, window, cx); }` — the subscription closure must take `window` (use `subscribe_in`, which it already does).

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib term_paths` → 3 pass; build ok.

- [ ] **Step 5: Gate and commit** — `git commit -m "feat: Ctrl+click a path in hydraui terminal output to open it"`.

---

### Task 3: Quick open

**Files:**
- Create: `crates/hydraui/src/quick_open.rs`, `crates/hydraui/src/workspace/finder.rs`
- Modify: `keys.rs` (`OpenFile` action, `Command::OpenFile` = `open_file` / `ctrl-shift-o`), `launcher.rs` (`Action::OpenFile`, entry "Open file…"), `workspace/launcher.rs` (dispatch), `workspace.rs` (`mod finder;`, action handler), `lib.rs`
- Test: `quick_open.rs`, `launcher.rs` entries tests, `keys.rs` defaults

**Interfaces:**
- Consumes: `Workspace::show_file_at`.
- Produces: `pub fn score(query: &str, path: &str) -> Option<i32>`; `pub fn split_line(query: &str) -> (&str, usize)`; `pub fn list_files(root: &Path, cap: usize) -> (Vec<PathBuf>, bool)` (relative paths, `true` when cut off); `Workspace::open_file_finder(&mut self, window, cx)`.

- [ ] **Step 1: Write the failing tests** (`quick_open.rs`)

```rust
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
        let names: Vec<String> = files.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        assert!(names.contains(&"src/a.rs".to_string()) && !names.iter().any(|n| n.starts_with("target")));
        assert!(!cut);
        let (two, cut) = list_files(d.path(), 2);
        assert_eq!(two.len(), 2);
        assert!(cut);
    }
}
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p hydraui --lib quick_open` → compile error.

- [ ] **Step 3: Implement** (`quick_open.rs`)

```rust
//! Quick open (spec §7.10): the folder's files, fuzzy-matched. No UI.
use std::path::{Path, PathBuf};

/// How well `query` matches `path`: `None` unless its letters appear in order (ignoring
/// case). Consecutive letters, word starts and letters in the file name score higher, and
/// shorter paths win ties.
pub fn score(query: &str, path: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let name_start = path.rfind(['/', '\\']).map_or(0, |i| path[..=i].chars().count());
    let mut score = 0i32;
    let mut pi = 0;
    let mut last: Option<usize> = None;
    for qc in &q {
        let found = (pi..p.len()).find(|&i| p[i] == *qc)?;
        score += 1;
        if last == Some(found.wrapping_sub(1)) {
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
        pi = found + 1;
    }
    Some(score * 100 - p.len() as i32)
}

/// `name:42` → (`name`, 42); otherwise (`query`, 0).
pub fn split_line(query: &str) -> (&str, usize) {
    match query.rsplit_once(':') {
        Some((name, n)) if !name.is_empty() && n.parse::<usize>().is_ok() => (name, n.parse().unwrap()),
        _ => (query, 0),
    }
}

/// The files under `root` (relative), `.gitignore` respected, at most `cap`; `true` when cut off.
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
```

`workspace/finder.rs` — `open_file_finder`: list files in the background (`cx.background_spawn`) for `root = self.focused_cwd(cx)`; open a dialog with an `InputState` (placeholder "File name, or name:line") and, below it, up to 50 rows (the best `score` of `split_line(query).0` against each path's display string, sorted descending), recomputed on `InputEvent::Change`; a note "Showing the first 50,000 files" when cut off; Enter or a row click closes the dialog and calls `show_file_at(root.join(rel), line, 0)`. Use the same dialog pattern as `pick_account` (a `Cell` first-render focus, `window.open_dialog`). Arrow keys move a highlighted row (store `selected: usize` in a small `Entity<FinderState>`).

`keys.rs`: add `OpenFile` and `SearchFiles` to `actions!`; add `Command::OpenFile` (name `open_file`, default `ctrl-shift-o`) and `Command::Search` (name `search`, default `ctrl-shift-f`) to the enum, `ALL`, `name()`, `default_keys()`, `action()` — update the `ALL` length and any test that lists every command.

`launcher.rs`: `Action::OpenFile` ("Open file…") and `Action::SearchFiles` ("Search in files…") before "Open layout file…"; update both entries tests' expected lists.

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib` → pass; build ok.

- [ ] **Step 5: Gate and commit** — `git commit -m "feat: hydraui quick open (Ctrl+Shift+O)"`.

---

### Task 4: Search in files

**Files:**
- Create: `crates/hydraui/src/search.rs`
- Modify: `Cargo.toml` (workspace `regex = "1"`), `crates/hydraui/Cargo.toml`, `workspace/finder.rs` (`search_dialog`), `workspace/launcher.rs`, `workspace.rs` (action), `lib.rs`
- Test: `search.rs`

**Interfaces:**
- Consumes: `file_doc::{MAX_BYTES, decode}`, `quick_open::list_files`, `show_file_at`.
- Produces: `pub struct Hit { pub path: PathBuf, pub line: usize, pub column: usize, pub text: String }`; `pub struct Query { pub text: String, pub regex: bool, pub case: bool }`; `pub fn matcher(q: &Query) -> Result<regex::Regex, String>`; `pub fn search(root: &Path, q: &Query, cap: usize, cancelled: &dyn Fn() -> bool) -> Result<(Vec<Hit>, bool), String>` (`true` when capped); `pub struct Generation(Arc<AtomicU64>)` with `next() -> u64` and `is_current(u64) -> bool`.

- [ ] **Step 1: Write the failing tests** (`search.rs`)

```rust
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
        Query { text: text.into(), regex, case }
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
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p hydraui --lib search` → compile error.

- [ ] **Step 3: Implement** (`search.rs`)

```rust
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

pub fn matcher(q: &Query) -> Result<regex::Regex, String> {
    let pattern = if q.regex { q.text.clone() } else { regex::escape(&q.text) };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!q.case)
        .build()
        .map_err(|e| e.to_string())
}

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
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(doc) = file_doc::decode(&bytes) else { continue };
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
```

Test-order note: hits are in file order then line order; `a.rs` line 1 `fn main` column 4 (1-based), line 2 `let Main` column 5. Adjust the first test's expected column to the implementation's 1-based value if needed (the test says `(1, 4)` for "fn main()": `main` starts at index 3 → column 4).

`workspace/finder.rs` — `search_dialog`: an `InputState` plus two toggle buttons ("Aa" case, ".*" regex); on every change (debounced 200 ms with a timer task replaced per keystroke), `let gen = generation.next()` and run `search(root, &query, 1000, &|| !generation.is_current(gen))` in `cx.background_spawn`; when it returns and `is_current(gen)`, store the results (or the error, shown in red under the box) and notify. Rows: `file:line` (muted, relative to root) and the trimmed text; Enter/click → `show_file_at(path, line, column)` and close. When capped, a note "Stopped at 1,000 results".

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib search` → pass.

- [ ] **Step 5: Gate and commit** — `git commit -m "feat: hydraui search in files (Ctrl+Shift+F)"`.

---

### Task 5: Git in the file tree, and diffs

**Files:**
- Modify: `crates/hydraui/src/git_status.rs` (per-file map, `diff_patch`), `file_tree_view.rs` (markers, right-click Show changes, `TreeEvent::ShowChanges`), `file_pane.rs` (`Body::Diff`, `open_diff`), `workspace.rs` (feed statuses to the tree, handle ShowChanges)
- Test: `git_status.rs`

**Interfaces:**
- Produces: `pub enum Change { Modified, Added, Untracked, Deleted, Renamed, Ignored }`; `GitStatus.files: BTreeMap<PathBuf, Change>` (paths relative to the repo root) and `GitStatus.root: Option<PathBuf>`; `pub fn folder_changed(files: &BTreeMap<PathBuf, Change>, folder: &Path) -> bool`; `pub fn diff_patch(repo_dir: &Path, file: &Path, untracked: bool) -> Result<String, String>`; `FileTreeView::set_git(&mut self, root: Option<PathBuf>, files: BTreeMap<PathBuf, Change>, cx)`; `TreeEvent::ShowChanges(PathBuf)`; `FilePane::open_diff(&mut self, path: PathBuf, patch: Result<String, String>, window, cx)`.

- [ ] **Step 1: Write the failing tests** (git_status.rs tests)

```rust
    #[test]
    fn files_get_their_change() {
        let text = "# branch.head main\n\
            1 .M N... 100644 100644 100644 a a src/a.rs\n\
            1 A. N... 000000 100644 100644 0 b src/new.rs\n\
            1 .D N... 100644 100644 000000 c c gone.rs\n\
            ? notes.txt\n\
            ! target/x\n";
        let s = parse(text);
        assert_eq!(s.files.get(Path::new("src/a.rs")), Some(&Change::Modified));
        assert_eq!(s.files.get(Path::new("src/new.rs")), Some(&Change::Added));
        assert_eq!(s.files.get(Path::new("gone.rs")), Some(&Change::Deleted));
        assert_eq!(s.files.get(Path::new("notes.txt")), Some(&Change::Untracked));
        assert_eq!(s.files.get(Path::new("target/x")), Some(&Change::Ignored));
    }

    #[test]
    fn renames_mark_the_new_path() {
        let s = parse("2 R. N... 100644 100644 100644 a a R100 src/new.rs\tsrc/old.rs\n");
        assert_eq!(s.files.get(Path::new("src/new.rs")), Some(&Change::Renamed));
        assert!(!s.files.contains_key(Path::new("src/old.rs")));
    }

    #[test]
    fn a_folder_holding_changes_is_marked() {
        let s = parse("1 .M N... 100644 100644 100644 a a src/ui/a.rs\n! target/x\n");
        assert!(folder_changed(&s.files, Path::new("src")));
        assert!(folder_changed(&s.files, Path::new("src/ui")));
        assert!(!folder_changed(&s.files, Path::new("docs")));
        assert!(!folder_changed(&s.files, Path::new("target")), "ignored isn't a change");
    }
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p hydraui --lib git_status` → compile error.

- [ ] **Step 3: Implement**

`git_status.rs`: add `Change`, the `files` map and `root`; in `parse`, for `1 XY ... <path>` take the path after the 8th space-separated field (`line.splitn(9, ' ').nth(8)`), for `2 XY ... <path>\t<orig>` the part before the tab (`splitn(10, ' ').nth(9)` then split at `\t`), for `? p` / `! p` the rest. Change from XY: `R` in either → Renamed; `D` in either → Deleted; `A` in X → Added; otherwise Modified. Keep the counts as they are. `read` adds `--ignored=matching`? No — keep the status bar's command; run `git -C dir rev-parse --show-toplevel` once alongside to fill `root`, and add `--ignored` to the status args so `!` lines appear (they don't affect the counts).

```rust
pub fn folder_changed(files: &BTreeMap<PathBuf, Change>, folder: &Path) -> bool {
    files
        .iter()
        .any(|(p, c)| *c != Change::Ignored && p.starts_with(folder) && p != folder)
}

/// `git diff HEAD -- <file>` (untracked: `--no-index` against an empty file), without a
/// console window; off the UI thread.
pub fn diff_patch(repo_dir: &Path, file: &Path, untracked: bool) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_dir).arg("diff").arg("--no-color");
    if untracked {
        cmd.args(["--no-index", "--", "NUL"]).arg(file);
    } else {
        cmd.args(["HEAD", "--"]).arg(file);
    }
    let out = hydra_platform::process::hide_window(&mut cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    // `--no-index` exits 1 when files differ: that's a diff, not a failure.
    if out.status.success() || (untracked && out.status.code() == Some(1)) {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
```

`file_tree_view.rs`: `set_git(root, files)` stores them; each row looks up `entry.path` relative to `root` — files: marker letter (M/A/?/D/R) right-aligned and text colour (yellow/green/red via `cx.theme().warning/success/danger`), Ignored → muted; folders: a `•` when `folder_changed`. Right-click a changed file: `context_menu` with "Show changes" → `cx.emit(TreeEvent::ShowChanges(path))` (`Untracked` passes through — the workspace looks it up again).

`workspace.rs`: when `refresh_git` gets a `GitStatus`, `tree.update(|t, cx| t.set_git(status.root.clone(), status.files.clone(), cx))`. Handle `TreeEvent::ShowChanges(path)`: look up whether it is untracked; `cx.background_spawn(diff_patch(root, rel, untracked))`, then `self.files_pane_for_tab().open_diff(path, result)` (create the file pane like `show_file_at` does when there is none).

`file_pane.rs`: `Body::Diff { state: Entity<DiffState> }` or `Body::DiffError(String)`; `open_diff` parses with `DiffFile::parse(&patch)` (a parse error or binary → "binary file changed" / the error as text), builds `cx.new(|cx| DiffState::new(files, cx).with_mode(DiffMode::Split))`, and pushes a file tab labelled `"<name> (diff)"`; its header has **Open file** → `self.open(path)`. Render `Diff::new(&state)` (read the gpui-component `diff/mod.rs` for the element's name and builder). A diff tab is never dirty and isn't saved in the session (`saved()` skips it).

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib git_status` → pass; build ok.

- [ ] **Step 5: Gate and commit** — `git commit -m "feat: hydraui marks git changes in the tree and shows diffs side by side"`.

---

### Task 6: Delete an environment and set its home from the launcher

**Files:**
- Modify: `crates/hydraui/src/hydra.rs` (`delete_env`, `set_env_home`), `launcher.rs` (`Action::DeleteEnv`, `Action::SetEnvHome`, entries), `workspace/launcher.rs` (dialogs), tests
- Test: `hydra.rs`, `launcher.rs`

**Interfaces:**
- Produces: `Hydra::delete_env(&self, env: &str, open_in_hydraui: usize, delete_logins: bool) -> Result<RemoveReport, String>`; `Hydra::set_env_home(&self, env: &str, dir: &Path) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests** (hydra.rs tests)

```rust
    #[test]
    fn delete_is_refused_while_hydraui_has_its_terminals_open() {
        let f = fx("");
        let err = f.hydra.delete_env("work", 2, false).unwrap_err();
        assert!(err.contains("2 terminals"), "{err}");
        assert!(f.hydra.env_exists("work"));
    }

    #[test]
    fn delete_removes_the_env_and_its_secrets_keeping_logins_unless_asked() {
        let f = fx("");
        f.hydra.set_secret("work/tok", "v").unwrap();
        let state = f.hydra.paths.state_dir(&EnvName::parse("work").unwrap());
        std::fs::create_dir_all(&state).unwrap();
        let report = f.hydra.delete_env("work", 0, false).unwrap();
        assert!(!f.hydra.env_exists("work"));
        assert_eq!(report.secrets, vec!["tok".to_string()]);
        assert!(state.exists(), "saved logins kept");
        assert!(!report.state_deleted);

        let personal = f.hydra.paths.state_dir(&EnvName::parse("personal").unwrap());
        std::fs::create_dir_all(&personal).unwrap();
        let report = f.hydra.delete_env("personal", 0, true).unwrap();
        assert!(report.state_deleted);
        assert!(!personal.exists(), "saved logins deleted when asked");
    }

    #[test]
    fn setting_home_writes_it_and_binds_the_folder() {
        let f = fx("");
        f.hydra.set_env_home("work", &f.free).unwrap();
        let text = std::fs::read_to_string(f.hydra.paths.env_file(&EnvName::parse("work").unwrap())).unwrap();
        assert!(text.contains("home ="), "{text}");
        let config = std::fs::read_to_string(f.hydra.paths.config_file()).unwrap();
        assert!(config.contains("\"work\""), "{config}");
    }
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p hydraui --lib delete_ setting_home` → compile error.

- [ ] **Step 3: Implement** (hydra.rs)

```rust
    /// Deletes `env` as `hydra env rm` does (spec §7.4): refused while hydraui or another
    /// process has its terminals open; saved logins only when `delete_logins`.
    pub fn delete_env(
        &self,
        env: &str,
        open_in_hydraui: usize,
        delete_logins: bool,
    ) -> Result<RemoveReport, String> {
        if open_in_hydraui > 0 {
            return Err(format!(
                "{env} has {open_in_hydraui} terminals open in hydraui: close them first"
            ));
        }
        let name = EnvName::parse(env).map_err(|e| e.to_string())?;
        envs::remove(&self.paths, &name, self.store.as_ref(), delete_logins).map_err(|e| e.to_string())
    }

    /// Makes `dir` `env`'s home (new terminals open there) and binds the folder to it, as
    /// `hydra env new --home` does.
    pub fn set_env_home(&self, env: &str, dir: &Path) -> Result<(), String> {
        let name = EnvName::parse(env).map_err(|e| e.to_string())?;
        let file = self.paths.env_file(&name);
        let text = self.env_text(env)?;
        let text = bindedit::set_home(&text, dir)?;
        let config_file = self.paths.config_file();
        let config = std::fs::read_to_string(&config_file).unwrap_or_default();
        let config = bindedit::add_rule(&config, &bindedit::rule_for_dir(dir), env)?;
        std::fs::write(&file, text).map_err(|e| e.to_string())?;
        std::fs::write(&config_file, config).map_err(|e| e.to_string())
    }
```

(`envs::remove(paths, name, store, delete_state)` deletes the env folder and its secrets, deletes `state/<env>` only when `delete_state`, and reports `secrets`, `state_deleted` and the `bindings` still pointing at it; it refuses with `EnvError::Running` while hydra's running lock is held, and `EnvError::Lends` while another env borrows from it — both reach the user as the error text.)

Launcher: two fixed entries, **Delete environment…** (`Action::DeleteEnv`) and **Set environment home…** (`Action::SetEnvHome`), placed before "New environment…"; each first asks which environment with the env picker (the colour-swatch list `pick_env` uses), then runs the flow below. Update the entries tests' expected lists.

Dialogs (`workspace/launcher.rs`): **Delete** — count hydraui panes whose `env()` is that env; if > 0, toast the refusal; else confirm dialog "Delete work? Its folder and secrets go." with Cancel / Delete; Delete → second dialog "Also delete work's saved logins?" Keep / Delete → `delete_env(env, 0, choice)`; success toast lists deleted secrets and any `report.bindings` ("still bound: …"); then `reload_config` so pickers drop it. **Set home** — the Open dialog in folder mode (`PathPromptOptions { files: false, directories: true, multiple: false, prompt: None }`, with the debug `HYDRAUI_TEST_OPEN_PATH` bypass) → `set_env_home` → toast, `reload_config`.

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib` → pass.

- [ ] **Step 5: Smoke (debug build, temp HYDRA_HOME, harness events from the launching process)**
1. Run `echo src/a.rs:3` style output in a terminal in a scratch folder with that file; Ctrl+click can't be sent (no real Ctrl) — instead verify `path_at` via the unit tests and check Ctrl+click by hand note.
2. Quick open / search: open via the launcher entries — the launcher needs Ctrl+Shift+P, which the harness can't send; verify via unit tests, and leave the dialogs for the user's hands-on check (note it in the summary).
3. Git markers: a scratch git repo with a modified, an added and an untracked file → tree shows M / A / ? colours and folder dots; right-click → Show changes → side-by-side diff tab.
4. Delete/Set home: covered by unit tests; dialogs left for the hands-on check.

- [ ] **Step 6: Gate and commit** — `git commit -m "feat: hydraui deletes environments and sets their home from the launcher"`.
