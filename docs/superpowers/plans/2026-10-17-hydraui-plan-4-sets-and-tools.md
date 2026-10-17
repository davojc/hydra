# hydraui Plan 4: Sets and Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the hydraui spec: sets open and close in the window (with session restore and reload on change), a launcher palette, sign-in and set-secret actions, a file tree, and packaging in the release.

**Architecture:** Plain, tested modules carry the logic: `layout` (a set's tab → split tree + pane requests), `launcher` (palette entries and their actions), `file_tree` (gitignore-aware one-level listings, quoting), plus new `Hydra` methods (sign-in requests, missing secrets, storing a secret). The GPUI side grows in three places: `workspace/sets.rs` and `workspace/launcher.rs` (child modules of `workspace.rs`, so they extend `Workspace` directly) and a new `FileTreeView`. Packaging adds a GUI-subsystem build with version info and a release asset.

**Tech Stack:** as plan 3, plus `hydraui-sets` (plan 1), `ignore` 0.4 and `notify` 8, `winresource` 0.1 (build), `windows-sys` 0.61 (`MessageBoxW`).

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` §5.4, §7.1 (set groups), §7.3, §7.4, §7.5, §7.7 (roadmap: `docs/superpowers/plans/2026-10-17-hydraui-roadmap.md`; GPUI notes: `docs/superpowers/notes/2026-10-17-gpui-api-notes.md`; plan 3: `docs/superpowers/plans/2026-10-17-hydraui-plan-3-app-shell.md`).

## Global Constraints

- **Prerequisites:** PowerShell 7 on PATH; Git for Windows for the bash checks.
- **Pinned stack** as plan 3: `gpui-pre` =0.3.9, `gpui-pre-platform` =0.3.9, `gpui-component` =0.7.3, `gpui-kit-assets` =0.7.3. New: `ignore = "0.4"`, `notify = "8"`, `winresource = "0.1"` (build-dependency, as hydra-cli uses).
- **hydraui's files:** `<HYDRA_HOME>/hydraui/sets/<name>.toml` (hand-edited, read through `hydraui-sets`), `<HYDRA_HOME>/hydraui/session.toml` (written by hydraui). hydraui never edits hydra's `config.toml`, `envs/` or `.hydra` files; it writes secrets only through hydra's secret store (spec §3.1).
- **Sets (spec §5.4):** loading a loaded set focuses its first tab; unloading closes all its tabs, asking first when any pane has a command running (OSC 133); closing a set's last tab by hand unloads it; a changed file shows "changed — reload?" and running panes are never changed in place; `session.toml` lists loaded sets in order and they reload at start, an invalid or deleted set being skipped with a toast.
- **Sign in (spec §7.4):** runs the same sign-in command as `hydra auth <tool> <env>` (the provider's `auth_command`, with its `auth_unset` variables dropped and missing secrets allowed) in a new pane in that environment.
- **File tree (spec §7.3):** rooted at the focused pane's last OSC 7 folder; folders load lazily; `.gitignore` respected; `notify` keeps it current; click a file inserts its quoted path; Ctrl-click opens it in `editor` (else the OS default); double-click a folder sends `cd` + Enter only when the pane is `AtPrompt`.
- **Packaging (spec §7.7):** `hydraui.exe` is a GUI-subsystem exe with version info like `hydra.exe`; it runs without `hydra.exe`; the release ships both; `scripts/` builds both.
- **GPUI facts** as plan 3 (a bound key never reaches `on_key_down`; plain text arrives through the input handler; drags use `window.on_mouse_event`). `gpui_component::init` registers its own key bindings, so hydraui never calls `cx.clear_key_bindings()`.
- **Tests** never touch the real `~/.hydra`, `~/.claude`, `~/.claude.json`, global git config or Credential Manager: temp `HydraPaths` and `hydra_core::secret::MemoryStore`.
- **Commits:**
  - Repo identity (davojc). No `--no-verify`. No employer names.
  - Every commit message ends with:
    ```
    Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
    ```
- **Before every commit, these must pass:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace` (no new failures; `keyring_store::tests::round_trip_through_credential_manager` is a known flake — rerun once)

## Review Focus

1. **Unloading a set while `npm run dev` runs in one of its panes:** hydraui asks first and names the running panes; Cancel leaves everything running. Pinned in Task 3.
2. **File and folder names with spaces, apostrophes or `[ ]`,** inserted or `cd`'d into from the tree: they arrive as one correctly quoted argument in pwsh (`-LiteralPath`, so brackets aren't wildcards) and in Git Bash. Pinned in Task 7.
3. **A sign-in command whose program path or arguments contain spaces or apostrophes:** it runs as one command in both shells. Pinned in Task 4.
4. **Opening the tree on a big folder** (home, a repo with `node_modules`/`target`): only one level is read, gitignored folders are hidden, and nothing below is touched until opened. Pinned in Task 7.
5. **A secret value with spaces** is stored exactly as typed; an empty or blank one is refused instead of stored. Pinned in Task 4.

---

### Task 1: Launcher and file tree commands

**Files:**
- Modify: `crates/hydraui/src/keys.rs`
- Test: unit tests in `keys.rs`

**Interfaces:**
- Produces (used by Tasks 6, 8): GPUI actions `keys::Launcher`, `keys::FileTree`; `Command::Launcher` (`[keys]` name `launcher`, default `ctrl-shift-p`) and `Command::FileTree` (`file_tree`, `ctrl-b`); `Command::ALL` has 15 entries.

- [ ] **Step 1: Write the failing test.** In `keys.rs` tests, in `the_defaults_bind_every_command`, add after the `NextTab` assertion:

```rust
        assert!(keys.contains(&(Command::Launcher, "ctrl-shift-p".to_string())));
        assert!(keys.contains(&(Command::FileTree, "ctrl-b".to_string())));
        assert_eq!(Command::FileTree.name(), "file_tree");
```

- [ ] **Step 2: Run it to see it fail.** `cargo test -p hydraui keys` — expected: compile error (no variant `Launcher` / `FileTree`).

- [ ] **Step 3: Implement.** In `keys.rs`:
  - Change the module doc's last sentence ("The launcher and file tree commands arrive with those features (plan 4).") to nothing — delete it.
  - In `gpui::actions!`, add `Launcher,` and `FileTree` after `Find,` (keep `Find` followed by a comma).
  - Add `Launcher,` and `FileTree,` at the end of `enum Command`.
  - `pub const ALL: [Command; 15]`, adding `Command::Launcher, Command::FileTree,` at the end.
  - In `name`: `Command::Launcher => "launcher",` and `Command::FileTree => "file_tree",`.
  - In `default_keys`: `Command::Launcher => "ctrl-shift-p",` and `Command::FileTree => "ctrl-b",`.
  - In `action`: `Command::Launcher => Box::new(Launcher),` and `Command::FileTree => Box::new(FileTree),`.

- [ ] **Step 4: Run the tests.** `cargo test -p hydraui keys` — expected: PASS.

- [ ] **Step 5: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui launcher and file tree commands

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: A set's tab as panes

**Files:**
- Modify: `Cargo.toml` (workspace: `hydraui-sets`), `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`
- Modify: `crates/hydraui/src/hydra.rs` (`PaneRequest::new`, `note`), `crates/hydraui/src/tabs.rs` (`Tab.title`, `add_tree`), `crates/hydraui/src/pane_view.rs` (note in the warning strip), `crates/hydraui/src/workspace.rs` (use `PaneRequest::new`, tab titles)
- Create: `crates/hydraui/src/layout.rs`
- Test: unit tests in `layout.rs` and `tabs.rs`

**Interfaces:**
- Consumes: `hydraui_sets::{TabSpec, Node, PaneSpec, SplitDir}` (plan 1: `TabSpec { title: String, root: Node }`, `Node::Split { dir, ratio, first, second }` / `Node::Pane(PaneSpec { env: EnvName, cwd, shell, command, bound_elsewhere })`).
- Produces (used by Tasks 3–8):
  - `hydra::PaneRequest` gains `note: Option<String>`; `PaneRequest::new(env: impl Into<String>, cwd: PathBuf, shell: ShellKind) -> PaneRequest` (no command, no note)
  - `tabs::Tab` gains `title: Option<String>`; `Tabs::add_tree(set: Option<String>, title: Option<String>, tree: Tree, focused: PaneId) -> TabId` (`add` calls it with `None` and a leaf)
  - `layout::tab_layout<F: FnMut() -> PaneId>(tab: &TabSpec, next_id: &mut F) -> (Tree, Vec<(PaneId, PaneRequest)>)`

- [ ] **Step 1: Add the dependency and module.** Root `Cargo.toml` `[workspace.dependencies]`, after `hydraui-term = { path = "crates/hydraui-term" }`:

```toml
hydraui-sets = { path = "crates/hydraui-sets" }
```

`crates/hydraui/Cargo.toml` `[dependencies]`, after `hydraui-term.workspace = true`:

```toml
hydraui-sets.workspace = true
```

`crates/hydraui/src/lib.rs`: add `pub mod layout;` after `pub mod keys;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hydra_core::name::EnvName;
    use hydra_platform::shell::ShellKind;
    use hydraui_sets::PaneSpec;

    use super::*;

    fn pane(command: Option<&str>, bound_elsewhere: Option<&str>) -> Node {
        Node::Pane(PaneSpec {
            env: EnvName::parse("work").unwrap(),
            cwd: PathBuf::from("C:\\work\\api"),
            shell: ShellKind::Pwsh,
            command: command.map(str::to_string),
            bound_elsewhere: bound_elsewhere.map(str::to_string),
        })
    }

    fn ids() -> impl FnMut() -> PaneId {
        let mut n = 9;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn a_single_pane_tab_is_one_leaf() {
        let tab = TabSpec {
            title: "infra".into(),
            root: pane(Some("terraform plan"), None),
        };
        let (tree, panes) = tab_layout(&tab, &mut ids());
        assert_eq!(tree, Tree::Leaf(10));
        assert_eq!(panes.len(), 1);
        let (id, req) = &panes[0];
        assert_eq!(*id, 10);
        assert_eq!(req.env, "work");
        assert_eq!(req.cwd, PathBuf::from("C:\\work\\api"));
        assert_eq!(req.shell, ShellKind::Pwsh);
        assert_eq!(req.command.as_deref(), Some("terraform plan"));
        assert_eq!(req.note, None);
    }

    #[test]
    fn splits_become_the_same_tree_with_fresh_ids() {
        let tab = TabSpec {
            title: "api".into(),
            root: Node::Split {
                dir: SplitDir::Right,
                ratio: 0.6,
                first: Box::new(pane(Some("claude"), None)),
                second: Box::new(Node::Split {
                    dir: SplitDir::Down,
                    ratio: 0.5,
                    first: Box::new(pane(Some("npm run dev"), None)),
                    second: Box::new(pane(None, Some("personal"))),
                }),
            },
        };
        let (tree, panes) = tab_layout(&tab, &mut ids());
        assert_eq!(
            tree,
            Tree::Split {
                axis: Axis::Row,
                ratio: 0.6,
                first: Box::new(Tree::Leaf(10)),
                second: Box::new(Tree::Split {
                    axis: Axis::Column,
                    ratio: 0.5,
                    first: Box::new(Tree::Leaf(11)),
                    second: Box::new(Tree::Leaf(12)),
                }),
            }
        );
        let ids: Vec<PaneId> = panes.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![10, 11, 12]);
        assert_eq!(panes[1].1.command.as_deref(), Some("npm run dev"));
        assert_eq!(
            panes[2].1.note.as_deref(),
            Some("this folder is bound to personal")
        );
    }
}
```

