# hydraui: Pickup cards go to a free Claude session

Status: design, 2026-10-20. Builds on the task board
(`2026-10-19-hydraui-task-board-design.md`) and the MCP server and session
hooks (`2026-10-20-hydraui-mcp-server-design.md`).

## 1. Why

Moving a card into Pickup approves it, but nothing happens until the user goes
to a Claude terminal and types "check Pickup". The user wants an open, idle
Claude session in that project to take the card on its own.

Claude Code's Channels feature (an MCP server pushing events into a session)
was considered and set aside for now. It is a research preview, a custom
channel needs `claude --dangerously-load-development-channels` with a
confirmation prompt at every launch, and it has to be a separate stdio process
because hydraui's MCP server is HTTP. Instead, hydraui types a short prompt into
the chosen terminal, as the user would.

## 2. Goals, non-goals, anti-goals

Goals
- A card arriving in Pickup is typed into one free Claude session of that
  project, which then takes it with `tasks_take` and works as usual.
- When no session is free, the card stays in Pickup and a toast says so.
- A right-click **Send to a free session** on any Pickup card does the same
  by hand (for a card that found no one free, or one already sitting there).

Non-goals
- Channels. Negotiation between sessions over who takes a card (random choice
  for now). Starting a new Claude session when none is free. Queueing a card
  until a session frees up. Cards in other hydraui windows' terminals.

Anti-goals (a failure even if it works)
- Typing into a terminal where the user has a half-written prompt, or where
  they are typing right now. When in doubt, the terminal is not free.
- Sending one card to two sessions, or two cards to one session at once.

## 3. When it fires

A card *arrives* in Pickup only when the user moves it there on this window's
board (drag, Move to Pickup, from another column). Only the user can put cards
in Pickup (MCP tools can't), and they do it on the board, so this is the case
that matters.

Changes that reach the file from elsewhere do not fire (my call): a hand edit,
a `git pull`, or another hydraui window. Each window is its own process
watching the same file and can't tell another window's move from a pull, so
reacting to reloads would send one card from two windows. Cards that arrive
that way, and cards already in Pickup when hydraui starts, are sent with the
menu item (§8).

Since a move happens on an open Tasks tab, the board (and its project) is
always loaded when a card arrives; no board needs to be kept alive for this.

A config switch turns auto-sending off: `auto_pickup = false` in hydraui's
`config.toml`, default `true` (my call). The menu item works either way.

## 4. Which sessions are free

A terminal is a candidate when it is in this hydraui window, runs Claude
(it has reported through the status hooks), and its folder is in the board's
project's main work tree: `tasks_project::git_root(cwd)` is the board's project
(or, outside git, its folder is the project).

Terminals in a linked worktree are not candidates (my call). A worktree is made
for one card (Take in new worktree, or a session that went into one because
others were working), and a new card typed there would land its work on that
card's branch.

A candidate is **free** when all of these hold:
1. Its last hook state is **idle** (from `SessionStart` or `Stop`). Working,
   waiting (a question or permission) and ended are not free. A terminal whose
   Claude has not reported anything yet is not free.
2. It has **no draft** (§5).
3. It is not the terminal the user is typing in: not the focused terminal
   while the hydraui window is active (my call; simpler than a time window and
   covers "typing right now").
4. It has not been **sent a card it hasn't started yet** (§6).

Among the free ones, pick one at random.

## 5. Drafts

hydraui keeps a per-terminal `draft` flag from the input the user sends it.

- Typing sets it: printable keys, text from the input handler (IME/AltGr),
  paste, Backspace/Delete and other editing keys. Mouse reports, focus events
  and hydraui's own injected text do not.
- **Enter** (unmodified) clears it: the prompt was sent.
- A change of hook state from *idle* to *working* clears it: while idle, only
  `UserPromptSubmit` reports *working*, so Claude's input was just submitted
  (my call). hydraui only sees the state names, not the hook events, and
  *working* also comes from `PostToolUse` and *idle* from `Stop`, which must
  not clear type-ahead; so no other state change clears it, and the hook
  protocol stays as it is.
- **Esc Esc** (two Escapes in a row) and **Ctrl+C** may have cleared Claude's
  input but don't always (a single Esc never does; in menus and dialogs they
  mean other things). After one of these, hydraui reads Claude's input line
  from the terminal grid; only if it is empty is the flag cleared. If the
  screen can't be read with confidence (Claude's layout changed, the input box
  isn't found), the flag stays set.

Typing while Claude works sets the flag too: Claude Code keeps type-ahead in
its input box.

Getting this wrong in the safe direction (a terminal thought to have a draft
when it doesn't) only means the card goes elsewhere or gets the toast.

## 6. Sending

hydraui types into the chosen terminal a one-line prompt naming the card, then
Enter, e.g.:

```
A card was moved to Pickup: "<card text, first ~120 chars>". Take it with tasks_take and do it.
```

The exact wording is the builder's call. It must give Claude the card's exact
text, or enough to find it on `tasks_read`, and point at the board instructions
it already has (take one card, work, commit, `tasks_verify`). Line breaks in a
card become spaces.

The text and the Enter must arrive as typed input, not as a paste with a
newline in it (Claude Code may treat a burst containing a newline as a
multi-line paste and not submit). The builder decides how (bracketed paste of
the text then a separate `\r` after a short delay, or a plain write then a
delayed `\r`) and verifies it in a real Claude session.

After sending, the terminal is **reserved**: not free (§4.4) until its next
hook state arrives (`UserPromptSubmit` makes it working anyway). If no hook
arrives within ~30 s (my call), the reservation lapses.

A successful send shows a brief info toast, "Sent to <terminal> in <env>",
which shows that terminal when clicked (my call, matching the Claude alert
toasts).

## 7. When no one is free

The card stays in Pickup. A toast (warning, auto-hides) says "No free Claude
session for this card", naming the project when the window has more than one.
There is no retry: the user sends it with **Send to a free session**, or tells
a session themselves.

The same toast answers the menu item when no one is free.

## 8. The menu item

Pickup cards' right-click menu gets **Send to a free session**, next to Take
in new worktree. Shown on every Pickup card; picks and sends as §4 and §6.

## 9. Left to the builder

- Where the draft flag lives (likely `PaneView`, set where key, text and paste
  input reach `send`) and how injected text skips it.
- How the board tells the workspace a card arrived (a new `TaskBoardEvent`,
  e.g. `ArrivedInPickup(text)`, from `change` and from `reload`).
- Reading Claude's input line from the grid (§5).
- Tests: draft flag transitions, the free-session filter, Pickup diff on
  reload, the prompt text. A hands-on check in a real Claude session that the
  prompt submits and Claude takes the card.
- Docs: the task board spec (§5 menu), the website's task board section, and
  `config.toml`'s `auto_pickup`.
