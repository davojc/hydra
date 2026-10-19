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
  hydra-update/   release check, download, checksum, exe swap; shared by
                  `hydra update` and hydraui's Update button (§7.11)
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
  layouts/         saved layouts (*.hydraui), Save As's default folder (§7.12)
  recent.toml      recent layouts, shared by every hydraui window
  running/         <pid>-<id>.toml: each running window's layout and handle
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
- A set can also be opened from a group file anywhere (§7.9). It is then
  known by its file's path; its name (the group header) comes from the file
  name, made valid (lowercase; other characters become `-`), with `-2`, `-3`…
  when a set of that name is already loaded.
- If a loaded set's file changes, its group header shows "changed — reload?".
  Reload is unload + load. Running panes are never changed in place.
- A **layout** records what is open in a window: the loaded sets in order
  (sets from the sets folder by name, sets opened from group files by path);
  the Loose tabs in the set file's tab format (layout, ratios, and each pane's
  env, shell and current folder); whether the file tree was open and its
  width; and the active tab's position in the strip. A window backed by a
  layout file (§7.12) writes it when tabs, splits, sets or the tree change,
  and when the window closes. Opening a layout loads its sets (startup
  commands run again), then the Loose tabs, the tree and the active tab. An
  invalid or deleted set or group file, or a Loose tab whose env or folder is
  gone, is skipped with a toast. Layouts without group paths still load. A
  Loose tab's file pane is saved as a `files` node (§5.1) and comes back with
  its files and active file tab; unsaved edits are not kept. Scrollback,
  running commands, window size and dragged ratios in set tabs are not kept.
  (Before §7.12 this was one `session.toml`, written and reopened
  automatically; it is converted once, §7.12.)

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
│ ⑂ client-a ▾               [Update hydra 0.7.0]  _  □  ✕  │  title bar (§7.11)
├───────────────────────────────────────────────────────────┤
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
- The status bar shows the focused pane's environment, its accounts as each
  tool's name with a green ✓ (OK) or red ✗ (missing or mismatched) from
  hydra's whoami logic (cached, refreshed on focus change and every 60 s;
  info-only checks left out; the details are in the accounts popover), and its
  current folder.
- Clicking the environment's name opens its accounts: one row per provider
  check (✓ ok, ! mismatch, ✗ missing, · info, with the detail, such as an AWS
  ARN or a GitHub login), and Refresh to check again now. Its "Add account…"
  picker turns a tool on in env.toml (as `hydra add`) for the tools that need
  only a sign-in (claude, github, azure, gcloud, codex, gws), then signs in to it
  in a new tab; or borrows a tool another environment has with its own login
  (`from = "<env>"`). Tools that need values first have a form: git (name,
  email; optional SSH and signing key paths, which must exist), aws
  (profile; then its sign-in), gemini (the API key, stored as the secret
  `<env>/gemini` and referenced from env.toml), kube (an optional kubeconfig
  path; empty gives the environment its own). A form shows what's wrong and
  stays open; nothing is written until the values check out. aws's private
  config files, claude's MCP exclusions and github's owner/strict settings
  stay with `hydra add`. git can't be borrowed, so it isn't offered.
- A borrowed login shows "from <env>" on its row. Each row whose tool another
  environment has with its own login has **Use from…**: pick that
  environment and the tool borrows its login (`from = "<env>"`, replacing the
  tool's own settings except github's owner/strict). Switching back to an own
  login is `hydra remove` + add.

### 7.2 Terminal view

Renders the `Term` grid following Zed's `terminal_view`: colours (16, 256,
true colour), bold/italic/underline, cursor shapes, selection, scrollback,
OSC 8 hyperlinks (Ctrl-click opens), mouse reporting, bracketed paste, find in
scrollback. Font, size and colour scheme come from `config.toml`, with a
built-in default theme.

- Finishing a selection (drag, double- or triple-click) copies it to the
  clipboard, with a "Copied" label in the pane's corner for a moment; an empty
  selection copies nothing. Ctrl+C copies too.
- Scrolled up into the history, a round ↓ button shows at the pane's bottom
  right; clicking it jumps to the latest output, and it goes once at the
  bottom. New output doesn't move a view that is scrolled up.
- Opening the accounts popover (§7.1) checks the accounts again, as Refresh.

