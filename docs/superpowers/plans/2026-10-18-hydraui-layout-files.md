# hydraui Layout and Group Files Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Save the whole window to a layout file, a tab or the Loose tabs or a group to a group file, anywhere on disk through the Windows dialogs, and open them again — with groups opened from files surviving a restart.

**Architecture:** hydraui-sets gains the file formats: writable set files (group files), layout files in `session.toml`'s format, group names from file names, set references that are either a name or a path, and `LoadedSet` remembering its file. hydraui gains one snapshot path (the live tabs as `TabFile`s, used by the session, group files and layout files alike) and one open path (`open_layout`, used by restore and by Open layout). The UI adds launcher entries, right-click menus on tabs and group headers, and async Save As / Open dialogs.

**Tech Stack:** Rust 2024, hydraui-sets (serde/toml), GPUI `cx.prompt_for_new_path` / `cx.prompt_for_paths`, gpui-component `menu::{ContextMenuExt, PopupMenu, PopupMenuItem}`.

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` — §7.9 Layout and group files, §5.4 Loaded sets, §5.1 Format.

## Global Constraints

- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`; no employer names.
- Gate before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Tests use `tempfile` dirs only; never the real `~/.hydra`.
- Saving is a snapshot: open tabs don't change (§7.9).
- Group file = set file format (§5.1); layout file = `session.toml` format, suggested name `layout.hydraui.toml` (§7.9).
- Group name from file name: lowercase, other characters become `-`, `-2`, `-3`… on a clash (§5.4).
- Session records sets-folder sets by name and file sets by path; old session files still load (§5.4).
- Writes are atomic (temporary file, then replace); failures are toasts that change nothing (§7.9).
- The dialog bypass for smoke tests exists only under `#[cfg(debug_assertions)]`.
- Windows; write edit scripts with the Write tool (heredocs halve backslashes here).

## Review Focus

1. Opening a layout whose sets are already loaded must not open them twice (by name for the sets folder, by path for files). Pinned in Task 3 (`open_plan_skips_sets_already_loaded`).
2. A group file named `My Layout.toml`, or one whose name collides with a loaded set, must still open, under a valid unique name. Pinned in Task 1 (`group_names_come_from_file_names_and_dont_clash`).
3. Picking a layout file in "Open group file…" (or the reverse) must say which kind it is, not show a TOML error. Pinned in Task 1 (`file_kind_tells_groups_from_layouts`).
4. A set tab saved to a group file must keep its title and startup command, or reopening it loses its purpose. Pinned in Task 2 (`set_tabs_keep_their_title_and_command`).
5. A session written before this change (sets by name only) must restore exactly as before. Pinned in Task 1 (`set_refs_are_names_or_paths`).

---

### Task 1: File formats in hydraui-sets

**Files:**
- Create: `crates/hydraui-sets/src/files.rs`
- Modify: `crates/hydraui-sets/src/file.rs` (SetFile Serialize), `loaded.rs` (LoadedSet.file), `resolve.rs` (load_set fills `file`), `session.rs` (atomic write shared), `lib.rs` (exports)
- Test: in `files.rs`, `loaded.rs`

