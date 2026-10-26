# hydraui: Pickup cards go to a free Claude session

Status: design, 2026-10-20. Builds on the task board
(`2026-10-19-hydraui-task-board-design.md`) and the MCP server and session
hooks (`2026-10-20-hydraui-mcp-server-design.md`).

## 1. Why

Moving a card into Pickup approves it, but nothing happens until the user goes
to a Claude terminal and types "check Pickup". The user wants an open Claude
session in that project that has finished its work to take the card on its
own.

The user wants a reliable decision on whether a session is ready for new
work, not guesses from wording. Claude's hooks can't make it: a turn that ends
with a plain-text question ("Should I go ahead?") fires `Stop`, the same idle
as finished work. So:

- hydraui always checks facts it can be sure of, including whether the user
  has left a draft in the input (§5.1);
- sessions that are clearly finished are free, with or without Jev: a new
  session with no prompt yet, or one whose last turn moved its card to Verify
  (§5.2);
- for the other idle sessions there remains uncertainty, and only **Jev**
  (TypeSafe's decision model: yes/no answers with probabilities, fast and
  cheap), when hydraui has a key for it, can make the final call on whether
  Claude is waiting for the user or finished (§5.3). Without Jev they don't
  count. Nothing is guessed from wording.

Claude Code's Channels feature (an MCP server pushing events into a session)
was considered and set aside: it is a research preview, a custom channel needs
`claude --dangerously-load-development-channels` with a confirmation prompt at
every launch, and it has to be a separate stdio process because hydraui's MCP
server is HTTP. Instead, hydraui types a short prompt into the chosen
terminal, as the user would.

## 2. Goals, non-goals, anti-goals

Goals
- hydraui can hold a Jev API key (§3), one for the whole app, kept out of
  terminals and out of plain-text files.
- A card the user moves into Pickup is typed into one free Claude session of
  that project, which takes it with `tasks_take` and works as usual. When no
  session is free, the card stays in Pickup and a toast says so.
- A card's right-click menu can send it by hand (§8), with or without Jev.

Non-goals
- Channels. Negotiation between sessions over who takes a card (random choice
  for now). Starting a new Claude session when none is free. Retrying or
  queueing a card until a session frees up. Terminals in other hydraui windows.
- Telling, without Jev, a session that finished other work (not a card) from
  one that asked a question: it doesn't count as free until Jev is set up.

Anti-goals (a failure even if it works)
- Typing into a session that is asking the user something, or where the user
  has a half-written prompt or is typing right now.
- Sending one card to two sessions, or two cards to one session at once.
- The Jev key reaching a terminal's environment (and so Claude or scripts).

## 3. Part 1: hydraui's Jev key

Built first, as its own commit, so the user can add their key while the rest
is built. One key for hydraui as a whole, not per environment (the user's
call).

- **Stored** in the Windows credential store as hydraui's own entry (as
  hydra's secrets are, via `hydra_platform`'s keyring), not in `config.toml`,
  so it is never in a plain-text file. The entry's name is the builder's call
  (e.g. target `hydraui/jev`); it must not collide with hydra's `<env>/<key>`
  secrets.
- **Launcher entries**: **Set Jev API key…** (a dialog with a masked field;
  an empty value is refused) and **Remove Jev API key** (shown only when one is
  set, asks first). A toast confirms each.
- **Reading it**: a helper returning the key or `None`, read when Jev is
  needed (so setting or removing applies at once, without a restart).
- Never put into a terminal's environment or a log.
- Docs: the website's launcher/settings section.

## 4. When a card is sent automatically

