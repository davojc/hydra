# hydraui Layouts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each hydraui window is untitled or backed by one `.hydraui` layout file that it keeps saved; startup shows a start page of recent layouts; one window per file across processes; the title shows the layout's name; the installer registers `.hydraui`.

**Architecture:** hydraui-sets gains the file logic (layout names, `.toml` → `.hydraui` conversion, the one-time `session.toml` migration, the shared recent list). hydraui gains `claim.rs` (a named mutex per layout path plus `running/<pid>.toml` records to find the window that holds it), a start page model and view, and a Workspace that knows its backing file: `save_session` becomes `autosave` (writes only the window's own file), Open/Save As go through one `open_layout_path` / `save_layout_to` pair, and the title bar shows the name with a menu. `main.rs` takes an optional file argument. hydra-setup writes the file association.

**Tech Stack:** Rust 2024, serde/toml, sha2 (already in the workspace), windows-sys (`CreateMutexW`, `OpenMutexW`, `OpenProcess`, `SetForegroundWindow`), raw-window-handle 0.6 (the window's HWND), gpui-component `menu::DropdownMenu`, `input::{InputState, Input}`, the `windows` crate in hydra-setup (registry, `SHChangeNotify`).

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` — §7.12 Layouts and the start page, §7.11 (Layout name, Restart), §7.9 (layout files), §5.4 (layouts), §7.7 (Layout files in the installer).

## Global Constraints

- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`.
- Gate before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (check its exit status), `cargo test --workspace`.
- Tests use `tempfile` dirs only; never the real `~/.hydra`, never the real HKCU file associations (hydra-setup tests use `FakeSystem`).
- Layout files are `*.hydraui`, TOML inside, `Session`'s format (§7.9). Save As starts in `~/.hydra/hydraui/layouts/` (created on first use) or the current file's folder.
- Recent list: `~/.hydra/hydraui/recent.toml`, at most 12, newest first, read-modify-replace atomically on every change, paths compared case-insensitively with `/` and `\` alike (§7.12).
- Mutex name: `Local\hydraui-layout-<hex sha256 of the full path, lowercase, \ separators>` (§7.12).
- Window title: `hydraui - <layout>` / `hydraui - untitled`; `hydraui` on the start page (§7.11).
- Writes are atomic (`write_atomic`); a failed write is a toast and changes nothing.
- The dialog bypasses (`HYDRAUI_TEST_SAVE_PATH`, `HYDRAUI_TEST_OPEN_PATH`) stay `#[cfg(debug_assertions)]` only.
- Windows; write edit scripts with the Write/Edit tools (shell heredocs halve backslashes here).

## Review Focus

1. **Restart after an update must release the claim before starting the new hydraui**, or the new process sees the file claimed, brings the old (closing) window forward and exits — the user loses the window. Pinned in Task 2 (`a_dropped_claim_frees_the_file_at_once`) and enforced in Task 3's `restart`.
2. **A stale `running/<pid>.toml` whose pid was reused** by an unrelated process must not bring a stranger's window forward: the record's window must exist and belong to that pid. Pinned in Task 2 (`records_of_dead_processes_are_ignored_and_removed`).
3. **Converting `client-a.hydraui.toml` when `client-a.hydraui` already exists** must not overwrite or delete anything. Pinned in Task 1 (`conversion_never_overwrites`).
4. **Two hydrauis updating `recent.toml` in turn** must keep both entries (each change rereads the file). Pinned in Task 1 (`each_change_rereads_the_list`).
5. **`C:/Work/A.hydraui` and `c:\work\a.hydraui` are the same layout** for the recent list and for the claim. Pinned in Task 1 (`paths_match_ignoring_case_and_slashes`) and Task 2 (`the_mutex_name_ignores_case_and_slashes`).

---

### Task 1: Layout files and the recent list (hydraui-sets)

**Files:**
- Create: `crates/hydraui-sets/src/layouts.rs`, `crates/hydraui-sets/src/recent.rs`
- Modify: `crates/hydraui-sets/src/lib.rs` (mods and exports), `crates/hydraui-sets/src/session.rs` (doc comment: now only the migration reads it; `write_session` stays for tests of the migration)
- Test: in `layouts.rs`, `recent.rs`

**Interfaces:**
- Consumes: `Session`, `read_session`, `session_file`, `files::{read_layout, write_layout, write_atomic}` (existing).
- Produces (all re-exported from `hydraui_sets`):
  - `pub const LAYOUT_EXT: &str = "hydraui";`
  - `pub fn layouts_dir(paths: &HydraPaths) -> PathBuf` — `<root>/hydraui/layouts`
  - `pub fn layout_name(path: &Path) -> String` — the file name without `.hydraui`, `.hydraui.toml` or `.toml` (case-insensitive)
  - `pub fn converted_path(path: &Path) -> PathBuf` — `<dir>/<layout_name>.hydraui`
  - `pub fn free_path(dir: &Path, stem: &str) -> PathBuf` — `stem.hydraui`, else `stem-2.hydraui`, `stem-3.hydraui`…
  - `pub enum Opened { Current(Session), Converted { layout: Session, from: PathBuf, to: PathBuf }, Untitled { layout: Session, exists: PathBuf } }` (`Debug, PartialEq`)
  - `pub fn open_layout_file(path: &Path) -> Result<Opened, String>`
  - `pub fn migrate_session(paths: &HydraPaths) -> Option<PathBuf>`
  - `pub struct RecentEntry { pub path: PathBuf, pub opened: u64 }` (`Debug, Clone, PartialEq, Serialize, Deserialize`; `opened` = Unix seconds)
  - `pub const MAX_RECENT: usize = 12;`
  - `pub fn same_path(a: &Path, b: &Path) -> bool`
  - `pub fn read_recent(paths: &HydraPaths) -> Vec<RecentEntry>`
  - `pub fn touch_recent(paths: &HydraPaths, path: &Path, now: u64) -> std::io::Result<()>`
  - `pub fn forget_recent(paths: &HydraPaths, path: &Path) -> std::io::Result<()>`
  - `pub fn rename_recent(paths: &HydraPaths, from: &Path, to: &Path, now: u64) -> std::io::Result<()>`

Ruling carried from the spec: §7.12 names `*.hydraui.toml`; any layout file whose extension isn't `.hydraui` (a hand-named `work.toml` too) is converted the same way, since the user asked for existing `.toml` layouts to become `.hydraui`.

- [ ] **Step 1: Write the failing tests** — create both modules with the signatures above bodied `todo!()`, register them in `lib.rs`, and add these tests.

`layouts.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::TabFile;
    use crate::session::{session_file, write_session};

    fn a_layout() -> Session {
        Session {
            sets: vec!["client-a".into()],
            loose: vec![TabFile {
                env: Some("work".into()),
                cwd: Some("C:/w".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn names_drop_every_layout_extension() {
        assert_eq!(layout_name(Path::new(r"C:\l\client-a.hydraui")), "client-a");
        assert_eq!(layout_name(Path::new(r"C:\l\client-a.HydraUI.TOML")), "client-a");
        assert_eq!(layout_name(Path::new(r"C:\l\work.toml")), "work");
        assert_eq!(
            converted_path(Path::new(r"C:\l\client-a.hydraui.toml")),
            PathBuf::from(r"C:\l\client-a.hydraui")
        );
    }

    #[test]
    fn free_paths_count_up() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(free_path(d.path(), "untitled"), d.path().join("untitled.hydraui"));
        std::fs::write(d.path().join("untitled.hydraui"), "").unwrap();
        std::fs::write(d.path().join("untitled-2.hydraui"), "").unwrap();
        assert_eq!(free_path(d.path(), "untitled"), d.path().join("untitled-3.hydraui"));
    }

    #[test]
    fn a_hydraui_file_opens_as_it_is() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.hydraui");
        write_layout(&p, &a_layout()).unwrap();
        assert_eq!(open_layout_file(&p).unwrap(), Opened::Current(a_layout()));
    }

    #[test]
    fn an_old_file_becomes_a_hydraui_file() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("a.hydraui.toml");
        write_layout(&old, &a_layout()).unwrap();
        let new = d.path().join("a.hydraui");
        assert_eq!(
            open_layout_file(&old).unwrap(),
            Opened::Converted { layout: a_layout(), from: old.clone(), to: new.clone() }
        );
        assert!(!old.exists(), "the old file is deleted");
        assert_eq!(read_layout(&new).unwrap(), a_layout());
    }

    #[test]
    fn conversion_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("a.hydraui.toml");
        write_layout(&old, &a_layout()).unwrap();
        let new = d.path().join("a.hydraui");
        std::fs::write(&new, "sets = [\"other\"]\n").unwrap();
        assert_eq!(
            open_layout_file(&old).unwrap(),
            Opened::Untitled { layout: a_layout(), exists: new.clone() }
        );
        assert!(old.exists(), "nothing deleted");
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "sets = [\"other\"]\n");
    }

    #[test]
    fn a_bad_file_is_an_error_naming_it() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.hydraui");
        std::fs::write(&p, "sets = 3\n").unwrap();
        assert!(open_layout_file(&p).unwrap_err().contains("bad.hydraui"));
        assert!(open_layout_file(&d.path().join("gone.hydraui")).is_err());
    }

    #[test]
    fn the_session_moves_into_the_layouts_folder_once() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        write_session(&paths, &a_layout()).unwrap();
        let moved = migrate_session(&paths).unwrap();
        assert_eq!(moved, layouts_dir(&paths).join("last-session.hydraui"));
        assert_eq!(read_layout(&moved).unwrap(), a_layout());
        assert!(!session_file(&paths).exists());
        assert_eq!(migrate_session(&paths), None, "only once");
    }

    #[test]
    fn an_empty_or_broken_session_is_just_deleted() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        for text in ["", "sets = 3\n"] {
            std::fs::create_dir_all(session_file(&paths).parent().unwrap()).unwrap();
            std::fs::write(session_file(&paths), text).unwrap();
            assert_eq!(migrate_session(&paths), None, "{text:?}");
            assert!(!session_file(&paths).exists(), "{text:?}");
        }
        assert!(!layouts_dir(&paths).exists(), "nothing written");
    }

    #[test]
    fn the_migrated_session_takes_a_free_name() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        std::fs::create_dir_all(layouts_dir(&paths)).unwrap();
        std::fs::write(layouts_dir(&paths).join("last-session.hydraui"), "").unwrap();
        write_session(&paths, &a_layout()).unwrap();
        assert_eq!(
            migrate_session(&paths).unwrap(),
            layouts_dir(&paths).join("last-session-2.hydraui")
        );
    }
}
```

`recent.rs`:

```rust
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
        assert_eq!(got, [(r"C:\l\a.hydraui".into(), 30), (r"C:\l\b.hydraui".into(), 20)]);
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
        assert!(same_path(Path::new("C:/Work/A.hydraui"), Path::new(r"c:\work\a.hydraui")));
        assert!(!same_path(Path::new(r"C:\a.hydraui"), Path::new(r"C:\b.hydraui")));
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
        rename_recent(&paths, Path::new(r"C:\l\a.hydraui.toml"), Path::new(r"C:\l\a.hydraui"), 3)
            .unwrap();
        forget_recent(&paths, Path::new(r"C:\l\b.hydraui")).unwrap();
        let list = read_recent(&paths);
        assert_eq!(list, [RecentEntry { path: r"C:\l\a.hydraui".into(), opened: 3 }]);
    }

    #[test]
    fn an_unreadable_list_counts_as_empty() {
        let (_d, paths) = home();
        std::fs::create_dir_all(paths.root().join("hydraui")).unwrap();
        std::fs::write(paths.root().join("hydraui").join("recent.toml"), "\u{0}junk").unwrap();
        assert!(read_recent(&paths).is_empty());
        touch_recent(&paths, Path::new(r"C:\l\a.hydraui"), 1).unwrap();
        assert_eq!(read_recent(&paths).len(), 1);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui-sets layouts recent`
Expected: FAIL — the `todo!()` panics ("not yet implemented") in every new test.

- [ ] **Step 3: Implement `layouts.rs`**

```rust
//! Layout files (spec §7.12): `*.hydraui`, TOML inside in `Session`'s format; older
//! `.toml` layouts converted when opened; the one-time move of `session.toml`.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;

use crate::files::{read_layout, write_layout};
use crate::session::{Session, read_session, session_file};

pub const LAYOUT_EXT: &str = "hydraui";

pub fn layouts_dir(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("layouts")
}

/// The file name without `.hydraui`, `.hydraui.toml` or `.toml`, any case.
pub fn layout_name(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_lowercase();
    for ext in [".hydraui.toml", ".hydraui", ".toml"] {
        if lower.ends_with(ext) && lower.len() > ext.len() {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name
}

fn is_current(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(LAYOUT_EXT))
}

pub fn converted_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.{LAYOUT_EXT}", layout_name(path)))
}

pub fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{LAYOUT_EXT}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.{LAYOUT_EXT}")))
        .find(|p| !p.exists())
        .expect("a free name")
}

#[derive(Debug, PartialEq)]
pub enum Opened {
    /// A `.hydraui` file.
    Current(Session),
    /// An older layout, now written to `to`; `from` is deleted.
    Converted { layout: Session, from: PathBuf, to: PathBuf },
    /// An older layout whose `.hydraui` name is taken: opened untitled, nothing touched.
    Untitled { layout: Session, exists: PathBuf },
}

/// Reads a layout file, converting an older one to `.hydraui` beside it.
pub fn open_layout_file(path: &Path) -> Result<Opened, String> {
    let layout = read_layout(path)?;
    if is_current(path) {
        return Ok(Opened::Current(layout));
    }
    let to = converted_path(path);
    if to.exists() {
        return Ok(Opened::Untitled { layout, exists: to });
    }
    write_layout(&to, &layout).map_err(|e| format!("can't write {}: {e}", to.display()))?;
    // The new file holds everything; an old file that can't be deleted is only clutter.
    let _ = std::fs::remove_file(path);
    Ok(Opened::Converted { layout, from: path.to_path_buf(), to })
}

/// Moves `session.toml` (from before layouts were files) into the layouts folder, once.
/// Returns the new file; an empty or unreadable session is deleted and gives `None`.
pub fn migrate_session(paths: &HydraPaths) -> Option<PathBuf> {
    let file = session_file(paths);
    if !file.exists() {
        return None;
    }
    let session = read_session(paths);
    let target = (session != Session::default()).then(|| {
        let to = free_path(&layouts_dir(paths), "last-session");
        write_layout(&to, &session).ok().map(|()| to)
    });
    match target {
        // Couldn't write: keep session.toml for the next start.
        Some(None) => None,
        Some(Some(to)) => {
            let _ = std::fs::remove_file(&file);
            Some(to)
        }
        None => {
            let _ = std::fs::remove_file(&file);
            None
        }
    }
}
```

- [ ] **Step 4: Implement `recent.rs`**

```rust
//! `<HYDRA_HOME>/hydraui/recent.toml` (spec §7.12): recently opened layouts, newest first,
//! shared by every hydraui. Each change rereads the file, so windows don't lose each
//! other's entries.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};

use crate::files::write_atomic;

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

pub fn same_path(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

pub fn read_recent(paths: &HydraPaths) -> Vec<RecentEntry> {
    std::fs::read_to_string(recent_file(paths))
        .ok()
        .and_then(|t| toml::from_str::<RecentFile>(&t).ok())
        .map(|f| f.layout)
        .unwrap_or_default()
}

fn change(
    paths: &HydraPaths,
    edit: impl FnOnce(&mut Vec<RecentEntry>),
) -> std::io::Result<()> {
    let mut list = read_recent(paths);
    edit(&mut list);
    list.truncate(MAX_RECENT);
    let text = toml::to_string(&RecentFile { layout: list }).map_err(std::io::Error::other)?;
    write_atomic(&recent_file(paths), &text)
}

pub fn touch_recent(paths: &HydraPaths, path: &Path, now: u64) -> std::io::Result<()> {
    change(paths, |list| {
        list.retain(|e| !same_path(&e.path, path));
        list.insert(0, RecentEntry { path: path.to_path_buf(), opened: now });
    })
}

pub fn forget_recent(paths: &HydraPaths, path: &Path) -> std::io::Result<()> {
    change(paths, |list| list.retain(|e| !same_path(&e.path, path)))
}

pub fn rename_recent(paths: &HydraPaths, from: &Path, to: &Path, now: u64) -> std::io::Result<()> {
    change(paths, |list| {
        list.retain(|e| !same_path(&e.path, from) && !same_path(&e.path, to));
        list.insert(0, RecentEntry { path: to.to_path_buf(), opened: now });
    })
}
```

In `lib.rs`: `pub mod layouts; pub mod recent;` and
`pub use layouts::{LAYOUT_EXT, Opened, converted_path, free_path, layout_name, layouts_dir, migrate_session, open_layout_file};`
`pub use recent::{MAX_RECENT, RecentEntry, forget_recent, read_recent, rename_recent, same_path, touch_recent};`

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hydraui-sets`
Expected: PASS, all tests including the 15 new ones.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui-sets
git commit -m "feat: hydraui-sets reads .hydraui layouts, converts old ones, keeps a recent list"
```

---

### Task 2: One window per layout file (`claim.rs`)

**Files:**
- Create: `crates/hydraui/src/claim.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod claim;`), `crates/hydraui/Cargo.toml` (`sha2.workspace = true`, `raw-window-handle = "0.6"`, `toml.workspace = true` if not already there, `serde.workspace = true` if not already there), root `Cargo.toml` windows-sys features (add `Win32_System_Threading` is present; nothing else needed — `OpenProcess`/`GetExitCodeProcess`/`CreateMutexW`/`OpenMutexW` are in Threading, `IsWindow`/`GetWindowThreadProcessId`/`SetForegroundWindow`/`ShowWindow`/`IsIconic` in WindowsAndMessaging)
- Test: in `claim.rs`

**Interfaces:**
- Consumes: `hydraui_sets::same_path`.
- Produces:
  - `pub fn running_dir(paths: &HydraPaths) -> PathBuf` — `<root>/hydraui/running`
  - `pub fn mutex_name(path: &Path) -> String`
  - `pub fn record_path(running: &Path, layout: &Path) -> PathBuf` — this process's record for that layout
  - `pub struct Claim` (holds the mutex handle; `Drop` closes it and deletes its record); `impl Claim { pub fn path(&self) -> &Path }`
  - `pub enum Taken { Mine(Claim), Elsewhere(Option<isize>) }` — `Elsewhere` carries the holder's window when its record is found
  - `pub fn take(running: &Path, layout: &Path, hwnd: isize) -> Taken`
  - `pub fn is_claimed(layout: &Path) -> bool`
  - `pub fn holder_window(running: &Path, layout: &Path) -> Option<isize>`
  - `pub fn bring_forward(hwnd: isize)`
  - `pub fn hwnd_of(window: &gpui::Window) -> isize` (0 when unknown)

Ruling: §7.12 names the record `running/<pid>.toml`; it is `running/<pid>-<id>.toml` (one per claim), because Save layout as… holds the new claim before the old one drops, and a shared per-pid file would be deleted by the old claim. The spec says so already (amended with this plan).

- [ ] **Step 1: Write the failing tests** — create `claim.rs` with the signatures above bodied `todo!()` (and `struct Claim { handle: isize, path: PathBuf, record: PathBuf }`), plus:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A layout path no other test (or real hydraui) uses, so mutex names don't collide.
    fn unique(d: &tempfile::TempDir) -> PathBuf {
        d.path().join("t.hydraui")
    }

    #[test]
    fn the_mutex_name_ignores_case_and_slashes() {
        let a = mutex_name(Path::new("C:/Work/A.hydraui"));
        assert_eq!(a, mutex_name(Path::new(r"c:\work\a.hydraui")));
        assert!(a.starts_with(r"Local\hydraui-layout-"), "{a}");
        assert_eq!(a.len(), r"Local\hydraui-layout-".len() + 64, "{a}");
        assert_ne!(a, mutex_name(Path::new(r"c:\work\b.hydraui")));
    }

    #[test]
    fn a_second_claim_finds_the_first_window() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("running");
        let layout = unique(&d);
        // This test's own process stands in for the holder, so its pid is alive; the window
        // check needs a real window, which a test has none of, so the holder's window is
        // whatever `holder_window` accepts: `None` here (hwnd 0 isn't a window).
        let Taken::Mine(first) = take(&running, &layout, 0) else { panic!("first claim") };
        assert_eq!(first.path(), layout.as_path());
        assert!(is_claimed(&layout));
        assert!(matches!(take(&running, &layout, 0), Taken::Elsewhere(None)));
        drop(first);
    }

    #[test]
    fn a_dropped_claim_frees_the_file_at_once() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("running");
        let layout = unique(&d);
        let Taken::Mine(c) = take(&running, &layout, 0) else { panic!() };
        let record = record_path(&running, &layout);
        assert!(record.exists(), "the running record is written");
        drop(c);
        assert!(!is_claimed(&layout));
        assert!(!record.exists(), "and removed");
        assert!(matches!(take(&running, &layout, 0), Taken::Mine(_)));
    }

    #[test]
    fn records_of_dead_processes_are_ignored_and_removed() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("running");
        std::fs::create_dir_all(&running).unwrap();
        let layout = unique(&d);
        // A pid that isn't running (Windows pids are multiples of 4).
        let dead = running.join("4294967291-0123456789abcdef.toml");
        std::fs::write(
            &dead,
            format!("layout = {:?}\nwindow = 1234\n", layout.display().to_string()),
        )
        .unwrap();
        assert_eq!(holder_window(&running, &layout), None);
        assert!(!dead.exists());
        // A live pid whose recorded window isn't its own (pid reuse) is ignored too.
        let me = record_path(&running, &layout);
        std::fs::write(
            &me,
            format!("layout = {:?}\nwindow = 1234\n", layout.display().to_string()),
        )
        .unwrap();
        assert_eq!(holder_window(&running, &layout), None);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui claim`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement `claim.rs`**

```rust
//! One window per layout file, across hydraui processes (spec §7.12): a named mutex per
//! layout path says it is taken, and `running/<pid>.toml` says which window has it, so a
//! second open can bring that window forward. Windows closes the mutex if hydraui crashes.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetExitCodeProcess, OpenMutexW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, IsIconic, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

