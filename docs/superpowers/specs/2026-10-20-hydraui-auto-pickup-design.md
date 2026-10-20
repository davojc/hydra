# hydraui: Pickup cards go to a free Claude session

Status: design, 2026-10-20. Builds on the task board
(`2026-10-19-hydraui-task-board-design.md`) and the MCP server and session
hooks (`2026-10-20-hydraui-mcp-server-design.md`).

## 1. Why

Moving a card into Pickup approves it, but nothing happens until the user goes
to a Claude terminal and types "check Pickup". The user wants an open Claude
session in that project that has finished its work to take the card on its
own.

Whether a session is ready for new work is decided by a model, not guessed
from keystrokes or wording. The user has an API key for **Jev**, TypeSafe's
decision model (yes/no answers with probabilities, fast and cheap). Jev makes
the call; without Jev nothing is sent automatically. The user would rather
have a reliable decision model than heuristics about what may or may not be
typed.

Claude Code's Channels feature (an MCP server pushing events into a session)
was considered and set aside: it is a research preview, a custom channel needs
`claude --dangerously-load-development-channels` with a confirmation prompt at
every launch, and it has to be a separate stdio process because hydraui's MCP
server is HTTP. Instead, hydraui types a short prompt into the chosen
terminal, as the user would.

## 2. Goals, non-goals, anti-goals

Goals
- An environment can hold a Jev API key (§3), kept out of its terminals.
- With Jev, a card the user moves into Pickup is typed into one free Claude
  session of that project, which takes it with `tasks_take` and works as
  usual. When no session is free, the card stays in Pickup and a toast says so.
- A card's right-click menu can send it by hand (§8), with or without Jev.

Non-goals
- Channels. Negotiation between sessions over who takes a card (random choice
  for now). Starting a new Claude session when none is free. Retrying or
  queueing a card until a session frees up. Terminals in other hydraui windows.
- Any automatic sending without Jev.

Anti-goals (a failure even if it works)
- Typing into a session that is asking the user something, or where the user
  has a half-written prompt or is typing right now.
- Sending one card to two sessions, or two cards to one session at once.
- The Jev key reaching a terminal's environment (and so Claude or scripts).

## 3. Part 1: a Jev key per environment

Built first, as its own commit, so the user can add their key while the rest
is built. It follows `[gemini] api_key` through each layer.

- **env.toml** (`hydra-core/src/config.rs`): a `[jev]` section with
  `api_key`, which must be a secret reference (`api_key = "secret:work/jev"`);
  a plain value is an error naming `[jev] api_key`, as for gemini.
- **Adding it** (`hydra-core/src/envedit.rs`, `hydra-cli`): `jev` joins the
  tools `hydra env add-tool` knows, with gemini's `--api-key-secret` flag:
  `hydra env add-tool work jev --api-key-secret work/jev`.
- **A tool with no variables** (new `hydra-providers/src/jev.rs`): unlike
  gemini, it contributes **no** environment variable, so the key never reaches
  a terminal. Its check reports whether the secret is stored (`jev ✓`, or
  missing), so it shows in the environment's accounts/status view.
- **hydraui setup** (`hydraui/src/accounts.rs`): `jev` in the add-tool choices
  beside gemini, one field for the key; it stores the secret as `<env>/jev`
  and writes the `[jev]` section.
- **hydraui reads it** (`hydraui/src/hydra.rs`): a helper returning an
  environment's Jev key from the secret store, or `None` (no section, or the
  secret missing).
- Docs: the website's tools section and hydra's env.toml reference.

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

On arrival hydraui looks for a free session (§5) among the candidates whose
environment has a Jev key. If none of the candidates' environments has one,
nothing happens: no toast (my call; the user hasn't turned the feature on for
these sessions).

## 5. Which sessions are free

**Candidates** (facts, checked first and cheaply):
1. In this hydraui window, running Claude (it has reported through the status
   hooks).
2. In the board's project's main work tree: `tasks_project::git_root(cwd)` is
   the board's project (or, outside git, its folder is the project).
   Terminals in a linked worktree are not candidates (my call): a worktree is
   made for one card, and new work typed there would land on that card's
   branch.
3. Last hook state **idle** (from `SessionStart` or `Stop`). Working, waiting
   (a permission or question dialog) and ended are not candidates; nor is a
   terminal whose Claude hasn't reported yet.
4. Not the focused terminal while the hydraui window is active (my call: the
   user may be typing there right now; it costs nothing to skip).
5. Not reserved by a card sent in the last moments (§6).

**Jev decides** for each candidate whose environment has a Jev key. One call
per candidate, with that environment's key, asking three yes/no ("noul")
questions about the terminal's screen:

- `waiting`: Is the assistant waiting for the user to answer a question, make a
  decision or give more information?
- `finished`: Has the assistant finished its work and is it ready for a new,
  unrelated task?
- `draft`: Is there unsent text typed in the input box at the bottom?

The **state** sent is the text of the terminal's last ~60 rows (screen plus
the end of scrollback), as hydraui's grid holds it: that shows the end of
Claude's last message and its input box (my call; no change to the hooks is
needed. The builder may add Claude's last message from the transcript if
testing shows the screen isn't enough).

A candidate is **free** when `finished ≥ 0.8`, `waiting ≤ 0.2` and
`draft ≤ 0.2` (my call on the thresholds; constants, tuned during the hands-on
check). A Jev error, timeout (~5 s) or unexpected reply means not free.

Candidates are asked in parallel, off the UI thread. Among the free ones, one
is picked at random.

Jev API (docs.typesafe.ai/api): `POST https://api.typesafe.ai/v1/systemone`,
`Authorization: Bearer <key>`, body
`{"model": "jev-latest", "state": "...", "questions": {"<id>": {"type": "noul", "instructions": "..."}}}`,
reply `{"answers": {"<id>": {"type": "noul", "noul": 0.93}}, ...}`. Jev is in
early access; the model alias is a constant.

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
There is no retry: the user sends it by hand (§8) or tells a session
themselves.

## 8. Sending by hand

Pickup cards' right-click menu, next to Take in new worktree:

- **Send to a free session**: shown when some candidate's environment has a
  Jev key. Picks and sends as §5 and §6, or shows §7's toast.
- **Send to ▸**: a submenu of the project's candidate terminals (§5.1–5.3,
  idle Claude sessions in the main work tree), by name and environment. The
  user picks; no Jev call. Works with or without Jev (my call). Disabled
  (greyed) when there are none.

Both send as §6, reservation and toast included.

## 9. Left to the builder

- How the board tells the workspace a card arrived (a new `TaskBoardEvent`,
  e.g. `ArrivedInPickup(text)`, emitted by `change` for a move from another
  column into Pickup).
- The HTTP client for Jev (the crate already makes HTTPS calls to GitHub for
  updates) and the request/response types.
- Reading the terminal's rows as text for the state.
- Tests: the `[jev]` config and add-tool, the jev tool contributing no
  variables, the candidate filter, the free decision from Jev answers
  (thresholds, errors), the prompt text, arrival only on moves into Pickup.
  A hands-on check in a real Claude session that the prompt submits and Claude
  takes the card, and that Jev's answers look right on a few real screens (a
  finished session, one asking a question, one with a draft).
- Docs: the task board spec (§5 menu), the website's task board section, and
  Jev setup.
