# hydraui: a desktop terminal for hydra environments

Date: 2026-10-17. Status: approved design, awaiting spec review.
Builds on `2026-10-03-hydra-design.md` and `2026-10-05-hydra-borrowed-logins-design.md`.

## 1. Purpose

hydra opens one terminal per identity, but each terminal is a separate window
opened by hand. hydraui is a Warp-style desktop terminal, written in Rust,
that holds many hydra terminals as tabs and split panes, and opens and closes
named **sets** of them from configuration. Every pane runs in a hydra
**environment** (the user's "profile"), so one pane uses the work GitHub and
Claude accounts while the next uses personal ones.

hydraui shares `~/.hydra` with the hydra CLI. Either tool can be used on the
same environments, bindings and secrets.

### Success criteria

- Loading the set `client-a` opens its tabs and panes, each in the right
  environment, cwd and startup command; unloading it closes them and ends
  every process they started.
- Several sets can be loaded at once; each tab shows which set and which
  environment it belongs to.
- A pane gets exactly the isolation `hydra shell <env>` gives: same variables
  set and removed, same fail-closed rules, same git identity. The one
  exception is the folder guard, which hydraui leaves out (§4.1).
- An environment created with `hydra env new` / `hydra add` is usable in
  hydraui without restarting it, and the reverse for secrets set in hydraui.
- A file tree shows the focused pane's current folder.

## 2. Scope

In scope (this spec):

- Small changes to hydra's crates so a GUI can use them (§4).
- `hydraui-term`: ConPTY + terminal emulation + shell integration (§6).
- `hydraui-sets`: set files and load/unload bookkeeping (§5).
- `hydraui`: the GPUI app with tabs, split panes, file tree, set launcher,
  sign-in and secret actions (§7).
- Windows 11 x64 only, like hydra.

Not in scope:

- **The modern input editor.** It gets its own spec; this spec provides the
  OSC 133 prompt events it will build on (§6.3).
- Command blocks, AI features, team or cloud features.
- GUI editors for environments or sets. They stay hand-edited TOML (or managed
  with the hydra CLI). hydraui writes only its own files and secrets.
- Restoring scrollback after a restart.
- File operations in the file tree (create, rename, delete, drag).

## 3. Repository and crates

hydraui lives in the hydra repository (user decision: one workspace, released
together, so the shared on-disk format can't drift).

```
crates/
  hydra-core, hydra-platform, hydra-providers, hydra-cli   existing
  hydraui-term/   PTY, alacritty_terminal, OSC 7/133 parsing; no UI
  hydraui-sets/   set model, inheritance, validation, loaded-set state; no UI
  hydraui/        GPUI app, bin `hydraui.exe`
```

Dependencies: `hydraui` → `hydraui-sets`, `hydraui-term`, hydra crates.
`hydraui-sets` → `hydra-core` (names, bindings). `hydraui-term` → `hydra-core`
(`LaunchEnv`), `hydra-platform` (shells). `hydraui-term` knows nothing of sets
or the UI.

Key third-party crates: `gpui` (Zed's UI framework), `gpui-component`
(inputs, resizable panels, lists), `alacritty_terminal` (VT parsing and grid),
`portable-pty` (ConPTY), `notify` (file watching), `ignore` (gitignore-aware
walking). Versions are chosen in the plan; GPUI must be pinned to an exact
revision because its API changes often.

### 3.1 Files

hydraui's own files sit under `~/.hydra/hydraui/` (respects `HYDRA_HOME`).
hydra ignores this folder.

```
~/.hydra/hydraui/
  config.toml      UI settings: font, theme, default env, [keys], editor
  sets/<name>.toml set definitions, hand-edited
  session.toml     what was open at last exit (written by hydraui)
```

hydraui reads, but does not edit, `config.toml` `[bindings]`, `envs/` and
`.hydra` files. It writes secrets (through hydra's secret store, which updates
`secrets.toml`) and the per-launch files under `state/<env>/` that `prepare`
regenerates anyway. Secrets go through `hydra-platform`'s `KeyringStore`,
which keeps the `secrets.toml` index as the CLI does.

Each pane holds hydra's shared shell lock (`lock::hold_shared`) for as long as
its shell runs, exactly like `hydra shell`, so `hydra env rm` / `rename` see
the environment as in use while a hydraui pane has it open.

## 4. Changes to hydra's crates

Each change keeps the CLI's behaviour identical.

1. **Guards can be left out.** Add `guards: bool` to `PrepareOptions`
   (default `true`, the CLI's behaviour). hydraui passes `false` (user
   decision: a push to the wrong account fails on credentials, so hydraui
   doesn't need the folder guard). With `false`:
   - The git provider writes `state/<env>/git/gitconfig-noguard`, the same
     content without `core.hooksPath`, writes no hook wrappers, and points
     `GIT_CONFIG_GLOBAL` at it. The CLI's `gitconfig` and `hooks/` are left
     untouched, so open `hydra shell` terminals keep their guard.
   - The Claude provider neither adds nor removes the PreToolUse hook: it
     keeps whatever the existing `settings.json` has. (`settings.json` sits
     in the environment's Claude folder with its sign-in, so it can't be
     split per tool.) If the CLI has launched the environment, Claude in a
     hydraui pane is still guarded through `hydra.exe`; if `hydra.exe` is
     gone, the hook fails open.

   hydraui never needs `hydra.exe`, and nothing calls `hydraui.exe` from a
   hook.
2. **Warnings are returned, not printed.** Provider warnings currently go to
   stderr (the Claude provider's `warn`). Collect them in a
   `LaunchEnv::warnings: Vec<String>`; the CLI prints them as it does now,
   hydraui shows them in the pane (§7.5).
3. **Variables as pairs.** Add `LaunchEnv::env_changes()` returning the
   variables to set and remove (with `path_drop` already applied to `PATH`),
   for callers that don't build a `std::process::Command` (portable-pty).
   `apply` is reimplemented on top of it so the two can't disagree.
4. **Shell integration option.** Add `integration: bool` to `PromptStyle`.
   When true, `render_pwsh_init` / `render_bash_init` also emit OSC 7 and
   OSC 133 (§6.3) and run an optional startup command after loading the
   user's profiles. The CLI passes `false`; the rendered scripts are unchanged.
5. **Startup command in init.** `render_init(kind, style, startup)` renders an
   init script with an optional startup command appended after the prompt
   setup; `write_init` is unchanged for the CLI. hydraui writes per-pane init scripts to
   `state/<env>/shell/hydraui/<pane-id>.ps1|.bash` so parallel panes don't
   overwrite each other or the CLI's `init.ps1`; the folder is cleared of
   stale files at hydraui start.

## 5. Sets (`hydraui-sets`)

### 5.1 Format

`~/.hydra/hydraui/sets/client-a.toml`; the file stem is the set name and must
be a valid hydra name.

```toml
env = "work"                      # default environment for every pane
cwd = "E:/work/client-a"          # default folder

[[tab]]
title = "api"
cwd = "api"                       # relative: resolves against the set's cwd
layout = { split = "right", ratio = 0.6, panes = [
  { command = "claude" },
  { split = "down", panes = [
      { command = "npm run dev" },
      { },                          # plain shell
  ] },
] }

[[tab]]
title = "infra"
env = "personal"                  # overrides the set default
command = "terraform plan"        # single-pane tab: no layout needed
```

- The key is `env`, matching hydra's own term.
- A tab is either a single pane (`env`, `cwd`, `command`, `shell` on the tab)
  or has a `layout`. A layout node is either a **split** (`split` = `right`
  or `down`, optional `ratio` 0.1–0.9 default 0.5, `panes` with exactly two
  entries) or a **pane** (`env`, `cwd`, `command`, `shell`, all optional).
  Three-way splits are nested splits.
- A layout node can instead be a **file pane** (§7.8): `files` (a list of
  paths, absolute or relative like `cwd`) and an optional `active` (index
  into `files`, default 0), with none of the pane keys. A tab has at most one
  file pane, and a tab's layout can't be only a file pane. A file that
  doesn't exist is dropped when the set opens; a file pane left with no files
  isn't created (its split collapses to the other side).
- `shell` is `pwsh` or `bash`; default from hydraui `config.toml`, else pwsh.
- `cwd` accepts `~`. Relative paths resolve against the nearest ancestor `cwd`
  (pane → tab → set); with none, against the user's home.
- Unknown keys are an error (as in hydra's config).

### 5.2 Environment resolution

For each pane, the first that applies: pane `env` → tab `env` → set `env` →
hydra's binding for the pane's resolved `cwd` (`bindings::resolve`, so `.hydra`
files count). If none applies, the set is invalid: "pane 2 of tab "api" has no
environment and E:/work/client-a/api isn't bound".

If an explicit `env` differs from the folder's binding, the pane opens with a
warning badge ("this folder is bound to personal"). It is the only signal:
hydraui panes have no folder guard (§4.1).

### 5.3 Validation

`load_set(name) -> Result<SetSpec, SetError>` parses and resolves the whole
set before anything opens: TOML errors with line numbers, unknown
environments, missing `cwd` folders, unresolvable environments. A set that
fails validation does not open at all.

Environment problems found by `prepare` (missing SSH key, unset secret) are
**not** set validation errors: those panes open with the error banner (§7.5)
and the rest of the set starts normally.

### 5.4 Loaded sets

`LoadedSets` tracks set name → the tab ids it opened, plus the set file's
modification time at load.

- Loading a set that is already loaded focuses its first tab.
- Unloading closes all its tabs (§6.4). If any of its panes has a command
  running (OSC 133 state), a confirm dialog lists them first.
- Closing a set's last tab by hand unloads it.
- A tab can't move between sets; ad-hoc tabs belong to the "Loose" group.
- If a loaded set's file changes, its group header shows "changed — reload?".
  Reload is unload + load. Running panes are never changed in place.
- `session.toml` records what was open: the loaded sets in order; the Loose
  tabs in the set file's tab format (layout, ratios, and each pane's env,
  shell and current folder); whether the file tree was open and its width;
  and the active tab's position in the strip. It is written when tabs,
  splits, sets or the tree change, and when the window closes. On start,
  hydraui loads the sets again (startup commands run again), then reopens the
  Loose tabs, the tree and the active tab. An invalid or deleted set, or a
  Loose tab whose env or folder is gone, is skipped with a toast. A Loose
  tab's file pane is saved as a `files` node (§5.1) and comes back with its
  files and active file tab; unsaved edits are not kept. Scrollback,
  running commands, window size and dragged ratios in set tabs are not kept.

## 6. Terminal core (`hydraui-term`)

### 6.1 Starting a pane

`Pane::spawn(SpawnSpec) -> Result<(Pane, Receiver<TermEvent>), SpawnError>`,
where `SpawnSpec` (named apart from `hydraui-sets`' `PaneSpec`) holds hydra's
paths, the `LaunchEnv` already prepared by the app, the cwd (which must
exist), shell kind, `git_bash` setting, startup command, grid size and the
inherited environment:

1. `find_shell`; missing shell → error.
2. Write the per-pane init script (§4.4–4.5) with integration on.
3. Build the command as `shell_command` does, as a `portable_pty::CommandBuilder`,
   applying `env_changes()` on top of `TERM=xterm-256color` and
   `COLORTERM=truecolor` (an environment's `[env]` can override them). Start it in a ConPTY sized to the pane.
4. Assign the child to a new Windows **Job Object** with
   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so closing the pane ends the whole
   process tree (`npm` → `node`, etc.).

When the startup command exits, the user is left at a normal prompt in the
same environment.

### 6.2 Threads and events

Per pane, a reader thread feeds PTY bytes into an `alacritty_terminal::Term`
held behind a mutex and scans for the OSC sequences in §6.3. It sends events
on a channel:

```rust
enum TermEvent { Wakeup, CwdChanged(PathBuf), Prompt(PromptState), Title(String), Bell, Exited(Option<i32>) }
enum PromptState { AtPrompt, CommandRunning, CommandDone(Option<i32>) }
```

The UI renders from the shared grid at most once per frame on `Wakeup`, so
heavy output never blocks input. A pane sends one `Wakeup` until the app next
reads the grid (`Pane::with_term`), so heavy output never floods the channel
either. `Title("")` means the title was reset. Input goes the other way through the PTY
writer. Resizes go to both the PTY and `Term`.

### 6.3 Shell integration

With `integration = true` the init scripts emit:

- **OSC 7** `file://<host>/<path>` with the current folder, from each prompt.
- **OSC 133** `A` at prompt start, `B` at input start, `C` when a command
  starts, `D;<exit>` when it ends.

pwsh: the existing `prompt` wrapper emits `D`, OSC 7, `A`, the prompt, `B`;
PSReadLine's `AcceptLine` key handler is wrapped to emit `C`. bash: a
`PROMPT_COMMAND` hook emits `D`/OSC 7/`A`, `PS1` ends with `B`, and a `DEBUG`
trap (guarded to fire once per command) emits `C`. This is the scheme VS
Code and WezTerm use. If the user's own profile breaks it, the pane just has
no prompt state: unload never asks, and folder double-click in the tree is
disabled.

### 6.4 Exit and close

- Shell exits: the pane shows "exited (code) — Enter to restart, Ctrl+W to
  close". Restart runs `prepare` again, so edits to `env.toml` apply.
- Close: drop the PTY and close the Job Object handle, which kills the tree,
  then release the pane's shell lock (§3.1).

## 7. The app (`hydraui`)

### 7.1 Window

```
┌───────────────────────────────────────────────────────────┐
│ [client-a ▾] api● infra●  │ [personal ▾] blog●  │ Loose: +│  tab strip, grouped by set
├────────────┬──────────────────────────────────────────────┤
│ file tree  │  pane (claude)        │ pane (npm run dev)   │
│            │                       ├──────────────────────┤
│            │                       │ pane (shell)         │
├────────────┴──────────────────────────────────────────────┤
│ work · gh: <account> · claude: <account> · E:/work/…      │  status bar, focused pane
└───────────────────────────────────────────────────────────┘
```

- Tabs are tinted with their environment's `color` from `env.toml`; with
  none, a stable colour derived from the name. A pane split across
  environments shows the focused pane's colour.
- Set groups collapse and have a menu: Reload, Unload.
- Panes form a split tree with draggable dividers; the focused pane has a
  border in its environment's colour.
- The status bar shows the focused pane's environment, the accounts reported
  by hydra's whoami logic (cached, refreshed on focus change and every 60 s),
  and its current folder.
- Clicking the environment's name opens its accounts: one row per provider
  check (✓ ok, ! mismatch, ✗ missing, · info, with the detail, such as an AWS
  ARN or a GitHub login), and Refresh to check again now. Its "Add account…"
  picker turns a tool on in env.toml (as `hydra add`) for the tools that need
  only a sign-in (claude, github, azure, gcloud, codex, gws), then signs in to it
  in a new tab; or borrows a tool another environment has with its own login
  (`from = "<env>"`). Tools that need values first (git, aws, gemini,
  kube) are added with `hydra add`.

### 7.2 Terminal view

Renders the `Term` grid following Zed's `terminal_view`: colours (16, 256,
true colour), bold/italic/underline, cursor shapes, selection, scrollback,
OSC 8 hyperlinks (Ctrl-click opens), mouse reporting, bracketed paste, find in
scrollback. Font, size and colour scheme come from `config.toml`, with a
built-in default theme.

### 7.3 File tree

Toggled with `Ctrl+B`. Rooted at the focused pane's last OSC 7 folder (before
any OSC 7, the pane's start cwd). Folders load lazily; `.gitignore` is
respected; `notify` keeps it current.

- Click a file: open it in the tab's file pane (§7.8).
- Ctrl-click a file: insert its quoted path at the terminal's cursor.
- Double-click a folder: send `cd '<path>'` + Enter, only when the pane is
  `AtPrompt`; otherwise disabled with a tooltip.

### 7.4 Launcher

`Ctrl+Shift+P` opens a fuzzy command palette:

- Sets, marked loaded/unloaded/invalid: Enter loads or focuses; a second
  action unloads.
- Environments: open an ad-hoc tab in that environment.
- **New environment**: name it (checked as it is typed: a valid name, not
  taken), create it as `hydra env new` does, and open a tab in it. Also the
  last entry of the "Open a tab in…" picker, and what that picker opens when
  there are no environments yet.
- **Sign in**: pick environment and tool; runs the same sign-in command as
  `hydra auth <tool> <env>` in a new pane in that environment.
- **Set secret**: pick `<env>/<key>` (from references in `env.toml` that
  aren't stored yet, or typed), enter the value in a masked input, store via
  hydra's secret store.
- Open config folder; Reload config.

### 7.5 Errors

- `prepare` fails: the pane shows a banner with hydra's error text and fix
  hint, plus Retry. No shell starts.
- Warnings from `prepare` (§4.2): a dismissible strip at the top of the pane.
- Set or `config.toml` errors: a toast naming file and line; the launcher
  marks the set invalid.

### 7.6 Ad-hoc tabs and keys

`Ctrl+T` opens a Loose tab in the focused pane's cwd. Environment: folder
binding → `default_env` in `config.toml` → a picker.

| Action | Default |
|---|---|
| New tab | Ctrl+T |
| Close pane | Ctrl+W |
| Split right / down | Ctrl+Shift+D / Ctrl+Shift+E |
| Move focus | Alt+Arrows |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V |
| Find | Ctrl+F |
| Launcher | Ctrl+Shift+P |
| File tree | Ctrl+B |

Rebindable under `[keys]` in `config.toml`. A split pane inherits the
focused pane's environment and current folder.

### 7.7 Packaging

`hydraui.exe` is a GUI-subsystem exe (`windows_subsystem = "windows"`) with
version info like `hydra.exe`. It runs on its own; `hydra.exe` is not
required. Each release ships both executables
(`hydra-x86_64-pc-windows-msvc.exe` and `hydraui-x86_64-pc-windows-msvc.exe`,
each with a `.sha256`); `scripts/deploy.ps1` builds and installs both. `hydra update`
updating hydraui too is out of scope.

### 7.8 File pane

A pane that shows files, beside a tab's terminals in the same split tree.

**Opening.** Clicking a file in the tree (§7.3) opens it in the tab's file
pane. A tab has at most one; when it has none, the terminal (below) is split
to the right, 50/50, and the file pane takes the right side. A file already
open switches to its file tab.

**Layout.** The pane has a row of file tabs (name, a dot for unsaved edits,
×), a header, and the file. Header: a **Raw / Rendered** switch for Markdown
(`.md`, `.markdown`), and **Open in editor** (`editor` from `config.toml`,
else the OS default).

- **Raw**: gpui-component's code editor, editable, with syntax highlighting
  chosen by extension, line numbers and find (`Ctrl+F`). `Ctrl+S` saves.
- **Rendered**: Markdown, read-only, reflecting unsaved edits. Markdown files
  open in Rendered, others in Raw. Links open in the browser when clicked;
  images aren't fetched from the network; nothing runs.

**Reading.** A file over 5 MB, or binary (a NUL byte in the first 8 KB, or not
valid UTF-8), shows "can't show this file" with Open in editor; so does one
that can't be read, with the reason. A file's line endings (CRLF or LF) and
UTF-8 BOM are recorded on load and written back on save: a save changes only
what was edited.

**Saving** is atomic: a temporary file beside the original, then a replace.
A save that fails (read-only, locked, folder gone) is a toast; the edits stay,
still unsaved.

**Closing.** Closing a file tab with unsaved edits asks Save / Discard /
Cancel. Closing the last file tab closes the pane and the terminal gets the
width back. Closing the pane (`Ctrl+W` while it has the focus, or closing the
tab) asks once when any file has unsaved edits. Closing a tab's last terminal
closes the tab, asking first when its file pane has unsaved edits.

**Changes on disk** (`notify`, as the tree): a file with no unsaved edits
reloads; one with edits shows "changed on disk" with Reload / Keep mine. A
deleted or renamed file's tab is marked deleted and keeps its edits, so Save
writes it back.

**Which terminal.** The status bar, git summary, the tree's root, Ctrl-click
insert, double-click `cd` and Split use the focused pane when it is a
terminal, else the tab's last focused terminal. Split, close and focus moves
work on either kind of pane.

## 8. Errors summary

| Situation | Result |
|---|---|
| Set TOML invalid / env unresolvable | Set doesn't open; toast with file and line |
| `prepare` fails for a pane | That pane shows banner + Retry; rest of set opens |
| Shell not found | Pane banner naming the shell and how to install it |
| Shell exits | "exited (code)", Enter restarts with fresh `prepare` |
| Set file changed while loaded | "changed — reload?" on the group |
| No OSC 133 from shell | No running-command confirm; tree `cd` disabled |

## 9. Testing

- **hydra crates:** unit tests for each §4 change, including that the CLI's
  rendered init scripts and hook files are byte-identical with defaults.
- **hydraui-sets:** parsing, inheritance, relative `cwd`, binding fallback,
  validation messages, loaded-set bookkeeping, using temp `HYDRA_HOME`
  fixtures.
- **hydraui-term:** OSC parser unit tests (split across reads, malformed
  input). Integration tests that start real pwsh and Git Bash in ConPTY with
  a temp `HYDRA_HOME` and check: `CwdChanged` after `cd`, prompt states
  around a command, exit code in `CommandDone`, an environment variable from
  `[env]` visible, an inherited `GH_TOKEN` removed, Job Object kills a child
  process on close. Skipped with a message when the shell isn't installed.
- **hydraui:** view-model logic (split tree operations, set↔tab mapping,
  launcher filtering, key config parsing) as plain unit tests. Rendering is
  checked by hand against a smoke checklist in the plan.

## 10. Risks

- **GPUI on Windows.** Its Windows backend is newer than macOS's and its API
  changes often. The plan's first task is a throwaway spike: one GPUI window
  rendering one pwsh pane through `alacritty_terminal` and ConPTY, with
  typing, resizing and colour. If it isn't workable, stop and revisit the
  UI stack (egui with `egui_term` is the fallback) before building further.
  `hydraui-sets`, `hydraui-term` and the §4 changes don't depend on GPUI.
- **Shared state with the CLI.** Both tools regenerate `state/<env>/` on
  launch from the same code. The only difference is the guard (§4.1), and
  hydraui writes its guard-free gitconfig to its own file so it can't switch
  off the guard for CLI terminals.
- **No folder guard in hydraui panes.** Credentials stop a push to a repo
  the account can't reach, but not a local commit, and not a push when both
  accounts can reach the repo. Each environment's git author still applies,
  so commits are attributed correctly. Accepted by the user.
- **Shell integration vs user profiles.** Heavily customised prompts can
  break OSC 133; the features that need it degrade as in §6.3.