### 7.3 File tree

Toggled with `Ctrl+B`. Rooted at the focused pane's last OSC 7 folder (before
any OSC 7, the pane's start cwd). Folders load lazily; `.gitignore` is
respected; `notify` keeps it current.

- Click a file: open it in the tab's file pane (§7.8).
- Ctrl-click a file: insert its quoted path at the terminal's cursor.
- Double-click a folder: send `cd '<path>'` + Enter, only when the pane is
  `AtPrompt`; otherwise disabled with a tooltip.
- In a git repository, files show their status from `git status`
  (`porcelain=v2`, refreshed with the status bar's): **M** modified (yellow),
  **A** added and **?** untracked (green), **D** deleted (red), ignored files
  dimmed; a folder holding changes gets a dot. Outside a repository nothing
  is shown.
- Right-click a changed file: **Show changes** opens a side-by-side diff of
  the file against the last commit in the file pane (§7.8).

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
- **Open file…** (Ctrl+Shift+O) and **Search in files…** (Ctrl+Shift+F),
  §7.10.
- **Delete environment…**: like `hydra env rm`: refused while the
  environment has open terminals (hydraui's panes, or hydra's running lock for
  other processes), naming them; otherwise a confirm listing its folder and
  secrets, then a separate question whether to delete its saved logins too.
  Bindings still pointing at it are listed, not removed. A failure partway is
  reported; what was deleted stays deleted.
- **Set environment home…**: a folder from the Open dialog becomes the
  environment's home (new terminals open there) and is bound to it, as
  `hydra env new --home` does.
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
each with a `.sha256`); `scripts/deploy.ps1` builds and installs both. The
release job checks that both exes carry the release's version: `hydra
--version` prints it, and `hydraui.exe`'s ProductVersion (file properties)
must equal it. `hydra update` still updates `hydra.exe` only; hydraui's
Update button updates both (§7.11).

**Installer.** Each release also ships `hydra-setup.exe` (with its `.sha256`),
a small Rust program in the workspace (`crates/hydra-setup`, built by cargo like
the rest, no third-party installer tool). It installs both apps for the
current user, with no admin prompt:

- **Dialog.** A native Windows task dialog: "Install hydra <version>", what
  goes where, a "Create a desktop shortcut" tick box, Install / Cancel; then a
  progress bar while it downloads; then "Installed" with a ticked "Launch
  hydraui" (or the reason it failed, with nothing left half-done).
- **Download.** It fetches that release's `hydraui-x86_64-pc-windows-msvc.exe`
  and `hydra-x86_64-pc-windows-msvc.exe` (by its own version's tag, not
  "latest") and checks each against its `.sha256`, with `hydra-update`'s code.
  `--from <folder>` installs from local copies of those files and their
  `.sha256` instead (offline, and CI before the release exists). A `-dev`
  build only installs `--from`.
- **Where.** `%LOCALAPPDATA%\Programs\hydra` as `hydraui.exe` and `hydra.exe`,
  plus `hydra-setup.exe` itself as the uninstaller. The folder is the user's,
  so `hydra update` and the Update button keep working, and the Update button
  always finds `hydra.exe` beside hydraui. Exes that are running (an upgrade
  while hydraui is open) are replaced as the updaters replace them: renamed
  aside, cleaned up at their next start.
- **Start menu and shortcuts.** A Start menu entry for hydraui; a desktop
  shortcut only if ticked.
- **Layout files.** `.hydraui` files (§7.12) open in hydraui: the ProgID
  `hydraui.layout` under `HKCU\Software\Classes` (open command
  `"<folder>\hydraui.exe" "%1"`, hydraui's icon, "hydraui layout") and
  `.hydraui` pointing at it; Explorer is told associations changed. Uninstall
  removes both, but leaves `.hydraui` alone if it no longer points at
  `hydraui.layout`. The portable exe registers nothing.
- **PATH.** The folder is added to the user's `PATH` (`HKCU\Environment`) once:
  an entry already there (any case, with or without a trailing `\`) isn't
  added again; Windows is told the environment changed. If another
  `hydra.exe` is on `PATH`, the last page names its folder and leaves it alone.
- **`hydra init`** runs, hidden, when `config.toml` doesn't exist yet under
  `HYDRA_HOME` (else `~/.hydra`).
- **Settings → Apps.** An entry `hydra` under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra` (version,
  publisher, icon, size, install location, uninstall commands). Running a newer
  `hydra-setup.exe` upgrades in place.