**Interfaces:**
- Produces:
  - `pub enum SetRef { Named(String), File(PathBuf) }` (`Debug, Clone, PartialEq`) and `pub fn set_ref(entry: &str) -> SetRef` — a valid set name is `Named`, anything else a path.
  - `pub fn group_name(path: &Path, taken: &[String]) -> String`
  - `pub enum FileKind { Group, Layout }` and `pub fn file_kind(text: &str) -> Option<FileKind>`
  - `pub fn load_set_file(ctx: &Context, path: &Path, taken: &[String]) -> Result<SetSpec, SetError>`
  - `pub fn write_set_file(path: &Path, set: &SetFile) -> std::io::Result<()>`
  - `pub fn read_layout(path: &Path) -> Result<Session, String>` and `pub fn write_layout(path: &Path, layout: &Session) -> std::io::Result<()>`
  - `pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()>` (session.rs uses it too)
  - `LoadedSet { name, file: PathBuf, tabs, modified }`; `LoadedSets::changed(&self) -> Vec<String>` (by each set's own file); `LoadedSets::by_file(&self, path: &Path) -> Option<&LoadedSet>`; `LoadedSets::all(&self) -> &[LoadedSet]`.

- [ ] **Step 1: Write the failing tests** (`files.rs`, module with stub fns that `todo!()`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_refs_are_names_or_paths() {
        assert_eq!(set_ref("client-a"), SetRef::Named("client-a".into()));
        assert_eq!(set_ref("C:/work/a.toml"), SetRef::File(PathBuf::from("C:/work/a.toml")));
        assert_eq!(set_ref("C:\\work\\b.toml"), SetRef::File(PathBuf::from("C:\\work\\b.toml")));
        assert_eq!(set_ref("Not A Name"), SetRef::File(PathBuf::from("Not A Name")));
    }

    #[test]
    fn group_names_come_from_file_names_and_dont_clash() {
        assert_eq!(group_name(Path::new("C:/x/My Layout.toml"), &[]), "my-layout");
        assert_eq!(group_name(Path::new("C:/x/api__web!.toml"), &[]), "api-web");
        assert_eq!(group_name(Path::new("C:/x/___.toml"), &[]), "group");
        let taken = vec!["work".to_string(), "work-2".to_string()];
        assert_eq!(group_name(Path::new("C:/x/work.toml"), &taken), "work-3");
        let long = "a".repeat(40);
        assert_eq!(group_name(Path::new(&format!("C:/x/{long}.toml")), &[]).len(), 32);
    }

    #[test]
    fn file_kind_tells_groups_from_layouts() {
        assert_eq!(file_kind("env = \"w\"\n[[tab]]\n"), Some(FileKind::Group));
        assert_eq!(file_kind("sets = [\"a\"]\n"), Some(FileKind::Layout));
        assert_eq!(file_kind("[[loose]]\nenv = \"w\"\n"), Some(FileKind::Layout));
        assert_eq!(file_kind("not = toml = at all"), None);
        assert_eq!(file_kind("other = 1\n"), None);
    }

    #[test]
    fn a_group_file_written_reads_back_the_same() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.toml");
        let set = SetFile {
            env: Some("work".into()),
            tabs: vec![TabFile {
                title: Some("api".into()),
                command: Some("npm run dev".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        write_set_file(&p, &set).unwrap();
        let back = crate::file::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.tabs, set.tabs);
        assert_eq!(back.env, set.env);
    }

    #[test]
    fn a_layout_file_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.hydraui.toml");
        let layout = Session {
            sets: vec!["a".into(), "C:/g/b.toml".into()],
            tree: true,
            ..Default::default()
        };
        write_layout(&p, &layout).unwrap();
        assert_eq!(read_layout(&p).unwrap(), layout);
        std::fs::write(&p, "[[tab]]\n").unwrap();
        assert!(read_layout(&p).unwrap_err().contains("group file"));
    }
}
```

and in `loaded.rs` tests (update the `set()` helper to fill `file: PathBuf::from(format!("C:/s/{name}.toml"))`):

```rust
    #[test]
    fn a_set_is_found_by_its_file() {
        let mut l = LoadedSets::default();
        l.insert(set("a", &[1]));
        assert_eq!(l.by_file(Path::new("C:/s/a.toml")).map(|s| s.name.as_str()), Some("a"));
        assert!(l.by_file(Path::new("C:/s/b.toml")).is_none());
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui-sets`
Expected: compile errors (no `files` module items, `LoadedSet.file`) then, once stubs compile, panics at `todo!()`.

- [ ] **Step 3: Implement**

`file.rs`: `SetFile` derives `Serialize` too; `#[serde(skip_serializing_if = "Option::is_none")]` on `env`, `cwd`, `shell`; `tabs` keeps `#[serde(default, rename = "tab")]`.

`session.rs`: move the temp-file write into `files::write_atomic` and call it from `write_session`.

`files.rs`:

```rust
//! Layout and group files anywhere on disk (spec §7.9): group files are set files, layout
//! files are `session.toml`'s format; and the names and references that tie them to sets.
use std::path::{Path, PathBuf};

use hydra_core::name::EnvName;

use crate::file::SetFile;
use crate::resolve::{Context, SetError, parse_set};
use crate::session::Session;
use crate::spec::SetSpec;

/// A set in the session or a layout: from the sets folder by name, or a group file by path.
#[derive(Debug, Clone, PartialEq)]
pub enum SetRef {
    Named(String),
    File(PathBuf),
}

/// A valid set name can't contain `/`, `\`, `:` or `.`, so anything else is a path.
pub fn set_ref(entry: &str) -> SetRef {
    if EnvName::parse(entry).is_ok() {
        SetRef::Named(entry.to_string())
    } else {
        SetRef::File(PathBuf::from(entry))
    }
}

/// The group header's name for a file: its name made valid, unique among `taken`.
pub fn group_name(path: &Path, taken: &[String]) -> String {
    let stem = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let stem = stem.trim_end_matches(".toml").trim_end_matches(".hydraui");
    let mut base = String::new();
    for c in stem.chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
        if !(c == '-' && (base.is_empty() || base.ends_with('-'))) {
            base.push(c);
        }
    }
    let mut base = base.trim_end_matches('-').to_string();
    base.truncate(32);
    let base = base.trim_end_matches('-').to_string();
    let base = if base.is_empty() { "group".to_string() } else { base };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let mut b = base.clone();
            b.truncate(32 - suffix.len());
            format!("{}{suffix}", b.trim_end_matches('-'))
        })
        .find(|name| !taken.contains(name))
        .unwrap_or(base)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Group,
    Layout,
}

/// Which kind of file `text` is, by its top-level keys; `None` when it is neither (or not TOML).
pub fn file_kind(text: &str) -> Option<FileKind> {
    let doc: toml::Table = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()?;
    if doc.contains_key("tab") {
        return Some(FileKind::Group);
    }
    ["sets", "loose", "active", "tree", "tree_width"]
        .iter()
        .any(|k| doc.contains_key(*k))
        .then_some(FileKind::Layout)
}

/// Opens a group file as a set named after the file.
pub fn load_set_file(ctx: &Context, path: &Path, taken: &[String]) -> Result<SetSpec, SetError> {
    let text = std::fs::read_to_string(path).map_err(|source| SetError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if file_kind(&text) == Some(FileKind::Layout) {
        return Err(SetError::Invalid {
            path: path.to_path_buf(),
            problems: vec!["this is a layout file: use Open layout file…".into()],
        });
    }
    parse_set(ctx, &group_name(path, taken), path, &text)
}

pub fn write_set_file(path: &Path, set: &SetFile) -> std::io::Result<()> {
    write_atomic(path, &toml::to_string(set).map_err(std::io::Error::other)?)
}

pub fn read_layout(path: &Path) -> Result<Session, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    if file_kind(&text) == Some(FileKind::Group) {
        return Err(format!("{} is a group file: use Open group file…", path.display()));
    }
    toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text))
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_layout(path: &Path, layout: &Session) -> std::io::Result<()> {
    write_atomic(path, &toml::to_string(layout).map_err(std::io::Error::other)?)
}

/// Writes through a temporary file beside `path`, then replaces it.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
```

`loaded.rs`: add `pub file: PathBuf` to `LoadedSet`; `changed(&self)` compares `file_modified(&s.file)`; add `by_file` (compare case-insensitively with `/`→`\`) and `all`. Callers of `changed(&paths)` drop the argument.

`resolve.rs` / hydraui `open_set`: `LoadedSet { file: spec.file.clone(), .. }` (spec.file is already the path read).

`lib.rs`: `mod files; pub use files::{FileKind, SetRef, file_kind, group_name, load_set_file, read_layout, set_ref, write_atomic, write_layout, write_set_file};` and export `file::SetFile` is already reachable through `pub mod file`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui-sets` then `cargo build -p hydraui`
Expected: all pass; hydraui builds after its `changed()` call and `LoadedSet` literal are updated.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates
git commit -m "feat: layout and group file formats in hydraui-sets"
```

---

### Task 2: Saved tabs keep titles and startup commands

**Files:**
- Modify: `crates/hydraui/src/layout.rs`, `crates/hydraui/src/workspace/sets.rs` (caller)
- Test: `layout.rs`

**Interfaces:**
- Consumes: `SavedPane` (layout.rs).
- Produces: `SavedPane::Terminal { env, cwd, shell, command: Option<String> }`; `pub fn tab_file(tree: &Tree, title: Option<&str>, pane: &impl Fn(PaneId) -> Option<SavedPane>) -> Option<TabFile>` — title goes on the tab; a one-pane tab's command goes on the tab, a split's on its pane nodes.

- [ ] **Step 1: Write the failing test** (append; update existing calls to `tab_file(&t, None, &f)` and `SavedPane::Terminal { …, command: None }`)

```rust
    #[test]
    fn set_tabs_keep_their_title_and_command() {
        let pane = |id| {
            Some(SavedPane::Terminal {
                env: "work".into(),
                cwd: PathBuf::from(format!("C:/w/{id}")),
                shell: ShellKind::Pwsh,
                command: (id == 2).then(|| "npm run dev".to_string()),
            })
        };
        let single = tab_file(&Tree::Leaf(2), Some("api"), &pane).unwrap();
        assert_eq!(single.title.as_deref(), Some("api"));
        assert_eq!(single.command.as_deref(), Some("npm run dev"));
        let split = Tree::Split {
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(Tree::Leaf(1)),
            second: Box::new(Tree::Leaf(2)),
        };
        let tab = tab_file(&split, Some("web"), &pane).unwrap();
        assert_eq!(tab.title.as_deref(), Some("web"));
        let panes = tab.layout.unwrap().panes.unwrap();
        assert_eq!(panes[0].command, None);
        assert_eq!(panes[1].command.as_deref(), Some("npm run dev"));
    }
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p hydraui --lib layout`
Expected: compile errors (`command` field, `tab_file` arity).

- [ ] **Step 3: Implement**

In `tab_file`, set `title: title.map(str::to_string)` on the returned `TabFile` (both arms), and `command` from the `Terminal` variant: on the tab for a root leaf, on the `NodeFile` for leaves inside splits. `save_session`'s pane closure fills `command: None` for Loose tabs (a Loose tab's shell has no startup command to keep; `v.request.command` is `None` for them anyway — use `v.request.command.clone()` so the same closure serves Task 3's groups).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui --lib layout`
Expected: all layout tests pass.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: saved hydraui tabs keep their title and startup command"
```

---

### Task 3: One snapshot path and one open path in the workspace

**Files:**
- Create: `crates/hydraui/src/layout_plan.rs` (pure: what to open from a layout, session entries)
- Modify: `crates/hydraui/src/workspace/sets.rs`, `crates/hydraui/src/lib.rs`
- Test: `layout_plan.rs`

**Interfaces:**
- Consumes: Task 1 (`SetRef`, `set_ref`, `load_set_file`, `LoadedSets::by_file/all`, `LoadedSet.file`), Task 2 (`tab_file` with title).
- Produces:
  - `pub fn session_entry(name: &str, file: &Path, sets_dir: &Path) -> String` — the name when `file` is `<sets_dir>/<name>.toml`, else the path.
  - `pub fn open_plan(entries: &[String], loaded_names: &[String], loaded_files: &[PathBuf]) -> Vec<SetRef>` — the layout's sets not loaded yet, in order.
  - Workspace (sets.rs): `fn snapshot_tab(&self, tab: &Tab, cx: &App) -> Option<TabFile>`; `fn group_file_of(&self, tabs: &[TabId], cx: &App) -> SetFile`; `fn current_layout(&self, cx: &App) -> Session`; `fn open_layout(&mut self, layout: Session, window, cx) -> bool`; `fn open_group_file(&mut self, path: &Path, window, cx) -> bool`.

- [ ] **Step 1: Write the failing tests** (`layout_plan.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_folder_sets_are_saved_by_name_others_by_path() {
        let dir = Path::new("C:/h/hydraui/sets");
        assert_eq!(session_entry("a", Path::new("C:/h/hydraui/sets/a.toml"), dir), "a");
        assert_eq!(
            session_entry("a", Path::new("C:/repo/a.toml"), dir),
            "C:/repo/a.toml"
        );
    }

    #[test]
    fn open_plan_skips_sets_already_loaded() {
        let entries = vec!["a".to_string(), "C:/g/b.toml".to_string(), "c".to_string()];
        let plan = open_plan(&entries, &["a".to_string()], &[PathBuf::from("c:\\g\\B.toml")]);
        assert_eq!(plan, vec![SetRef::Named("c".into())]);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui --lib layout_plan`
Expected: compile errors (module missing).

- [ ] **Step 3: Implement**

`layout_plan.rs`:

```rust
//! What opening a layout opens, and how the session names a loaded set (spec §5.4, §7.9).
use std::path::{Path, PathBuf};

use hydraui_sets::{SetRef, set_ref};

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

pub fn session_entry(name: &str, file: &Path, sets_dir: &Path) -> String {
    if key(file) == key(&sets_dir.join(format!("{name}.toml"))) {
        name.to_string()
    } else {
        file.to_string_lossy().into_owned()
    }
}

pub fn open_plan(entries: &[String], loaded_names: &[String], loaded_files: &[PathBuf]) -> Vec<SetRef> {
    entries
        .iter()
        .map(|e| set_ref(e))
        .filter(|r| match r {
            SetRef::Named(n) => !loaded_names.contains(n),
            SetRef::File(p) => !loaded_files.iter().any(|f| key(f) == key(p)),
        })
        .collect()
}
```

`sets.rs`:
- `snapshot_tab(tab)`: the pane closure from `save_session` (terminals → `SavedPane::Terminal` with `command: v.request.command.clone()`, file panes → `SavedPane::Files`), then `tab_file(&tab.tree, tab.title.as_deref(), &pane)`.
- `current_layout()`: `Session { sets: loaded.all().iter().map(|s| session_entry(&s.name, &s.file, &sets_dir(&self.hydra.paths))).collect(), loose: Loose tabs via snapshot_tab, active, tree, tree_width }` — the body of `save_session`, which now writes `current_layout()`.
- `group_file_of(tabs)`: `SetFile { tabs: tabs.iter().filter_map(|t| self.tabs.get(*t)).filter_map(|t| self.snapshot_tab(t, cx)).collect(), ..Default::default() }` (every pane carries its own env/cwd/shell, so no set-level defaults are needed).
- `open_group_file(path)`: if `loaded.by_file(path)` focus its first tab and return true; else `load_set_file(&self.set_context(), path, &self.loaded.names())` → `open_set(spec)`; errors → toast `Notification::error(e.to_string()).title("Open group")`.
- `open_layout(layout)`: for each `open_plan(&layout.sets, names, files)` entry: `Named(n)` → `load_set(n)`, `File(p)` → `open_group_file(p)`; record the tab ids each opened, in order; then the Loose-tab loop from `restore_session` (moved here) over `layout.loose`, recording ids; tree as in `restore_session`; activate `opened.get(layout.active)` when present. Returns whether anything opened.
- `restore_session()` becomes `let any = self.open_layout(read_session(&self.hydra.paths), window, cx); self.save_session(window, cx); any`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui --lib` and `cargo build -p hydraui`
Expected: all pass, builds.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: hydraui snapshots and opens layouts through one path"
```

---

### Task 4: Launcher entries, right-click menus and file dialogs

**Files:**
- Modify: `crates/hydraui/src/launcher.rs` (Action variants, entries), `crates/hydraui/src/workspace/launcher.rs` (dispatch), `crates/hydraui/src/workspace/sets.rs` (save/open with dialogs, header menu), `crates/hydraui/src/workspace.rs` (tab menu)
- Test: `launcher.rs` entries test

**Interfaces:**
- Consumes: Task 3 workspace methods, Task 1 writers/readers.
- Produces: `Action::{SaveLayout, OpenLayout, OpenGroupFile, SaveLooseAsGroup}`; workspace `fn save_dialog(&mut self, dir: PathBuf, name: String, window, cx, then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static)` and `fn open_dialog_path(&mut self, window, cx, then: …)`.

- [ ] **Step 1: Write the failing test** — extend both launcher entries tests: the fixed tail becomes `["Open layout file…", "Save layout to file…", "Open group file…", "Save Loose tabs as group…", "New environment…", "Set secret: other…", "Open config folder", "Reload config"]` with the matching `Action`s.

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p hydraui --lib launcher`
Expected: FAIL (labels missing) / compile error (variants missing).

- [ ] **Step 3: Implement**

Dialogs (workspace/sets.rs):

```rust
    /// Asks where to save (the Save As dialog), then calls `then` with the path. Cancel does
    /// nothing. Debug builds take `HYDRAUI_TEST_SAVE_PATH` instead, for smoke tests.
    fn save_dialog(
        &mut self,
        dir: PathBuf,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("HYDRAUI_TEST_SAVE_PATH") {
            then(self, PathBuf::from(p), window, cx);
            return;
        }
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update_in(cx, |ws, window, cx| then(ws, path, window, cx));
            }
        })
        .detach();
    }

    /// Asks which file to open (the Open dialog), then calls `then` with it. Cancel does
    /// nothing. Debug builds take `HYDRAUI_TEST_OPEN_PATH` instead, for smoke tests.
    fn open_dialog_path(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("HYDRAUI_TEST_OPEN_PATH") {
            then(self, PathBuf::from(p), window, cx);
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |ws, window, cx| then(ws, path, window, cx));
            }
        })
        .detach();
    }
```

Commands:
- `SaveLayout`: `save_dialog(home, "layout.hydraui.toml")` → `write_layout(&path, &ws.current_layout(cx))`, success toast "Saved layout to …", error toast.
- `OpenLayout`: `open_dialog_path` → `read_layout(&path)` → `open_layout(layout)`, error toast; then `save_session`.
- `OpenGroupFile`: `open_dialog_path` → `open_group_file(&path)`; then `save_session`.
- `SaveLooseAsGroup`: Loose tab ids; none → info toast "No Loose tabs to save"; else `save_dialog(home, "group.toml")` → `write_set_file(&path, &ws.group_file_of(&ids, cx))`.
- Tab right-click (workspace.rs tab element): `.context_menu(move |menu, _, _| menu.item(PopupMenuItem::new("Save tab as group…").on_click(…save tab…)).item(PopupMenuItem::new("Close tab").on_click(…close_tab…)))`; save tab suggests `<label>.toml`.
- Group header right-click (`render_set_header`): "Save group…" → `save_dialog(file.parent(), file.file_name())` → `write_set_file(&path, &ws.group_file_of(&set.tabs, cx))`; "Unload group" → `unload_set(name, false)`.
- Write errors toast `"can't save {path}: {e}"`; nothing else changes.

- [ ] **Step 4: Run tests, then smoke**

Run: `cargo test --workspace`
Expected: pass.

Smoke (debug build; temp `HYDRA_HOME`; launch with `HYDRAUI_TEST_SAVE_PATH=<scratch>\g.toml` and `HYDRAUI_TEST_OPEN_PATH=<scratch>\g.toml`; send events from the launching process; hover before clicking):
1. Right-click a Loose tab → Save tab as group… → `g.toml` exists and parses as a group.
2. Launcher → Open group file… → a `g` group header appears with that tab.
3. Close the window, relaunch → the `g` group is back (session lists the path).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: hydraui saves and opens layout and group files"
```