const STILL_ACTIVE: u32 = 259;
const SYNCHRONIZE: u32 = 0x0010_0000;

pub fn running_dir(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("running")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn mutex_name(path: &Path) -> String {
    let key = path.to_string_lossy().replace('/', "\\").to_lowercase();
    format!(r"Local\hydraui-layout-{:x}", Sha256::digest(key.as_bytes()))
}

/// `<pid>-<first 16 hex of the mutex hash>.toml`: one per claim, so a window switching
/// files (Save layout as…) never deletes its new record when the old claim drops.
pub fn record_path(running: &Path, layout: &Path) -> PathBuf {
    let name = mutex_name(layout);
    let id = &name[name.len() - 64..][..16];
    running.join(format!("{}-{id}.toml", std::process::id()))
}

#[derive(Serialize, Deserialize)]
struct Record {
    layout: PathBuf,
    window: isize,
}

/// This window holds a layout file; dropping it lets go.
pub struct Claim {
    handle: isize,
    path: PathBuf,
    record: PathBuf,
}

impl Claim {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // SAFETY: a handle CreateMutexW returned, closed once.
        unsafe { CloseHandle(self.handle as _) };
        let _ = std::fs::remove_file(&self.record);
    }
}

pub enum Taken {
    Mine(Claim),
    /// Another window has it; its window handle when its record says.
    Elsewhere(Option<isize>),
}