In `tabs.rs` tests add:

```rust
    #[test]
    fn add_tree_keeps_the_layout_title_and_focus() {
        let mut t = Tabs::default();
        t.add(None, 1);
        let tree = Tree::Split {
            axis: Axis::Row,
            ratio: 0.6,
            first: Box::new(Tree::Leaf(2)),
            second: Box::new(Tree::Leaf(3)),
        };
        let id = t.add_tree(Some("client-a".into()), Some("api".into()), tree.clone(), 2);
        let tab = t.get(id).unwrap();
        assert_eq!(tab.tree, tree);
        assert_eq!(tab.focused, 2);
        assert_eq!(tab.title.as_deref(), Some("api"));
        assert_eq!(t.active_id(), Some(id));
        assert_eq!(t.iter().next().unwrap().id, id, "a set's tab goes before the Loose tabs");
        assert_eq!(t.get(t.tab_of(1).unwrap()).unwrap().title, None);
    }
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui` — expected: compile errors (`tab_layout`, `add_tree`, `note`, `title` missing).

- [ ] **Step 4: Implement.**

`layout.rs`, above its tests:

```rust
//! A set's tab as panes (spec §5): hydraui-sets' resolved layout becomes a split tree, and
//! each pane what it runs.
use hydraui_sets::{Node, SplitDir, TabSpec};

use crate::hydra::PaneRequest;
use crate::split::{Axis, PaneId, Tree};

/// The tab's tree, with `next_id` naming each pane, and each pane's request in tree order
/// (left to right, top to bottom).
pub fn tab_layout<F: FnMut() -> PaneId>(
    tab: &TabSpec,
    next_id: &mut F,
) -> (Tree, Vec<(PaneId, PaneRequest)>) {
    let mut panes = Vec::new();
    let tree = node(&tab.root, next_id, &mut panes);
    (tree, panes)
}

fn node<F: FnMut() -> PaneId>(
    n: &Node,
    next_id: &mut F,
    panes: &mut Vec<(PaneId, PaneRequest)>,
) -> Tree {
    match n {
        Node::Pane(p) => {
            let id = next_id();
            let mut req = PaneRequest::new(p.env.to_string(), p.cwd.clone(), p.shell);
            req.command = p.command.clone();
            // An explicit env on a folder bound elsewhere: the only signal (spec §5.2).
            req.note = p
                .bound_elsewhere
                .as_ref()
                .map(|e| format!("this folder is bound to {e}"));
            panes.push((id, req));
            Tree::Leaf(id)
        }
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => Tree::Split {
            axis: match dir {
                SplitDir::Right => Axis::Row,
                SplitDir::Down => Axis::Column,
            },
            ratio: *ratio,
            first: Box::new(node(first, next_id, panes)),
            second: Box::new(node(second, next_id, panes)),
        },
    }
}
```

`hydra.rs`: add to `PaneRequest` after `command`:

```rust
    /// Shown in the pane's warning strip (e.g. the folder is bound to another environment).
    pub note: Option<String>,
```

and below the struct:

```rust
impl PaneRequest {
    /// A plain shell in `env` at `cwd`.
    pub fn new(env: impl Into<String>, cwd: PathBuf, shell: ShellKind) -> Self {
        Self {
            env: env.into(),
            cwd,
            shell,
            command: None,
            note: None,
        }
    }
}
```

In `hydra.rs` test `start_runs_a_real_pane`, replace the `PaneRequest { env: "work".into(), cwd: f.free.clone(), shell: ShellKind::Pwsh, command: None }` literal with `PaneRequest::new("work", f.free.clone(), ShellKind::Pwsh)`.

`tabs.rs`: add to `Tab` after `set`:

```rust
    /// The set file's title for the tab; Loose tabs have none and show their pane's label.
    pub title: Option<String>,
```

replace `add` with:

```rust
    /// Adds a one-pane Loose-style tab; see `add_tree`.
    pub fn add(&mut self, set: Option<String>, pane: PaneId) -> TabId {
        self.add_tree(set, None, Tree::Leaf(pane), pane)
    }

    /// Adds a tab after the last tab of its set (a new set goes before the Loose tabs; a
    /// Loose tab goes last) and makes it active.
    pub fn add_tree(
        &mut self,
        set: Option<String>,
        title: Option<String>,
        tree: Tree,
        focused: PaneId,
    ) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        let at = match &set {
            Some(name) => self
                .tabs
                .iter()
                .rposition(|t| t.set.as_ref() == Some(name))
                .map(|i| i + 1)
                .unwrap_or_else(|| {
                    self.tabs
                        .iter()
                        .position(|t| t.set.is_none())
                        .unwrap_or(self.tabs.len())
                }),
            None => self.tabs.len(),
        };
        self.tabs.insert(
            at,
            Tab {
                id,
                set,
                title,
                tree,
                focused,
            },
        );
        self.active = at;
        id
    }
```

`pane_view.rs`:
- In `PaneView::new`, before `let mut view = Self {`, add `let warnings: Vec<String> = request.note.iter().cloned().collect();` and in the struct literal replace `warnings: Vec::new(),` with `warnings,`.
- In `started`, replace `self.warnings = s.warnings;` with `self.warnings = self.request.note.iter().cloned().chain(s.warnings).collect();`.

`workspace.rs`:
- In `open_tab`, replace the `PaneRequest { env, cwd, shell, command: None }` literal with `PaneRequest::new(env, cwd, shell)`.
- In `split`, replace the request block with:

```rust
        let request = {
            let v = from.read(cx);
            PaneRequest::new(v.env(), v.cwd().clone(), v.request.shell)
        };
```

- In `render_tab_strip`, after `let tint = self.tint(&env);` add `let label = tab.title.clone().unwrap_or(label);`.

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui` — expected: PASS (the 3 new tests included).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui
git commit -m "feat: hydraui turns a set's tab into panes

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Sets in the window

**Files:**
- Modify: `crates/hydraui/src/workspace.rs`
- Create: `crates/hydraui/src/workspace/sets.rs`
- Test: unit tests in `workspace/sets.rs` (`unload_question`); the window behaviour is in the smoke checklist (Task 9)

**Interfaces:**
- Consumes: `layout::tab_layout`, `Tabs::add_tree`, `Tab.title` (Task 2); `hydraui_sets::{Context, LoadedSet, LoadedSets, Session, SetSpec, file_modified, load_set, read_session, write_session}`; `PaneView::prompt()`, `PaneView::label()`.
- Produces (used by Task 6): `Workspace` methods (in `workspace/sets.rs`) `load_set(&mut self, name: &str, window, cx) -> bool`, `unload_set(&mut self, name: &str, reload: bool, window, cx)`, `set_context(&self) -> hydraui_sets::Context<'_>`; `workspace::sets::unload_question(set: &str, running: &[String]) -> Option<String>`.

- [ ] **Step 1: Write the failing test.** Create `crates/hydraui/src/workspace/sets.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloading_asks_only_when_something_runs_and_names_it() {
        assert_eq!(unload_question("client-a", &[]), None);
        let q = unload_question(
            "client-a",
            &["api: npm run dev".to_string(), "api: work".to_string()],
        )
        .unwrap();
        assert!(q.contains("client-a"), "{q}");
        assert!(q.contains("  api: npm run dev\n  api: work"), "{q}");
        assert!(q.contains("stopped"), "{q}");
    }
}
```

and in `workspace.rs`, after the module doc comment, add `mod sets;`.

- [ ] **Step 2: Run it to see it fail.** `cargo test -p hydraui unloading` — expected: compile error (`unload_question` not found).

- [ ] **Step 3: Implement `workspace/sets.rs`.** Above its tests:

```rust
//! Sets in the window (spec §5.4): loading a set opens its tabs, unloading closes them
//! (asking first when commands are running), a changed file offers a reload, and the loaded
//! sets are saved for the next start.
use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{App, Context, Hsla, SharedString, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};
use hydraui_sets::{
    Context as SetContext, LoadedSet, Session, SetSpec, file_modified, load_set, read_session,
    write_session,
};
use hydraui_term::PromptState;

use super::Workspace;
use crate::layout::tab_layout;
use crate::tabs::TabId;

/// What to ask before unloading `set` when some of its panes are running a command (OSC 133);
/// `running` names each as `tab: pane`. `None`: nothing runs, so don't ask.
pub fn unload_question(set: &str, running: &[String]) -> Option<String> {
    if running.is_empty() {
        return None;
    }
    let list: Vec<String> = running.iter().map(|r| format!("  {r}")).collect();
    Some(format!(
        "{set} has commands running:\n{}\nThey will be stopped.",
        list.join("\n")
    ))
}

impl Workspace {
    pub(super) fn set_context(&self) -> SetContext<'_> {
        SetContext {
            paths: &self.hydra.paths,
            user_home: &self.hydra.user_home,
            bindings: &self.hydra.global.bindings,
            default_shell: self.hydra.shell(self.ui.default_shell.as_deref()),
        }
    }

    /// Opens a set, or focuses its first tab when it is already open. A set that doesn't
    /// validate opens nothing and says why. Returns whether the set is open now.
    pub(super) fn load_set(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(first) = self.loaded.get(name).and_then(|s| s.tabs.first().copied()) {
            self.tabs.activate(first);
            self.focus_active(window, cx);
            cx.notify();
            return true;
        }
        let spec = match load_set(&self.set_context(), name) {
            Ok(spec) => spec,
            Err(e) => {
                window.push_notification(
                    Notification::error(e.to_string()).title(format!("Set {name}")),
                    cx,
                );
                return false;
            }
        };
        self.open_set(spec, window, cx);
        true
    }

    fn open_set(&mut self, spec: SetSpec, window: &mut Window, cx: &mut Context<Self>) {
        let mut tabs = Vec::new();
        for tab in &spec.tabs {
            let next = &mut self.next_pane;
            let (tree, panes) = tab_layout(tab, &mut || {
                *next += 1;
                *next
            });
            let focused = panes[0].0;
            for (id, request) in panes {
                self.create_pane(id, request, window, cx);
            }
            tabs.push(self.tabs.add_tree(
                Some(spec.name.clone()),
                Some(tab.title.clone()),
                tree,
                focused,
            ));
        }
        self.loaded.insert(LoadedSet {
            name: spec.name.clone(),
            tabs: tabs.clone(),
            modified: file_modified(&spec.file),
        });
        self.changed_sets.remove(&spec.name);
        if let Some(first) = tabs.first() {
            self.tabs.activate(*first);
        }
        if let Some(focused) = self.tabs.active().map(|t| t.focused) {
            self.focus_soon(focused, window, cx);
        }
        self.save_session(window, cx);
        cx.notify();
    }

    /// The panes of these tabs that are running a command, as `tab: pane`.
    fn running(&self, tabs: &[TabId], cx: &App) -> Vec<String> {
        let mut out = Vec::new();
        for tab in tabs.iter().filter_map(|t| self.tabs.get(*t)) {
            for id in tab.tree.panes() {
                let Some(view) = self.panes.get(&id) else {
                    continue;
                };
                let v = view.read(cx);
                if v.prompt() == Some(PromptState::CommandRunning) {
                    let tab_name = tab.title.clone().unwrap_or_else(|| v.label());
                    out.push(format!("{tab_name}: {}", v.label()));
                }
            }
        }
        out
    }

    /// Closes a set's tabs, asking first when commands are running; `reload` opens it again
    /// from its file afterwards (spec §5.4: reload is unload + load).
    pub(super) fn unload_set(
        &mut self,
        name: &str,
        reload: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(set) = self.loaded.get(name) else {
            return;
        };
        let running = self.running(&set.tabs, cx);
        let Some(question) = unload_question(name, &running) else {
            self.close_set(name, reload, window, cx);
            return;
        };
        let workspace = cx.entity();
        let name = name.to_string();
        let verb = if reload { "Reload" } else { "Unload" };
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, set) = (workspace.clone(), name.clone());
            dialog
                .title(format!("{verb} {name}?"))
                .child(v_flex().children(question.lines().map(|l| div().child(l.to_string()))))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("confirm").label(verb).danger().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                ws.update(cx, |ws, cx| ws.close_set(&set, reload, window, cx));
                            },
                        )),
                )
        });
    }

    fn close_set(&mut self, name: &str, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.loaded.remove(name) {
            for tab in set.tabs {
                for id in self.tabs.close_tab(tab) {
                    self.drop_pane(id);
                }
            }
        }
        self.changed_sets.remove(name);
        self.save_session(window, cx);
        if !reload || !self.load_set(name, window, cx) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// A tab closed by hand: the set whose last tab it was is unloaded (spec §5.4).
    pub(super) fn tab_gone(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.loaded.tab_closed(tab) {
            self.changed_sets.remove(&set);
            self.save_session(window, cx);
        }
    }

    pub(super) fn save_session(&self, window: &mut Window, cx: &mut App) {
        let session = Session {
            sets: self.loaded.names(),
        };
        if let Err(e) = write_session(&self.hydra.paths, &session) {
            window.push_notification(
                Notification::warning(format!("can't save the open sets: {e}")).title("hydraui"),
                cx,
            );
        }
    }

    /// Notes the sets whose file changed or went away since they were loaded.
    pub(super) fn check_sets(&mut self, cx: &mut Context<Self>) {
        let changed: HashSet<String> = self.loaded.changed(&self.hydra.paths).into_iter().collect();
        if changed != self.changed_sets {
            self.changed_sets = changed;
            cx.notify();
        }
    }

    /// Reopens the sets open at last exit; one that fails is skipped with a toast (and so
    /// dropped from the session). Returns whether any opened.
    pub(super) fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let names = read_session(&self.hydra.paths).sets;
        if names.is_empty() {
            return false;
        }
        let mut any = false;
        for name in &names {
            any |= self.load_set(name, window, cx);
        }
        self.save_session(window, cx);
        any
    }

    /// A set's group label in the tab strip: collapse toggle, "changed - reload?", reload,
    /// unload (spec §7.1, §5.4).
    pub(super) fn render_set_header(&self, set: &str, muted: Hsla, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let collapsed = self.collapsed.contains(set);
        let changed = self.changed_sets.contains(set);
        let id = |what: &str| SharedString::from(format!("{what}-{set}"));
        let toggle = set.to_string();
        let changed_name = set.to_string();
        let reload_name = set.to_string();
        let unload_name = set.to_string();
        h_flex()
            .gap_1()
            .px_1()
            .child(
                div()
                    .id(id("set"))
                    .px_1()
                    .text_sm()
                    .text_color(muted)
                    .child(format!("{} {set}", if collapsed { "▸" } else { "▾" }))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if !ws.collapsed.remove(&toggle) {
                            ws.collapsed.insert(toggle.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(changed, |d| {
                d.child(
                    Button::new(id("changed"))
                        .label("changed - reload?")
                        .warning()
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.unload_set(&changed_name, true, window, cx)
                        })),
                )
            })
            .child(
                Button::new(id("reload"))
                    .label("reload")
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.unload_set(&reload_name, true, window, cx)
                    })),
            )
            .child(
                Button::new(id("unload"))
                    .label("unload")
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.unload_set(&unload_name, false, window, cx)
                    })),
            )
    }
}
```

- [ ] **Step 4: Wire it into `workspace.rs`.**
  - Imports: add `use hydraui_sets::LoadedSets;` and change `use crate::tabs::{TabId, Tabs};` to `use crate::tabs::{Closed, TabId, Tabs};`.
  - Add fields to `Workspace` after `drag: Option<Drag>,`:

```rust
    loaded: LoadedSets,
    /// Loaded sets whose file changed since loading.
    changed_sets: HashSet<String>,
    /// Set groups folded away in the tab strip.
    collapsed: HashSet<String>,
    _set_watch: Task<()>,
```

  - In `new`, after the `timer` spawn add:

```rust
        // A loaded set's file edited or deleted: offer a reload (spec §5.4).
        let set_watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                if this.update(cx, |ws, cx| ws.check_sets(cx)).is_err() {
                    break;
                }
            }
        });
```

  and in the struct literal add `loaded: LoadedSets::default(), changed_sets: HashSet::new(), collapsed: HashSet::new(), _set_watch: set_watch,`.
  - Replace `startup`'s last two lines (`let home = …; self.new_tab_in(home, window, cx);`) with:

```rust
        // The sets open at last exit (spec §5.4); a plain tab when there were none.
        if !self.restore_session(window, cx) {
            let home = self.hydra.user_home.clone();
            self.new_tab_in(home, window, cx);
        }
```

  - Split `new_pane` in two, so a set can name its panes first:

```rust
    fn new_pane(&mut self, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        self.next_pane += 1;
        let id = self.next_pane;
        self.create_pane(id, request, window, cx);
        id
    }

    fn create_pane(&mut self, id: PaneId, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) {
        let tint = self.hydra.tint(&request.env);
        self.tints.insert(request.env.clone(), tint);
        let (hydra, style) = (self.hydra.clone(), self.style.clone());
        let view = cx.new(|cx| PaneView::new(id, request, hydra, style, window, cx));
        let sub = cx.subscribe_in(&view, window, move |ws, _, ev: &PaneEvent, _, cx| {
            if *ev == PaneEvent::Focused {
                ws.tabs.focus_pane(id);
                ws.refresh_status(cx);
            }
            cx.notify();
        });
        self.subs.insert(id, sub);
        self.panes.insert(id, view);
    }
```

  - In `close_pane`, replace `self.tabs.close_pane(id);` with:

```rust
        if let Closed::Tab(tab) = self.tabs.close_pane(id) {
            self.tab_gone(tab, window, cx);
        }
```

  - In `close_tab`, after the `for id in …` loop add `self.tab_gone(tab, window, cx);`.
  - In `render_tab_strip`, replace the group label block (`strip = strip.child(div().px_2()… .child(set.unwrap_or_else(|| "Loose".into())));`) with:

```rust
            strip = match &set {
                Some(name) => strip.child(self.render_set_header(name, muted, cx)),
                None => strip.child(div().px_2().text_sm().text_color(muted).child("Loose")),
            };
            if set.as_ref().is_some_and(|s| self.collapsed.contains(s)) {
                continue;
            }
```

- [ ] **Step 5: Run the test and build.** `cargo test -p hydraui unloading` — expected: PASS. `cargo clippy -p hydraui --all-targets -- -D warnings` — expected: clean. Fix GPUI signature mismatches against the pinned source with the smallest change that keeps the documented behaviour, recording each as a ruling.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui opens, unloads, reloads and restores sets

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `Hydra`: sign-in panes, missing secrets, storing a secret

