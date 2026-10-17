# hydraui File Pane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clicking a file in hydraui's tree opens it in a file pane split to the right of the terminal, with file tabs, an editable Raw view (Ctrl+S saves), a Rendered view for Markdown, and Ctrl+click inserting the path as Click does today.

**Architecture:** The tab's split tree stays a tree of `PaneId`s. The workspace keeps terminals in `panes` (unchanged) and file panes in a new `files` map, so every existing "this pane is a terminal" lookup stays correct; a leaf whose id is in `files` renders a `FilePane`. Reading/saving rules (`file_doc.rs`) and file-tab bookkeeping (`open_files.rs`) are pure modules with full tests; `file_pane.rs` is the GPUI view. Sets and `session.toml` gain a `files` layout node, resolved by hydraui-sets.

**Tech Stack:** Rust 2024, GPUI (`gpui-pre =0.3.9`), gpui-component `=0.7.3` (`input::{Editor, EditorState, InputEvent}`, `text::TextView`, tree-sitter highlighter features), `notify`, `toml`/`serde`.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` — §7.8 File pane, §7.3 File tree, §5.1 (files node), §5.4 (session).

## Global Constraints

- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; repo identity davojc; never `--no-verify`; no employer names anywhere.
- Gate before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Tests never touch the real `~/.hydra`, `~/.claude`, `~/.claude.json`, global git config or Credential Manager: use `tempfile` directories.
- File over 5 MB, or binary (a NUL byte in the first 8 KB, or not valid UTF-8): "can't show this file" with Open in editor (§7.8).
- Line endings (CRLF or LF) and UTF-8 BOM recorded on load and written back on save (§7.8).
- Saving is atomic: a temporary file beside the original, then a replace (§7.8).
- Markdown is `.md` / `.markdown`; opens in Rendered, everything else in Raw (§7.8).
- At most one file pane per tab; a tab's layout can't be only a file pane (§5.1).
- Rendered Markdown: links open in the browser only when clicked; images aren't fetched from the network; nothing runs (§7.8).
- Windows only; Python heredocs halve backslashes in this environment — write edit scripts with the Write tool, not `python - <<EOF`.

## Review Focus

1. A Windows file with CRLF endings and a BOM, opened, edited one line and saved — must differ from the original only in that line (no whole-file line-ending rewrite). Pinned in Task 2 (`a_crlf_bom_file_round_trips_with_only_the_edit_changed`).
2. Saving onto a read-only file — must fail with a message, leave the original untouched and leave no temp file behind. Pinned in Task 2 (`saving_onto_a_read_only_file_fails_and_leaves_it_alone`).
3. hydraui's own save firing the disk watcher — must not show "changed on disk" or clobber the editor. Pinned in Task 2 (`our_own_save_seen_on_disk_does_nothing`).
4. Closing a tab's last terminal while its file pane has unsaved edits — must ask before anything is lost. Pinned in Task 3 (`closing_the_last_terminal_closes_the_tab`) for the rule; the prompt is smoke-checked in Task 6.
5. A Markdown file with a remote image (`![x](https://…)`) — Rendered must not fetch it. Pinned in Task 2 (`remote_images_become_links`).

---

## File Structure

- `crates/hydraui-sets/src/file.rs` (modify) — `NodeFile` gains `files: Option<Vec<String>>`, `active: Option<usize>`.
- `crates/hydraui-sets/src/spec.rs` (modify) — `Node::Files(FilesSpec)`, `FilesSpec { files: Vec<PathBuf>, active: usize }`.
- `crates/hydraui-sets/src/resolve.rs` (modify) — resolves a files node: relative paths, drops missing files, collapses an empty file pane, rejects a files-only layout, a second file pane, and mixed keys.
- `crates/hydraui/src/file_doc.rs` (create) — decode/encode/load/save, Markdown and language detection, remote-image defanging, the disk-change decision.
- `crates/hydraui/src/open_files.rs` (create) — file-tab bookkeeping (find, next active after close) and the "which terminal" rule.
- `crates/hydraui/src/tabs.rs` (modify) — `Tabs::split_pane(target, axis, new)`.
- `crates/hydraui/src/layout.rs` (modify) — `TabLayout` with the file pane; `SavedPane` becomes an enum; `tab_file` writes files nodes.
- `crates/hydraui/src/file_pane.rs` (create) — the view: file tabs, header, editor / rendered Markdown, save, unsaved prompt, disk watcher, events.
- `crates/hydraui/src/file_tree_view.rs` (modify) — Click → `TreeEvent::Show`, Ctrl+click → `TreeEvent::Insert`; hint text.
- `crates/hydraui/src/workspace.rs`, `workspace/sets.rs` (modify) — `files` map, last terminal per tab, showing files, closing, rendering, session.
- `crates/hydraui/src/keys.rs`, `main.rs` (modify) — `SaveFile` action bound to `ctrl-s` in the `FilePane` context.
- `Cargo.toml` (modify) — gpui-component tree-sitter features.
- `docs/superpowers/specs/2026-10-17-hydraui-design.md` (modify, Task 6) — closing a tab's last terminal closes the tab.

---

### Task 1: The files node in sets and session files

**Files:**
- Modify: `crates/hydraui-sets/src/file.rs` (NodeFile)
- Modify: `crates/hydraui-sets/src/spec.rs` (Node, new FilesSpec)
- Modify: `crates/hydraui-sets/src/resolve.rs` (node(), tab())
- Modify: `crates/hydraui-sets/src/lib.rs` (export FilesSpec)
- Test: in `crates/hydraui-sets/src/resolve.rs` tests module

**Interfaces:**
- Consumes: existing `Resolver::{node, tab, cwd, problem}`, `NodeFile`, `Node`.
- Produces:
  - `pub struct FilesSpec { pub files: Vec<PathBuf>, pub active: usize }` (`#[derive(Debug, Clone, PartialEq)]`), exported as `hydraui_sets::FilesSpec`.
  - `Node::Files(FilesSpec)` variant.
  - `NodeFile { files: Option<Vec<String>>, active: Option<usize>, .. }`.
  - `Node::files(&self) -> Option<&FilesSpec>` — the tab's file pane, if any.

- [ ] **Step 1: Write the failing tests** (append to the `tests` module in `resolve.rs`; it already has `Fx::new()` — env `work`, set folder `fx.root` — and `fx.parse(text)`, which replaces `ROOT` with that folder and returns `Result<SetSpec, SetError>`)

```rust
    const WITH_FILES: &str = "env = \"work\"\ncwd = \"ROOT\"\n\n[[tab]]\ntitle = \"t\"\n\
        layout = { split = \"right\", panes = [ { }, { files = [FILES], active = 1 } ] }\n";

    fn with_files(fx: &Fx, files: &str) -> Result<SetSpec, SetError> {
        fx.parse(&WITH_FILES.replace("FILES", files))
    }

    #[test]
    fn a_files_node_resolves_relative_paths_and_keeps_active() {
        let fx = Fx::new();
        std::fs::write(fx.root.join("a.md"), "a").unwrap();
        std::fs::write(fx.root.join("b.rs"), "b").unwrap();
        let spec = with_files(&fx, "\"a.md\", \"b.rs\"").unwrap();
        let files = spec.tabs[0].root.files().unwrap();
        assert_eq!(files.files, vec![fx.written("a.md"), fx.written("b.rs")]);
        assert_eq!(files.active, 1);
    }

    #[test]
    fn missing_files_are_dropped_and_an_empty_file_pane_collapses() {
        let fx = Fx::new();
        std::fs::write(fx.root.join("a.md"), "a").unwrap();
        let spec = with_files(&fx, "\"gone.txt\", \"a.md\"").unwrap();
        let files = spec.tabs[0].root.files().unwrap();
        assert_eq!(files.files, vec![fx.written("a.md")]);
        assert_eq!(files.active, 0, "active clamps to the files kept");
        let spec = with_files(&fx, "\"gone.txt\"").unwrap();
        assert!(matches!(spec.tabs[0].root, Node::Pane(_)), "the split collapses to the terminal");
    }

    #[test]
    fn a_files_node_must_sit_beside_a_terminal_alone_and_unmixed() {
        let fx = Fx::new();
        std::fs::write(fx.root.join("a.md"), "a").unwrap();
        let err = |text: &str| fx.parse(text).unwrap_err().to_string();
        let only = "env = \"work\"\ncwd = \"ROOT\"\n[[tab]]\nlayout = { files = [\"a.md\"] }\n";
        assert!(err(only).contains("only a file pane"), "{}", err(only));
        let two = "env = \"work\"\ncwd = \"ROOT\"\n[[tab]]\nlayout = { split = \"right\", panes = [ \
            { files = [\"a.md\"] }, { split = \"down\", panes = [ { }, { files = [\"a.md\"] } ] } ] }\n";
        assert!(err(two).contains("one file pane"), "{}", err(two));
        let mixed = "env = \"work\"\ncwd = \"ROOT\"\n[[tab]]\nlayout = { split = \"right\", panes = [ \
            { }, { files = [\"a.md\"], command = \"x\" } ] }\n";
        assert!(err(mixed).contains("files can't be combined"), "{}", err(mixed));
    }
```

`fx.written(sub)` is the existing helper giving `ROOT/sub` as the set file spells it; relative `files` resolve against the set's `cwd` the same way `cwd` does. If `SetError`'s `Display` doesn't include the problem list, compare against the variant holding the problems instead (read `SetError` in `resolve.rs`).

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p hydraui-sets files_node`  and  `cargo test -p hydraui-sets a_files_node`
Expected: compile errors — no field `files` on `NodeFile`, no method `files` on `Node`.

- [ ] **Step 3: Implement**

`file.rs`, add to `NodeFile` after `command`:

```rust
    /// A file pane (spec §7.8): the files it shows, absolute or relative like `cwd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Index into `files` of the file tab shown first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
```

`spec.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct FilesSpec {
    /// Existing files, in file-tab order.
    pub files: Vec<PathBuf>,
    /// Always a valid index into `files`.
    pub active: usize,
}
```

add `Files(FilesSpec),` to `Node`, extend `panes()` with `Node::Files(_) => Vec::new(),`, and add:

```rust
    /// The tab's file pane, if it has one.
    pub fn files(&self) -> Option<&FilesSpec> {
        match self {
            Node::Pane(_) => None,
            Node::Files(f) => Some(f),
            Node::Split { first, second, .. } => first.files().or_else(|| second.files()),
        }
    }
```

`lib.rs`: export `FilesSpec` beside `Node`.

`resolve.rs`, in `node()`, before the `let Some(dir) = &n.split else {` line:

```rust
        if let Some(list) = &n.files {
            let loc = format!("the file pane of {tab}");
            if n.split.is_some()
                || n.panes.is_some()
                || n.ratio.is_some()
                || n.env.is_some()
                || n.cwd.is_some()
                || n.shell.is_some()
                || n.command.is_some()
            {
                self.problem(format!("{loc}: files can't be combined with other keys"));
                return None;
            }
            let files: Vec<PathBuf> = list
                .iter()
                .map(|f| self.cwd(Some(f), &inh.cwd))
                .filter(|p| p.is_file())
                .collect();
            if files.is_empty() {
                return Some(Node::Files(FilesSpec { files, active: 0 }));
            }
            let active = n.active.unwrap_or(0).min(files.len() - 1);
            return Some(Node::Files(FilesSpec { files, active }));
        }
```

and in the split branch, after building `first`/`second`, collapse an empty file pane:

```rust
        let (first, second) = (first?, second?);
        let empty = |n: &Node| matches!(n, Node::Files(f) if f.files.is_empty());
        if empty(&first) {
            return Some(second);
        }
        if empty(&second) {
            return Some(first);
        }
        Some(Node::Split {
            dir,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        })
```

(replacing the existing `Some(Node::Split { … first: Box::new(first?), … })`).

In `tab()`, after the layout's root `Node` is built (where `Some(layout) => { … }` produces it), add the tab-level checks:

```rust
                let root = self.node(&loc, layout, &inh, &mut count)?;
                let file_panes = count_files(&root);
                if matches!(root, Node::Files(_)) {
                    self.problem(format!("{loc}: a layout can't be only a file pane"));
                    return None;
                }
                if file_panes > 1 {
                    self.problem(format!("{loc}: a tab has at most one file pane"));
                    return None;
                }
                root
```

with, at module level:

```rust
fn count_files(n: &Node) -> usize {
    match n {
        Node::Pane(_) => 0,
        Node::Files(_) => 1,
        Node::Split { first, second, .. } => count_files(first) + count_files(second),
    }
}
```

The empty-files case: a root that is `Node::Files` with no files (the whole layout was a files node whose files are all missing) also hits "only a file pane" — correct, it was invalid anyway. Note: count before collapse is not needed; a collapsed empty pane is gone.

The compiler then flags the `match` in `crates/hydraui/src/layout.rs` `node()`. Add the arm `Node::Files(_) => Tree::Leaf(next_id()),` there; Task 4 replaces it with the real handling.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui-sets`
Expected: all pass, including the three new tests.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui-sets crates/hydraui/src/layout.rs
git commit -m "feat: sets and session files can hold a file pane

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Reading and saving files (`file_doc.rs`)

**Files:**
- Create: `crates/hydraui/src/file_doc.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod file_doc;`)
- Test: in `file_doc.rs`

**Interfaces:**
- Produces:
  - `pub const MAX_BYTES: u64 = 5 * 1024 * 1024;`
  - `pub struct FileDoc { pub text: String, pub crlf: bool, pub bom: bool }` (`Debug, Clone, PartialEq`) — `text` uses `\n` only, no BOM.
  - `pub fn decode(bytes: &[u8]) -> Result<FileDoc, String>` — `Err("binary file")` for NUL in first 8 KB or invalid UTF-8.
  - `pub fn encode(text: &str, crlf: bool, bom: bool) -> Vec<u8>`
  - `pub fn load(path: &Path) -> Result<FileDoc, String>` — size check, read errors as text.
  - `pub fn save(path: &Path, doc: &FileDoc) -> Result<(), String>` — atomic.
  - `pub fn is_markdown(path: &Path) -> bool`
  - `pub fn language(path: &Path) -> &'static str` — highlighter name, `"text"` when unknown.
  - `pub fn defang_remote_images(markdown: &str) -> String`
  - `pub enum DiskChange { Nothing, Reload(FileDoc), Changed, Deleted }` and `pub fn on_disk(saved: &FileDoc, dirty: bool, now: Option<Result<FileDoc, String>>) -> DiskChange` — `now` is `None` when the file no longer exists.

- [ ] **Step 1: Write the failing tests** (create the file with the test module and stub signatures that `todo!()`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_bom_are_recorded_and_stripped() {
        let doc = decode(b"\xEF\xBB\xBFone\r\ntwo\r\n").unwrap();
        assert_eq!(doc, FileDoc { text: "one\ntwo\n".into(), crlf: true, bom: true });
        let doc = decode(b"one\ntwo").unwrap();
        assert_eq!(doc, FileDoc { text: "one\ntwo".into(), crlf: false, bom: false });
    }

    #[test]
    fn a_crlf_bom_file_round_trips_with_only_the_edit_changed() {
        let original = b"\xEF\xBB\xBFline 1\r\nline 2\r\nline 3\r\n";
        let mut doc = decode(original).unwrap();
        doc.text = doc.text.replace("line 2", "LINE TWO");
        assert_eq!(
            encode(&doc.text, doc.crlf, doc.bom),
            b"\xEF\xBB\xBFline 1\r\nLINE TWO\r\nline 3\r\n".to_vec()
        );
    }

    #[test]
    fn binary_and_invalid_utf8_cant_be_shown() {
        assert_eq!(decode(b"abc\0def").unwrap_err(), "binary file");
        assert_eq!(decode(&[0xff, 0xfe, 0x41]).unwrap_err(), "binary file");
    }

    #[test]
    fn big_files_cant_be_shown() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("big.txt");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_BYTES + 1).unwrap();
        assert!(load(&p).unwrap_err().contains("5 MB"));
        assert!(load(&d.path().join("missing.txt")).is_err());
    }

    #[test]
    fn save_writes_the_endings_back_atomically() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "old\r\n").unwrap();
        let doc = FileDoc { text: "new\nline\n".into(), crlf: true, bom: false };
        save(&p, &doc).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new\r\nline\r\n");
        let leftovers: Vec<_> = std::fs::read_dir(d.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temp file left");
    }

    #[test]
    fn saving_onto_a_read_only_file_fails_and_leaves_it_alone() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ro.txt");
        std::fs::write(&p, "keep").unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms.clone()).unwrap();
        let doc = FileDoc { text: "changed".into(), crlf: false, bom: false };
        assert!(save(&p, &doc).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "keep");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "no temp file left");
        perms.set_readonly(false);
        std::fs::set_permissions(&p, perms).unwrap();
    }

    #[test]
    fn markdown_and_languages_come_from_the_extension() {
        assert!(is_markdown(Path::new("README.md")));
        assert!(is_markdown(Path::new("notes.MARKDOWN")));
        assert!(!is_markdown(Path::new("main.rs")));
        assert_eq!(language(Path::new("main.rs")), "rust");
        assert_eq!(language(Path::new("Cargo.toml")), "toml");
        assert_eq!(language(Path::new("a.ps1")), "text");
        assert_eq!(language(Path::new("Makefile")), "make");
        assert_eq!(language(Path::new("x.tsx")), "tsx");
    }

    #[test]
    fn remote_images_become_links() {
        let md = "![logo](https://example.com/a.png) and ![local](img/b.png)";
        assert_eq!(
            defang_remote_images(md),
            "[logo](https://example.com/a.png) and ![local](img/b.png)"
        );
    }

    fn doc(t: &str) -> FileDoc {
        FileDoc { text: t.into(), crlf: false, bom: false }
    }

    #[test]
    fn our_own_save_seen_on_disk_does_nothing() {
        assert_eq!(on_disk(&doc("a"), false, Some(Ok(doc("a")))), DiskChange::Nothing);
        assert_eq!(on_disk(&doc("a"), true, Some(Ok(doc("a")))), DiskChange::Nothing);
    }

    #[test]
    fn a_change_on_disk_reloads_unless_there_are_edits() {
        assert_eq!(on_disk(&doc("a"), false, Some(Ok(doc("b")))), DiskChange::Reload(doc("b")));
        assert_eq!(on_disk(&doc("a"), true, Some(Ok(doc("b")))), DiskChange::Changed);
        assert_eq!(on_disk(&doc("a"), false, None), DiskChange::Deleted);
        assert_eq!(on_disk(&doc("a"), true, Some(Err("binary file".into()))), DiskChange::Changed);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib file_doc`
Expected: tests panic at `todo!()` (not yet implemented).

- [ ] **Step 3: Implement**

```rust
//! Reading and saving a file for the file pane (spec §7.8): too big or binary can't be shown,
//! line endings and a BOM come back on save, saves are atomic; and what a change on disk means.
use std::io::Write;
use std::path::Path;

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
const SNIFF: usize = 8 * 1024;
const BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, PartialEq)]
pub struct FileDoc {
    /// The text with `\n` line endings and no BOM, as the editor holds it.
    pub text: String,
    /// The file used CRLF (judged by its first line ending).
    pub crlf: bool,
    pub bom: bool,
}