pub fn take(running: &Path, layout: &Path, hwnd: isize) -> Taken {
    let name = wide(&mutex_name(layout));
    // SAFETY: a NUL-terminated name; no security attributes.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: read straight after the call it describes.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if handle.is_null() || existed {
        if !handle.is_null() {
            // SAFETY: our handle to the existing mutex; we don't keep it.
            unsafe { CloseHandle(handle) };
        }
        return Taken::Elsewhere(holder_window(running, layout));
    }
    let record = record_path(running, layout);
    let text = toml::to_string(&Record { layout: layout.to_path_buf(), window: hwnd })
        .unwrap_or_default();
    let _ = std::fs::create_dir_all(running).and_then(|()| std::fs::write(&record, text));
    Taken::Mine(Claim { handle: handle as isize, path: layout.to_path_buf(), record })
}

pub fn is_claimed(layout: &Path) -> bool {
    let name = wide(&mutex_name(layout));
    // SAFETY: a NUL-terminated name; the handle, if any, is closed at once.
    unsafe {
        let h = OpenMutexW(SYNCHRONIZE, 0, name.as_ptr());
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
    }
    true
}

fn alive(pid: u32) -> bool {
    // SAFETY: query-only access; the handle is closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE
    }
}

fn window_of(pid: u32, hwnd: isize) -> bool {
    let hwnd = hwnd as HWND;
    let mut owner = 0u32;
    // SAFETY: IsWindow accepts any value; the pid is written to a local.
    unsafe { IsWindow(hwnd) != 0 && GetWindowThreadProcessId(hwnd, &mut owner) != 0 }
        && owner == pid
}