**Files:**
- Modify: `crates/hydraui/src/hydra.rs`
- Modify: `crates/hydraui/src/main.rs` only if it names `Hydra::new` (it doesn't: it uses `from_env`)
- Test: unit tests in `hydra.rs`

**Interfaces:**
- Consumes: `Provider::{auth_command, auth_unset, is_configured, id}`, `hydra_providers::{all, by_id}`, `hydra_core::secret::{SecretRef, SecretStore, MemoryStore}`, `hydra_platform::shell::sh_quote`.
- Produces (used by Tasks 5, 6, 8):
  - `PaneRequest` gains `drop_vars: Vec<String>` and `allow_missing_secrets: bool` (both off in `PaneRequest::new`)
  - `Hydra::new(paths: HydraPaths, user_home: PathBuf, store: Box<dyn SecretStore + Send + Sync>) -> (Hydra, Option<String>)`
  - `Hydra::prepare(&self, req: &PaneRequest) -> Result<LaunchEnv, String>` (replaces `prepare(&str)`)
  - `Hydra::sign_ins(&self, env: &str) -> Vec<String>` (tool ids), `Hydra::sign_in_request(&self, env: &str, tool: &str, cwd: PathBuf, shell: ShellKind) -> Result<PaneRequest, String>`, `Hydra::missing_secrets(&self, env: &str) -> Vec<String>` (`env/key` paths), `Hydra::set_secret(&self, path: &str, value: &str) -> Result<(), String>`
  - `hydra::command_line(shell: ShellKind, argv: &[String]) -> String`, `hydra::missing_secrets(env_toml: &str, store: &dyn SecretStore) -> Vec<String>`

- [ ] **Step 1: Write the failing tests.** In `hydra.rs` tests:
  - Add `use hydra_core::secret::{MemoryStore, SecretRef, SecretStore};` to the test imports.
  - In `fx`, extend the environment list to `[("work", ""), ("personal", "color = \"#112233\"\n"), ("gh", "[github]\n"), ("keyed", "[env]\nLINEAR_API_KEY = \"secret:keyed/linear\"\nPLAIN = \"x\"\n")]`, and replace the `KeyringStore::new(..)` store with `Box::new(MemoryStore::default())` (pass it to `Hydra::new`).
  - In `lists_environments`, expect `vec!["gh", "keyed", "personal", "work"]`.
  - In `a_bad_hydra_config_is_reported_not_fatal`, replace the `KeyringStore::new(..)` store with `Box::new(MemoryStore::default())`.
  - In `prepare_works_and_a_missing_environment_says_so`, `prepare_from_several_threads_at_once_all_succeed`: replace `f.hydra.prepare("work")` with `f.hydra.prepare(&PaneRequest::new("work", f.free.clone(), ShellKind::Pwsh))` and `prepare("nobody")` likewise with `"nobody"`.
  - Add:

```rust
    #[test]
    fn prepare_needs_secrets_unless_signing_in() {
        let f = fx("");
        let mut req = PaneRequest::new("keyed", f.free.clone(), ShellKind::Pwsh);
        assert!(f.hydra.prepare(&req).is_err());
        req.allow_missing_secrets = true;
        assert!(f.hydra.prepare(&req).is_ok());
    }

    #[test]
    fn missing_secrets_are_the_unstored_references() {
        let store = MemoryStore::default();
        store
            .set(&SecretRef::parse_path("work/b").unwrap(), "1")
            .unwrap();
        let text = "[env]\nA = \"secret:work/a\"\nB = \"secret:work/b\"\nC = \"plain\"\n\
                    [gws]\nlist = [\"secret:acme/x\"]\n";
        assert_eq!(missing_secrets(text, &store), vec!["acme/x", "work/a"]);
        assert!(missing_secrets("not toml [", &store).is_empty());
        let f = fx("");
        assert_eq!(f.hydra.missing_secrets("keyed"), vec!["keyed/linear"]);
        assert!(f.hydra.missing_secrets("work").is_empty());
        assert!(f.hydra.missing_secrets("nobody").is_empty());
    }

    #[test]
    fn set_secret_stores_exactly_and_refuses_blank_values() {
        let f = fx("");
        f.hydra.set_secret("keyed/linear", "  a b  ").unwrap();
        assert!(f.hydra.missing_secrets("keyed").is_empty());
        let stored = f
            .hydra
            .store
            .get(&SecretRef::parse_path("keyed/linear").unwrap())
            .unwrap();
        assert_eq!(stored.as_deref(), Some("  a b  "));
        assert!(
            f.hydra
                .set_secret("keyed/other", "   ")
                .unwrap_err()
                .contains("empty")
        );
        assert!(f.hydra.set_secret("no-slash", "x").is_err());
    }

    #[test]
    fn sign_ins_are_the_configured_tools_with_a_sign_in_command() {
        let f = fx("");
        assert_eq!(f.hydra.sign_ins("gh"), vec!["github"]);
        assert!(f.hydra.sign_ins("work").is_empty());
        assert!(f.hydra.sign_ins("nobody").is_empty());
    }

    #[test]
    fn a_sign_in_pane_runs_the_tools_own_command() {
        let f = fx("");
        let req = f
            .hydra
            .sign_in_request("gh", "github", f.free.clone(), ShellKind::Pwsh)
            .unwrap();
        assert_eq!(
            req.command.as_deref(),
            Some("& 'gh' 'auth' 'login' '--hostname' 'github.com'")
        );
        let unset: Vec<String> = hydra_providers::by_id("github")
            .unwrap()
            .auth_unset()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(req.drop_vars, unset);
        assert!(req.allow_missing_secrets);
        assert!(
            f.hydra
                .sign_in_request("work", "github", f.free.clone(), ShellKind::Pwsh)
                .unwrap_err()
                .contains("isn't configured")
        );
        assert!(
            f.hydra
                .sign_in_request("gh", "nope", f.free.clone(), ShellKind::Pwsh)
                .unwrap_err()
                .contains("nope")
        );
    }

    #[test]
    fn command_lines_quote_every_word() {
        let argv = vec!["C:\\Program Files\\x\\gh.exe".to_string(), "it's".to_string()];
        assert_eq!(
            command_line(ShellKind::Pwsh, &argv),
            "& 'C:\\Program Files\\x\\gh.exe' 'it''s'"
        );
        assert_eq!(
            command_line(ShellKind::Bash, &argv),
            "'C:\\Program Files\\x\\gh.exe' 'it'\\''s'"
        );
    }
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p hydraui hydra` — expected: compile errors (`missing_secrets`, `set_secret`, `sign_ins`, `command_line`, `allow_missing_secrets`, `Hydra::new` taking a box, …).

- [ ] **Step 3: Implement.** In `hydra.rs`:
  - Imports: `use std::collections::{BTreeSet, HashMap};`; `use hydra_core::config::{EnvConfig, GlobalConfig, load_env, load_global};`; `use hydra_core::secret::{SecretRef, SecretStore};`; `use hydra_platform::shell::{ShellKind, sh_quote};` (keep the rest).
  - `PaneRequest`: add after `note`:

```rust
    /// Variables the pane's shell must not inherit (a sign-in's `auth_unset`).
    pub drop_vars: Vec<String>,
    /// Start even when a referenced secret isn't stored (signing in is how it gets stored).
    pub allow_missing_secrets: bool,
```

  and set both in `PaneRequest::new` (`drop_vars: Vec::new(), allow_missing_secrets: false`).
  - `Hydra`: change the field to `store: Box<dyn SecretStore + Send + Sync>,`; `Hydra::new`'s parameter to `store: Box<dyn SecretStore + Send + Sync>`; in `from_env`, `let store = Box::new(KeyringStore::from_env(&paths));`.
  - Replace `prepare` with:

```rust
    /// `prepare` for a pane: guards off (spec §4.1), plus a sign-in's dropped variables and
    /// leniency about missing secrets.
    pub fn prepare(&self, req: &PaneRequest) -> Result<LaunchEnv, String> {
        let name = EnvName::parse(&req.env).map_err(|e| e.to_string())?;
        let opts = PrepareOptions {
            no_guards: true,
            allow_missing_secrets: req.allow_missing_secrets,
            drop_vars: req.drop_vars.clone(),
            inherited_env_vars: std::env::var(ENV_VARS_MARKER).ok(),
            ..Default::default()
        };
        self.prepare_in_turn(&name, &hydra_providers::all(), &opts)
            .map_err(|e| e.to_string())
    }
```

  - In `start`, `let launch = self.prepare(req)?;`.
  - Add a context helper and use it in `whoami` (replacing its `Ctx { .. }` literal, whose `secrets: &self.store` no longer type-checks):

```rust
    fn ctx<'a>(&'a self, name: &'a EnvName, config: &'a EnvConfig, warnings: &'a Warnings) -> Ctx<'a> {
        Ctx {
            name,
            env: config,
            paths: &self.paths,
            user_home: &self.user_home,
            secrets: self.store.as_ref(),
            guards: false,
            warnings,
        }
    }
```

  (in `whoami`: `let ctx = self.ctx(&name, &launch.config, &warnings);`).
  - Add the new methods to `impl Hydra`:

```rust
    /// The tools in `env` that have a sign-in command (what `hydra auth <tool> <env>`
    /// accepts), by id.
    pub fn sign_ins(&self, env: &str) -> Vec<String> {
        let Ok(name) = EnvName::parse(env) else {
            return Vec::new();
        };
        let Ok(config) = load_env(&self.paths, &name) else {
            return Vec::new();
        };
        let warnings = Warnings::default();
        let ctx = self.ctx(&name, &config, &warnings);
        hydra_providers::all()
            .iter()
            .filter(|p| p.is_configured(&config) && p.auth_command(&ctx).is_some())
            .map(|p| p.id().to_string())
            .collect()
    }

    /// A pane that signs `tool` in for `env` as `hydra auth <tool> <env>` does (spec §7.4):
    /// the tool's sign-in command as the startup command, its `auth_unset` variables dropped,
    /// missing secrets allowed.
    pub fn sign_in_request(
        &self,
        env: &str,
        tool: &str,
        cwd: PathBuf,
        shell: ShellKind,
    ) -> Result<PaneRequest, String> {
        let p = hydra_providers::by_id(tool).ok_or_else(|| format!("unknown tool {tool:?}"))?;
        let mut req = PaneRequest::new(env, cwd, shell);
        req.allow_missing_secrets = true;
        req.drop_vars = p.auth_unset().iter().map(|s| s.to_string()).collect();
        let launch = self.prepare(&req)?;
        if !p.is_configured(&launch.config) {
            return Err(format!(
                "{id} isn't configured for {env}; add a [{id}] section to its env.toml",
                id = p.id()
            ));
        }
        let warnings = Warnings::default();
        let argv = p
            .auth_command(&self.ctx(&launch.name, &launch.config, &warnings))
            .ok_or_else(|| {
                format!(
                    "{} has no sign-in command; store its secret with Set secret",
                    p.id()
                )
            })?;
        req.command = Some(command_line(shell, &argv));
        Ok(req)
    }

    /// The `secret:` references in `env`'s env.toml that aren't stored yet, as `env/key`.
    pub fn missing_secrets(&self, env: &str) -> Vec<String> {
        let Ok(name) = EnvName::parse(env) else {
            return Vec::new();
        };
        match std::fs::read_to_string(self.paths.env_file(&name)) {
            Ok(text) => missing_secrets(&text, self.store.as_ref()),
            Err(_) => Vec::new(),
        }
    }

    /// Stores a secret through hydra's store, which also keeps secrets.toml's index. The
    /// value is kept exactly as typed; an empty or blank one is refused.
    pub fn set_secret(&self, path: &str, value: &str) -> Result<(), String> {
        let r = SecretRef::parse_path(path.trim()).map_err(|e| e.to_string())?;
        if value.trim().is_empty() {
            return Err(format!("{}: the value is empty", r.path()));
        }
        self.store.set(&r, value).map_err(|e| e.to_string())
    }
```

  - Add the free functions (after `impl Hydra`):

```rust
/// `argv` as one command line for `shell`, every word quoted, so paths with spaces or
/// apostrophes stay one word.
pub fn command_line(shell: ShellKind, argv: &[String]) -> String {
    match shell {
        ShellKind::Pwsh => {
            let words: Vec<String> = argv
                .iter()
                .map(|a| format!("'{}'", a.replace('\'', "''")))
                .collect();
            format!("& {}", words.join(" "))
        }
        ShellKind::Bash => argv
            .iter()
            .map(|a| sh_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The `secret:` references anywhere in env.toml text that `store` doesn't hold, as sorted
/// `env/key` paths. Text that isn't TOML has none.
pub fn missing_secrets(env_toml: &str, store: &dyn SecretStore) -> Vec<String> {
    let Ok(table) = toml::from_str::<toml::Table>(env_toml.strip_prefix('\u{feff}').unwrap_or(env_toml))
    else {
        return Vec::new();
    };
    let mut refs = BTreeSet::new();
    collect_refs(&toml::Value::Table(table), &mut refs);
    refs.into_iter()
        .filter(|r| matches!(store.get(r), Ok(None)))
        .map(|r| r.path())
        .collect()
}

fn collect_refs(value: &toml::Value, out: &mut BTreeSet<SecretRef>) {
    match value {
        toml::Value::String(s) => {
            if let Ok(Some(r)) = SecretRef::parse_value(s) {
                out.insert(r);
            }
        }
        toml::Value::Array(items) => items.iter().for_each(|v| collect_refs(v, out)),
        toml::Value::Table(t) => t.values().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}
```

  - `crates/hydraui/Cargo.toml` already has `toml`; nothing to add.

- [ ] **Step 4: Run the tests.** `cargo test -p hydraui hydra` — expected: PASS (all, including the 6 new tests). `Hydra::prepare`'s other callers (`workspace.rs` doesn't call it) need no change.