pub fn decode(bytes: &[u8]) -> Result<FileDoc, String> {
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    if body[..body.len().min(SNIFF)].contains(&0) {
        return Err("binary file".into());
    }
    let raw = std::str::from_utf8(body).map_err(|_| "binary file".to_string())?;
    let crlf = raw.find('\n').is_some_and(|i| i > 0 && raw.as_bytes()[i - 1] == b'\r');
    Ok(FileDoc { text: raw.replace("\r\n", "\n"), crlf, bom })
}

pub fn encode(text: &str, crlf: bool, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 3);
    if bom {
        out.extend_from_slice(BOM);
    }
    if crlf {
        out.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    out
}

pub fn load(path: &Path) -> Result<FileDoc, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err("over 5 MB".into());
    }
    decode(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// Writes a temporary file beside `path`, then replaces `path` with it; on any failure the
/// temporary file is removed and `path` is untouched.
pub fn save(path: &Path, doc: &FileDoc) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(format!("{} is read-only", path.display()));
    }
    let name = path.file_name().ok_or("no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.hydraui-save"));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&encode(&doc.text, doc.crlf, doc.bom))?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("can't save {}: {e}", path.display())
    })
}

pub fn is_markdown(path: &Path) -> bool {
    matches!(ext(path).as_deref(), Some("md" | "markdown"))
}