/// The window that has `layout`, from the running records. Records of processes that
/// are gone are deleted; a record whose window isn't its process's (a reused pid) is
/// skipped.
pub fn holder_window(running: &Path, layout: &Path) -> Option<isize> {
    let entries = std::fs::read_dir(running).ok()?;
    let mut found = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let pid = path
            .file_stem()
            .and_then(|s| s.to_str()?.split('-').next()?.parse::<u32>().ok());
        let Some(pid) = pid else {
            continue;
        };
        if !alive(pid) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Some(rec) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<Record>(&t).ok())
        else {
            continue;
        };
        if hydraui_sets::same_path(&rec.layout, layout) && window_of(pid, rec.window) {
            found = Some(rec.window);
        }
    }
    found
}

pub fn bring_forward(hwnd: isize) {
    let hwnd = hwnd as HWND;
    // SAFETY: plain window calls; a stale handle makes them fail harmlessly.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

pub fn hwnd_of(window: &gpui::Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}
```

If a windows-sys name doesn't resolve (constant paths move between versions), find it with `grep -rn "fn OpenMutexW\|PROCESS_QUERY_LIMITED_INFORMATION" ~/.cargo/registry/src/*/windows-sys-0.61*/src` and fix the `use`; that is not a deviation needing a ruling.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydraui claim`
Expected: PASS (4 tests).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add Cargo.toml Cargo.lock crates/hydraui
git commit -m "feat: hydraui claims a layout file per window across processes"
```

---

### Task 3: The window knows its layout file

**Files:**
- Create: `crates/hydraui/src/window_layout.rs` (pure: title text, where an open goes)
- Modify: `crates/hydraui/src/lib.rs`, `crates/hydraui/src/workspace.rs` (fields, title, startup, close), `crates/hydraui/src/workspace/sets.rs` (`save_session` → `autosave`; `restore_session` removed; `open_layout_path`, `save_layout_to`, dialogs), `crates/hydraui/src/workspace/title_bar.rs` (`restart`), `crates/hydraui/src/launcher.rs` ("Save layout as…"), and every `save_session(` call site (rename to `autosave(`)
- Test: in `window_layout.rs`, `launcher.rs` (label)

**Interfaces:**
- Consumes: Task 1 (`open_layout_file`, `Opened`, `layout_name`, `layouts_dir`, `free_path`, `touch_recent`, `rename_recent`, `same_path`, `write_layout`), Task 2 (`claim::*`).
- Produces:
  - `pub fn window_title(layout: Option<&Path>, start_page: bool) -> String`
  - `pub fn now_secs() -> u64` (Unix seconds, for the recent list)
  - `pub fn spawn_hydraui(file: Option<&Path>) -> std::io::Result<()>` — starts another `hydraui.exe` (current exe), with the file as its argument
  - Workspace fields `layout: Option<PathBuf>`, `claim: Option<claim::Claim>`
  - `Workspace::autosave(&self, window, cx)` — writes `current_layout` to `self.layout` if any
  - `Workspace::open_layout_path(&mut self, path: PathBuf, window, cx) -> bool` — §7.12's open rules; true when this window opened it
  - `Workspace::save_layout_to(&mut self, path: PathBuf, window, cx) -> bool`
  - `Workspace::is_start_page(&self) -> bool` (returns `false` until Task 4 adds the page)

- [ ] **Step 1: Write the failing tests** — `window_layout.rs` with `todo!()` bodies and:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_the_layout_not_the_tab() {
        assert_eq!(window_title(Some(Path::new(r"C:\l\client-a.hydraui")), false), "hydraui - client-a");
        assert_eq!(window_title(None, false), "hydraui - untitled");
        assert_eq!(window_title(None, true), "hydraui");
        assert_eq!(window_title(Some(Path::new(r"C:\l\x.hydraui")), true), "hydraui");
    }
}
```

and in `launcher.rs`'s existing tests change the expected label `"Save layout to file…"` to `"Save layout as…"` (both places).

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui window_layout launcher`
Expected: FAIL — `todo!()` in `window_title`; the launcher tests fail on the old label.

- [ ] **Step 3: Implement `window_layout.rs` and the launcher label**

```rust
//! What a window's layout file means for its title, and starting another window.
use std::path::Path;

pub fn window_title(layout: Option<&Path>, start_page: bool) -> String {
    match (start_page, layout) {
        (true, _) => "hydraui".into(),
        (false, Some(p)) => format!("hydraui - {}", hydraui_sets::layout_name(p)),
        (false, None) => "hydraui - untitled".into(),
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Another hydraui window: one layout per window, so a second layout opens in a new process.
pub fn spawn_hydraui(file: Option<&Path>) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    if let Some(f) = file {
        cmd.arg(f);
    }
    cmd.spawn().map(drop)
}
```

`launcher.rs`: `add("Save layout as…".into(), Action::SaveLayout);`.

- [ ] **Step 4: Run them**

Run: `cargo test -p hydraui window_layout launcher`
Expected: PASS.

- [ ] **Step 5: Wire the Workspace** (`use hydraui_sets::Opened;` in `sets.rs`) (no new unit tests: this is UI glue over Tasks 1–2's tested pieces; Task 5's smoke run checks it)

1. Fields in `Workspace` (and `Self { … }` in `new`): `layout: Option<PathBuf>` (`None`), `claim: Option<crate::claim::Claim>` (`None`).
2. Rename every `save_session(` to `autosave(` (definition and the 18 call sites: `grep -rn "save_session(" crates/hydraui/src`). New body in `sets.rs`:

```rust
    /// Writes the window's layout to its file (spec §7.12); an untitled window writes nothing.
    pub(super) fn autosave(&self, window: &mut Window, cx: &mut App) {
        let Some(path) = &self.layout else { return };
        if let Err(e) = write_layout(path, &self.current_layout(cx)) {
            window.push_notification(
                Notification::warning(format!("can't save the layout to {}: {e}", path.display()))
                    .title("hydraui"),
                cx,
            );
        }
    }
```

   Delete `restore_session` and the `read_session`/`write_session` imports.
3. Add to `sets.rs`:

```rust
    pub(super) fn is_start_page(&self) -> bool {
        false
    }

    /// Opens a layout file (spec §7.12): in this window when it has no layout, else in a
    /// new hydraui; a file another window has brings that window forward instead. Returns
    /// whether this window opened it.
    pub(super) fn open_layout_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        if let Some(mine) = &self.layout {
            if !hydraui_sets::same_path(mine, &path)
                && let Err(e) = crate::window_layout::spawn_hydraui(Some(&path))
            {
                window.push_notification(
                    Notification::error(format!("can't start another hydraui: {e}"))
                        .title("Open layout"),
                    cx,
                );
            }
            return false;
        }
        let running = crate::claim::running_dir(&self.hydra.paths);
        if crate::claim::is_claimed(&path) {
            self.elsewhere(crate::claim::holder_window(&running, &path), window);
            return false;
        }
        let paths = self.hydra.paths.clone();
        let now = crate::window_layout::now_secs();
        let (layout, backing) = match hydraui_sets::open_layout_file(&path) {
            Err(e) => {
                window.push_notification(Notification::error(e).title("Open layout"), cx);
                return false;
            }
            Ok(Opened::Current(l)) => (l, Some(path)),
            Ok(Opened::Converted { layout, from, to }) => {
                window.push_notification(
                    Notification::info(format!(
                        "Converted {} → {}",
                        from.display(),
                        to.display()
                    ))
                    .title("Open layout"),
                    cx,
                );
                let _ = hydraui_sets::rename_recent(&paths, &from, &to, now);
                (layout, Some(to))
            }
            Ok(Opened::Untitled { layout, exists }) => {
                window.push_notification(
                    Notification::warning(format!(
                        "{} already exists, so this opens untitled: Save layout as… to keep it",
                        exists.display()
                    ))
                    .title("Open layout"),
                    cx,
                );
                (layout, None)
            }
        };
        if let Some(file) = backing {
            match crate::claim::take(&running, &file, crate::claim::hwnd_of(window)) {
                crate::claim::Taken::Mine(c) => {
                    let _ = hydraui_sets::touch_recent(&paths, &file, now);
                    self.claim = Some(c);
                    self.layout = Some(file);
                }
                crate::claim::Taken::Elsewhere(holder) => {
                    self.elsewhere(holder, window);
                    return false;
                }
            }
        }
        self.leave_start_page(window, cx);
        self.open_layout(layout, window, cx);
        self.autosave(window, cx);
        cx.notify();
        true
    }

    /// Another window has the file: bring it forward; a start page that only existed to
    /// open it closes.
    fn elsewhere(&mut self, holder: Option<isize>, window: &mut Window) {
        if let Some(h) = holder {
            crate::claim::bring_forward(h);
        }
        if self.is_start_page() {
            window.remove_window();
        }
    }

    /// Placeholder until Task 4's start page; then it hides the page.
    pub(super) fn leave_start_page(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// Writes the window to `path` and backs the window with it from then on (Save layout
    /// as…, spec §7.12). A file another window has is refused; a failed write keeps the
    /// old file.
    pub(super) fn save_layout_to(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        if self.layout.as_deref().is_some_and(|p| hydraui_sets::same_path(p, &path)) {
            self.autosave(window, cx);
            return true;
        }
        let refuse = |window: &mut Window, cx: &mut App| {
            window.push_notification(
                Notification::error(format!(
                    "{} is open in another hydraui window",
                    path.display()
                ))
                .title("Save layout"),
                cx,
            )
        };
        let running = crate::claim::running_dir(&self.hydra.paths);
        let claim = match crate::claim::take(&running, &path, crate::claim::hwnd_of(window)) {
            crate::claim::Taken::Mine(c) => c,
            crate::claim::Taken::Elsewhere(_) => {
                refuse(window, cx);
                return false;
            }
        };
        let result = write_layout(&path, &self.current_layout(cx));
        let ok = result.is_ok();
        self.saved("the layout", &path, result, window, cx);
        if !ok {
            return false; // the new claim drops here; the old file stays
        }
        let _ = hydraui_sets::touch_recent(
            &self.hydra.paths,
            &path,
            crate::window_layout::now_secs(),
        );
        self.claim = Some(claim); // the old claim drops: the old file is let go
        self.layout = Some(path);
        cx.notify();
        true
    }
```

4. Replace `save_layout_file` and `open_layout_file`:

```rust
    pub(super) fn save_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (dir, name) = match &self.layout {
            Some(p) => (
                p.parent().map(Path::to_path_buf).unwrap_or_default(),
                p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            ),
            None => {
                let dir = hydraui_sets::layouts_dir(&self.hydra.paths);
                let _ = std::fs::create_dir_all(&dir);
                (dir, "layout.hydraui".into())
            }
        };
        self.save_dialog(dir, name, window, cx, |ws, path, window, cx| {
            ws.save_layout_to(path, window, cx);
        });
    }

    pub(super) fn open_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog_path(false, window, cx, |ws, path, window, cx| {
            ws.open_layout_path(path, window, cx);
        });
    }
```

5. Title in `Workspace::render` (`workspace.rs`), replacing the tab-label title:

```rust
        let title = crate::window_layout::window_title(self.layout.as_deref(), self.is_start_page());
```

6. `startup` (`workspace.rs`): replace the `restore_session` block with

```rust
        // A session from before layouts were files becomes one, once (spec §7.12).
        if let Some(moved) = hydraui_sets::migrate_session(&self.hydra.paths) {
            let _ = hydraui_sets::touch_recent(
                &self.hydra.paths,
                &moved,
                crate::window_layout::now_secs(),
            );
        }
        let home = self.hydra.user_home.clone();
        self.new_tab_in(home, window, cx);
```

   (Task 4 replaces the last two lines with the start page / file argument.)
7. `restart` (`title_bar.rs`): replace its first line and the spawn:

```rust
    /// Saves the layout (an untitled window first gets a file, so nothing is lost), lets go
    /// of it, starts the new hydraui with it and closes this window; unsaved edits are asked
    /// about first, as when closing.
    pub(super) fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.is_none() && !self.tabs.is_empty() {
            let dir = hydraui_sets::layouts_dir(&self.hydra.paths);
            let path = hydraui_sets::free_path(&dir, "untitled");
            if !self.save_layout_to(path, window, cx) {
                return;
            }
        }
        self.autosave(window, cx);
        let file = self.layout.clone();
        let relaunch = move |ws: &mut Workspace, window: &mut Window, cx: &mut gpui::App| {
            // Let go first, or the new hydraui sees the file taken and gives way to us.
            ws.claim = None;
            match crate::window_layout::spawn_hydraui(file.as_deref()) {
                Ok(()) => window.remove_window(),
                Err(e) => window.push_notification(
                    Notification::error(format!("can't start the new hydraui: {e}"))
                        .title("Update hydra"),
                    cx,
                ),
            }
        };
        let all: Vec<_> = self.files.keys().copied().collect();
        if self.unsaved_in(&all, cx).is_empty() {
            relaunch(self, window, cx);
        } else {
            self.confirm_unsaved(all, window, cx, move |ws, w, cx| relaunch(ws, w, cx));
        }
    }
```

   `Tabs::is_empty` — use the existing emptiness check on `Tabs` (`self.tabs.active().is_none()` if there is no `is_empty`). If `confirm_unsaved`'s callback doesn't receive `&mut Workspace`, capture a `WeakEntity<Workspace>` and `update` it inside instead; record which in the ledger.
8. Launcher dispatch (`workspace/launcher.rs`) is unchanged: `Action::SaveLayout` → `save_layout_file`, `Action::OpenLayout` → `open_layout_file`.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: each hydraui window keeps its own layout file; the title names it"
```

---

### Task 4: The start page and the file argument

**Files:**
- Create: `crates/hydraui/src/start_page.rs` (pure model), `crates/hydraui/src/workspace/start_page.rs` (view and keys)
- Modify: `crates/hydraui/src/lib.rs`, `crates/hydraui/src/workspace.rs` (mod, field, `new` signature, `startup`, render body/tab strip/status bar), `crates/hydraui/src/workspace/sets.rs` (`is_start_page`, `leave_start_page` real bodies), `crates/hydraui/src/main.rs` (argument, early hand-over), `crates/hydraui/src/launcher.rs` + `workspace/launcher.rs` ("Start page" entry)
- Test: in `start_page.rs`, `launcher.rs`

**Interfaces:**
- Consumes: Task 1 (`RecentEntry`, `layout_name`, `read_recent`, `forget_recent`), Task 2 (`is_claimed`, `holder_window`, `bring_forward`, `running_dir`), Task 3 (`open_layout_path`, `spawn_hydraui`, `elsewhere`).
- Produces:
  - `pub struct Row { pub path: PathBuf, pub name: String, pub folder: String, pub ago: String, pub elsewhere: bool }` (`Debug, Clone, PartialEq`)
  - `pub fn rows(recent: &[RecentEntry], filter: &str, home: &Path, now: u64, claimed: &dyn Fn(&Path) -> bool) -> Vec<Row>`
  - `pub fn ago(secs: u64) -> String`
  - `pub fn folder_label(path: &Path, home: &Path) -> String`
  - `pub enum StartWith { Page, File(PathBuf) }` (in `workspace.rs`, `pub`), passed to `Workspace::new`
  - `Action::StartPage` in the launcher

- [ ] **Step 1: Write the failing tests** — `start_page.rs` with `todo!()` bodies and:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(p: &str, opened: u64) -> RecentEntry {
        RecentEntry { path: PathBuf::from(p), opened }
    }

    #[test]
    fn ago_reads_like_a_person_would_say_it() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(5 * 60), "5 min ago");
        assert_eq!(ago(2 * 3600 + 10), "2 h ago");
        assert_eq!(ago(3 * 86400), "3 days ago");
        assert_eq!(ago(86400), "1 day ago");
    }

    #[test]
    fn folders_under_home_use_a_tilde() {
        let home = Path::new(r"C:\Users\d");
        assert_eq!(folder_label(Path::new(r"C:\Users\d\.hydra\x.hydraui"), home), r"~\.hydra");
        assert_eq!(folder_label(Path::new(r"E:\work\x.hydraui"), home), r"E:\work");
    }

    #[test]
    fn rows_filter_by_name_or_folder_and_mark_open_ones() {
        let home = Path::new(r"C:\Users\d");
        let recent = [
            entry(r"E:\work\client-a.hydraui", 1000),
            entry(r"C:\Users\d\blog.hydraui", 900),
        ];
        let open_elsewhere = |p: &Path| p.ends_with("blog.hydraui");
        let all = rows(&recent, "", home, 1000, &open_elsewhere);
        assert_eq!(
            all,
            [
                Row {
                    path: r"E:\work\client-a.hydraui".into(),
                    name: "client-a".into(),
                    folder: r"E:\work".into(),
                    ago: "just now".into(),
                    elsewhere: false,
                },
                Row {
                    path: r"C:\Users\d\blog.hydraui".into(),
                    name: "blog".into(),
                    folder: "~".into(),
                    ago: "1 min ago".into(),
                    elsewhere: true,
                },
            ]
        );
        assert_eq!(rows(&recent, "WORK", home, 1000, &open_elsewhere).len(), 1, "folder, any case");
        assert_eq!(rows(&recent, "blo", home, 1000, &open_elsewhere)[0].name, "blog");
        assert!(rows(&recent, "zzz", home, 1000, &open_elsewhere).is_empty());
    }
}
```

In `launcher.rs` tests add `"Start page"` to both expected label lists, right after `"Save layout as…"`, and shift the later index assertions by one (`got[11]` becomes `Action::StartPage`, the rest move down one).

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydraui start_page launcher`
Expected: FAIL — `todo!()` panics; launcher label lists differ.