- [ ] **Step 5: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui sign-in panes, missing secrets and storing a secret

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Launcher entries

**Files:**
- Modify: `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/launcher.rs`
- Test: unit tests in `launcher.rs`

**Interfaces:**
- Produces (used by Task 6):
  - `launcher::SetState::{Loaded, Unloaded, Invalid(String)}`
  - `launcher::Action::{LoadSet(String), UnloadSet(String), OpenEnv(String), SignIn { env: String, tool: String }, SetSecret(String), NewSecret, OpenConfigFolder, ReloadConfig, Problem(String)}` (`Debug, Clone, PartialEq, Eq`)
  - `launcher::Entry { label: String, action: Action }`
  - `launcher::entries(sets: &[(String, SetState)], envs: &[String], sign_ins: &[(String, String)], secrets: &[String]) -> Vec<Entry>`

- [ ] **Step 1: Add the module.** In `crates/hydraui/src/lib.rs`, add `pub mod launcher;` after `pub mod keys;`.

- [ ] **Step 2: Write the failing test.** `crates/hydraui/src/launcher.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_cover_sets_environments_sign_in_secrets_and_config() {
        let sets = [
            ("client-a".to_string(), SetState::Loaded),
            ("personal".to_string(), SetState::Unloaded),
            ("broken".to_string(), SetState::Invalid("line 3: unknown field".into())),
        ];
        let got = entries(
            &sets,
            &["work".to_string()],
            &[("work".to_string(), "github".to_string())],
            &["work/linear".to_string()],
        );
        let labels: Vec<&str> = got.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Set: client-a (loaded)",
                "Unload set: client-a",
                "Set: personal",
                "Set: broken (invalid)",
                "New tab: work",
                "Sign in: github in work",
                "Set secret: work/linear",
                "Set secret: other…",
                "Open config folder",
                "Reload config",
            ]
        );
        assert_eq!(got[0].action, Action::LoadSet("client-a".into()));
        assert_eq!(got[1].action, Action::UnloadSet("client-a".into()));
        assert_eq!(got[3].action, Action::Problem("line 3: unknown field".into()));
        assert_eq!(got[4].action, Action::OpenEnv("work".into()));
        assert_eq!(
            got[5].action,
            Action::SignIn {
                env: "work".into(),
                tool: "github".into()
            }
        );
        assert_eq!(got[6].action, Action::SetSecret("work/linear".into()));
        assert_eq!(got[7].action, Action::NewSecret);
    }

    #[test]
    fn with_nothing_configured_only_the_fixed_entries_remain() {
        let got = entries(&[], &[], &[], &[]);
        let labels: Vec<&str> = got.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Set secret: other…", "Open config folder", "Reload config"]
        );
    }
}
```

- [ ] **Step 3: Run it to see it fail.** `cargo test -p hydraui launcher` — expected: compile errors (`entries`, `SetState`, `Action` not found).

- [ ] **Step 4: Implement.** Above the tests in `launcher.rs`:

```rust
//! The launcher's entries (spec §7.4): sets, environments, sign-in, secrets and config, each
//! with what Enter does. The palette filters them by substring.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetState {
    Loaded,
    Unloaded,
    /// Why the set file doesn't validate.
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Opens the set, or focuses it when it is already loaded.
    LoadSet(String),
    UnloadSet(String),
    /// A Loose tab in the environment.
    OpenEnv(String),
    SignIn { env: String, tool: String },
    /// Store a value for this `env/key`.
    SetSecret(String),
    /// Store a value for an `env/key` typed in the dialog.
    NewSecret,
    OpenConfigFolder,
    ReloadConfig,
    /// Says why the entry can't be used (an invalid set).
    Problem(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub action: Action,
}

pub fn entries(
    sets: &[(String, SetState)],
    envs: &[String],
    sign_ins: &[(String, String)],
    secrets: &[String],
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut add = |label: String, action: Action| out.push(Entry { label, action });
    for (name, state) in sets {
        match state {
            SetState::Loaded => {
                add(format!("Set: {name} (loaded)"), Action::LoadSet(name.clone()));
                add(format!("Unload set: {name}"), Action::UnloadSet(name.clone()));
            }
            SetState::Unloaded => add(format!("Set: {name}"), Action::LoadSet(name.clone())),
            SetState::Invalid(e) => {
                add(format!("Set: {name} (invalid)"), Action::Problem(e.clone()))
            }
        }
    }
    for env in envs {
        add(format!("New tab: {env}"), Action::OpenEnv(env.clone()));
    }
    for (env, tool) in sign_ins {
        add(
            format!("Sign in: {tool} in {env}"),
            Action::SignIn {
                env: env.clone(),
                tool: tool.clone(),
            },
        );
    }
    for path in secrets {
        add(format!("Set secret: {path}"), Action::SetSecret(path.clone()));
    }
    add("Set secret: other…".into(), Action::NewSecret);
    add("Open config folder".into(), Action::OpenConfigFolder);
    add("Reload config".into(), Action::ReloadConfig);
    out
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui launcher` — expected: PASS (2 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui launcher entries

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: The launcher in the window

**Files:**
- Modify: `crates/hydraui/src/workspace.rs`, `crates/hydraui/src/pane_view.rs` (`set_style`)
- Create: `crates/hydraui/src/workspace/launcher.rs`
- Test: none automated (GPUI); smoke checklist (Task 9)

**Interfaces:**
- Consumes: `launcher::{entries, Action, SetState}` (Task 5); `Hydra::{sign_ins, sign_in_request, missing_secrets, set_secret, env_names}` (Task 4); `Workspace::{load_set, unload_set, set_context}` (Task 3); `keys::Launcher` (Task 1); `hydraui_sets::{list_sets, load_set}`; `config::load`, `Palette::from_config`.
- Produces: `Workspace::open_launcher(&mut self, &Launcher, window, cx)` (bound in `render`); `PaneView::set_style(&mut self, Arc<TermStyle>, cx)`; `Workspace::focused_cwd(&self, &App) -> PathBuf`, `Workspace::open_request(&mut self, PaneRequest, window, cx)` (Task 8 uses `focused_cwd`).

- [ ] **Step 1: `PaneView::set_style`.** In `pane_view.rs`, add to `impl PaneView` after `label`:

```rust
    /// New font and colours (Reload config): drawn from the next frame.
    pub fn set_style(&mut self, style: Arc<TermStyle>, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }
```

- [ ] **Step 2: Shared helpers in `workspace.rs`.**
  - Add `mod launcher;` after `mod sets;`.
  - Add, in `impl Workspace`:

```rust
    /// The focused pane's folder, else the user's home.
    fn focused_cwd(&self, cx: &App) -> PathBuf {
        self.focused()
            .map(|v| v.read(cx).cwd().clone())
            .unwrap_or_else(|| self.hydra.user_home.clone())
    }

    /// A Loose tab running `request`.
    fn open_request(&mut self, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.new_pane(request, window, cx);
        self.tabs.add(None, id);
        self.focus_soon(id, window, cx);
        cx.notify();
    }
```

  - Replace `open_tab`'s body with:

```rust
        let shell = self.hydra.shell(self.ui.default_shell.as_deref());
        self.open_request(PaneRequest::new(env, cwd, shell), window, cx);
```

  - Replace `new_tab`'s body with `let cwd = self.focused_cwd(cx); self.new_tab_in(cwd, window, cx);`.
  - In `render`, add `.on_action(cx.listener(Self::open_launcher))` after `.on_action(cx.listener(Self::previous_tab))`.

- [ ] **Step 3: Write `workspace/launcher.rs`.**

```rust
//! The launcher (spec §7.4): Ctrl+Shift+P opens a palette over sets, environments, sign-in,
//! secrets and config, and Enter runs the entry's action.
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Window, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{Theme, ThemeMode, WindowExt, h_flex, v_flex};
use hydraui_sets::{list_sets, load_set};

use super::Workspace;
use crate::config::{self, ThemeChoice};
use crate::hydra::Hydra;
use crate::keys::Launcher;
use crate::launcher::{Action, SetState, entries};
use crate::palette::Palette;
use crate::pane_view::TermStyle;

impl Workspace {
    pub(super) fn open_launcher(&mut self, _: &Launcher, window: &mut Window, cx: &mut Context<Self>) {
        let ctx = self.set_context();
        let sets: Vec<(String, SetState)> = list_sets(&self.hydra.paths)
            .unwrap_or_default()
            .into_iter()
            .map(|name| {
                let state = if self.loaded.get(&name).is_some() {
                    SetState::Loaded
                } else {
                    match load_set(&ctx, &name) {
                        Ok(_) => SetState::Unloaded,
                        Err(e) => SetState::Invalid(e.to_string()),
                    }
                };
                (name, state)
            })
            .collect();
        let envs = self.hydra.env_names();
        let sign_ins: Vec<(String, String)> = envs
            .iter()
            .flat_map(|e| self.hydra.sign_ins(e).into_iter().map(move |t| (e.clone(), t)))
            .collect();
        let secrets: Vec<String> = envs
            .iter()
            .flat_map(|e| self.hydra.missing_secrets(e))
            .collect();
        let list = Rc::new(entries(&sets, &envs, &sign_ins, &secrets));
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (chosen, ws) = (list.clone(), workspace.clone());
            dialog.title("hydraui").child(
                Command::new(&state)
                    .items(list.iter().map(|e| CommandItem::new().label(e.label.clone())))
                    .placeholder("Set, environment, sign in, secret…")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(entry) = chosen.get(ix.row) {
                            let action = entry.action.clone();
                            window.close_dialog(cx);
                            ws.update(cx, |ws, cx| ws.run(action, window, cx));
                        }
                    }),
            )
        });
    }

    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::LoadSet(name) => {
                self.load_set(&name, window, cx);
            }
            Action::UnloadSet(name) => self.unload_set(&name, false, window, cx),
            Action::OpenEnv(env) => {
                let cwd = self.focused_cwd(cx);
                self.open_tab(env, cwd, window, cx);
            }
            Action::SignIn { env, tool } => {
                let cwd = self.focused_cwd(cx);
                let shell = self.hydra.shell(self.ui.default_shell.as_deref());
                match self.hydra.sign_in_request(&env, &tool, cwd, shell) {
                    Ok(request) => self.open_request(request, window, cx),
                    Err(e) => window.push_notification(Notification::error(e).title("Sign in"), cx),
                }
            }
            Action::SetSecret(path) => self.secret_dialog(Some(path), window, cx),
            Action::NewSecret => self.secret_dialog(None, window, cx),
            Action::OpenConfigFolder => {
                let dir = self.hydra.paths.root().join("hydraui");
                let _ = std::fs::create_dir_all(&dir);
                cx.open_with_system(&dir);
            }
            Action::ReloadConfig => self.reload_config(window, cx),
            Action::Problem(message) => {
                window.push_notification(Notification::error(message).title("hydraui"), cx)
            }
        }
    }

    /// Asks for a secret's value in a masked box (and its `env/key` when not given) and stores
    /// it through hydra's store.
    fn secret_dialog(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let fixed = path.is_some();
        let name = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder("env/key, like work/linear");
            match path {
                Some(p) => input.default_value(p),
                None => input,
            }
        });
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("Value").masked(true));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                let target = if fixed { &value } else { &name };
                target.update(cx, |s, cx| s.focus(window, cx));
            }
            let (n, v, ws) = (name.clone(), value.clone(), workspace.clone());
            dialog
                .title("Set secret")
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&name))
                        .child(Input::new(&value).mask_toggle()),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("save").label("Save").primary().on_click(
                            move |_, window, cx| {
                                let path = n.read(cx).value().to_string();
                                let secret = v.read(cx).value().to_string();
                                match ws.read(cx).hydra.set_secret(&path, &secret) {
                                    Ok(()) => {
                                        window.close_dialog(cx);
                                        window.push_notification(
                                            Notification::success(format!("stored {}", path.trim()))
                                                .title("Set secret"),
                                            cx,
                                        );
                                    }
                                    Err(e) => window.push_notification(
                                        Notification::error(e).title("Set secret"),
                                        cx,
                                    ),
                                }
                            },
                        )),
                )
        });
    }

    /// Reads hydraui's and hydra's config.toml again: font, colours, theme, default
    /// environment and shell, and hydra's bindings apply now (new and open panes); key
    /// changes apply at the next start, since rebinding would also clear gpui-component's
    /// own keys.
    fn reload_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (ui, mut problems) = config::load(&self.hydra.paths);
        let (palette, colour_problems) = Palette::from_config(&ui.colors);
        problems.extend(colour_problems);
        if ui.keys != self.ui.keys {
            problems.push("key changes apply when hydraui next starts".into());
        }
        match Hydra::from_env() {
            Ok((hydra, problem)) => {
                problems.extend(problem);
                self.hydra = Arc::new(hydra);
            }
            Err(e) => problems.push(e),
        }
        let mode = match ui.theme {
            ThemeChoice::Dark => ThemeMode::Dark,
            ThemeChoice::Light => ThemeMode::Light,
        };
        Theme::change(mode, None, cx);
        self.style = Arc::new(TermStyle {
            font: ui.font.clone().into(),
            font_size: px(ui.font_size),
            palette,
        });
        let style = self.style.clone();
        for view in self.panes.values() {
            view.update(cx, |v, cx| v.set_style(style.clone(), cx));
        }
        let envs: HashSet<String> = self.tints.keys().cloned().collect();
        for env in envs {
            let tint = self.hydra.tint(&env);
            self.tints.insert(env, tint);
        }
        self.ui = ui;
        window.push_notification(Notification::info("Settings reloaded").title("hydraui"), cx);
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        cx.notify();
    }
}
```

- [ ] **Step 4: Build.** `cargo clippy -p hydraui --all-targets -- -D warnings` and `cargo test -p hydraui` — expected: clean / PASS. Fix GPUI signature mismatches against the pinned source with the smallest change that keeps the documented behaviour; record each as a ruling.

- [ ] **Step 5: fmt, clippy, workspace tests, commit.**

```bash
git add crates/hydraui
git commit -m "feat: hydraui launcher with sets, sign-in, secrets and config

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: File tree model

**Files:**
- Modify: `Cargo.toml` (workspace: `ignore`), `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`
- Create: `crates/hydraui/src/file_tree.rs`
- Test: unit tests in `file_tree.rs`

**Interfaces:**
- Consumes: `hydra_platform::shell::{ShellKind, sh_quote}`; `ignore::WalkBuilder`.
- Produces (used by Task 8):
  - `file_tree::Entry { path: PathBuf, name: String, is_dir: bool }`, `file_tree::Row { entry: Entry, depth: usize, expanded: bool }`
  - `file_tree::list_dir(&Path) -> Vec<Entry>`
  - `file_tree::FileTree`: `new(PathBuf)`, `root(&self) -> &Path`, `set_root(&mut self, PathBuf) -> bool`, `toggle(&mut self, &Path)`, `open_dirs(&self) -> impl Iterator<Item = &PathBuf>`, `refresh(&mut self, &Path)`, `rows(&mut self) -> Vec<Row>`
  - `file_tree::quote_path(ShellKind, &Path) -> String`, `file_tree::cd_command(ShellKind, &Path) -> String`

- [ ] **Step 1: Add the dependency and module.** Root `Cargo.toml` `[workspace.dependencies]`, after `futures = "0.3"`:

```toml
ignore = "0.4"
notify = "8"
```

`crates/hydraui/Cargo.toml` `[dependencies]`, after `futures.workspace = true`:

```toml
ignore.workspace = true
```

`crates/hydraui/src/lib.rs`: add `pub mod file_tree;` after `pub mod config;`.

- [ ] **Step 2: Write the failing tests.** `crates/hydraui/src/file_tree.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// src/ (with inner.rs), target/ (ignored), Zeta/, .git/, and files.
    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for dir in ["src", "target", "Zeta", ".git"] {
            std::fs::create_dir_all(p.join(dir)).unwrap();
        }
        for file in ["b.txt", "A.md", "x.log", ".env", "src/inner.rs", "target/out.bin"] {
            std::fs::write(p.join(file), "").unwrap();
        }
        std::fs::write(p.join(".gitignore"), "target/\n*.log\n").unwrap();
        d
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn a_listing_is_one_level_folders_first_without_ignored_files() {
        let d = project();
        let entries = list_dir(d.path());
        assert_eq!(
            names(&entries),
            vec!["src", "Zeta", ".env", ".gitignore", "A.md", "b.txt"]
        );
        assert!(entries[0].is_dir && !entries[2].is_dir);
        assert_eq!(entries[0].path, d.path().join("src"));
    }

    #[test]
    fn open_folders_show_their_entries_below_them() {
        let d = project();
        let mut tree = FileTree::new(d.path().to_path_buf());
        assert_eq!(tree.rows().len(), 6);
        tree.toggle(&d.path().join("src"));
        let rows = tree.rows();
        assert_eq!(rows[0].entry.name, "src");
        assert!(rows[0].expanded);
        assert_eq!((rows[1].entry.name.as_str(), rows[1].depth), ("inner.rs", 1));
        assert_eq!(tree.open_dirs().count(), 1);
        tree.toggle(&d.path().join("src"));
        assert_eq!(tree.rows().len(), 6);
    }

    #[test]
    fn listings_are_kept_until_refreshed() {
        let d = project();
        let mut tree = FileTree::new(d.path().to_path_buf());
        assert_eq!(tree.rows().len(), 6);
        std::fs::write(d.path().join("new.txt"), "").unwrap();
        assert_eq!(tree.rows().len(), 6);
        tree.refresh(d.path());
        assert_eq!(tree.rows().len(), 7);
    }

    #[test]
    fn a_new_root_starts_closed() {
        let d = project();
        let mut tree = FileTree::new(d.path().to_path_buf());
        tree.toggle(&d.path().join("src"));
        assert!(!tree.set_root(d.path().to_path_buf()));
        assert!(tree.set_root(d.path().join("src")));
        assert_eq!(tree.root(), d.path().join("src"));
        assert_eq!(tree.open_dirs().count(), 0);
        assert_eq!(names(&tree.rows().into_iter().map(|r| r.entry).collect::<Vec<_>>()), vec!["inner.rs"]);
    }

    #[test]
    fn paths_are_quoted_for_each_shell() {
        let p = Path::new("C:\\work\\it's [here]");
        assert_eq!(quote_path(ShellKind::Pwsh, p), "'C:\\work\\it''s [here]'");
        assert_eq!(quote_path(ShellKind::Bash, p), "'C:/work/it'\\''s [here]'");
        assert_eq!(
            cd_command(ShellKind::Pwsh, p),
            "Set-Location -LiteralPath 'C:\\work\\it''s [here]'\r"
        );
        assert_eq!(cd_command(ShellKind::Bash, p), "cd 'C:/work/it'\\''s [here]'\r");
    }
}
```

- [ ] **Step 3: Run them to see them fail.** `cargo test -p hydraui file_tree` — expected: compile errors (`list_dir`, `FileTree`, `quote_path`, `cd_command` not found).

- [ ] **Step 4: Implement.** Above the tests in `file_tree.rs`:

```rust
//! The file tree's model (spec §7.3): folders read one level at a time when first shown,
//! `.gitignore` respected, and the shell text for inserting a path or changing folder.
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use hydra_platform::shell::{ShellKind, sh_quote};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// A folder's entries: folders first, then by name ignoring case; without what `.gitignore`
/// files in the folder or above it exclude (repository or not), and without `.git`. One level
/// only, so a big folder costs nothing until it is opened.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| Entry {
            is_dir: e.file_type().is_some_and(|t| t.is_dir()),
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.into_path(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// A visible row: an entry at its depth below the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub entry: Entry,
    pub depth: usize,
    pub expanded: bool,
}

