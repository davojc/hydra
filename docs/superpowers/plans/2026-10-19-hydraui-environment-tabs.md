# hydraui environment tabs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn hydraui's window into one tab per environment, each with a list of its terminals
on the left, one terminal (plus an optional file pane) in the middle and the file tree on the
right; layouts save environments and terminals; sets, group files and terminal splits go.

**Architecture:** A new layout file format (`version = 2`) and its resolution live in
`hydraui-sets` next to the old code until the switch-over. A new window model, `EnvTabs`
(`crates/hydraui/src/envs.rs`), replaces `Tabs` and the split tree. The `Workspace` switches to
both in one task, then gains the terminal list and the right-hand tree, then the dead code goes.

**Tech Stack:** Rust 2024, gpui + gpui-component, serde/toml, existing `hydra-core` helpers.

**Spec:** `docs/superpowers/specs/2026-10-19-hydraui-environment-tabs-design.md` (amends
`docs/superpowers/specs/2026-10-17-hydraui-design.md`). Read both before starting.

## Global Constraints

- Every task ends green on: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (what CI runs).
- Layout files stay `*.hydraui`, TOML inside, written atomically (`write_atomic`); the claim, recent list, start page and command line (§7.12 of the hydraui spec) keep working unchanged.
- A layout without `version = 2` is never opened or converted; nothing on disk is touched.
- `~/.hydra/hydraui/sets/` is never read, written or deleted.
- A terminal's name is the user's: Rename / layout `title`, else its folder name. Program titles (`TermEvent::Title`) never name a terminal and are never saved.
- hydraui writes `command` only for terminals that came from a layout file's hand-written `command`.
- Running commands (OSC 133 `CommandRunning`) are confirmed before Close environment, Restart environment, closing one terminal, and Restart on a row.
- Comments and naming match the surrounding code: short doc comments that say what and why, spec section references like `(spec §7.12)`.

## Review Focus

1. **The same environment twice in a layout file** (hand-edited): one tab, terminals merged in file order; the second entry's `active`/`files` ignored. Test: `duplicate_envs_merge_into_one_tab` (Task 2).
2. **`active` pointing past the end or at a skipped terminal**: the first opened terminal is shown, never a panic or a blank middle. Test: `active_falls_back_to_the_first_opened_terminal` (Task 2).
3. **A layout whose environments are all gone**: the file is let go untouched and the start page returns; a layout with zero environments (all closed and saved) opens as an empty window and keeps its file. Test: `let_go_only_when_entries_existed_and_none_opened` (Task 6).
4. **Closing the shown terminal while its environment's file pane has focus**, and Ctrl+W on the file pane: the file pane closes on Ctrl+W when it has focus; otherwise the shown terminal closes and the next (else previous) is shown. Test: `closing_the_shown_terminal_shows_the_next_else_the_previous` and `files_focus_is_cleared_when_a_terminal_is_shown` (Task 3).
5. **Skipped entries survive a save**: a terminal whose folder is missing today, or an environment that's gone, is still in the file after autosave. Test: `unopened_terminals_and_envs_are_kept_on_write` (Task 6).

---

### Task 1: The layout file, version 2

**Files:**
- Create: `crates/hydraui-sets/src/layout.rs`
- Modify: `crates/hydraui-sets/src/lib.rs` (add `pub mod layout;` — not re-exported at the root yet, to avoid clashing with the old `read_layout`/`write_layout`)