fn ext(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// The highlighter's language for the file; `"text"` (no highlighting) when unknown.
pub fn language(path: &Path) -> &'static str {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase());
    if matches!(name.as_deref(), Some("makefile" | "gnumakefile")) {
        return "make";
    }
    match ext(path).as_deref() {
        Some("rs") => "rust",
        Some("toml") => "toml",
        Some("json" | "jsonc") => "json",
        Some("md" | "markdown") => "markdown",
        Some("js" | "mjs" | "cjs" | "jsx") => "javascript",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        Some("py") => "python",
        Some("sh" | "bash") => "bash",
        Some("yml" | "yaml") => "yaml",
        Some("css" | "scss") => "css",
        Some("html" | "htm") => "html",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "cc" | "cxx" | "hpp" | "hh") => "cpp",
        Some("cs") => "csharp",
        Some("sql") => "sql",
        _ => "text",
    }
}

/// `![alt](http…)` → `[alt](http…)`: Rendered never fetches from the network (spec §7.8).
pub fn defang_remote_images(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(i) = rest.find("![") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let remote = tail
            .find("](")
            .map(|j| &tail[j + 2..])
            .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"));
        if !remote {
            out.push('!');
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiskChange {
    Nothing,
    Reload(FileDoc),
    /// Changed on disk while there are unsaved edits (or it can't be read now).
    Changed,
    Deleted,
}

/// What a change on disk means for an open file: `saved` is what was last loaded or saved,
/// `now` what is on disk (`None`: gone).
pub fn on_disk(saved: &FileDoc, dirty: bool, now: Option<Result<FileDoc, String>>) -> DiskChange {
    match now {
        None => DiskChange::Deleted,
        Some(Ok(doc)) if doc == *saved => DiskChange::Nothing,
        Some(Ok(doc)) if !dirty => DiskChange::Reload(doc),
        Some(_) => DiskChange::Changed,
    }
}
```

Add `pub mod file_doc;` to `lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui --lib file_doc`
Expected: 10 passed.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui/src/file_doc.rs crates/hydraui/src/lib.rs
git commit -m "feat: hydraui reads and saves files for the file pane

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: File-tab bookkeeping, splitting a given pane, and which terminal

**Files:**
- Create: `crates/hydraui/src/open_files.rs`
- Modify: `crates/hydraui/src/tabs.rs` (add `split_pane`)
- Modify: `crates/hydraui/src/lib.rs` (`pub mod open_files;`)
- Test: in both files

**Interfaces:**
- Produces:
  - `pub fn find(paths: &[PathBuf], path: &Path) -> Option<usize>` — matches ignoring ASCII case and `/` vs `\` (Windows paths).
  - `pub fn after_close(len: usize, active: usize, closed: usize) -> Option<usize>` — the new active index once `closed` is removed from a list of `len`; `None` when the list is now empty. The tab to the right becomes active, or the left one if it was last; closing a tab other than the active one keeps the same file active.
  - `pub fn terminal_for(focused: PaneId, last: Option<PaneId>, tree_panes: &[PaneId], is_terminal: &dyn Fn(PaneId) -> bool) -> Option<PaneId>` — focused if a terminal, else `last` if still in the tree and a terminal, else the first terminal in the tree.
  - `pub fn last_terminal_closes_tab(tree_panes: &[PaneId], closing: PaneId, is_terminal: &dyn Fn(PaneId) -> bool) -> bool`
  - `Tabs::split_pane(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool` — splits `target` in whichever tab holds it, focuses `new`, activates that tab.

- [ ] **Step 1: Write the failing tests**

`open_files.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_file_is_found_however_its_path_is_written() {
        let paths = vec![PathBuf::from("C:\\work\\README.md"), PathBuf::from("C:\\work\\a.rs")];
        assert_eq!(find(&paths, Path::new("c:/work/readme.md")), Some(0));
        assert_eq!(find(&paths, Path::new("C:\\work\\a.rs")), Some(1));
        assert_eq!(find(&paths, Path::new("C:\\work\\b.rs")), None);
    }

    #[test]
    fn closing_picks_the_next_file_tab() {
        assert_eq!(after_close(3, 1, 1), Some(1), "the right one moves into place");
        assert_eq!(after_close(3, 2, 2), Some(1), "the last one: the left one");
        assert_eq!(after_close(3, 2, 0), Some(1), "another tab: the same file stays active");
        assert_eq!(after_close(3, 0, 2), Some(0));
        assert_eq!(after_close(1, 0, 0), None);
    }

    #[test]
    fn the_terminal_is_the_focused_one_else_the_last_else_the_first() {
        let term = |id: PaneId| id != 9;
        assert_eq!(terminal_for(2, Some(1), &[1, 2, 9], &term), Some(2));
        assert_eq!(terminal_for(9, Some(1), &[1, 2, 9], &term), Some(1));
        assert_eq!(terminal_for(9, Some(5), &[1, 2, 9], &term), Some(1), "last is gone");
        assert_eq!(terminal_for(9, None, &[9], &term), None);
    }

    #[test]
    fn closing_the_last_terminal_closes_the_tab() {
        let term = |id: PaneId| id != 9;
        assert!(last_terminal_closes_tab(&[1, 9], 1, &term));
        assert!(!last_terminal_closes_tab(&[1, 2, 9], 1, &term));
        assert!(!last_terminal_closes_tab(&[1, 9], 9, &term), "closing the file pane");
    }
}
```

`tabs.rs` test:

```rust
    #[test]
    fn split_pane_splits_that_pane_and_focuses_the_new_one() {
        let mut t = Tabs::default();
        let a = t.add(None, 1);
        let b = t.add(None, 2);
        assert_eq!(t.active_id(), Some(b));
        assert!(t.split_pane(1, Axis::Row, 3));
        assert_eq!(t.active_id(), Some(a));
        let tab = t.active().unwrap();
        assert_eq!(tab.focused, 3);
        assert_eq!(tab.tree.panes(), vec![1, 3]);
        assert!(!t.split_pane(42, Axis::Row, 4));
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib open_files` and `cargo test -p hydraui --lib split_pane`
Expected: compile errors (missing module / method).

- [ ] **Step 3: Implement**

`open_files.rs`:

```rust
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

/// The terminal a tab's status bar, git summary, tree root, insert, `cd` and Split use.
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
```

`tabs.rs`, beside `split_focused`:

```rust
    /// Splits `target`, in whichever tab holds it, and focuses `new` there.
    pub fn split_pane(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.tree.contains(target)) else {
            return false;
        };
        if !self.tabs[i].tree.split(target, axis, new) {
            return false;
        }
        self.active = i;
        self.tabs[i].focused = new;
        true
    }
```

Add `pub mod open_files;` to `lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui --lib open_files` and `cargo test -p hydraui --lib tabs`
Expected: all pass.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui/src/open_files.rs crates/hydraui/src/tabs.rs crates/hydraui/src/lib.rs
git commit -m "feat: hydraui file-tab bookkeeping and which terminal stands for a tab

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Layouts with a file pane, both ways

**Files:**
- Modify: `crates/hydraui/src/layout.rs`
- Modify: `crates/hydraui/src/workspace/sets.rs` (callers of `tab_layout` and `SavedPane`)
- Test: in `layout.rs`

**Interfaces:**
- Consumes: `hydraui_sets::{Node, FilesSpec}` (Task 1), `NodeFile.files/active`.
- Produces:
  - `pub struct TabLayout { pub tree: Tree, pub terminals: Vec<(PaneId, PaneRequest)>, pub files: Option<(PaneId, FilesSpec)> }`
  - `pub fn tab_layout<F: FnMut() -> PaneId>(tab: &TabSpec, next_id: &mut F) -> TabLayout`
  - `pub enum SavedPane { Terminal { env: String, cwd: PathBuf, shell: ShellKind }, Files { files: Vec<PathBuf>, active: usize } }`
  - `pub fn tab_file(tree: &Tree, pane: &impl Fn(PaneId) -> Option<SavedPane>) -> Option<TabFile>` — unchanged signature; a root `Files` leaf gives `None` (not a valid tab).

- [ ] **Step 1: Write the failing tests** (append to `layout.rs` tests; update the existing tests' `SavedPane { env, cwd, shell }` literals to `SavedPane::Terminal { env, cwd, shell }` and `let (tree, panes) = tab_layout(..)` to `let l = tab_layout(..); let (tree, panes) = (l.tree, l.terminals);`)

```rust
    #[test]
    fn a_file_pane_saves_as_a_files_node_and_reopens() {
        let d = tempfile::tempdir().unwrap();
        let paths = hydra_core::paths::HydraPaths::new(d.path().join(".hydra"));
        let work = EnvName::parse("work").unwrap();
        std::fs::create_dir_all(paths.env_dir(&work)).unwrap();
        std::fs::write(paths.env_file(&work), "").unwrap();
        let bindings = std::collections::BTreeMap::new();
        let ctx = hydraui_sets::Context {
            paths: &paths,
            user_home: d.path(),
            bindings: &bindings,
            default_shell: ShellKind::Pwsh,
        };
        let a = d.path().join("a.md");
        let b = d.path().join("b.rs");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let tree = Tree::Split {
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(Tree::Leaf(1)),
            second: Box::new(Tree::Leaf(2)),
        };
        let saved = |id| match id {
            1 => Some(SavedPane::Terminal {
                env: "work".into(),
                cwd: d.path().to_path_buf(),
                shell: ShellKind::Pwsh,
            }),
            2 => Some(SavedPane::Files { files: vec![a.clone(), b.clone()], active: 1 }),
            _ => None,
        };
        let file = tab_file(&tree, &saved).unwrap();
        let node = file.layout.as_ref().unwrap();
        let fnode = &node.panes.as_ref().unwrap()[1];
        assert_eq!(fnode.active, Some(1));
        assert_eq!(fnode.files.as_ref().unwrap().len(), 2);
        let spec = hydraui_sets::resolve_loose(&ctx, &file).unwrap();
        let mut n = 10;
        let l = tab_layout(&spec, &mut || {
            n += 1;
            n
        });
        assert_eq!(l.terminals.len(), 1);
        let (fid, files) = l.files.unwrap();
        assert_eq!(files.files, vec![a, b]);
        assert_eq!(files.active, 1);
        assert_eq!(l.tree.panes(), vec![l.terminals[0].0, fid]);
    }

    #[test]
    fn a_tab_that_is_only_a_file_pane_isnt_saved() {
        let saved = |_| Some(SavedPane::Files { files: vec![PathBuf::from("C:/x.md")], active: 0 });
        assert_eq!(tab_file(&Tree::Leaf(1), &saved), None);
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib layout`
Expected: compile errors (`SavedPane::Files`, `l.files`).

- [ ] **Step 3: Implement**

Replace the top of `layout.rs`:

```rust
/// A tab's tree, its terminals (in tree order) and its file pane.
pub struct TabLayout {
    pub tree: Tree,
    pub terminals: Vec<(PaneId, PaneRequest)>,
    pub files: Option<(PaneId, FilesSpec)>,
}

pub fn tab_layout<F: FnMut() -> PaneId>(tab: &TabSpec, next_id: &mut F) -> TabLayout {
    let mut terminals = Vec::new();
    let mut files = None;
    let tree = node(&tab.root, next_id, &mut terminals, &mut files);
    TabLayout { tree, terminals, files }
}
```

`node()` gains the `files: &mut Option<(PaneId, FilesSpec)>` parameter, passed down in the split arm, and:

```rust
        Node::Files(f) => {
            let id = next_id();
            *files = Some((id, f.clone()));
            Tree::Leaf(id)
        }
```

`SavedPane` becomes the enum above. In `tab_file`, the root-leaf arm:

```rust
        Tree::Leaf(id) => match pane(*id)? {
            SavedPane::Terminal { env, cwd, shell } => Some(TabFile {
                env: Some(env),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                shell: Some(shell.as_str().into()),
                ..Default::default()
            }),
            SavedPane::Files { .. } => None,
        },
```

and in `node_file` the leaf arm:

```rust
        Tree::Leaf(id) => Some(match pane(*id)? {
            SavedPane::Terminal { env, cwd, shell } => NodeFile {
                env: Some(env),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                shell: Some(shell.as_str().into()),
                ..Default::default()
            },
            SavedPane::Files { files, active } => NodeFile {
                files: Some(files.iter().map(|f| f.to_string_lossy().into_owned()).collect()),
                active: (active > 0).then_some(active),
                ..Default::default()
            },
        }),
```

Imports: `use hydraui_sets::{FilesSpec, Node, SplitDir, TabSpec};`.

The existing `a_saved_tab_reopens_as_the_same_layout` test rewrites `cwd` with struct-update syntax; with the enum write it as:

```rust
        let saved_here = |id: PaneId| {
            saved(id).map(|p| match p {
                SavedPane::Terminal { env, shell, .. } => SavedPane::Terminal { env, cwd: folder(id), shell },
                other => other,
            })
        };
```

and the new test builds its `Context` the same way that test does (temp `HydraPaths` with env `work`, empty bindings, `user_home: d.path()`), as written above.

`workspace/sets.rs` callers open terminals only in this task. A set or session that has a file pane opens without it until Task 6 adds file panes to the workspace; that intermediate state is fine because nothing writes file panes yet:

- `open_set`: `let TabLayout { tree, terminals, files: _ } = tab_layout(tab, &mut || { … });`, `let focused = terminals[0].0;`, create each terminal, `self.tabs.add_tree(…, tree, focused)`.
- `restore_session`: the same destructuring.
- `save_session`'s `pane` closure returns `SavedPane::Terminal { … }`.

Task 6 Step 3 (item 9) replaces `files: _` at both call sites.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui --lib layout`
Expected: all layout tests pass (5 old, 2 new).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui/src/layout.rs crates/hydraui/src/workspace/sets.rs
git commit -m "feat: hydraui layouts carry a file pane to and from session files

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The file pane view

**Files:**
- Create: `crates/hydraui/src/file_pane.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod file_pane;`)
- Modify: `crates/hydraui/src/keys.rs` (actions `SaveFile`; `file_pane_bindings()`), `crates/hydraui/src/main.rs` (bind them)
- Modify: `Cargo.toml` (gpui-component features)
- Test: `keys.rs` (binding precedence), smoke in Task 6

**Interfaces:**
- Consumes: `file_doc::{load, save, decode, FileDoc, is_markdown, language, defang_remote_images, on_disk, DiskChange}`, `open_files::{find, after_close}`.
- Produces:
  - `pub struct FilePane` (GPUI view, `Focusable`), `FilePane::new(files: &[PathBuf], active: usize, window, cx) -> Self`.
  - `FilePane::open(&mut self, path: PathBuf, window, cx)` — switches to an open file or adds a file tab.
  - `FilePane::saved(&self) -> (Vec<PathBuf>, usize)` — open files and active index, for the session.
  - `FilePane::unsaved(&self) -> Vec<PathBuf>` — files with unsaved edits.
  - `FilePane::save_all(&mut self, window, cx) -> Result<(), String>`.
  - `pub enum FilePaneEvent { Focused, Empty, OpenInEditor(PathBuf) }` — `Empty` when the last file tab closed.
  - `keys::SaveFile` action; `keys::file_pane_bindings() -> Vec<KeyBinding>` (`ctrl-s` in context `"FilePane"`).

- [ ] **Step 1: Write the failing binding test** (in `keys.rs` tests, beside the terminal one)

```rust
    #[test]
    fn ctrl_s_saves_in_a_file_pane() {
        let keymap = gpui::Keymap::new(file_pane_bindings());
        let stack = [
            gpui::KeyContext::parse("Workspace").unwrap(),
            gpui::KeyContext::parse("FilePane").unwrap(),
        ];
        let (found, _) = keymap.bindings_for_input(&[Keystroke::parse("ctrl-s").unwrap()], &stack);
        assert!(found[0].action().partial_eq(&SaveFile));
    }
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p hydraui --lib ctrl_s_saves`
Expected: compile error (no `file_pane_bindings`, `SaveFile`).

- [ ] **Step 3: Implement**

`keys.rs`: add `SaveFile` to the `actions!` list and

```rust
/// Ctrl+S in a file pane (spec §7.8).
pub fn file_pane_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("ctrl-s", SaveFile, Some("FilePane"))]
}
```

`main.rs`: `cx.bind_keys(keys::file_pane_bindings());` after the terminal bindings.

`Cargo.toml` (workspace): 

```toml
gpui-component = { version = "=0.7.3", features = [
    "tree-sitter-rust", "tree-sitter-toml", "tree-sitter-json", "tree-sitter-markdown",
    "tree-sitter-markdown-inline", "tree-sitter-javascript", "tree-sitter-typescript",
    "tree-sitter-tsx", "tree-sitter-python", "tree-sitter-bash", "tree-sitter-yaml",
    "tree-sitter-css", "tree-sitter-html", "tree-sitter-go", "tree-sitter-c",
    "tree-sitter-cpp", "tree-sitter-csharp", "tree-sitter-sql", "tree-sitter-make",
] }
```

`file_pane.rs` (the view; read gpui-component's `input/editor.rs` and `text/compat.rs` if a call below doesn't compile, and ledger the adjustment):

```rust
//! The file pane (spec §7.8): file tabs, Raw (an editor; Ctrl+S saves) and Rendered (Markdown),
//! Open in editor, the unsaved-edits prompt, and changes on disk.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Editor, EditorState, InputEvent};
use gpui_component::notification::Notification;
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::file_doc::{self, DiskChange, FileDoc};
use crate::keys::SaveFile;
use crate::open_files::{after_close, find};

#[derive(Debug, Clone, PartialEq)]
pub enum FilePaneEvent {
    Focused,
    /// The last file tab closed: the pane goes away.
    Empty,
    OpenInEditor(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Raw,
    Rendered,
}

enum Body {
    Text {
        editor: Entity<EditorState>,
        /// What was last loaded or saved: unsaved edits are a difference from it.
        saved: FileDoc,
        view: View,
        _sub: Subscription,
    },
    CantShow(String),
}

struct FileTab {
    path: PathBuf,
    body: Body,
    dirty: bool,
    changed_on_disk: bool,
    deleted: bool,
}

pub struct FilePane {
    tabs: Vec<FileTab>,
    active: usize,
    focus: FocusHandle,
    watcher: Option<RecommendedWatcher>,
    watched: BTreeSet<PathBuf>,
    _pump: Task<()>,
}

impl EventEmitter<FilePaneEvent> for FilePane {}

impl Focusable for FilePane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn watcher(tx: UnboundedSender<PathBuf>) -> Option<RecommendedWatcher> {
    notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                let _ = tx.unbounded_send(path);
            }
        }
    })
    .ok()
}

impl FilePane {
    pub fn new(files: &[PathBuf], active: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<PathBuf>();
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Some(path) = rx.next().await {
                if this
                    .update_in(cx, |pane, window, cx| pane.disk_changed(&path, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut pane = Self {
            tabs: Vec::new(),
            active: 0,
            focus: cx.focus_handle(),
            watcher: watcher(tx),
            watched: BTreeSet::new(),
            _pump: pump,
        };
        for f in files {
            pane.push(f.clone(), window, cx);
        }
        pane.active = active.min(pane.tabs.len().saturating_sub(1));
        pane.sync_watches();
        pane
    }

    fn push(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let body = match file_doc::load(&path) {
            Ok(doc) => self.text_body(&path, doc, window, cx),
            Err(e) => Body::CantShow(e),
        };
        self.tabs.push(FileTab { path, body, dirty: false, changed_on_disk: false, deleted: false });
    }

    fn text_body(&self, path: &Path, doc: FileDoc, window: &mut Window, cx: &mut Context<Self>) -> Body {
        let lang = file_doc::language(path);
        let text = doc.text.clone();
        let editor = cx.new(|cx| {
            let mut e = EditorState::new(window, cx).language(lang);
            e.set_value(text, window, cx);
            e
        });
        let key = path.to_path_buf();
        let sub = cx.subscribe(&editor, move |pane, _, ev: &InputEvent, cx| {
            if *ev == InputEvent::Change {
                pane.edited(&key, cx);
            }
        });
        let view = if file_doc::is_markdown(path) { View::Rendered } else { View::Raw };
        Body::Text { editor, saved: doc, view, _sub: sub }
    }

    /// Recomputes the file's unsaved state after an edit.
    fn edited(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.path == path) {
            if let Body::Text { editor, saved, .. } = &t.body {
                t.dirty = editor.read(cx).value().as_ref() != saved.text;
                cx.notify();
            }
        }
    }

    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.active = match find(&paths, &path) {
            Some(i) => i,
            None => {
                self.push(path, window, cx);
                self.tabs.len() - 1
            }
        };
        self.sync_watches();
        cx.notify();
    }

    pub fn saved(&self) -> (Vec<PathBuf>, usize) {
        (self.tabs.iter().map(|t| t.path.clone()).collect(), self.active)
    }

    pub fn unsaved(&self) -> Vec<PathBuf> {
        self.tabs.iter().filter(|t| t.dirty).map(|t| t.path.clone()).collect()
    }

    fn save_tab(&mut self, i: usize, cx: &mut Context<Self>) -> Result<(), String> {
        let t = &mut self.tabs[i];
        let Body::Text { editor, saved, .. } = &mut t.body else {
            return Ok(());
        };
        let doc = FileDoc { text: editor.read(cx).value().to_string(), crlf: saved.crlf, bom: saved.bom };
        file_doc::save(&t.path, &doc)?;
        *saved = doc;
        t.dirty = false;
        t.changed_on_disk = false;
        t.deleted = false;
        cx.notify();
        Ok(())
    }

    pub fn save_all(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        for i in 0..self.tabs.len() {
            if self.tabs[i].dirty {
                self.save_tab(i, cx)?;
            }
        }
        Ok(())
    }

    fn save_active(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.save_tab(self.active, cx) {
            window.push_notification(Notification::error(e).title("Save"), cx);
        }
    }

    /// Closes a file tab, asking first when it has unsaved edits.
    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs[i].dirty {
            self.remove_tab(i, cx);
            return;
        }
        let name = file_name(&self.tabs[i].path);
        let pane = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (save, discard) = (pane.clone(), pane.clone());
            dialog
                .title(format!("{name} has unsaved edits"))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("cancel").label("Cancel").on_click(|_, w, cx| w.close_dialog(cx)))
                        .child(Button::new("discard").label("Discard").on_click(move |_, w, cx| {
                            w.close_dialog(cx);
                            discard.update(cx, |p, cx| p.remove_tab(i, cx));
                        }))
                        .child(Button::new("save").label("Save").primary().on_click(move |_, w, cx| {
                            w.close_dialog(cx);
                            save.update(cx, |p, cx| match p.save_tab(i, cx) {
                                Ok(()) => p.remove_tab(i, cx),
                                Err(e) => w.push_notification(Notification::error(e).title("Save"), cx),
                            });
                        })),
                )
        });
    }

    fn remove_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        match after_close(self.tabs.len(), self.active, i) {
            None => {
                self.tabs.clear();
                cx.emit(FilePaneEvent::Empty);
            }
            Some(next) => {
                self.tabs.remove(i);
                self.active = next;
            }
        }
        self.sync_watches();
        cx.notify();
    }

    fn sync_watches(&mut self) {
        let want: BTreeSet<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|t| t.path.parent().map(Path::to_path_buf))
            .collect();
        if let Some(w) = self.watcher.as_mut() {
            for gone in self.watched.difference(&want) {
                let _ = w.unwatch(gone);
            }
            for new in want.difference(&self.watched) {
                let _ = w.watch(new, RecursiveMode::NonRecursive);
            }
        }
        self.watched = want;
    }

    fn disk_changed(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.tabs.iter().position(|t| t.path == path) else {
            return;
        };
        let t = &mut self.tabs[i];
        let Body::Text { editor, saved, .. } = &mut t.body else {
            return;
        };
        let now = path.is_file().then(|| file_doc::load(path));
        match file_doc::on_disk(saved, t.dirty, now) {
            DiskChange::Nothing => {}
            DiskChange::Reload(doc) => {
                let text = doc.text.clone();
                editor.update(cx, |e, cx| e.set_value(text, window, cx));
                *saved = doc;
                t.deleted = false;
            }
            DiskChange::Changed => t.changed_on_disk = true,
            DiskChange::Deleted => t.deleted = true,
        }
        cx.notify();
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let t = &mut self.tabs[self.active];
        if let (Body::Text { editor, saved, .. }, Ok(doc)) = (&mut t.body, file_doc::load(&t.path)) {
            let text = doc.text.clone();
            editor.update(cx, |e, cx| e.set_value(text, window, cx));
            *saved = doc;
            t.dirty = false;
            t.changed_on_disk = false;
        }
        cx.notify();
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, bar, accent) = (theme.muted_foreground, theme.border, theme.tab_bar, theme.accent);
        let mut strip = h_flex().h(px(30.)).gap_1().px_1().bg(bar).border_b_1().border_color(border);
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!("{}{}", file_name(&t.path), if t.dirty { " ●" } else { "" });
            strip = strip.child(
                h_flex()
                    .id(("file-tab", i))
                    .px_2()
                    .gap_1()
                    .rounded_sm()
                    .when(i == self.active, |d| d.bg(accent))
                    .child(div().text_sm().child(label))
                    .child(
                        div()
                            .id(("file-close", i))
                            .text_xs()
                            .text_color(muted)
                            .child("×")
                            .on_click(cx.listener(move |p, _, w, cx| p.close_tab(i, w, cx))),
                    )
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.active = i;
                        cx.notify();
                    })),
            );
        }
        let Some(t) = self.tabs.get(self.active) else {
            return v_flex().size_full().track_focus(&self.focus).child(strip).into_any_element();
        };
        let path = t.path.clone();
        let mut header = h_flex().h(px(28.)).px_2().gap_2().justify_end().border_b_1().border_color(border);
        if let Body::Text { view, .. } = &t.body {
            if file_doc::is_markdown(&t.path) {
                let other = if *view == View::Raw { "Rendered" } else { "Raw" };
                header = header.child(Button::new("view-mode").label(other).ghost().xsmall().on_click(
                    cx.listener(|p, _, _, cx| {
                        if let Some(FileTab { body: Body::Text { view, .. }, .. }) = p.tabs.get_mut(p.active) {
                            *view = if *view == View::Raw { View::Rendered } else { View::Raw };
                        }
                        cx.notify();
                    }),
                ));
            }
        }
        let open = path.clone();
        header = header.child(Button::new("open-in-editor").label("Open in editor").ghost().xsmall().on_click(
            cx.listener(move |_, _, _, cx| cx.emit(FilePaneEvent::OpenInEditor(open.clone()))),
        ));
        let banner = if t.deleted {
            Some(div().px_2().text_sm().text_color(theme.warning).child("Deleted on disk. Save writes it back."))
        } else if t.changed_on_disk {
            Some(
                h_flex()
                    .px_2()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.warning)
                    .child("Changed on disk.")
                    .child(Button::new("reload").label("Reload").xsmall().on_click(cx.listener(|p, _, w, cx| p.reload(w, cx))))
                    .child(Button::new("keep").label("Keep mine").xsmall().on_click(cx.listener(|p, _, _, cx| {
                        if let Some(t) = p.tabs.get_mut(p.active) {
                            t.changed_on_disk = false;
                        }
                        cx.notify();
                    }))),
            )
        } else {
            None
        };
        let body = match &t.body {
            Body::CantShow(why) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(muted)
                .child(format!("Can't show this file: {why}"))
                .into_any_element(),
            Body::Text { editor, view: View::Raw, .. } => Editor::new(editor).bordered(false).h_full().into_any_element(),
            Body::Text { editor, view: View::Rendered, .. } => {
                let md = file_doc::defang_remote_images(editor.read(cx).value().as_ref());
                div()
                    .size_full()
                    .p_3()
                    .child(
                        TextView::markdown(SharedString::from(format!("md-{}", path.display())), md)
                            .selectable(true)
                            .scrollable(true),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("FilePane")
            .on_action(cx.listener(Self::save_active))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(FilePaneEvent::Focused)))
            .child(strip)
            .child(header)
            .children(banner)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
```

Add `pub mod file_pane;` to `lib.rs`.

If `Editor::h_full` doesn't exist, use `.h(gpui::relative(1.))`. If `editor.read(cx).value()` isn't on `EditorState`, use the `InputBaseState::value` method (same name; import the trait if one is required). Ledger any such adjustment as a ruling.

- [ ] **Step 4: Run tests and build**

Run: `cargo test -p hydraui --lib ctrl_s_saves` then `cargo build -p hydraui`
Expected: test passes; build succeeds (first build is slow: the tree-sitter grammars compile).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add Cargo.toml Cargo.lock crates/hydraui/src/file_pane.rs crates/hydraui/src/keys.rs crates/hydraui/src/main.rs crates/hydraui/src/lib.rs
git commit -m "feat: hydraui file pane view: file tabs, Raw editor, Rendered Markdown

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The workspace opens, closes, renders and restores file panes

**Files:**
- Modify: `crates/hydraui/src/file_tree_view.rs` (click mapping, hint)
- Modify: `crates/hydraui/src/workspace.rs`
- Modify: `crates/hydraui/src/workspace/sets.rs`
- Modify: `docs/superpowers/specs/2026-10-17-hydraui-design.md` (§7.8 Closing: last terminal)
- Test: `file_tree_view.rs` click mapping as a pure function; smoke checks

**Interfaces:**
- Consumes: everything above: `FilePane` API and `FilePaneEvent`, `TabLayout`, `SavedPane`, `open_files::{terminal_for, last_terminal_closes_tab}`, `Tabs::split_pane`.
- Produces: `TreeEvent::{Show(PathBuf), Insert(PathBuf), Cd(PathBuf)}` (replaces `Open`); `pub fn file_click(ctrl: bool, path: PathBuf) -> TreeEvent`.

- [ ] **Step 1: Write the failing test** (in `file_tree_view.rs`, new `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_shows_a_file_and_ctrl_click_inserts_its_path() {
        let p = PathBuf::from("C:/x/a.md");
        assert_eq!(file_click(false, p.clone()), TreeEvent::Show(p.clone()));
        assert_eq!(file_click(true, p.clone()), TreeEvent::Insert(p));
    }
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p hydraui --lib click_shows_a_file`
Expected: compile error (no `file_click`, no `TreeEvent::Show`).

- [ ] **Step 3: Implement**

`file_tree_view.rs`: replace `Open(PathBuf)` with `Show(PathBuf)` (doc: "Show the file in the tab's file pane."); add

```rust
/// A click on a file: show it; with Ctrl, insert its path (spec §7.3).
pub fn file_click(ctrl: bool, path: PathBuf) -> TreeEvent {
    if ctrl { TreeEvent::Insert(path) } else { TreeEvent::Show(path) }
}
```

and in `click()` replace the `else if ev.modifiers().control { Open } else { Insert }` branch with `else { cx.emit(file_click(ev.modifiers().control, path)); }`. Hint text: `"Click: open · Ctrl+click: insert the path"`.

`workspace.rs`:

1. Fields: `files: HashMap<PaneId, Entity<FilePane>>`, `file_subs: HashMap<PaneId, Subscription>`, `last_terminal: HashMap<TabId, PaneId>`; initialise empty in `new`.

2. `create_pane`'s subscription, on `PaneEvent::Focused`, also records `if let Some(tab) = ws.tabs.tab_of(id) { ws.last_terminal.insert(tab, id); }`.

3. `focused()` uses the rule:

```rust
    /// The terminal standing for the active tab: the focused pane if it's a terminal, else
    /// the tab's last focused terminal (spec §7.8).
    fn focused(&self) -> Option<Entity<PaneView>> {
        let tab = self.tabs.active()?;
        let id = terminal_for(
            tab.focused,
            self.last_terminal.get(&tab.id).copied(),
            &tab.tree.panes(),
            &|p| self.panes.contains_key(&p),
        )?;
        self.panes.get(&id).cloned()
    }
```

4. `focus_pane(id)`: if `self.files.get(&id)` focus its handle (`view.read(cx).focus_handle(cx)`), else the existing terminal path.

5. Creating a file pane:

```rust
    fn create_files_pane(&mut self, id: PaneId, files: &[PathBuf], active: usize, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| FilePane::new(files, active, window, cx));
        let sub = cx.subscribe_in(&view, window, move |ws, _, ev: &FilePaneEvent, window, cx| match ev {
            FilePaneEvent::Focused => {
                ws.tabs.focus_pane(id);
                cx.notify();
            }
            FilePaneEvent::Empty => ws.remove_files_pane(id, window, cx),
            FilePaneEvent::OpenInEditor(path) => ws.open_file(path, window, cx),
        });
        self.file_subs.insert(id, sub);
        self.files.insert(id, view);
    }

    /// The tab's file pane: shows `path` there, splitting the tab's terminal to the right
    /// first when the tab has none (spec §7.8).
    fn show_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.active() else { return };
        let existing = tab.tree.panes().into_iter().find(|p| self.files.contains_key(p));
        let id = match existing {
            Some(id) => {
                self.files[&id].update(cx, |f, cx| f.open(path, window, cx));
                self.tabs.focus_pane(id);
                id
            }
            None => {
                let Some(term) = self.focused().map(|v| v.read(cx).id) else { return };
                self.next_pane += 1;
                let id = self.next_pane;
                self.create_files_pane(id, &[path], 0, window, cx);
                self.tabs.split_pane(term, Axis::Row, id);
                id
            }
        };
        self.focus_pane(id, window, cx);
        self.save_session(window, cx);
        cx.notify();
    }

    fn remove_files_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Closed::Tab(tab) = self.tabs.close_pane(id) {
            self.tab_gone(tab, window, cx);
        }
        self.files.remove(&id);
        self.file_subs.remove(&id);
        self.focus_active(window, cx);
        self.save_session(window, cx);
        cx.notify();
    }
```

6. `on_tree_event`: `TreeEvent::Show(path) => self.show_file(path.clone(), window, cx)`; `Insert` and `Cd` unchanged (they already use `self.focused()`, now the tab's terminal). Remove the `Open` arm.

6b. `split()` splits the tab's terminal, not a focused file pane: replace `self.tabs.split_focused(axis, id);` with `let target = from.read(cx).id; self.tabs.split_pane(target, axis, id);` (`from` is the `self.focused()` terminal it already reads).

7. Closing with unsaved edits. Add:

```rust
    /// Files with unsaved edits among `panes`.
    fn unsaved_in(&self, panes: &[PaneId], cx: &App) -> Vec<PathBuf> {
        panes.iter().filter_map(|p| self.files.get(p)).flat_map(|f| f.read(cx).unsaved()).collect()
    }

    /// Runs `then` now, or after asking Save all / Discard / Cancel when files have unsaved edits.
    fn confirm_unsaved(
        &mut self,
        panes: Vec<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let unsaved = self.unsaved_in(&panes, cx);
        if unsaved.is_empty() {
            then(self, window, cx);
            return;
        }
        let names: Vec<String> = unsaved.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        let ws = cx.entity();
        let then = std::rc::Rc::new(then);
        window.open_dialog(cx, move |dialog, _, _| {
            let (save_ws, discard_ws, panes) = (ws.clone(), ws.clone(), panes.clone());
            let (save_then, discard_then) = (then.clone(), then.clone());
            dialog
                .title("Unsaved edits")
                .child(div().child(format!("Unsaved: {}", names.join(", "))))
                .footer(
                    h_flex().gap_2().justify_end()
                        .child(Button::new("cancel").label("Cancel").on_click(|_, w, cx| w.close_dialog(cx)))
                        .child(Button::new("discard").label("Discard").on_click(move |_, w, cx| {
                            w.close_dialog(cx);
                            discard_ws.update(cx, |ws, cx| discard_then(ws, w, cx));
                        }))
                        .child(Button::new("save").label("Save all").primary().on_click(move |_, w, cx| {
                            w.close_dialog(cx);
                            save_ws.update(cx, |ws, cx| {
                                let mut failed = None;
                                for p in &panes {
                                    if let Some(f) = ws.files.get(p).cloned() {
                                        if let Err(e) = f.update(cx, |f, cx| f.save_all(cx)) {
                                            failed = Some(e);
                                        }
                                    }
                                }
                                match failed {
                                    Some(e) => w.push_notification(Notification::error(e).title("Save"), cx),
                                    None => save_then(ws, w, cx),
                                }
                            });
                        })),
                )
        });
    }
```

`close_pane` (Ctrl+W): with `id = tab.focused` and `panes = tab.tree.panes()`:
- if `self.files.contains_key(&id)` → `self.confirm_unsaved(vec![id], window, cx, move |ws, w, cx| ws.remove_files_pane(id, w, cx))`;
- else if `last_terminal_closes_tab(&panes, id, &|p| self.panes.contains_key(&p))` → `let tab = …id; self.close_tab(tab, window, cx)`;
- else the existing terminal close.

`close_tab(tab)`: `let panes = tab's panes; self.confirm_unsaved(panes, window, cx, move |ws, w, cx| ws.close_tab_now(tab, w, cx))`, where `close_tab_now` is the existing body, with `drop_pane` also removing `files`/`file_subs` entries.

Unloading a set (`sets.rs` `unload`) closes its tabs through `close_tab_now` after its existing running-command question; also run `confirm_unsaved` over the set's panes first (wrap the existing unload continuation).

8. `render_tree` leaf: 

```rust
            Tree::Leaf(id) => {
                if let Some(view) = self.files.get(id) {
                    let edge = if *id == focused { cx.theme().accent } else { transparent_black() };
                    return div().size_full().border_1().border_color(edge).child(view.clone()).into_any_element();
                }
                … existing terminal code …
            }
```

9. Session: `save_session`'s `pane` closure:

```rust
        let pane = |id| {
            if let Some(f) = self.files.get(&id) {
                let (files, active) = f.read(cx).saved();
                return Some(SavedPane::Files { files, active });
            }
            self.panes.get(&id).map(|v| { let v = v.read(cx); SavedPane::Terminal { env: v.env().to_string(), cwd: v.cwd().clone(), shell: v.request.shell } })
        };
```

`open_set` and `restore_session` (replace the Task 4 lines that dropped `files`):

```rust
            let l = tab_layout(&spec_tab, &mut || { *next += 1; *next });
            let focused = l.terminals[0].0;
            for (id, request) in l.terminals {
                self.create_pane(id, request, window, cx);
            }
            if let Some((id, f)) = l.files {
                self.create_files_pane(id, &f.files, f.active, window, cx);
            }
            … add_tree(…, l.tree, focused) …
```

10. Spec, §7.8 **Closing**, append: "Closing a tab's last terminal closes the tab, asking first when its file pane has unsaved edits."

- [ ] **Step 4: Run tests, then smoke-check a debug build**

Run: `cargo test --workspace`
Expected: all pass, including `click_shows_a_file_and_ctrl_click_inserts_its_path`.

Smoke (debug build, temporary `HYDRA_HOME`, the PrintWindow/PostMessage harness; send every key and click from the process that launched hydraui; client coordinates = window coordinates minus the window border and title bar):
1. Open the tree (Ctrl+B), click `README.md` → a file pane splits to the right showing Rendered Markdown. Screenshot.
2. Switch to Raw, type a character, check the tab shows `●`; Ctrl+S → `●` gone and the file on disk changed by that character only (restore it afterwards).
3. Click a `.rs` file → a second file tab, highlighted Rust.
4. Ctrl+click a file → its quoted path appears in the terminal.
5. Close the window and relaunch → the file pane comes back with both files.
6. Edit, then Ctrl+W on the terminal (the tab's last terminal) → the unsaved prompt appears; Cancel keeps everything.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui docs/superpowers/specs/2026-10-17-hydraui-design.md
git commit -m "feat: hydraui opens files in a file pane beside the terminal

Click in the file tree opens the file in the tab's file pane, splitting
the terminal to the right the first time; Ctrl+click inserts the path.
File panes close with an unsaved-edits prompt, render beside terminals,
and come back with the session.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
