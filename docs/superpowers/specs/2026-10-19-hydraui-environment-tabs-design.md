# hydraui: environment tabs

Changes the window from "tabs, each with its own environment and split panes, grouped by set"
to "one tab per environment, each environment with its own list of terminals". Replaces the
parts of the [hydraui spec](2026-10-17-hydraui-design.md) named below; everything not named
there stays as it is.

## 1. Why

Work happens per environment. Today an environment's terminals are scattered across tabs and
splits, sets add a second grouping on top, and the tab strip shows every tab of every set. The
user wants to pick which environments a window shows, see each one's terminals ("conversations",
often Claude sessions) as a list, and switch between them, with the file tree out of the way on
the right.

### Goals

- A layout holds the environments added to it and, for each, its open terminals.
- The tab strip shows only those environments, one tab each.
- The left side lists the selected environment's terminals; the middle shows one of them.
- The file tree sits on the right, rooted at the shown terminal's folder.
- Opening a file still splits the middle, the file pane on the right next to the tree.

### Non-goals

- **Terminal splits.** Removed for now; the user hasn't settled how they should behave.
  Don't design for their return beyond not making it hard.
- **Sets and group files.** They go away (§4).
- **Old layouts.** The user is fine starting fresh: no conversion of today's `.hydraui` files.

### Anti-goals

- Showing every environment hydra knows. Only the ones added to the layout appear.
- Losing running work silently: closing an environment's tab with commands running asks first,
  as unloading a set does today.

## 2. The window

```
┌──────────────────────────────────────────────────────────────────┐
│ ⑂ client-a ▾                       [Update hydra 0.7.0]  _  □  ✕ │ title bar (unchanged)
├──────────────────────────────────────────────────────────────────┤
│ [work●] [personal] [client-a●]  +                                │ environment tabs
├────────────┬──────────────────────────────┬─────────┬────────────┤
│ + New      │                              │         │            │
│ ● claude   │  terminal (shown)            │  file   │ file tree  │
│   api      │                              │  pane   │            │
│ ◌ npm dev  │                              │(if open)│            │
├────────────┴──────────────────────────────┴─────────┴────────────┤
│ work · gh: ✓ · claude: ✓ · E:/work/…                             │ status bar (unchanged)
└──────────────────────────────────────────────────────────────────┘
```

### 2.1 Environment tabs (top)

- One tab per environment in the layout, in the layout's order, labelled with the environment's
  name and tinted with its colour (as tabs are tinted today).
- With no environments in the layout (all closed), the strip shows only **+** and the middle an
  empty state offering it.
- Each tab shows the busiest activity of its terminals (working / waiting / idle, the existing
  `activity` module), so a background environment that needs attention is visible.