A card *arrives* in Pickup only when the user moves it there on this window's
board (drag, Move to Pickup, from another column). Only the user can put cards
in Pickup (MCP tools can't), and they do it on the board.

Changes that reach the file from elsewhere do not fire (my call): a hand edit,
a `git pull`, or another hydraui window. Each window is its own process
watching the same file and can't tell another window's move from a pull, so
reacting to reloads would send one card from two windows. Cards that arrive
that way, and cards already in Pickup when hydraui starts, are sent by hand
(§8).

Since a move happens on an open Tasks tab, the board and its project are
loaded when a card arrives.

On arrival hydraui looks for a free session (§5).

### 4.1 Cards from the phone

Amended by the phone link (docs/superpowers/specs/2026-10-23-hydra-mobile-design.md §4): a
card the phone puts into Pickup (added there, or moved there from another column) arrives
too. It is offered by the window holding the phone link, and only that one, whether or not
a Tasks tab there has the board loaded. The user is away from the desktop, so no session
counts as focused (§5.1, 4); every other rule stands. The phone is told where the card went
("Sent to <terminal> in <env>"), or that no free session took it and it stays in Pickup.

## 5. Which sessions are free

### 5.1 Facts, always checked

**Candidates**, checked first and cheaply:
1. In this hydraui window, running Claude (it has reported through the status
   hooks).
2. In the board's project's main work tree: `tasks_project::git_root(cwd)` is
   the board's project (or, outside git, its folder is the project).
   Terminals in a linked worktree are not candidates (my call): a worktree is
   made for one card, and new work typed there would land on that card's
   branch.
3. Last hook state **idle** (from `SessionStart` or `Stop`; a resumed start,
   reported as `resumed`, is idle too). Working, waiting
   (a permission or question dialog) and ended are not candidates; nor is a
   terminal whose Claude hasn't reported yet.
4. Not the focused terminal while the hydraui window is active (my call: the
   user may be typing there right now; it costs nothing to skip).
5. Not reserved by a card sent in the last moments (§6).
6. **No draft** in its input. hydraui keeps a per-terminal flag from the input
   the user sends it:
   - typing sets it: printable keys, text from the input handler (IME/AltGr),
     paste, Backspace/Delete and other editing keys. Mouse reports, focus
     events do not. A prompt hydraui itself typed (§6) counts as a draft until
     the session starts working on it (idle→working) or ends, so a prompt that
     didn't submit keeps the session from being sent another card;
   - **Enter** (unmodified) clears it;
   - a change of hook state from *idle* to *working* clears it: while idle,
     only `UserPromptSubmit` reports *working*, so the input was just
     submitted (my call). hydraui sees only state names, and *working* also
     comes from `PostToolUse` and *idle* from `Stop`, which must not clear
     type-ahead, so no other state change clears it and the hook protocol
     stays as it is;
   - **Esc Esc** (two in a row) and **Ctrl+C** may have cleared Claude's input
     but don't always (a single Esc never does; in menus they mean other
     things). After one, hydraui reads Claude's input line from the grid and
     clears the flag only if it is empty; if it can't tell, the flag stays;
   - typing while Claude works sets it too: Claude Code keeps type-ahead.

A candidate that passes all six is then either clearly finished (§5.2) or
uncertain (§5.3).

### 5.2 Clearly finished: free, with or without Jev

A candidate is clearly finished, and so free, when it is:
- **fresh**: its Claude session started new (`SessionStart` with source
  `startup` or `clear`) and has had no prompt since (no idle→working change).
  A resumed conversation (source `resume`: `claude -c`, `claude -r`,
  `/resume`, and the session a layout restore starts) is not fresh: it may end
  in a question. hydra's SessionStart status hooks are split by matcher:
  `startup|clear` reports `idle`, `resume` reports `resumed` (idle in every
  other way). `compact` reports nothing; or