- **Uninstall** (Settings → Apps, or `hydra-setup.exe --uninstall`): asks
  first; removes both exes, left-over `*.old.exe` and `*.new.exe`, the
  shortcuts, its `PATH` entry, the `.hydraui` association and its Settings
  entry, then itself; `~/.hydra` stays.
- **Silent.** `--silent` installs (and with `--uninstall`, uninstalls) with no
  dialog; `--dir <folder>` installs elsewhere (tests).
- After an in-app update, hydraui sets `DisplayVersion` in that Settings entry
  when it is running from the installed folder.

The release job builds `hydra-setup.exe` with the release's version, then
tests it before publishing: a silent install `--from dist` into a temporary
folder (twice, as an upgrade), checking the files, `hydra --version`, the
`PATH` entry (once) and the Settings entry, then a silent uninstall checking
they're all gone and other `PATH` entries are untouched. The portable exes
stay in every release; the updaters use them.

### 7.8 File pane

A pane that shows files, beside a tab's terminals in the same split tree.

**Opening.** Clicking a file in the tree (§7.3) opens it in the tab's file
pane. A tab has at most one; when it has none, the terminal (below) is split
to the right, 50/50, and the file pane takes the right side. A file already
open switches to its file tab.

**Layout.** The pane has a row of file tabs (name, a dot for unsaved edits,
×), a header, and the file. Header: a **Raw / Rendered** switch for Markdown
(`.md`, `.markdown`) and OpenAPI specs (below), and **Open in editor**
(`editor` from `config.toml`, else the OS default).

- **Raw**: gpui-component's code editor, editable, with syntax highlighting
  chosen by extension, line numbers and find (`Ctrl+F`). `Ctrl+S` saves.
- **Rendered**: Markdown, read-only, reflecting unsaved edits. Markdown files
  and OpenAPI specs open in Rendered, others in Raw. Links open in the browser
  when clicked; images load only from local files, never from the network;
  only `http`, `https` and `mailto` links open; nothing runs.

**Reading.** A file over 5 MB, or binary (a NUL byte in the first 8 KB, or not
valid UTF-8), shows "can't show this file" with Open in editor; so does one
that can't be read, with the reason. A file's line endings (CRLF or LF) and
UTF-8 BOM are recorded on load and written back on save: a save changes only
what was edited. (A file with mixed line endings is saved with its first
line's ending throughout.) Text pasted with Windows line endings is saved
with the file's own.

**Saving** is atomic: a temporary file beside the original, then a replace.
A save that fails (read-only, locked, folder gone) is a toast; the edits stay,
still unsaved.

**Closing.** Closing a file tab with unsaved edits asks Save / Discard /
Cancel. Closing the last file tab closes the pane and the terminal gets the
width back. Closing the pane (`Ctrl+W` while it has the focus, or closing the
tab) asks once when any file has unsaved edits. Closing a tab's last terminal
closes the tab, asking first when its file pane has unsaved edits.

**Changes on disk** (`notify`, as the tree): a file with no unsaved edits
reloads, as one undoable edit that keeps the cursor's line and column; one with edits shows "changed on disk" with Reload / Keep mine. A
deleted or renamed file's tab is marked deleted and keeps its edits, so Save
writes it back.

Opening a file, choosing a file tab or moving the focus to the pane focuses
the file: its editor in Raw. Opening, closing and switching file tabs save the
layout at once (in a window backed by a file, §7.12).

**Diff tabs.** Show changes (§7.3) opens a read-only "diff" file tab: the file
against the last commit (`git diff HEAD -- <file>`; `--no-index` for an
untracked file) in gpui-component's diff view, side by side, with Open file
to switch to the file. If git fails the tab shows git's error; a binary file
shows "binary file changed".

**OpenAPI specs.** A `.yaml`, `.yml` or `.json` file whose top level has
`openapi:` with a value starting `3.` (OpenAPI 3.0 and 3.1) renders as API
docs: a native, read-only view, with no "try it out". Detection reads only the
file's first 4 KB for a top-level `openapi` key, so other YAML (CI files,
compose files) costs nothing. A Swagger 2.0 file (`swagger: "2.0"`) opens in
Raw with "Swagger 2.0 isn't rendered" in the header and no switch.