/// The tree under a root: which folders are open, and their listings, read when first shown.
#[derive(Debug, Default)]
pub struct FileTree {
    root: PathBuf,
    open: BTreeSet<PathBuf>,
    listings: HashMap<PathBuf, Vec<Entry>>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ..Default::default()
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Moves to a new root (the focused pane's folder), starting closed. Returns whether it
    /// changed.
    pub fn set_root(&mut self, root: PathBuf) -> bool {
        if root == self.root {
            return false;
        }
        *self = FileTree::new(root);
        true
    }

    pub fn toggle(&mut self, dir: &Path) {
        if !self.open.remove(dir) {
            self.open.insert(dir.to_path_buf());
        }
    }

    /// The open folders, for the watcher.
    pub fn open_dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.open.iter()
    }

    /// Forgets a folder's listing so it is read again (after a change on disk).
    pub fn refresh(&mut self, dir: &Path) {
        self.listings.remove(dir);
    }

    /// The root's entries, with each open folder's entries below it.
    pub fn rows(&mut self) -> Vec<Row> {
        let mut out = Vec::new();
        let root = self.root.clone();
        self.push_rows(&root, 0, &mut out);
        out
    }

    fn push_rows(&mut self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let entries = self
            .listings
            .entry(dir.to_path_buf())
            .or_insert_with(|| list_dir(dir))
            .clone();
        for entry in entries {
            let expanded = entry.is_dir && self.open.contains(&entry.path);
            let path = entry.path.clone();
            out.push(Row {
                entry,
                depth,
                expanded,
            });
            if expanded {
                self.push_rows(&path, depth + 1, out);
            }
        }
    }
}

/// A path as one word for the shell: single-quoted for pwsh, and single-quoted with forward
/// slashes for Git Bash.
pub fn quote_path(shell: ShellKind, path: &Path) -> String {
    let text = path.to_string_lossy();
    match shell {
        ShellKind::Pwsh => format!("'{}'", text.replace('\'', "''")),
        ShellKind::Bash => sh_quote(&text.replace('\\', "/")),
    }
}

/// What double-clicking a folder types: change folder, then Enter. pwsh uses `-LiteralPath`
/// so `[` and `]` in a name aren't wildcards.
pub fn cd_command(shell: ShellKind, dir: &Path) -> String {
    match shell {
        ShellKind::Pwsh => format!("Set-Location -LiteralPath {}\r", quote_path(shell, dir)),
        ShellKind::Bash => format!("cd {}\r", quote_path(shell, dir)),
    }
}
```

- [ ] **Step 5: Run the tests.** `cargo test -p hydraui file_tree` — expected: PASS (5 tests).

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui
git commit -m "feat: hydraui file tree model

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: The file tree panel

**Files:**
- Modify: `crates/hydraui/Cargo.toml`, `crates/hydraui/src/lib.rs`, `crates/hydraui/src/workspace.rs`, `crates/hydraui/src/pane_view.rs`
- Create: `crates/hydraui/src/file_tree_view.rs`
- Test: none automated (GPUI); smoke checklist (Task 9)

**Interfaces:**
- Consumes: `file_tree::{FileTree, Row, quote_path, cd_command}` (Task 7); `keys::FileTree` (Task 1); `Workspace::focused_cwd` (Task 6); `hydra_platform::process::resolve_program(program: &str, path: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf>`.
- Produces: `file_tree_view::{FileTreeView, TreeEvent::{Insert(PathBuf), Open(PathBuf), Cd(PathBuf)}}`; `PaneEvent::PromptChanged`; `PaneView::insert_text(&mut self, &str, cx)`.

- [ ] **Step 1: Pane additions.** In `pane_view.rs`:
  - Add `PromptChanged,` to `enum PaneEvent` (after `CwdChanged,`).
  - In `on_term_event`, replace `TermEvent::Prompt(p) => self.prompt = Some(p),` with:

```rust
            TermEvent::Prompt(p) => {
                self.prompt = Some(p);
                cx.emit(PaneEvent::PromptChanged);
            }
```

  - Add to `impl PaneView` after `set_style`:

```rust
    /// Text for the shell as if typed (a path from the file tree, a `cd`).
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.send(text.as_bytes(), cx);
    }
```

- [ ] **Step 2: Add the dependency and module.** `crates/hydraui/Cargo.toml` `[dependencies]`, after `ignore.workspace = true`: `notify.workspace = true`. `lib.rs`: add `pub mod file_tree_view;` after `pub mod file_tree;`.

- [ ] **Step 3: Write `file_tree_view.rs`.**

```rust
//! The file tree panel (spec §7.3): Ctrl+B shows it, rooted at the focused pane's folder.
//! Clicks ask the workspace to insert a path, open a file, or change folder.
use std::collections::BTreeSet;
use std::path::PathBuf;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{ClickEvent, Context, EventEmitter, SharedString, Task, Window, div, px};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, v_flex};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::file_tree::{FileTree, Row};

/// What a click asks the workspace to do with the focused pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEvent {
    /// Insert the file's quoted path at the cursor.
    Insert(PathBuf),
    /// Open the file in `editor`, else with the OS default.
    Open(PathBuf),
    /// Change the shell's folder (only offered at a prompt).
    Cd(PathBuf),
}

pub struct FileTreeView {
    tree: FileTree,
    can_cd: bool,
    watcher: Option<RecommendedWatcher>,
    /// The root and the open folders, each watched (not recursively).
    watched: BTreeSet<PathBuf>,
    _pump: Task<()>,
}

impl EventEmitter<TreeEvent> for FileTreeView {}

/// A watcher that reports the folder of each changed path; `None` if one can't be made (the
/// tree then just doesn't update by itself).
fn watcher(tx: UnboundedSender<PathBuf>) -> Option<RecommendedWatcher> {
    notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                if let Some(dir) = path.parent() {
                    let _ = tx.unbounded_send(dir.to_path_buf());
                }
            }
        }
    })
    .ok()
}

impl FileTreeView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<PathBuf>();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(dir) = rx.next().await {
                let refreshed = this.update(cx, |view, cx| {
                    view.tree.refresh(&dir);
                    cx.notify();
                });
                if refreshed.is_err() {
                    break;
                }
            }
        });
        let mut view = Self {
            tree: FileTree::new(root),
            can_cd: false,
            watcher: watcher(tx),
            watched: BTreeSet::new(),
            _pump: pump,
        };
        view.sync_watches();
        view
    }

    /// Follows the focused pane's folder.
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.tree.set_root(root) {
            self.sync_watches();
            cx.notify();
        }
    }

    /// Double-click `cd` works only while the focused pane is at its prompt (spec §7.3).
    pub fn set_can_cd(&mut self, can_cd: bool, cx: &mut Context<Self>) {
        if self.can_cd != can_cd {
            self.can_cd = can_cd;
            cx.notify();
        }
    }

    fn sync_watches(&mut self) {
        let want: BTreeSet<PathBuf> = std::iter::once(self.tree.root().to_path_buf())
            .chain(self.tree.open_dirs().cloned())
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

    fn click(&mut self, row: &Row, ev: &ClickEvent, cx: &mut Context<Self>) {
        let path = row.entry.path.clone();
        if row.entry.is_dir {
            // A double-click's first click already opened or closed it: the second undoes that.
            self.tree.toggle(&path);
            if ev.click_count() >= 2 && self.can_cd {
                cx.emit(TreeEvent::Cd(path));
            }
            self.sync_watches();
            cx.notify();
        } else if ev.modifiers().control {
            cx.emit(TreeEvent::Open(path));
        } else {
            cx.emit(TreeEvent::Insert(path));
        }
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let can_cd = self.can_cd;
        let root = self.tree.root().display().to_string();
        let rows = self.tree.rows();
        v_flex()
            .size_full()
            .text_sm()
            .child(div().px_2().py_1().text_color(muted).truncate().child(root))
            .child(
                v_flex()
                    .id("file-tree")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows.into_iter().enumerate().map(|(i, row)| {
                        let marker = match (row.entry.is_dir, row.expanded) {
                            (true, true) => "▾ ",
                            (true, false) => "▸ ",
                            _ => "  ",
                        };
                        let label = format!("{marker}{}", row.entry.name);
                        let tip: SharedString = if !row.entry.is_dir {
                            "Click: insert the path · Ctrl+click: open".into()
                        } else if can_cd {
                            "Double-click: cd here".into()
                        } else {
                            "Double-click cd works when the shell is at its prompt".into()
                        };
                        let indent = px(8. + 12. * row.depth as f32);
                        div()
                            .id(("row", i))
                            .pl(indent)
                            .pr_2()
                            .whitespace_nowrap()
                            .hover(move |d| d.bg(hover))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .on_click(cx.listener(move |view, ev: &ClickEvent, _, cx| {
                                view.click(&row, ev, cx)
                            }))
                            .child(label)
                    })),
            )
    }
}
```

- [ ] **Step 4: Wire it into `workspace.rs`.**
  - Imports: `use std::path::Path;` (with `PathBuf`), `use hydraui_term::PromptState;`, `use hydra_platform::process::resolve_program;`, `use crate::file_tree::{cd_command, quote_path};`, `use crate::file_tree_view::{FileTreeView, TreeEvent};`, and add `FileTree` to the `crate::keys::{..}` import.
  - Fields after `_set_watch`:

```rust
    tree: Option<Entity<FileTreeView>>,
    tree_sub: Option<Subscription>,
```

  initialised `tree: None, tree_sub: None,`.
  - In `create_pane`'s subscription, replace the closure body with:

```rust
            if *ev == PaneEvent::Focused {
                ws.tabs.focus_pane(id);
                ws.refresh_status(cx);
            }
            ws.sync_tree(cx);
            cx.notify();
