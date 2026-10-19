# hydraui: task board

Adds a Kanban board of ideas, one per project, opened as a tab beside a terminal. Builds on the
[environment tabs design](2026-10-19-hydraui-environment-tabs-design.md); everything not named
here stays as it is.

## 1. Why

While Claude is busy in a terminal, the user has ideas for what to do next. They want to jot each
one down without interrupting Claude, then pick it up with Claude later. In time a local MCP
server will let Claude read the board ("what's next?") and add to it; that server is not part of
this work, but the file it will read is.

### Goals

- A board per project, columns fixed: **Backlog**, **Pickup**, **InProgress**, **Done**.
- A card is a short paragraph describing an idea, explored later with Claude. No title.
- Stored in one markdown file in the project, readable and editable by hand (and by Claude).
- Opened from a terminal row's right-click menu as a **Tasks** tab beside that terminal.
- The user chooses whether the file is committed or git-ignored.

### Non-goals

- The MCP server. Only the file format is designed with it in mind.
- Card titles, labels, assignees, due dates, user-defined columns.
- A board that isn't tied to a project folder.

### Anti-goals

- Losing an idea: an edit made in hydraui must never silently overwrite a change Claude (or an
  editor) made to the file, and vice versa.
- A format only hydraui can read. The file must make sense opened in any editor.

## 2. The project and the file

- A terminal's **project** is the git work tree containing its current folder (OSC 7, else its
  start folder), i.e. the nearest folder at or above it holding `.git` (a directory or, for a
  linked worktree or submodule, a file), so a linked worktree has its own board; outside a
  repository, the folder itself. hydraui never writes outside the project folder. Terminals in the same project share one
  board.
- The board lives in `<project>/.hydra/tasks.md`. The file and the `.hydra` folder are created on
  the first change to the board (adding a card), not on opening it.
- Format (column headings in this order; one card per top-level list item; a card's paragraph
  may wrap over several lines, continuation lines indented two spaces):

```markdown
# Tasks

## Backlog
- Let the terminal list show which terminals have unread output, maybe a dot
  that clears when you look at it.

## Pickup
- Ctrl+K should open the launcher from anywhere, including the file pane.

## InProgress

## Done
- Collapse button for the terminal list.
```

- Order within a column is the order in the file, top first.
- Reading is forgiving, because Claude and people edit this file by hand:
  - A **card** is a top-level list item (`- `, `* ` or `+ `, optionally with a `[ ]` / `[x]`
    checkbox, which is dropped) under one of the four column headings, plus its indented
    continuation lines and any nested list items, all joined into one paragraph. Written back as
    `- ` with two-space continuation.
  - Anything else (text before the first column, unknown headings and everything under them,
    non-list text under a column) is kept verbatim and written back after the four columns, in
    its original order. A list item under an unknown heading is not a card and is kept there.
  - The file is never refused; never drop a card. Heading match ignores case and
  spaces, so `## In Progress` is InProgress. A missing column is an empty one.
- Writing always produces the canonical form above for the four columns.
- A card's text is plain text with soft wraps; hydraui doesn't render markdown in cards (our
  call). Blank lines inside a card aren't supported: a card is one paragraph.

## 3. Git: commit or keep private

- The first change is the first card saved with text (an added card left empty is never
  written). The file is written first, then the question below is asked.
- The first time a board is changed in a project where `.hydra/tasks.md` doesn't exist yet and
  the project is a git repository, hydraui asks once: **Commit tasks** (do nothing) or **Keep
  private** (add `.hydra/tasks.md` to the project's `.gitignore`, creating it if needed).
- The board's menu (a `⋯` button in the board's header) offers the other choice later: **Keep
  private** adds the line; **Commit tasks** removes the line hydraui added. If the file is
  ignored by some other rule, the menu says so rather than fighting it (builder's call on
  wording).
- Outside a repository, no question is asked.

## 4. The window