- **just verified**: during its last turn it called `tasks_verify` (the MCP
  server knows which terminal's token made the call), and no prompt has
  started since (an idle→working change resets this).

Jev is not asked about these: there is no uncertainty left.

### 5.3 Uncertain: Jev makes the final call, if hydraui has a key

Any other candidate (idle after a turn that didn't verify a card, which may
have finished other work or may be asking the user something) is free only if
Jev says so. Without a Jev key (§3) it is not free. A session that ended its
turn by asking something hasn't verified a card, so without Jev it is always
skipped; nothing is guessed from wording.

One call per uncertain candidate, asking one **choice** question about the
terminal's screen: what state is the assistant in? The options are `ready`
(finished, asks nothing of the user, though it may mention optional next steps),
`asking` (needs the user's answer, approval or choice to continue), `working`,
`draft` (unsent text in the input box: a second check on §5.1.6, from the
screen) and `none` (none of these, or it can't be told). `none` is always
offered (the user's rule): without it Jev picks one of the others even when it
can't tell.

(First built as three yes/no questions with thresholds. Tried on a real screen,
"finished" 0.86 lost to "waiting" 0.27: independent probabilities don't compete,
so the options now do.)

The **state** sent is the text of the terminal's last ~60 rows (screen plus
the end of scrollback), as hydraui's grid holds it: that shows the end of
Claude's last message and its input box (my call; no change to the hooks is
needed. The builder may add Claude's last message from the transcript if
testing shows the screen isn't enough). Faint text in the input box is left
out: Claude Code draws its placeholder dim and the prompt it suggests (such
as `push`) in grey, and sent as plain text either reads as a typed draft.
Text that is dim or grey (bright black, or an RGB grey from 0x40 to 0xBF)
counts as faint; what you type is in the normal colour.

A candidate is **free** when Jev picks `ready` with confidence ≥ 0.7. A Jev
error, timeout (~5 s), a pick that wasn't offered or any other unexpected reply
means not free.

The question's wording, its options (which must include `ready`; `none` is
added when left out), the model and the confidence can be changed in
`<HYDRA_HOME>/hydraui/jev.toml` (launcher: Edit Jev questions), read on every
ask. `hydraui.log` records the pick, its confidence, each option's probability,
Jev's reply and the screen sent.

Jev API (docs.typesafe.ai/api): `POST https://api.typesafe.ai/v1/systemone`,
`Authorization: Bearer <key>`, body
`{"model": "jev-latest", "state": "...", "questions": {"state": {"type": "choice", "instructions": "...", "criteria": {"ready": "...", ...}}}}`,
reply `{"answers": {"state": {"type": "choice", "choice": "ready", "confidence": 0.8, "probabilities": {...}}}, ...}`. Jev is in
early access; the model alias is a constant.

### 5.4 Picking

If any candidate is clearly finished (§5.2), one of those is picked at random
and Jev is not called (my call: saves calls and time). Otherwise, with a Jev
key, the uncertain candidates are asked in parallel, off the UI thread, and one
of those Jev finds free is picked at random.

## 6. Sending

hydraui types into the chosen terminal a one-line prompt naming the card, then
Enter, e.g.:

```
A card was moved to Pickup: "<card text, first ~120 chars>". Take it with tasks_take and do it.
```

The exact wording is the builder's call. It must give Claude the card's text,
or enough to find it on `tasks_read`, and lean on the board instructions it
already has (take one card, work, commit, `tasks_verify`). Line breaks in a
card become spaces.

The text and the Enter must arrive as typed input, not as a paste with a
newline in it (Claude Code may treat a burst containing a newline as a
multi-line paste and not submit). The builder decides how (bracketed paste of
the text, then a separate `\r` after a short delay, or similar) and verifies it
in a real Claude session.

The card must still be in Pickup when the send happens (the Jev calls take a
moment); if it has left Pickup, nothing is sent.

After sending, the terminal is **reserved**: not a candidate until its next
hook state arrives (`UserPromptSubmit` makes it working anyway), or ~30 s pass
(my call).

A successful send shows a brief info toast, "Sent to <terminal> in <env>",
which shows that terminal when clicked (my call, like the Claude alert
toasts).

## 7. When no one is free

The card stays in Pickup. A warning toast (auto-hides) says "No free Claude
session for this card", naming the project when the window has more than one.
When Jev was asked and every call failed, the toast says why, e.g. "No free
Claude session for this card (Jev: HTTP 401)"; the reason never contains the
key.
There is no retry: the user sends it by hand (§8) or tells a session
themselves.

## 8. Sending by hand

Pickup cards' right-click menu, next to Take in new worktree:

- **Send to a free session**: picks and sends as §5 and §6, or shows §7's
  toast. On a card already being offered or sent in the last 30 s, an info
  toast says "This card is already on its way".
- **Send to ▸**: a submenu of the project's idle Claude sessions in the main
  work tree (§5.1 items 1–3), by name and environment. The user picks; no
  draft, Jev or finished check (my call: the user is deciding). Disabled
  (greyed) when there are none.

Both send as §6, reservation and toast included.

## 9. Left to the builder

- How the board tells the workspace a card arrived (a new `TaskBoardEvent`,
  e.g. `ArrivedInPickup(text)`, emitted by `change` for a move from another
  column into Pickup).
- The HTTP client for Jev (the crate already makes HTTPS calls to GitHub for
  updates) and the request/response types.
- Reading the terminal's rows as text for the state.
- Where the draft flag and the fresh/just-verified state live (likely
  `PaneView`, beside its Claude state) and how injected text skips the draft
  flag.
- Tests: the Jev key in the credential store, draft flag transitions, fresh/just-verified transitions, the
  candidate filter, the free decision from Jev answers (thresholds, errors),
  the prompt text, arrival only on moves into Pickup.
  A hands-on check in a real Claude session that the prompt submits and Claude
  takes the card, and that Jev's answers look right on a few real screens (a
  finished session, one asking a question, one with a draft).
- Docs: the task board spec (§5 menu), the website's task board section, and
  Jev setup.