```

  - Add to `impl Workspace`:

```rust
    fn toggle_tree(&mut self, _: &FileTree, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree.take().is_some() {
            self.tree_sub = None;
        } else {
            let root = self.focused_cwd(cx);
            let view = cx.new(|cx| FileTreeView::new(root, cx));
            self.tree_sub = Some(cx.subscribe_in(&view, window, |ws, _, ev: &TreeEvent, window, cx| {
                ws.on_tree_event(ev, window, cx)
            }));
            self.tree = Some(view);
            self.sync_tree(cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Roots the tree at the focused pane's folder; `cd` only while it is at its prompt.
    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let (Some(tree), Some(view)) = (self.tree.clone(), self.focused()) else {
            return;
        };
        let (cwd, at_prompt) = {
            let v = view.read(cx);
            (v.cwd().clone(), v.prompt() == Some(PromptState::AtPrompt))
        };
        tree.update(cx, |t, cx| {
            t.set_root(cwd, cx);
            t.set_can_cd(at_prompt, cx);
        });
    }

    fn on_tree_event(&mut self, ev: &TreeEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.focused() else {
            return;
        };
        match ev {
            TreeEvent::Insert(path) => view.update(cx, |v, cx| {
                let text = format!("{} ", quote_path(v.request.shell, path));
                v.insert_text(&text, cx);
            }),
            TreeEvent::Cd(dir) => view.update(cx, |v, cx| {
                if v.prompt() == Some(PromptState::AtPrompt) {
                    let text = cd_command(v.request.shell, dir);
                    v.insert_text(&text, cx);
                }
            }),
            TreeEvent::Open(path) => self.open_file(path, window, cx),
        }
        self.focus_active(window, cx);
    }

    /// Opens a file in `editor` (a program name or path; `.cmd` launchers such as VS Code's
    /// `code` work), else with the OS default.
    fn open_file(&self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.ui.editor.as_deref() else {
            cx.open_with_system(path);
            return;
        };
        let search = std::env::var_os("PATH").unwrap_or_default();
        let program = resolve_program(editor, &search, std::env::var_os("PATHEXT").as_deref())
            .unwrap_or_else(|| PathBuf::from(editor));
        let script = program
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
        let mut cmd = if script {
            let mut c = std::process::Command::new("cmd");
            c.arg("/c").arg(&program);
            c
        } else {
            std::process::Command::new(&program)
        };
        if let Err(e) = cmd.arg(path).spawn() {
            window.push_notification(
                Notification::error(format!("can't start {editor}: {e}")).title("Open file"),
                cx,
            );
        }
    }
```

  - In `render`, add `.on_action(cx.listener(Self::toggle_tree))` after `.on_action(cx.listener(Self::open_launcher))`, and replace `.child(div().flex_1().min_h_0().child(body))` with:

```rust
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .when_some(self.tree.clone(), |d, tree| {
                        d.child(
                            div()
                                .w(px(260.))
                                .h_full()
                                .flex_none()
                                .border_r_1()
                                .border_color(border)
                                .child(tree),
                        )
                    })
                    .child(div().flex_1().min_w_0().h_full().child(body)),
            )
```

- [ ] **Step 5: Build.** `cargo clippy -p hydraui --all-targets -- -D warnings` and `cargo test -p hydraui` — expected: clean / PASS. Fix GPUI signature mismatches against the pinned source with the smallest change that keeps the documented behaviour; record each as a ruling.

- [ ] **Step 6: fmt, clippy, workspace tests, commit.**

```bash
git add Cargo.lock crates/hydraui
git commit -m "feat: hydraui file tree panel

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Packaging, release and the smoke check

**Files:**
- Modify: `Cargo.toml` (workspace `windows-sys` features), `crates/hydraui/Cargo.toml`, `crates/hydraui/src/main.rs`
- Create: `crates/hydraui/build.rs`
- Modify: `.github/workflows/ci.yml` (build and publish hydraui), `scripts/deploy.ps1` (build and install hydraui)
- Modify: `docs/superpowers/specs/2026-10-17-hydraui-design.md` §7.7
- Test: build and inspect the exe's version info (packaging is configuration, not logic); the smoke checklist

**Interfaces:**
- Produces: `hydraui.exe` as a GUI-subsystem exe with icon and version info; release assets `hydraui-x86_64-pc-windows-msvc.exe` and its `.sha256`.

- [ ] **Step 1: Version info and icon.** `crates/hydraui/Cargo.toml`: add

```toml
[build-dependencies]
winresource = "0.1"
```

Create `crates/hydraui/build.rs`:

```rust
fn main() {
    // hydraui's version info follows the release (HYDRA_VERSION) like hydra.exe's.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");
    println!("cargo:rerun-if-changed=../../assets/hydra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/hydra.ico")
            .set(
                "FileDescription",
                "hydraui - a desktop terminal for hydra environments",
            )
            .set("ProductName", "hydraui")
            .set("CompanyName", "davojc")
            .set("OriginalFilename", "hydraui.exe")
            .set("InternalName", "hydraui")
            .set("LegalCopyright", "Copyright (c) davojc");
        if let Some((v, packed)) = std::env::var("HYDRA_VERSION")
            .ok()
            .and_then(|v| release_version(&v).map(|p| (v, p)))
        {
            res.set("FileVersion", &v)
                .set("ProductVersion", &v)
                .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
                .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
        }
        res.compile()
            .expect("couldn't embed the hydraui icon and version info");
    }
}

/// `x.y.z` (digits only, each part fitting 16 bits) packed as `x << 48 | y << 32 | z << 16`.
fn release_version(v: &str) -> Option<u64> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut packed = 0_u64;
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u16 = p.parse().ok()?;
        packed |= u64::from(n) << (48 - 16 * i);
    }
    Some(packed)
}
```

- [ ] **Step 2: GUI subsystem, with startup errors in a message box.** Root `Cargo.toml`: add `"Win32_UI_WindowsAndMessaging"` to the `windows-sys` features list. `crates/hydraui/Cargo.toml` `[dependencies]`: add `windows-sys.workspace = true`. In `main.rs`:
  - Add as the first line: `#![windows_subsystem = "windows"]` (before the `//!` doc comment is fine; attributes and doc comments may come in either order).
  - Replace

```rust
        Err(e) => {
            eprintln!("hydraui: {e}");
            std::process::exit(1);
        }
```

  with `Err(e) => fatal(&e),` and add:

```rust
/// A GUI exe has no console: say why it can't start in a message box, then exit.
fn fatal(message: &str) -> ! {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(message), wide("hydraui"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    std::process::exit(1)
}
```

  - Also replace `.expect("hydraui can't open its window");` with `.unwrap_or_else(|e| fatal(&format!("hydraui can't open its window: {e}")));`.

- [ ] **Step 3: Build and check.** `cargo build --release -p hydraui`, then in PowerShell:

```powershell
(Get-Item target\release\hydraui.exe).VersionInfo | Select-Object ProductName, FileDescription, OriginalFilename
```

  Expected: `hydraui`, `hydraui - a desktop terminal for hydra environments`, `hydraui.exe`. Then `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — expected: pass.

- [ ] **Step 4: Release and deploy.** In `.github/workflows/ci.yml`:
  - Build step: `run: cargo build --release -p hydra-cli -p hydraui`.
  - Package step: after the hydra lines, add

```bash
          cd ..
          cp target/release/hydraui.exe dist/hydraui-x86_64-pc-windows-msvc.exe
          cd dist
          sha256sum hydraui-x86_64-pc-windows-msvc.exe | awk '{print tolower($1) "  hydraui-x86_64-pc-windows-msvc.exe"}' > hydraui-x86_64-pc-windows-msvc.exe.sha256
          cat hydraui-x86_64-pc-windows-msvc.exe.sha256
```

  (the step already `cd dist`s; the asset names keep hydra's pattern, and `hydra update` still downloads only `hydra-…exe`).

  In `scripts/deploy.ps1`: change the comment to "Builds hydra and hydraui and installs them to E:\Tools.", the running check to `Get-Process hydra, hydra.old, hydraui -ErrorAction SilentlyContinue` (message "hydra or hydraui is in use"), and after copying `hydra.exe` add `Copy-Item (Join-Path $repo 'target\release\hydraui.exe') (Join-Path $Target 'hydraui.exe') -Force`.

- [ ] **Step 5: Align the spec.** In §7.7, replace "The release zip ships both; `scripts/` builds both." with "Each release ships both executables (`hydra-x86_64-pc-windows-msvc.exe` and `hydraui-x86_64-pc-windows-msvc.exe`, each with a `.sha256`); `scripts/deploy.ps1` builds and installs both."

- [ ] **Step 6: Smoke checklist** (with a real `~/.hydra` holding at least one environment; plan 3's checklist still applies). Create `~/.hydra/hydraui/sets/demo.toml`:

```toml
env = "<an environment>"
cwd = "~"

[[tab]]
title = "two"
layout = { split = "right", panes = [ { command = "Get-Date" }, { } ] }

[[tab]]
title = "one"
```

  1. Ctrl+Shift+P → "Set: demo": a "▾ demo" group opens with tabs "two" (split, the left pane printed the date) and "one". Running "Set: demo (loaded)" again focuses its first tab.
  2. Edit demo.toml: within about 2 s the group shows "changed - reload?"; clicking it reopens the set from the file.
  3. Run `ping -t localhost` in a demo pane; "unload": a dialog names the pane; Cancel keeps it running; Unload closes the tabs and ping ends.
  4. Load demo, restart hydraui: demo reopens (startup commands run again). Close its tabs by hand, restart: it doesn't.
  5. Put an unknown key in demo.toml: the launcher shows "Set: demo (invalid)"; Enter shows the error; restoring a session that lists it skips it with a toast.
  6. "Sign in: github in <env>" (an environment with `[github]`) opens a pane running `gh auth login` in that environment.
  7. "Set secret: …": the value box is masked; a blank value is refused; a stored one makes `hydra whoami` / the pane find it.
  8. Ctrl+B: the tree shows the focused pane's folder and follows `cd`; gitignored folders are hidden; a new file in an open folder appears; click inserts a quoted path; Ctrl+click opens the file (`editor = "code"` in config.toml, or the default app); double-click a folder at the prompt changes to it; during `Start-Sleep 30` it doesn't (tooltip says why).
  9. "Open config folder" opens `~/.hydra/hydraui`; change `font_size` and a colour, "Reload config": open panes redraw; changing `[keys]` says it applies at the next start.
  10. The release `hydraui.exe` starts with no console window; renaming `~/.hydra` away and starting it shows nothing worse than the "no environments" toast (and a broken `HYDRA_HOME` gives a message box).

- [ ] **Step 7: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/hydraui .github/workflows/ci.yml scripts/deploy.ps1 docs/superpowers/specs/2026-10-17-hydraui-design.md
git commit -m "feat: hydraui packaged as a GUI app and shipped in releases

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```