```
┌────────────┬─────────────────────────────────────┬────────────┐
│ + New      │ [claude] [Tasks ✕]                  │            │
│ ● claude   ├─────────┬────────┬───────────┬──────┤ file tree  │
│   api      │ Backlog │ Pickup │InProgress │ Done │            │
│            │ +       │        │           │Clear │            │
│            │ ┌─────┐ │ ┌────┐ │           │┌───┐ │            │
│            │ │idea │ │ │... │ │           ││...│ │            │
│            │ └─────┘ │ └────┘ │           │└───┘ │            │
└────────────┴─────────┴────────┴───────────┴──────┴────────────┘
```

- A terminal row's right-click menu gains **Tasks**. It gives *that terminal* a Tasks tab and
  selects it. The Tasks tab belongs to the terminal (not the environment): switching to another
  terminal shows that terminal's own view, with or without its own Tasks tab.
- A terminal with a Tasks tab shows a small tab strip at the top of the middle: the terminal
  (labelled with its name) and **Tasks ✕**. A terminal without one shows no strip (as today).
- Clicking a tab selects it. ✕ closes the Tasks tab (the file stays). Choosing **Tasks** again
  while it's open just selects it. A key for switching between the two tabs: builder's call.
- The board belongs to the terminal's project as of when the tab is selected; if the terminal
  has moved to another project since, the board follows it (an edit in progress is saved to the
  board it was started on first).
- All Tasks tabs on the same project share one board in memory and one watcher; the watcher
  runs while any Tasks tab on that project is open in the window.
- The file pane is unchanged: if open, it stays on the right; the board replaces only the
  terminal's view.
- The layout file saves, per terminal, whether it has a Tasks tab and whether it was selected
  (`tasks = "open"` / `"shown"` on `[[env.terminal]]`, omitted when none; exact shape builder's
  call). Version stays 2. Layout files refuse unknown keys, so a hydraui build from before this
  feature won't open a layout that has a Tasks tab; accepted, as version 2 is days old. A terminal
  skipped on open keeps its `tasks` key on write, as skipped entries do.

## 5. The board

- Four columns, equal width, each with its heading and card count. Columns scroll on their own.
- **Backlog** has a **+** that adds an empty card at its top, already being edited.
- **Done** has a **Clear** button that removes all its cards (asks first, showing how many).
- A card shows its whole paragraph, wrapped. Clicking it edits it in place (multi-line input);
  Enter saves (a card is one paragraph, so no line breaks), Esc cancels, clicking away saves.
  Saving an empty card deletes it.
- Drag shows where the card will land (an insertion line) and scrolls a column when dragged
  near its edge; exact look is the builder's call.
- Moving cards: drag a card within or between columns. Its right-click menu also offers **Move
  to** each other column (to the top of that column), **Move up** / **Move down**, and
  **Delete**.
- Every change is written to the file at once (no Save button).

## 6. Keeping in step with the file

- The board watches `.hydra/tasks.md` (the `notify` crate, as `git_watch` does) and reloads when
  it changes on disk, so cards Claude adds by editing the file appear. A change whose contents
  equal what hydraui last wrote or read is ignored (so hydraui's own writes don't echo).
- Before writing, hydraui re-reads the file; if it changed since the board last read it, the
  board's change is applied to the fresh contents rather than overwriting. A card is identified
  by its column, its text and which occurrence of that text it is in the column (the 2nd
  identical card is the 2nd match). An add is inserted where it was made (top of Backlog);
  a move, edit or delete finds its card by that identity in the fresh file. If the card being edited or moved no longer
  exists, the change is dropped with a toast. (Our call: simple, and no idea is lost.)
- A card being edited while the file changes keeps its edit; the rest of the board reloads.
- Writes are atomic (temp file then rename, as layout files are written).

## 7. Testing

Unit tests for the file: parse (canonical, forgiving variants, missing columns, wrapped cards),
write, round trip, and merging a board change onto a file changed underneath. Unit tests for the
project lookup (in a repo subfolder, outside a repo) and the `.gitignore` edit (add once, remove
only hydraui's line). The UI is checked by running the app.

## 8. Calls made without the user

Marked "our call" above, plus: the project is the git work tree; the file is created on the
first change; the commit/private question is asked once at that moment; Clear asks first;
cards are plain text; Enter saves a card; the layout remembers each terminal's Tasks tab.