**Interfaces:**
- Produces (all in `hydraui_sets::layout`):
  ```rust
  pub const LAYOUT_VERSION: u32 = 2;
  #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  pub struct Layout {
      pub version: u32,                         // always written as LAYOUT_VERSION
      pub active_env: Option<String>,           // skip if None
      pub tree: Option<TreeFile>,               // skip if None
      #[serde(rename = "env")] pub envs: Vec<EnvFile>,   // default, skip if empty
  }
  pub struct TreeFile { pub open: bool, pub width: Option<f32> }
  pub struct EnvFile {
      pub name: String,
      pub active: Option<usize>,
      pub files: Option<FilesFile>,
      #[serde(rename = "terminal")] pub terminals: Vec<TerminalFile>,
  }
  pub struct FilesFile { pub files: Vec<String>, pub active: Option<usize> }
  pub struct TerminalFile { pub title: Option<String>, pub cwd: String, pub shell: Option<String>, pub command: Option<String> }
  pub enum LayoutError { NotLayout(PathBuf), Old(PathBuf), Read { path: PathBuf, message: String } }
  impl Display for LayoutError  // texts below
  pub fn parse_layout(path: &Path, text: &str) -> Result<Layout, LayoutError>
  pub fn read_layout(path: &Path) -> Result<Layout, LayoutError>
  pub fn write_layout(path: &Path, layout: &Layout) -> std::io::Result<()>
  ```
  All structs derive `Debug, Clone, Default, PartialEq, Serialize, Deserialize` with
  `deny_unknown_fields`; every `Option` is `skip_serializing_if = "Option::is_none"`, vectors
  `default`. Field order is as listed (TOML needs plain values before tables).