- [ ] **Step 3: Implement `start_page.rs` and the launcher entry**

```rust
//! The start page's rows (spec §7.12): recent layouts, filtered, with when each was last
//! opened and whether another window has it. No UI.
use std::path::{Path, PathBuf};

use hydraui_sets::{RecentEntry, layout_name};

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub folder: String,
    pub ago: String,
    pub elsewhere: bool,
}

pub fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "1 day ago".into(),
        _ => format!("{} days ago", secs / 86400),
    }
}

pub fn folder_label(path: &Path, home: &Path) -> String {
    let dir = path.parent().unwrap_or(path);
    match dir.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~\\{}", rest.display()),
        Err(_) => dir.display().to_string(),
    }
}

pub fn rows(
    recent: &[RecentEntry],
    filter: &str,
    home: &Path,
    now: u64,
    claimed: &dyn Fn(&Path) -> bool,
) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    recent
        .iter()
        .map(|e| Row {
            name: layout_name(&e.path),
            folder: folder_label(&e.path, home),
            ago: ago(now.saturating_sub(e.opened)),
            elsewhere: claimed(&e.path),
            path: e.path.clone(),
        })
        .filter(|r| {
            needle.is_empty()
                || r.name.to_lowercase().contains(&needle)
                || r.folder.to_lowercase().contains(&needle)
        })
        .collect()
}
```