- **+** at the end opens a picker of environments not yet in the layout (with **New
  environment…** last, as the launcher's "Open a tab in…" picker does). Choosing one adds its tab
  with one new terminal (§2.2 for its folder) and selects it.
- Right-click a tab: **New terminal**, **Restart environment**, **Close environment**.
  - **Close environment** asks first when unsaved file edits or running commands exist (reuse
    `confirm_unsaved` and the running-commands dialog from `workspace/sets.rs`), then closes all
    its terminals and its file pane and removes the tab.
  - **Restart environment** restarts every terminal of the environment so an `env.toml` edit takes
    effect (today's "Restart terminals", `restart_tab`), asking first when commands are running.
    Each terminal keeps its place in the list, its name and its current folder; a terminal with a
    startup `command` (§3) runs it again.
- Ctrl+Tab / Ctrl+Shift+Tab move between environment tabs (today's tab keys).
- Dragging to reorder tabs: builder's call; not required.

### 2.2 Terminal list (left)

- The selected environment's terminals, in the order they were created. Each row: the activity
  dot, the terminal's name, and a close button on hover. The shown terminal is highlighted.
- **A terminal's name is the user's.** It is the name given with Rename (or `title` in the layout
  file), else the terminal's current folder name. Titles a program sets through escape codes
  (`TermEvent::Title`; Claude Code changes it constantly) are **not** used for the name and never
  saved. (The user: "let me change the name and that name is the name.") Whether the program's
  title appears anywhere else, such as a row tooltip, is the builder's call.
- Clicking a row shows that terminal in the middle and focuses it.
- **+ New** at the top (and Ctrl+T) adds a terminal to the selected environment and shows it.
  Its folder: the shown terminal's current folder (OSC 7), else the environment's home, else the
  user's home.
- Right-click a row: **Rename…** (sets the name, which is saved; an empty name goes back to the
  folder name), **Restart** (that terminal only, as Restart environment does for all; asks first
  when a command is running), **Close**.
- Ctrl+W closes the shown terminal (or the file pane when it has focus, as now). Closing a
  terminal shows the next one in the list (else the previous one).
- Closing a single terminal (hover ✕, Ctrl+W, the row's Close) **asks first when a command is
  running in it**, with the same running-commands dialog; today Ctrl+W doesn't. (Our call: one
  mis-click shouldn't kill a live Claude session.)
- An environment whose last terminal closes **keeps its tab**; the middle shows an empty state
  with a **New terminal** button. (Our call: the tab is the environment, which the user added on
  purpose.)
- Keys to move to the next / previous terminal in the list: builder's call (suggest
  Ctrl+PageDown / Ctrl+PageUp), rebindable under `[keys]` like the rest.
- The list's width drags: its right edge is a divider, at least 120 px and never so wide that the
  middle falls under its minimum beside the tree; the width is saved in the layout (§3). It can be
  hidden with a key (builder's call) but is shown by default.

### 2.3 Middle

- Exactly one terminal: the selected environment's shown terminal. Switching environments shows
  that environment's last-shown terminal.
- Terminals that aren't shown keep running and keep their scrollback; only the view switches.
- The folder-binding warning badge (§5.2 of the hydraui spec) stays per terminal: a terminal
  whose folder is bound to a different environment says so.

### 2.4 File pane

- Opening a file (tree click, Open file…, Search in files…, Show changes) splits the middle: the
  shown terminal on the left, the file pane on its right, next to the tree. Same file pane as
  today (§7.8 of the hydraui spec), draggable divider.
- **One file pane per environment** (as today there is one per tab). It stays beside whichever of
  that environment's terminals is shown. (Our call.)
- This is the only split left. The split tree may be kept internally to hold "terminal | file
  pane", or replaced by something simpler: builder's call.

### 2.5 File tree (right)

- Moves from the left edge to the right edge. Otherwise as §7.3 of the hydraui spec: Ctrl+B
  toggles it, it is rooted at the shown terminal's current folder and follows it as you switch
  terminals or environments, draggable width, same right-click menu.
- **Open in new tab** on a folder becomes **New terminal here**: a terminal in the selected
  environment, starting in that folder.

## 3. Layout files

A layout file (`*.hydraui`, TOML) holds the window: its environments in tab order, each one's
terminals, the file panes, the tree, and what is selected. Everything about backing a window,
saving on change, the start page, the recent list, one window per file and the command line
(§7.12 of the hydraui spec) stays as it is.

```toml
version = 2
active_env = "work"
tree = { open = true, width = 280.0 }
list = { width = 240.0 }         # the terminal list; omitted at its default width

[[env]]
name = "work"
active = 0                       # index into this env's terminals: the one shown
files = { files = ["E:/work/api/README.md"], active = 0 }   # the env's file pane, if open

[[env.terminal]]
title = "claude"                 # optional: set by Rename
cwd = "E:/work/api"              # its current folder when saved
shell = "pwsh"                   # optional
command = "claude"               # optional: run when the layout opens (hand-written only)

[[env.terminal]]
cwd = "E:/work/api"

[[env]]
name = "personal"

[[env.terminal]]
cwd = "~/blog"
```

- `version = 2` marks the new format. A layout without it (today's format, sets and Loose tabs)
  isn't opened: a toast says it was made by an older hydraui and the window stays on the start
  page. Its recent-list entry stays until chosen, then is removed (as for a missing file). No
  conversion. The one-time `session.toml` / `*.hydraui.toml` conversion of §7.12 goes too.
- `command` is never written by hydraui for terminals it started itself; a hand-written one is
  kept when the layout is saved again, and runs again whenever the layout opens. (Our call: the
  user can still have "claude starts here" without sets.)
- `cwd` accepts `~`; relative paths resolve against the user's home.
- An environment that no longer exists, or a terminal whose folder is gone, is skipped with a
  toast but kept in what is written (as §7.12 does for sets that didn't open). If nothing opens,
  today's "nothing in this layout opened" behaviour applies.
- An `env` entry with no terminals opens as a tab with the empty state (§2.2).
- Saved on: environments added, closed or reordered, terminals added, closed or renamed, the
  shown terminal or selected environment changing, file pane files changing, the tree toggled
  or resized, the terminal list resized, and window close. Not saved: scrollback, running
  commands, unsaved edits.
- **New window** (start page) opens untitled with one environment tab and one terminal in the
  home folder; its environment is chosen as Ctrl+T's is today (folder binding → `default_env` →
  a picker).

## 4. What goes away

- **Sets**: the sets folder, `load_set` and set validation, the "Loose" group, group headers with
  their collapse / Reload / Unload menu, the "changed — reload?" watch, and the launcher's set
  entries. The `~/.hydra/hydraui/sets/` folder is left on disk untouched.
- **Group files**: Open group file…, Save group…, Save tab as group…, Save Loose tabs as group…
- **Splits between terminals**: Split right / down (Ctrl+Shift+D / Ctrl+Shift+E), Move focus
  (Alt+Arrows), and saving splits and ratios. A `[keys]` entry naming a removed action is
  ignored, not an error.
- **Per-tab environments**: a tab no longer has its own environment or title; the environment is
  the tab.

What happens to the `hydraui-sets` crate (trim it to the layout file and environment resolution,
rename it, or fold it into `hydraui`) is the builder's call. Spec sections §5 and §7.9 of the
hydraui spec are superseded by this document; §7.1's window drawing, §7.3's tree position,
§7.6's tab and split keys and §7.12's old-file conversion are amended as above.

## 5. Elsewhere in the app

- **Launcher.** "Environments" entries add that environment's tab (or select it if it's already
  there) instead of opening an ad-hoc tab. **Sign in** runs in a new terminal in that
  environment, adding its tab if needed. **Delete environment…** still refuses while the
  environment has open terminals.
- **Accounts popover, Add account…**: its sign-in opens a new terminal in the environment rather
  than a new tab.
- **Status bar**: unchanged; it describes the shown terminal.
- **Title bar**: unchanged.

## 6. Testing

As the existing plans: unit tests for the layout file (parse, write, round trip, version 1
refused, skipped environments kept on write) and for the model (adding and closing environments
and terminals, which terminal is shown after a close, the empty state). The UI is checked by
running the app.

## 7. Calls made without the user

Marked "our call" above: an environment's tab stays when its last terminal closes; one file pane
per environment; hand-written `command` kept and run on open; closing one terminal asks when a
command is running. Also: activity dots on the environment tabs; terminal list in creation order;
new terminal's folder rule; restarting reruns a terminal's startup `command`.