- Error texts (exact):
  - `NotLayout`: `"{path} isn't a layout file (*.hydraui)"`
  - `Old`: `"{path} was made by an older hydraui and can't be opened; start a new layout"`
  - `Read`: `"{path}: {message}"` (can't read, or TOML error)

- [ ] **Step 1: Write the failing tests** in `layout.rs`'s `mod tests`:
  - `the_spec_example_parses`: parse the TOML block from spec §3 verbatim; assert `active_env == Some("work")`, `tree == Some(TreeFile { open: true, width: Some(280.0) })`, two envs, `envs[0].terminals.len() == 2`, `envs[0].terminals[0].command == Some("claude")`, `envs[0].files.as_ref().unwrap().files == ["E:/work/api/README.md"]`, `envs[1].terminals[0].cwd == "~/blog"`.
  - `a_layout_round_trips`: `write_layout` then `read_layout` of a two-env layout equals the original; the written text starts with `version = 2`.
  - `a_layout_without_version_2_is_old`: for `"sets = [\"a\"]\n"`, `"[[loose]]\nenv = \"w\"\n"`, `"version = 1\n"`, `""` → `Err(LayoutError::Old(_))`; the file is unchanged afterwards.
  - `only_hydraui_files_are_layouts`: `read_layout` on `notes.txt` and `a.hydraui.toml` → `NotLayout`, message contains the file name.
  - `unknown_keys_and_bad_toml_name_the_file`: `"version = 2\nbogus = 1\n"` and `"version = 2\n[[env]]\n"` (missing `name`) → `Read`, message contains `bad.hydraui`.
  - `a_byte_order_mark_is_ignored`: `"\u{feff}version = 2\n"` parses.
- [ ] **Step 2: Run** `cargo test -p hydraui-sets layout::` — expect compile failure (module empty).
- [ ] **Step 3: Implement.** `parse_layout` reads the text as `toml::Table` first (BOM stripped): no `version` integer equal to 2 → `Old`; then deserialize `Layout`. `read_layout` checks the `.hydraui` extension (any case) before reading. `write_layout` sets `version = LAYOUT_VERSION` on a copy and uses `crate::files::write_atomic`.
- [ ] **Step 4: Run** `cargo test -p hydraui-sets` — all pass.
- [ ] **Step 5: Commit** `feat: hydraui-sets reads and writes version 2 layout files`

### Task 2: Resolving a layout into what opens

**Files:**
- Modify: `crates/hydraui-sets/src/layout.rs`
- Modify: `crates/hydraui-sets/src/resolve.rs` (move `Context` out to `layout.rs`; `resolve.rs` imports it), `crates/hydraui-sets/src/lib.rs` (`pub use layout::Context` in place of `resolve::Context`)

**Interfaces:**
- Consumes: Task 1 types; `crate::spec::{PaneSpec, FilesSpec}`; `hydra_core::{bindings, paths::expand_tilde, name::EnvName}`.
- Produces:
  ```rust
  pub struct PlannedTerminal { pub spec: PaneSpec, pub title: Option<String> }
  pub struct PlannedEnv { pub name: String, pub terminals: Vec<PlannedTerminal>, pub shown: Option<usize>, pub files: Option<FilesSpec> }
  pub struct LayoutPlan {
      pub envs: Vec<PlannedEnv>,
      pub active_env: Option<String>,
      pub tree: Option<TreeFile>,
      pub problems: Vec<String>,   // one toast each
      pub unopened: Layout,        // envs/terminals that didn't open, kept in what is written
  }
  pub fn plan_layout(ctx: &Context, layout: &Layout) -> LayoutPlan
  ```
  All derive `Debug, Clone, PartialEq`.

Rules `plan_layout` follows (spec §3):
- An env whose `env.toml` doesn't exist (`ctx.paths.env_file`) or whose name isn't valid: problem `"environment {name} no longer exists"`; the whole `EnvFile` goes to `unopened.envs`.
- A duplicate env name merges its terminals into the first entry, in file order; its `active` and `files` are ignored.
- Terminal `cwd`: `expand_tilde`, relative → `ctx.user_home.join`. Not a folder: problem `"{env}: folder {cwd} doesn't exist"`; the `TerminalFile` goes to `unopened` under an `EnvFile` of that name (merged if already there).
- `shell`: `ShellKind::parse`, else problem `"{env}: unknown shell {s:?}; use pwsh or bash"` and skipped as above; absent → `ctx.default_shell`.
- `bound_elsewhere`: `bindings::resolve(cwd, ctx.bindings)` naming a different env.
- `command` and `title` carried as they are.
- `shown`: the file's `active` mapped through skipped terminals; out of range or skipped → `Some(0)` if any terminal opened, else `None`.
- `files`: paths resolved like `cwd`, missing files dropped, none left → `None`; `active` clamped (0 when absent).
- `active_env`: kept when it names a planned env, else the first planned env's name.
- An env with zero terminals (in the file, or after skips) is still planned.

- [ ] **Step 1: Write the failing tests** (use a temp `HydraPaths` with `envs/work/env.toml` and `envs/personal/env.toml`, as `layout.rs` tests in `crates/hydraui/src/layout.rs` do):
  - `a_layout_plans_each_env_and_terminal`: two envs, three terminals; assert names, cwds, shells, `command`, `title`, `shown`.
  - `a_missing_env_is_a_problem_and_kept`: env `gone`; `problems == ["environment gone no longer exists"]`, `unopened.envs[0].name == "gone"`, `envs` has only the others.
  - `a_missing_folder_skips_only_that_terminal`: one bad cwd; that `TerminalFile` is in `unopened.envs[0].terminals`; the env still plans with the rest.
  - `duplicate_envs_merge_into_one_tab` (Review Focus 1).
  - `active_falls_back_to_the_first_opened_terminal` (Review Focus 2): `active = 5` with two terminals → `Some(0)`; `active = 1` with terminal 1 skipped → `Some(0)`; no terminals → `None`.
  - `tilde_and_relative_folders_resolve_from_the_user_home`.
  - `a_folder_bound_elsewhere_is_noted`.
  - `missing_files_drop_and_active_clamps`.
  - `active_env_falls_back_to_the_first_planned`.
- [ ] **Step 2: Run** `cargo test -p hydraui-sets layout::` — expect failures.
- [ ] **Step 3: Implement `plan_layout`** per the rules above.
- [ ] **Step 4: Run** `cargo test -p hydraui-sets` — pass; `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] **Step 5: Commit** `feat: hydraui-sets plans a version 2 layout's environments and terminals`

### Task 3: The window model, `EnvTabs`

**Files:**
- Create: `crates/hydraui/src/envs.rs` (and `mod envs;` in `crates/hydraui/src/lib.rs`)

**Interfaces:**
- Consumes: `crate::split::PaneId` (re-exported here as `pub use crate::split::PaneId;` until Task 8).
- Produces:
  ```rust
  pub const DEFAULT_FILES_RATIO: f32 = 0.5;
  #[derive(Debug, Clone, PartialEq)]
  pub struct EnvTab {
      pub env: String,
      pub terminals: Vec<PaneId>,   // creation order
      pub shown: Option<PaneId>,
      pub files: Option<PaneId>,    // the env's file pane
      pub files_focused: bool,
      pub files_ratio: f32,         // the terminal's share when the file pane shows
  }
  #[derive(Debug, Default)] pub struct EnvTabs { tabs: Vec<EnvTab>, active: usize }
  impl EnvTabs {
      pub fn is_empty(&self) -> bool;
      pub fn iter(&self) -> impl Iterator<Item = &EnvTab>;
      pub fn get(&self, env: &str) -> Option<&EnvTab>;
      pub fn get_mut(&mut self, env: &str) -> Option<&mut EnvTab>;
      pub fn active(&self) -> Option<&EnvTab>;
      pub fn active_mut(&mut self) -> Option<&mut EnvTab>;
      /// Appends a tab for `env` (none yet) and activates it; true when new.
      pub fn add_env(&mut self, env: &str) -> bool;
      pub fn activate(&mut self, env: &str) -> bool;
      pub fn next(&mut self); pub fn previous(&mut self);       // wrap
      /// Appends `id` to `env`'s terminals, shows it, activates `env`; false without that env.
      pub fn add_terminal(&mut self, env: &str, id: PaneId) -> bool;
      /// Activates `id`'s env and shows it (files_focused = false).
      pub fn show(&mut self, id: PaneId) -> bool;
      pub fn next_terminal(&mut self) -> Option<PaneId>; pub fn previous_terminal(&mut self) -> Option<PaneId>; // within the active env, wrap
      /// The env holding `id` as a terminal or its file pane.
      pub fn env_of(&self, id: PaneId) -> Option<&str>;
      /// Removes a terminal; a shown one gives way to the next in the list, else the previous.
      pub fn close_terminal(&mut self, id: PaneId) -> bool;
      /// Removes the tab; returns its terminals then its file pane. The tab to the right becomes active, else the left.
      pub fn close_env(&mut self, env: &str) -> Vec<PaneId>;
      pub fn set_files(&mut self, env: &str, files: Option<PaneId>);
  }
  pub fn new_terminal_cwd(shown: Option<&Path>, env_home: Option<&Path>, user_home: &Path) -> PathBuf;
  ```

- [ ] **Step 1: Write the failing tests:**
  - `adding_an_env_twice_only_activates_it`.
  - `terminals_append_in_creation_order_and_the_new_one_shows`.
  - `switching_envs_keeps_each_ones_shown_terminal`.
  - `closing_the_shown_terminal_shows_the_next_else_the_previous` (Review Focus 4): `[1,2,3]` show 2 close 2 → shown 3; close 3 → shown 1; close 1 → `None`, tab still there.
  - `closing_a_hidden_terminal_keeps_the_shown_one`.
  - `files_focus_is_cleared_when_a_terminal_is_shown` (Review Focus 4).
  - `close_env_returns_its_panes_and_activates_the_neighbour`: returns `[terminals..., files]`; right neighbour active, else left; the last close leaves `active() == None` and later `add_env` works.
  - `next_and_previous_terminal_wrap_within_the_active_env`.
  - `env_of_finds_terminals_and_file_panes`.
  - `new_terminal_cwd_prefers_shown_then_env_home_then_user_home`.
- [ ] **Step 2: Run** `cargo test -p hydraui envs::` — fail.
- [ ] **Step 3: Implement** `EnvTabs` and `new_terminal_cwd`. Mark the module `#![allow(dead_code)]` only if clippy complains before Task 6 wires it, and remove that in Task 6.
- [ ] **Step 4: Run** `cargo test -p hydraui envs::` — pass.
- [ ] **Step 5: Commit** `feat: hydraui models a window as environment tabs with terminal lists`

### Task 4: Terminal names and environment homes

**Files:**
- Modify: `crates/hydraui/src/pane_view.rs:83, 204, 248-256, 336-338`
- Modify: `crates/hydraui/src/hydra.rs` (near `env_exists`, line 135)

**Interfaces:**
- Produces:
  ```rust
  // pane_view.rs
  /// A terminal's name: the user's, else its folder's name (spec §2.2 of the env-tabs design).
  pub fn label_for(name: Option<&str>, cwd: &Path) -> String;
  impl PaneView {
      pub fn label(&self) -> String;                  // label_for(self.name, self.cwd)
      pub fn name(&self) -> Option<&str>;
      pub fn set_name(&mut self, name: Option<String>, cx: &mut Context<Self>); // trims; "" → None; emits PaneEvent::TitleChanged
      pub fn program_title(&self) -> Option<&str>;    // the old `title`, for a tooltip only
  }
  // hydra.rs
  /// `env`'s `home` from env.toml (with `~`), when that folder exists.
  pub fn env_home(&self, env: &str) -> Option<PathBuf>;
  ```
  `PaneView::new` gains no new parameter; the Workspace calls `set_name` after creating a pane
  from a layout `title`.

- [ ] **Step 1: Write the failing tests:** in `pane_view.rs` — `the_users_name_wins_else_the_folder` (`label_for(Some("api"), …) == "api"`, `label_for(None, Path::new("C:/w/web")) == "web"`, `label_for(Some("  "), …)` → folder name, a root path → its display); in `hydra.rs` — `env_home_reads_env_toml_and_needs_the_folder` (home set and existing → `Some`, `~` expanded, missing folder → `None`, no `home` → `None`).
- [ ] **Step 2: Run** `cargo test -p hydraui label_for env_home` — fail.
- [ ] **Step 3: Implement.** `label()` no longer reads the program title; keep `TermEvent::Title` storing it in `title` (renamed field use via `program_title`).
- [ ] **Step 4: Run** `cargo test -p hydraui` — pass.
- [ ] **Step 5: Commit** `feat: hydraui terminals are named by the user, not by their program`

### Task 5: Keys for terminals

**Files:**
- Modify: `crates/hydraui/src/keys.rs`
- Modify: `crates/hydraui/src/workspace.rs` (`new_tab` takes `&NewTerminal`)

**Interfaces:**
- Produces: actions `NewTerminal`, `NextTerminal`, `PreviousTerminal` (replacing `NewTab`; `NextTab`/`PreviousTab` stay and now move between environments). `Command` loses `NewTab, SplitRight, SplitDown, FocusLeft, FocusRight, FocusUp, FocusDown` and gains `NewTerminal` (`"new_terminal"`, `ctrl-t`), `NextTerminal` (`"next_terminal"`, `ctrl-pagedown`), `PreviousTerminal` (`"previous_terminal"`, `ctrl-pageup`). `pub const REMOVED: [&str; 7] = ["new_tab", "split_right", "split_down", "focus_left", "focus_right", "focus_up", "focus_down"];` — `keymap` ignores these names silently (spec §4).
  The old action types (`NewTab`, `SplitRight`, `SplitDown`, `Focus*`) stay declared until
  Task 6 deletes them with their handlers; in this task only `workspace.rs`'s
  `.on_action(cx.listener(Self::new_tab))` switches to taking `&NewTerminal`, so Ctrl+T keeps
  working between tasks. `NextTerminal`/`PreviousTerminal` are bound but unhandled until Task 6.

- [ ] **Step 1: Write the failing tests:** `removed_commands_are_ignored_not_errors` (`keymap({"split_right": "ctrl-x"})` → no problems, no binding for it); `terminal_keys_have_defaults` (`new_terminal` = `ctrl-t`, `next_terminal` = `ctrl-pagedown`, `previous_terminal` = `ctrl-pageup`); update existing tests that list `Command::ALL`.
- [ ] **Step 2: Run** `cargo test -p hydraui keys::` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p hydraui` — pass; clippy clean.
- [ ] **Step 5: Commit** `feat: hydraui's keys open and move between terminals; split keys go`

### Task 6: The switch-over: environments, terminals and version 2 layouts

The `Workspace` stops using `Tabs`, `Tree`, sets and group files, and uses `EnvTabs` and the
version 2 layout. After this task the window works end to end with the tree still on the left
and terminals switched by keys and the strip (the list comes in Task 7).

**Files:**
- Modify: `crates/hydraui/src/workspace.rs` (fields, `create_pane`, `focused`, file pane, close, restart, render)
- Replace: `crates/hydraui/src/workspace/sets.rs` → `crates/hydraui/src/workspace/layout_file.rs` (open, autosave, save as, dialogs; no sets or groups)
- Modify: `crates/hydraui/src/window_layout.rs`, `crates/hydraui/src/workspace/start_page.rs`, `crates/hydraui/src/workspace/title_bar.rs`, `crates/hydraui/src/launcher.rs`, `crates/hydraui/src/workspace/launcher.rs`, `crates/hydraui/src/workspace/accounts.rs`, `crates/hydraui/src/open_files.rs` (drop `terminal_for`, `last_terminal_closes_tab`), `crates/hydraui/src/file_tree_view.rs` (`TreeEvent::OpenTab` → `TreeEvent::NewTerminal`, menu label **New terminal here**)

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces (on `Workspace`, used by Task 7):
  ```rust
  fn add_env(&mut self, env: &str, window, cx)                       // tab + one terminal at new_terminal_cwd(None, env_home, user_home); selects existing tab otherwise
  fn new_terminal(&mut self, env: &str, cwd: Option<PathBuf>, window, cx) -> Option<PaneId>  // cwd None → new_terminal_cwd(shown cwd, env_home, user_home)
  fn open_terminal(&mut self, env: &str, request: PaneRequest, window, cx) -> PaneId          // adds the env tab if needed (sign-in, launcher)
  fn show_terminal(&mut self, id: PaneId, window, cx)
  fn close_terminal(&mut self, id: PaneId, window, cx)               // asks when running (Task 7 adds the dialog; here it closes)
  fn close_env(&mut self, env: &str, window, cx)                     // confirm_unsaved + running dialog, then closes
  fn restart_env(&mut self, env: &str, window, cx)                   // today's restart_tab over the env's terminals
  fn restart_terminal(&mut self, id: PaneId, window, cx)
  fn running(&self, panes: &[PaneId], cx: &App) -> Vec<String>       // "name" per running terminal
  pub fn running_question(what: &str, running: &[String]) -> Option<String>  // today's unload_question, renamed, same text
  fn current_layout(&self, cx: &App) -> Layout                        // hydraui_sets::layout::Layout
  ```
  New fields: `envs: EnvTabs`, `commands: HashMap<PaneId, String>` (hand-written layout commands to keep on save), `unopened: Layout`. Removed fields: `tabs`, `last_terminal`, `drag`, `split_bounds`, `loaded`, `changed_sets`, `collapsed`, `_set_watch`.
  `window_layout.rs`: `let_go_after_open(opened_any: bool, layout: &Layout) -> bool` is `!opened_any && !layout.envs.is_empty()`; `with_unopened(current: Layout, unopened: &Layout) -> Layout` appends unopened envs not open now and appends unopened terminals to an open env of the same name; `opens_here(backed, has_envs)`.
  `launcher.rs`: `Action::LoadSet`, `UnloadSet`, `OpenGroupFile`, `SaveLooseAsGroup`, `Problem` and the set entries go; `OpenEnv(env)` is labelled `"Environment: {env}"` and adds/selects its tab.

Behaviour to implement (spec sections in brackets):
- Strip [§2.1]: one tab per env, tinted, busiest activity dot of its terminals (`busy.rs` takes a list of panes instead of a `Tab`), ×; **+** opens the env picker (today's `pick_env`, retitled "Add an environment", showing only envs not in the window, **New environment…** last) and calls `add_env`. Right-click: **New terminal**, **Restart environment**, **Close environment**. File tree toggle button stays.
- Middle [§2.3, §2.4]: the active env's shown terminal; with a file pane, `h_flex(terminal w(relative(files_ratio)), 4px divider, file pane)`, divider drag sets `files_ratio` (clamped 0.1–0.9, reuse `MIN_RATIO`/`MAX_RATIO`). Shown terminal border in the env's tint; file pane border accent when `files_focused`. No env: "No environments open. + adds one." Env with no terminals: centered **New terminal** button.
- Focus: `PaneEvent::Focused` → `envs.show(id)`; `FilePaneEvent::Focused` → `files_focused = true`. Ctrl+W: file pane if `files_focused`, else the shown terminal. Ctrl+T: `new_terminal(active env, None)`; with no env, the picker. Ctrl+PageDown/Up: `next_terminal`/`previous_terminal` then focus.
- `show_file_at`: creates or reuses the **active env's** file pane (`set_files`), never splits a tree.
- Autosave [§3]: `current_layout` writes every env in order with terminals (`title` = `PaneView::name()`, `cwd` = current folder, `shell`, `command` from `self.commands`), `active` = index of shown, `files` from `FilePane::saved()`, `active_env`, `tree` (`width` only when not `DEFAULT_TREE_WIDTH`). Through `with_unopened`. Triggers: every place that changes envs, terminals, names, shown terminal, active env, files, tree.
- Open [§3]: `open_layout_path` uses `hydraui_sets::layout::read_layout` and `plan_layout`; `LayoutError::Old` and `NotLayout` → error toast, start page stays, and when chosen from the start page's recent list the entry is forgotten (`forget_recent`). Drop the `Opened::Converted`/`Untitled` branches and the startup `migrate_session` call. Each planned env → `add_env` without its default terminal, each terminal → `create_pane` + `set_name(title)` + `commands` entry when `command` is set; `shown`, `files`, `active_env`, tree applied; one toast per `problems` entry; `unopened` stored.
- Start page **New window** [§3]: `new_tab_in(home)` becomes "environment for home (binding → `default_env` → picker), then `add_env`".
- Launcher, accounts popover sign-in, tree **New terminal here** [§5, §2.5]: `open_terminal` / `new_terminal(env, Some(dir))`.
- Delete everything for splits (`split`, `split_right`, `split_down`, `move_focus`, `focus_*`, `render_tree`'s split branch, divider drags of splits), sets (`load_set`, `open_set`, `unload_set*`, `close_set`, `tab_gone`, `check_sets`, `render_set_header`), group files (`group_file_of`, `save_tabs_as_group`, `save_loose_as_group`, `save_tab_as_group`, `save_group`, `open_group_file*`), and the title bar's group entries.

- [ ] **Step 1: Write the failing tests:**
  - `window_layout.rs`: `let_go_only_when_entries_existed_and_none_opened` (Review Focus 3): `(false, layout with envs)` → true; `(false, Layout::default())` → false; `(true, _)` → false. `unopened_terminals_and_envs_are_kept_on_write` (Review Focus 5): current `{work: [a]}`, unopened `{work: [b], gone: [c]}` → `{work: [a, b], gone: [c]}`.
  - `launcher.rs`: rewrite `entries_cover_…` for the new entries: `Environment: work` → `OpenEnv("work")`; no `Set:` entries; no group entries.
  - `workspace/layout_file.rs`: `running_question` keeps today's `unloading_asks_only_when_something_runs_and_names_it` assertions (renamed).
- [ ] **Step 2: Run** `cargo test -p hydraui` — fail.
- [ ] **Step 3: Implement** the behaviour above, file by file. Keep `focus_soon` for focusing after dialogs.
- [ ] **Step 4: Run** the full CI trio — all green.
- [ ] **Step 5: Check by running the app** (`cargo run -p hydraui`, see the `run` skill): New window → one env tab; + adds a second env; Ctrl+T adds terminals; Ctrl+PageDown switches; clicking a tree file opens the file pane beside the terminal; Save layout as…, close, reopen from the start page → same envs, terminals, names, file pane and tree; an old `.hydraui` from before this change → "made by an older hydraui" toast.
- [ ] **Step 6: Commit** `feat: hydraui's tabs are environments and layouts save their terminals`

### Task 7: The terminal list, the tree on the right, and confirmations

**Files:**
- Create: `crates/hydraui/src/workspace/terminal_list.rs` (the left column)
- Modify: `crates/hydraui/src/workspace.rs` (render order; tree divider drag measured from the right edge)
- Modify: `crates/hydraui/src/workspace/layout_file.rs` (running dialogs)

**Interfaces:**
- Consumes: Task 6's `Workspace` methods.
- Produces:
  ```rust
  const LIST_WIDTH: f32 = 200.0;   // fixed (our call: spec leaves width to the builder)
  fn render_terminal_list(&self, cx: &mut Context<Self>) -> impl IntoElement   // in terminal_list.rs
  fn rename_dialog(&mut self, id: PaneId, window, cx)                         // name input, Enter confirms, empty → folder name
  /// The tree's width with its divider dragged to `x`, the tree on the right of a window `window_width` wide.
  fn tree_width_at(x: f32, window_width: f32) -> f32                          // now measured from the right edge
  ```

Behaviour:
- Left column [§2.2]: **+ New** at the top (as Ctrl+T); one row per terminal of the active env: activity dot (`busy.rs`), `label()`, × on hover; shown row highlighted with the env's tint; click → `show_terminal`; row tooltip = `program_title()` when set (our call). Right-click: **Rename…**, **Restart**, **Close**.
- Right [§2.5]: `[list][divider-less border][middle][4px divider][tree]`; the tree divider drags from the right: width = `window_width - x`, clamped as today (`MIN_TREE_WIDTH`, leaves `MIN_PANES_WIDTH`). The strip's tree toggle moves to the strip's right end.
- Confirmations [§2.1, §2.2]: `close_terminal` (×, Ctrl+W, Close) and `restart_terminal` ask with `running_question(<terminal name>, …)` when that terminal is `CommandRunning`; `close_env` and `restart_env` ask over all the env's terminals (`running_question(<env>, …)`), after `confirm_unsaved` for the file pane on close. Dialog titles: `"Close {name}?"` / `"Restart {name}?"`, buttons Cancel / Close (or Restart) danger, as `restart_tab`'s dialog today.
- Restart keeps the terminal's place, name and folder (`PaneView::restart` already restarts in the current folder) and reruns its `request.command`.

- [ ] **Step 1: Write the failing test:** update `the_tree_width_follows_the_pointer_within_limits` for the right edge: in a 1200 px window, `x = 900` → 300; `x = 1150` → `MIN_TREE_WIDTH`; `x = 100` → `1200 - MIN_PANES_WIDTH`; `NaN` → `DEFAULT_TREE_WIDTH`; a 250 px window → `MIN_TREE_WIDTH`.
- [ ] **Step 2: Run** `cargo test -p hydraui tree_width` — fail.
- [ ] **Step 3: Implement** the list, the right-hand tree, rename dialog, row menu and confirmations.
- [ ] **Step 4: Run** the CI trio — green.
- [ ] **Step 5: Check by running the app:** the list shows each env's terminals and follows tab switches; Rename sticks while `claude` runs and survives Save → reopen; Restart on a row and on the env tab ask while `ping -t localhost` runs and not at a prompt; × on a running row asks; the tree sits right, follows the shown terminal's folder, drags from its left edge; closing the last terminal leaves the env tab with **New terminal**.
- [ ] **Step 6: Commit** `feat: hydraui lists an environment's terminals on the left and the file tree on the right`

### Task 8: Remove what's left of sets and splits; documents

**Files:**
- Delete from `crates/hydraui-sets/src/`: `file.rs`, `loaded.rs`, `resolve.rs`, `session.rs`, `spec.rs` (move `PaneSpec` and `FilesSpec` into `layout.rs`), the conversion and `migrate_session` parts of `layouts.rs`, and from `files.rs` everything but `write_atomic` (fold into `layout.rs` or keep a small `files.rs`). Root-export the `layout` items (`pub use layout::*` for the public names) and update `lib.rs`'s crate doc: "hydraui's layout files: environments and their terminals (`*.hydraui`), the recent list. No UI."
- Delete from `crates/hydraui/src/`: `tabs.rs`, `split.rs` (`PaneId` moves to `envs.rs` as `pub type PaneId = u64;`), `layout.rs`, `layout_plan.rs`; fix imports.
- Modify: `docs/superpowers/specs/2026-10-17-hydraui-design.md` — a one-line note under §5, §7.1, §7.6, §7.9 and §7.12 ("Superseded by / amended by `2026-10-19-hydraui-environment-tabs-design.md`").
- Modify: `docs/backlog.md` — under hydraui add: "**Terminal splits.** Removed with environment tabs; the user wants to decide how splitting should behave before it returns." and drop items that named sets or group files ("saving a group file through the new Save As" in *Not yet tried*).

- [ ] **Step 1: Delete and move**; `cargo build --workspace` until it compiles.
- [ ] **Step 2: Run** the CI trio — green; `cargo test --workspace` count only lost the deleted modules' tests.
- [ ] **Step 3: Grep** `rg -n "Loose|load_set|SetSpec|group file|split_right|Tabs\b" crates docs/backlog.md` — nothing left but history in plans/specs.
- [ ] **Step 4: Commit** `refactor: remove hydraui's sets, group files and splits`