`launcher.rs`: add the variant `StartPage` to `Action` and `add("Start page".into(), Action::StartPage);` right after the Save layout line. `workspace/launcher.rs` dispatch: `Action::StartPage => { let _ = crate::window_layout::spawn_hydraui(None); }` — Ruling carried here: the launcher's Start page opens a new window showing the start page, because a window holds one layout and the page's choices would otherwise have nowhere to go.

- [ ] **Step 4: Run them**

Run: `cargo test -p hydraui start_page launcher`
Expected: PASS.

- [ ] **Step 5: The view** — `workspace/start_page.rs`:

```rust
//! The start page (spec §7.12): recent layouts with a filter, New window, Open layout file….
use gpui::prelude::*;
use gpui::{Context, Entity, KeyDownEvent, Subscription, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::start_page::{Row, rows};

pub(super) struct StartPage {
    pub input: Entity<InputState>,
    pub selected: usize,
    _sub: Subscription,
}

impl Workspace {
    pub(super) fn show_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter recent layouts"));
        let sub = cx.subscribe_in(&input, window, |ws, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                if let Some(p) = ws.start.as_mut() {
                    p.selected = 0;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => ws.choose_start_row(window, cx),
            _ => {}
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self.start = Some(StartPage { input, selected: 0, _sub: sub });
        cx.notify();
    }

    fn start_rows(&self, cx: &gpui::App) -> Vec<Row> {
        let filter = self
            .start
            .as_ref()
            .map(|p| p.input.read(cx).value().to_string())
            .unwrap_or_default();
        rows(
            &hydraui_sets::read_recent(&self.hydra.paths),
            &filter,
            &self.hydra.user_home,
            crate::window_layout::now_secs(),
            &crate::claim::is_claimed,
        )
    }

    /// Enter: the selected row, or New window when nothing is listed.
    fn choose_start_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.start_rows(cx);
        let i = self.start.as_ref().map_or(0, |p| p.selected);
        match rows.get(i).cloned() {
            None => self.start_new_window(window, cx),
            Some(row) => self.open_start_row(row, window, cx),
        }
    }

    fn open_start_row(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        if !row.path.exists() {
            let _ = hydraui_sets::forget_recent(&self.hydra.paths, &row.path);
            window.push_notification(
                Notification::warning(format!("{} no longer exists", row.path.display()))
                    .title("Recent layouts"),
                cx,
            );
            cx.notify();
            return;
        }
        self.open_layout_path(row.path, window, cx);
    }

    fn start_new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_start_page(window, cx);
        let home = self.hydra.user_home.clone();
        self.new_tab_in(home, window, cx);
    }

    fn start_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.start_rows(cx).len();
        let Some(page) = self.start.as_mut() else { return };
        match ev.keystroke.key.as_str() {
            "down" if count > 0 => page.selected = (page.selected + 1).min(count - 1),
            "up" => page.selected = page.selected.saturating_sub(1),
            "escape" => page.input.update(cx, |i, cx| i.set_value("", window, cx)),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_start_page(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(page) = &self.start else { return div().into_any_element() };
        let theme = cx.theme();
        let (muted, accent, border) = (theme.muted_foreground, theme.accent, theme.border);
        let rows = self.start_rows(cx);
        let list = v_flex().gap_1().children(rows.into_iter().enumerate().map(|(i, row)| {
            let chosen = row.clone();
            h_flex()
                .id(("recent", i))
                .px_3()
                .py_2()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .when(i == page.selected, |d| d.bg(accent))
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.open_start_row(chosen.clone(), window, cx)
                }))
                .child(div().min_w(px(160.)).child(row.name))
                .child(div().flex_1().text_color(muted).child(row.folder))
                .child(div().text_sm().text_color(muted).child(if row.elsewhere {
                    "open in another window".to_string()
                } else {
                    row.ago
                }))
        }));
        div()
            .size_full()
            .flex()
            .justify_center()
            .pt(px(64.))
            .capture_key_down(cx.listener(Self::start_key))
            .child(
                v_flex()
                    .w(px(560.))
                    .gap_3()
                    .child(div().text_xl().child("Recent layouts"))
                    .child(Input::new(&page.input))
                    .child(
                        div()
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .p_1()
                            .min_h(px(120.))
                            .child(list),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("start-new")
                                    .label("New window")
                                    .primary()
                                    .small()
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.start_new_window(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("start-open")
                                    .label("Open layout file…")
                                    .small()
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.open_layout_file(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
```

