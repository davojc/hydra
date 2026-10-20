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

- A board per project, columns fixed: **Backlog**, **Pickup**, **InProgress**, **Verify**,
  **Done**. Verify holds committed work waiting for the user to check it.
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
  linked worktree or submodule, a file); outside a repository, the folder itself. A linked
  worktree's project is its repository's main work tree (its `.git` file names
  `<repo>/.git/worktrees/<name>`), so every worktree of a repository shares one board (§9); a
  submodule keeps its own. hydraui never writes outside the project folder. Terminals in the same project share one
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

## Verify

## Done
- Collapse button for the terminal list.
```

- Order within a column is the order in the file, top first.
- Reading is forgiving, because Claude and people edit this file by hand:
  - A **card** is a top-level list item (`- `, `* ` or `+ `, optionally with a `[ ]` / `[x]`
    checkbox, which is dropped) under one of the five column headings, plus its indented
    continuation lines and any nested list items, all joined into one paragraph. Written back as
    `- ` with two-space continuation.
  - Anything else (text before the first column, unknown headings and everything under them,
    non-list text under a column) is kept and written back under a `## Notes` heading after the
    five columns, in its original order. A list item under an unknown heading is not a card and
    is kept there.
  - The file is never refused; never drop a card. Heading match ignores case and
  spaces, so `## In Progress` is InProgress. A missing column is an empty one (so a file from
  before Verify reads as it did, and gains `## Verify` the next time it's written).
- Writing always produces the canonical form above for the five columns.
- A card's text is plain text with soft wraps; hydraui doesn't render markdown in cards (our
  call). Blank lines inside a card aren't supported: a card is one paragraph.

## 3. Git: commit or keep private

- The first change is the first card saved with text (an added card left empty is never
  written). The file is written first, then the question below is asked.
- The first time a board is changed in a project where `.hydra/tasks.md` doesn't exist yet and
  the project is a git repository, hydraui asks once: **Commit tasks** (do nothing) or **Keep
  private** (add `.hydra/tasks.md` to the project's `.gitignore`, creating it if needed).
- The board's menu (a `⋯` button at the right of the Done column's header, shown only in a git repository) offers the other choice later: **Keep
  private** adds the line; **Commit tasks** removes the line hydraui added. If the file is
  ignored by some other rule, the menu says so rather than fighting it (builder's call on
  wording).
- Outside a repository, no question is asked; nor when `.gitignore` already ignores the file.

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

- Five columns, equal width, each with its heading and card count. Columns scroll on their own.
- Each column has a color, shown as a dot before its heading and a strip down its cards' left
  edge: Backlog grey, Pickup blue, InProgress orange, Verify yellow, Done green.
- **Backlog** has a **+** that adds an empty card at its top, already being edited.
- **Done** has a **Clear** button that removes all its cards (asks first, showing how many).
- A card shows its whole paragraph, wrapped. Clicking it edits it in place (multi-line input);
  Enter saves (a card is one paragraph, so no line breaks), Esc cancels, clicking away saves;
  switching to the terminal tab, another terminal or environment saves too. If the write fails the
  card stays open and its Tasks tab won't close until it succeeds.
  Saving an empty card deletes it.
- A Verify card a session moved shows its commit beside the taker badge; clicking the commit
  shows what it changed in the environment's file pane (MCP server spec §4).
- While the pointer is over a card it shows a delete icon; clicking it asks "Delete this card?"
  (showing the card's text) before deleting.
- Drag shows where the card will land (an insertion line) and scrolls a column when dragged
  near its edge; exact look is the builder's call.
- Moving cards: drag a card within or between columns. Its right-click menu also offers **Move
  to** each other column (to the top of that column), **Move up** / **Move down**, and
  **Delete** (which asks first, as the delete icon does).
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
  exists, the change is dropped with a toast, except an edit that changed the card's text: that
  text is added as a new card at the top of Backlog instead, with a toast saying so, so the idea
  typed isn't lost. (Our call: simple, and no idea is lost.)
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

## 9. A worktree per card

So several Claude sessions can work on one repository without touching each other's files, a
**Pickup** card's right-click menu offers **Take in new worktree** (only when the project is a git
repository):
- hydraui makes a branch `task/<slug>` from what the repository has checked out, in a worktree
  beside it: `<repo>-worktrees/<slug>`. The slug is up to five words of the card, lowercase
  letters and digits joined by `-`, at most 40 characters (`task` if none); `-2`, `-3`, ... when
  the folder or branch is taken.
- A terminal opens in the worktree, in the environment of the terminal whose Tasks tab was used,
  running `claude` with a prompt naming the card and branch and asking it to commit there and
  move the card to Verify with `tasks_verify`. The card is taken (Pickup → InProgress) for that
  terminal, so its session finds it with `tasks_mine`.
- Which card has which worktree (repository, card text, path, branch, base) is kept in
  `<hydra home>/hydraui/worktrees.toml`, never in the repository.
- When the user moves such a card to **Done** (drag or menu), hydraui asks "Merge
  `task/<slug>`?". **Merge** closes the terminals in the worktree, merges the branch into its base
  in the main work tree (`git merge --no-ff --no-edit`), removes the worktree and deletes the
  branch, and says what it did. It refuses, changing nothing, when the worktree or the main work
  tree has uncommitted changes or the main work tree isn't on the base branch; a merge with
  conflicts is aborted. **Not now** leaves everything as it is.

## 10. All boards

The launcher's **All boards** opens one read-only look across projects: every project a terminal
in the window is in (its project as §2 says), each with its **Pickup**, **InProgress** and
**Verify** cards (and their takers), read from the board files when the dialog opens. Projects with
none of those cards, and empty columns, are left out. Clicking a project's name shows a terminal in
it (the shown one when it is in that project) and opens its Tasks tab.