- **Model.** YAML (`serde-saphyr`) or JSON (`serde_json`) becomes: the title,
  version, description (Markdown) and servers; operations grouped by their
  first tag in the tags' order (`tags` list first, then as they appear), with
  untagged ones under "default", each operation in file order; and
  `components.schemas`. Path-level parameters apply to each of the path's
  operations; an operation's own parameter of the same name and `in` wins.
- **References.** `$ref`s within the file (`#/...`) are followed when shown,
  not inlined, so recursive schemas work; a ref back to a schema already open
  above shows "↻ Pet" instead of expanding. A ref into another file
  (`other.yaml#/Pet`) shows as a label that opens that file (resolved against
  the spec's folder) in the file pane. A ref that resolves to nothing shows
  "⚠ <ref> (missing)". Nothing is fetched from the network; server URLs are
  text.
- **Schemas** show as trees: each property's name, type (`format`, enum
  values), and required mark; arrays read `Pet[]`; `allOf` is merged into one
  object; `oneOf` / `anyOf` read "one of: A | B", each expandable; OpenAPI 3.1
  `type: [string, "null"]` reads `string?`. Trees open one level at a time and
  start collapsed below their first level.
- **View**, top to bottom: the title and version, the description, the
  servers; a filter box (narrows operations by path, summary, method or
  operationId, hiding tags with none left); a collapsible group per tag
  (open at first) with its description, then one row per operation: a method
  badge (GET blue, POST green, PUT amber, PATCH teal, DELETE red, others
  grey), the path in monospace, the summary muted, struck through when
  deprecated. Clicking a row opens its description, **Parameters** (a table:
  name, in, type, required, description), **Request body** (each media type's
  schema) and **Responses** (each status, 2xx green, 4xx amber, 5xx red, with
  its description and each media type's schema). Last, **Schemas**
  (collapsed): every `components.schemas` entry as a tree. Text can be
  selected and copied.
- **State.** What is open is kept per file tab while it is open, not in the
  layout. Rendered follows unsaved edits, parsing again 300 ms after typing
  stops and keeping open rows open (by method and path).
- **Errors.** A YAML or JSON error shows a banner with the message, its line
  and **Go to line** (Raw, at that line), with the last good render greyed
  below it, or nothing below if there was none. A parsable file missing
  `info` or `paths` renders what it has, with a banner naming what's missing.

**Opening at a line.** Quick open, search and clickable paths (§7.10) open a
file at a line and column: the file's tab (a new one or the open one), Raw
for Markdown, the cursor on that line, scrolled into view. A line past the end
goes to the last line.

**Which terminal.** The status bar, git summary, the tree's root, Ctrl-click
insert, double-click `cd` and Split use the focused pane when it is a
terminal, else the tab's last focused terminal. Split, close and focus moves
work on either kind of pane.

### 7.9 Layout and group files

Open tabs can be saved to files and opened from them, anywhere on disk,
through the Windows Save As / Open dialogs. Saving is a snapshot: the open
tabs don't change. Cancelling a dialog does nothing.

- A **group file** is a set file (§5.1): tabs with their layout, ratios,
  each pane's env, current folder, shell and startup command, file panes,
  and tab titles. Opening one opens its tabs as a set (§5.4), under a group
  header named after the file. Group files saved into the sets folder are
  ordinary sets and appear in the launcher's set list.
- A **layout file** (`*.hydraui`, TOML inside) is the window (§5.4): the
  open sets (by name or path), the Loose tabs, the file tree, the active tab.
  A window is backed by at most one, and keeps it saved (§7.12). Opening one
  in an untitled window with no tabs (the start page, say) fills that window
  and backs it; in any other window it opens in a new hydraui window.
  Layouts are no longer merged into a window.

Where:

- Launcher: **Save layout as…**, **Open layout file…**, **Open group
  file…**, **Save Loose tabs as group…** (every Loose tab into one group
  file). The title bar's layout menu has the layout entries too (§7.11).
- Right-click a tab: **Save tab as group…**, **Close tab**.
- Right-click a group header: **Save group…** (its tabs as they are now,
  with the Save As dialog starting on the group's own file), **Unload group**.

What is saved: current folders and splits; for set tabs also their titles and
startup commands; file panes (§7.8). Scrollback and running commands are not.

Errors: a file that isn't a valid group or layout (TOML, unknown env, missing
folder) opens nothing and a toast names the file and problem; a layout whose
groups or tabs partly fail opens the rest, with a toast per failure; a layout
file given to "Open group file…", or the reverse, says so. Writes are atomic
(a temporary file, then a replace); a failed write is a toast and changes
nothing.

### 7.10 Finding files

All three use the focused terminal's folder (§7.8 "Which terminal"; the
user's home when there is none) and respect `.gitignore` as the tree does.

- **Clickable paths.** Ctrl+click a path in terminal output to open it at its
  line (§7.8): `src/main.rs`, `src/main.rs:42`, `src/main.rs:42:7`,
  `src\main.rs(42,7)` (MSVC / C#), absolute or relative to the terminal's
  folder. Only existing files count, so other words never become links; the
  path underlines while hovered with Ctrl held. OSC 8 links keep working as
  before.
- **Quick open** (`Ctrl+Shift+O`, rebindable as `open_file`): a picker of the
  folder's files (at most 50,000, with a note when cut off), fuzzy-matched as
  typed — consecutive letters, word starts and the file name rank higher, so
  `wsrs` finds `workspace.rs`. `name:42` opens at line 42.
- **Search in files** (`Ctrl+Shift+F`, rebindable as `search`): plain text,
  with case-sensitive and regex toggles; results as `file:line` and the line,
  opening at that line. It stops at 1,000 results and says so, skips files
  over 5 MB and binary files (§7.8's rules), and a newer search cancels an
  older one. An invalid regex shows its error and searches nothing.

### 7.11 Title bar and updates

hydraui draws its own title bar (gpui-component's `TitleBar`): a slim row
above the tab strip, in the theme's colours, with the icon and the layout's
name on the left and minimise, maximise/restore and close on the right.
Dragging its empty part moves the window and double-clicking maximises, as a
system title bar does.

**Layout name.** The window title (taskbar, Alt+Tab) is `hydraui - <layout>`,
the layout file's name without `.hydraui`, or `hydraui - untitled`; the
current tab is no longer in it. The title bar shows the name with a ▾; an
untitled window's name is muted, with the tooltip "Not saved: Save layout as…
to keep this window". Clicking the name opens a menu: **Save layout as…**,
**Open layout file…**, **Recent ▸** (the recent layouts, §7.12), and
**Reveal in Explorer** (saved layouts only). On the start page the title
reads `hydraui` and the name is hidden.

**Version.** hydraui knows its version as hydra does: `HYDRA_VERSION` at
compile time in release builds, else `<Cargo version>-dev`.

**Checking.** At start and then every 6 hours, off the UI thread, hydraui
asks GitHub for the latest release (the same request as `hydra update`,
honouring `HYDRA_UPDATE_API`). When it is newer than the running version, the
title bar shows **Update hydra <version>** just left of the window controls.
A `-dev` build never shows it. No network, or an answer it can't read, shows
nothing and says nothing.

**Updating.** Clicking the button:

1. Refuses, with a toast naming the processes, while any `hydra.exe` (or a
   left-over `hydra.old.exe`) runs, as `hydra update` does. hydraui's
   terminals run shells, not hydra, so they don't count.
2. Downloads `hydraui-x86_64-pc-windows-msvc.exe` and, when a `hydra.exe` is
   found, `hydra-x86_64-pc-windows-msvc.exe`, and checks each against its
   `.sha256`. `hydra.exe` is the one next to `hydraui.exe`, else the first on
   `PATH`; with none, only hydraui is updated and the toast says so.
3. Swaps each exe as `hydra update` does: the current one is renamed
   `<name>.old.exe` (Windows allows renaming a running exe) and the new one
   takes its name. Everything is downloaded and checked before anything is
   renamed; if a rename fails, what was already swapped is put back, so
   nothing is left half-updated.
4. Turns the button into **Restart to finish** and toasts "Updated to
   <version>". Restart saves the layout (§7.12), starts the new
   `hydraui.exe` with the layout file (an untitled window is first saved as
   `~/.hydra/hydraui/layouts/untitled-<n>.hydraui`, the lowest free `n`, so
   nothing is lost) and closes this window: tabs come back, but what ran in their
   terminals stops, which is why it is never automatic. The button says
   "Updating…" while it works; a failure (no network, a checksum that doesn't
   match, a file in use) is a toast with the reason, and the button stays.

At start, hydraui deletes a left-over `hydraui.old.exe` next to it (and
`hydra.old.exe` next to the `hydra.exe` it found), ignoring failures — the
old exe may still be running for a moment.

The release check, download, checksum and swap live in `hydra-update`, used by
both `hydra update` (whose behaviour and output don't change) and hydraui.

### 7.12 Layouts and the start page

Each window is either **untitled** or backed by one layout file (§7.9). Two
hydraui windows never write the same file, and nothing is written behind the
user's back except the window's own layout.

**Saving.** A backed window writes its layout (atomically, §7.9) whenever
tabs, splits, sets or the tree change, and when it closes; not while the
layout is opening. Sets and Loose tabs of the layout that didn't open (a
missing folder, a renamed environment) stay in what is written. If nothing
in a layout opens, the window lets the file go untouched (a toast says so)
and shows the start page again. An untitled window
writes nothing; closing it loses its layout, with no prompt (the title bar
says so, §7.11). **Save layout as…** writes the window to a new file (the
dialog starts in `~/.hydra/hydraui/layouts/`, created on first use, or in the
current file's folder), backs the window with it from then on, and lets go
of the old file. A failed write is a toast; the window keeps its old file.

**Start page.** Starting hydraui with no file shows a start page in place of
the tab strip and panes (title bar and status bar stay; the status bar is
empty):

- **Recent layouts**, newest first: each row the name, its folder (with `~`),
  and when it was last opened ("2 h ago"). A row whose layout another hydraui
  has open says "open in another window".
- **New window**: one plain terminal in the home folder, untitled (today's
  start without a session).
- **Open layout file…**: the Open dialog.
- A filter box has focus: typing narrows the rows by name or folder; ↑/↓ move;
  Enter opens the selected row (with no rows, Enter is New window); Esc
  clears the filter. The page also opens from the launcher's **Start page**.
- Choosing a row whose file is missing removes it from the list with a toast.

**Recent list.** `~/.hydra/hydraui/recent.toml`: up to 12 entries (path,
last opened), newest first, shared by every hydraui. Opening or saving a
layout moves it to the top. Each change reads the file afresh, changes it and
replaces it atomically, so two windows don't lose each other's entries; an
unreadable file counts as empty. Paths are compared case-insensitively with
`/` and `\` alike.

**One window per file.** A window that opens or saves a layout claims it with
a named mutex, `Local\hydraui-layout-<hex sha256 of the full path, lowercase,
\ separators>`, held while the window has the file and released when it lets
go or exits (Windows releases it if hydraui crashes). It also writes
`~/.hydra/hydraui/running/<pid>-<id>.toml` (the layout path and the window
handle; one per claim), removed when it lets go; entries whose process is gone are ignored and
deleted. Opening a file another window has claimed brings that window to the
front (from its running entry) and opens nothing; a start-page window or a
process started only to open that file then closes itself. **Save layout
as…** onto a file another window has claimed is refused with a toast naming
it.

**Command line.** `hydraui.exe <file>` opens that layout in a new window
(the start page is skipped), or brings forward the window that has it. A
file that isn't a valid layout opens the start page with a toast naming the
file and the problem.

**Old files.** Opening a `.toml` layout such as `*.hydraui.toml` (Open
dialog, recent list, command line) converts it; any other file that isn't
`.hydraui`, and a `.toml` file that holds no layout (an empty one), is "not a
layout file" and is left alone. Conversion: its contents are written to `<name>.hydraui`
beside it, the old file is deleted, the window is backed by the new file, the
recent entry is updated, and a toast says "Converted <old> → <new>". If
`<name>.hydraui` already exists, nothing is written or deleted: the old file
opens untitled with a toast saying why. The first start of a hydraui with
§7.12 converts `~/.hydra/hydraui/session.toml` the same way into
`~/.hydra/hydraui/layouts/last-session.hydraui` (with a free name if that's
taken), puts it at the top of the recent list and deletes `session.toml`; an
empty or unreadable session is just deleted.

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