If an API differs in gpui-component 0.7.3 (`InputState::focus`, `set_value`'s arguments, `value()`, `capture_key_down`, `theme.accent`), match the existing use in `pane_view.rs`/`workspace/accounts.rs` or the crate source; not a ruling.

Then in `workspace.rs`:
- `mod start_page;` beside the other submodules; field `start: Option<start_page::StartPage>` (`None` in `new`).
- `pub enum StartWith { Page, File(PathBuf) }`; `Workspace::new(…, start: StartWith, window, cx)` passes it to `startup`.
- `startup`: after the migration block,

```rust
        // The page first: opening a file leaves it; a file that fails to open leaves the
        // page and its toast (§7.12 Command line); a file another window took meanwhile
        // closes this window, as a start page does.
        self.show_start_page(window, cx);
        if let StartWith::File(path) = start {
            self.open_layout_path(path, window, cx);
        }
```
- `render`: when `self.start.is_some()`, the body is `self.render_start_page(cx)`, the tab strip is not rendered, and `render_status` renders an empty bar (guard its contents with `if self.start.is_none()`).
- In `sets.rs`: `is_start_page` returns `self.start.is_some()`; `leave_start_page` sets `self.start = None` and `cx.notify()`.

`main.rs`, before `gpui_platform::application()`:

```rust
    // A layout file to open (spec §7.12): if another window has it, bring that one forward
    // and stop here.
    let file = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .map(|p| std::path::absolute(&p).unwrap_or(p));
    if let Some(f) = &file
        && hydraui::claim::is_claimed(f)
    {
        let running = hydraui::claim::running_dir(&hydra.paths);
        if let Some(h) = hydraui::claim::holder_window(&running, f) {
            hydraui::claim::bring_forward(h);
        }
        return;
    }
    let start = match file {
        Some(f) => hydraui::workspace::StartWith::File(f),
        None => hydraui::workspace::StartWith::Page,
    };
```

and pass `start` into `Workspace::new`. (`hydra` is moved into the closure later; take `running` before that, as above.)

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydraui
git commit -m "feat: hydraui starts on a page of recent layouts and opens a layout file given to it"
```

---

### Task 5: The title bar's layout menu; smoke run

**Files:**
- Modify: `crates/hydraui/src/workspace/title_bar.rs`
- Create (scratchpad only, not committed): a smoke script

**Interfaces:**
- Consumes: Task 3 (`layout`, `save_layout_file`, `open_layout_file`, `open_layout_path`, `is_start_page`), Task 1 (`read_recent`, `layout_name`, `same_path`).
- Produces: nothing new for other tasks.

- [ ] **Step 1: Replace the title text in `render_title_bar`** with the name button (hidden on the start page):

```rust
        let name = (!self.is_start_page()).then(|| {
            let untitled = self.layout.is_none();
            let label = match &self.layout {
                Some(p) => hydraui_sets::layout_name(p),
                None => "untitled".into(),
            };
            let ws = cx.entity().downgrade();
            let current = self.layout.clone();
            let recent = hydraui_sets::read_recent(&self.hydra.paths);
            let button = Button::new("layout-name")
                .label(format!("{label} ▾"))
                .ghost()
                .xsmall()
                .when(untitled, |b| {
                    b.text_color(muted)
                        .tooltip("Not saved: Save layout as… to keep this window")
                })
                .dropdown_menu(move |menu, window, cx| {
                    let (save, open) = (ws.clone(), ws.clone());
                    let mut menu = menu
                        .item(PopupMenuItem::new("Save layout as…").on_click(move |_, window, cx| {
                            let _ = save.update(cx, |ws, cx| ws.save_layout_file(window, cx));
                        }))
                        .item(PopupMenuItem::new("Open layout file…").on_click(move |_, window, cx| {
                            let _ = open.update(cx, |ws, cx| ws.open_layout_file(window, cx));
                        }));
                    let others: Vec<_> = recent
                        .iter()
                        .filter(|e| {
                            !current.as_deref().is_some_and(|c| hydraui_sets::same_path(c, &e.path))
                        })
                        .map(|e| e.path.clone())
                        .collect();
                    if !others.is_empty() {
                        let ws = ws.clone();
                        menu = menu.submenu("Recent", window, cx, move |sub, _, _| {
                            others.iter().fold(sub, |sub, p| {
                                let (ws, p) = (ws.clone(), p.clone());
                                sub.item(
                                    PopupMenuItem::new(hydraui_sets::layout_name(&p)).on_click(
                                        move |_, window, cx| {
                                            let _ = ws.update(cx, |ws, cx| {
                                                ws.open_layout_path(p.clone(), window, cx)
                                            });
                                        },
                                    ),
                                )
                            })
                        });
                    }
                    if let Some(c) = current.clone() {
                        menu = menu.item(PopupMenuItem::new("Reveal in Explorer").on_click(
                            move |_, _, _| {
                                let _ = std::process::Command::new("explorer.exe")
                                    .arg(format!("/select,{}", c.display()))
                                    .spawn();
                            },
                        ));
                    }
                    menu
                });
            div().occlude().child(button)
        });
```

and in the left `h_flex` use `.child(img(ICON.clone()).size(px(18.))).children(name)` in place of the old title `div`. Imports: `gpui_component::menu::{DropdownMenu, PopupMenuItem}`, `gpui_component::tooltip`-style `.tooltip` as the crate provides on `Button`. Delete the now-unused `title` text use in the bar (the field stays: it caches the OS title).

If `Button::text_color` doesn't exist, wrap the label in the muted colour another way the crate allows (for example `.label` with a styled child via `.child`); record what you used in the ledger.

- [ ] **Step 2: Build and run the whole suite**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all PASS.

- [ ] **Step 3: Smoke run** — with a scratch `HYDRA_HOME` (copy `smoke-session-hydra` from the scratchpad as earlier smoke runs did), and a debug build:
  1. Put a `session.toml` in the scratch home; start hydraui: the start page lists `last-session`; `session.toml` is gone; `layouts\last-session.hydraui` exists. Window title `hydraui`.
  2. Enter opens it: tabs come back; title `hydraui - last-session`; `running\<pid>.toml` exists.
  3. Start a second hydraui with `target\debug\hydraui.exe <that file>`: it exits within a few seconds and the first window comes to the front (check the second process is gone and only one hydraui window exists).
  4. In the first window, open the launcher's Start page: a second window appears with the row marked "open in another window".
  5. Close the first window; `running\<pid>.toml` is gone; Enter on the second window's row now opens it there.
  6. Put an old `x.hydraui.toml` (a copy of the layout) in the scratch home; `HYDRAUI_TEST_OPEN_PATH=<it>` + launcher Open layout file… on a fresh start-page window: `x.hydraui` exists, `x.hydraui.toml` doesn't, the toast says Converted.
  7. Title bar: click the name; the menu shows Save layout as…, Open layout file…, Recent ▸, Reveal in Explorer; screenshot it with `PrintWindow`.
  Record each result in the ledger; fix anything that fails (systematic-debugging, a test where one can reach it).

- [ ] **Step 4: Commit**

```bash
git add crates/hydraui
git commit -m "feat: hydraui's title bar names the layout, with its menu"
```

---

### Task 6: The installer registers `.hydraui`; docs

**Files:**
- Modify: `crates/hydra-setup/src/system.rs` (trait, `Real`, `FakeSystem`), `crates/hydra-setup/src/install.rs` (install and uninstall call it; tests), `scripts/test-setup.ps1` (checks), `site/app.html` (startup paragraph, layouts section, update restart line)
- Test: in `install.rs`

**Interfaces:**
- Consumes: the existing `System` trait and `reg` helpers in `system.rs` (`set_string`, `get_string`, `delete_tree`).
- Produces: `System::register_layout_files(&self, exe: &Path) -> anyhow::Result<()>`, `System::unregister_layout_files(&self)`; `FakeSystem.layout_files: RefCell<Option<PathBuf>>`.

- [ ] **Step 1: Write the failing tests** — add the two trait methods (with `FakeSystem` storing/clearing `layout_files`, and `Real` bodied `todo!()`), then extend the existing tests:

In `installs_files_path_shortcut_and_settings_entry`, after the shortcut assertions:

```rust
        assert_eq!(
            sys.layout_files.borrow().clone(),
            Some(plan.dir.join("hydraui.exe")),
            ".hydraui opens in the installed hydraui"
        );
```

In `uninstall_removes_what_install_added`, after the uninstall:

```rust
        assert_eq!(sys.layout_files.borrow().clone(), None, "the association is removed");
```

(Use the test's own names for `plan`/`sys`; they are the ones those tests already use.)

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p hydra-setup`
Expected: FAIL — `layout_files` is still `None` after install (install doesn't call it yet).

- [ ] **Step 3: Implement**

`install.rs`: after the shortcuts in `install`:

```rust
    sys.register_layout_files(&plan.dir.join("hydraui.exe"))
        .context("can't register .hydraui files")?;
```

and in `uninstall`, beside removing the shortcuts: `sys.unregister_layout_files();`.

`system.rs` `Real`:

```rust
    fn register_layout_files(&self, exe: &Path) -> anyhow::Result<()> {
        let exe = exe.display();
        let prog = r"Software\Classes\hydraui.layout";
        reg::set_string(prog, "", "hydraui layout", false)?;
        reg::set_string(&format!(r"{prog}\DefaultIcon"), "", &format!("\"{exe}\",0"), false)?;
        reg::set_string(
            &format!(r"{prog}\shell\open\command"),
            "",
            &format!("\"{exe}\" \"%1\""),
            false,
        )?;
        reg::set_string(r"Software\Classes\.hydraui", "", "hydraui.layout", false)?;
        reg::associations_changed();
        Ok(())
    }

    fn unregister_layout_files(&self) {
        // Leave .hydraui alone if something else has taken it over since.
        if reg::get_string(r"Software\Classes\.hydraui", "").as_deref() == Some("hydraui.layout") {
            reg::delete_tree(r"Software\Classes\.hydraui");
        }
        reg::delete_tree(r"Software\Classes\hydraui.layout");
        reg::associations_changed();
    }
```

and in `mod reg`:

```rust
    /// Tells Explorer file associations changed, so icons and Open follow at once.
    pub fn associations_changed() {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        // SAFETY: no item pointers with SHCNE_ASSOCCHANGED.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
```

Check `reg::set_string`/`get_string` accept `""` as the default value name (`RegSetValueExW` with an empty name writes the key's default); if `ensure_key` isn't called by `set_string`, call it. If `SHChangeNotify`'s signature differs in windows 0.61, match it from the crate source.

`scripts/test-setup.ps1`: after the install checks, assert
`(Get-ItemProperty 'HKCU:\Software\Classes\.hydraui').'(default)' -eq 'hydraui.layout'` and that `HKCU:\Software\Classes\hydraui.layout\shell\open\command` names the test folder's `hydraui.exe`; after the uninstall, assert both keys are gone. Use the script's existing check/fail helper. Also make its "refuses if hydra is installed" guard refuse if `HKCU:\Software\Classes\hydraui.layout` exists and points elsewhere (so a developer's real association isn't removed by the test's uninstall).

`site/app.html`:
- The startup paragraph (line ~69): "hydraui starts on a page of your recent layouts. Pick one, or **New window** for a plain terminal. A window saved as a layout keeps its file up to date as you work; an untitled window isn't kept when you close it."
- The layouts section: layout files are `*.hydraui`; double-clicking one opens it (with the installer); each window has one layout, shown in the title bar, whose ▾ menu has Save layout as…, Open layout file…, Recent and Reveal in Explorer; opening a layout that's already open brings its window forward; older `*.hydraui.toml` files are converted when opened.
- The update list's restart line: "Restarting reopens your layout (an untitled window is saved first), but stops what's running in it, so it's always your call."

- [ ] **Step 4: Run them**

Run: `cargo test -p hydra-setup`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/hydra-setup scripts/test-setup.ps1 site/app.html
git commit -m "feat: hydra-setup opens .hydraui files in hydraui; site describes layouts"
```
