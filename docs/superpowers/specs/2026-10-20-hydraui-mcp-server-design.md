# hydraui: MCP server (tasks)

Adds an MCP server hosted by each hydraui process, which every Claude session in that hydraui's
terminals finds by itself. Its first tools let a session work with its project's task board.
Builds on the [task board design](2026-10-19-hydraui-task-board-design.md); everything not named
here stays as it is.

## 1. Why

The user jots ideas on the task board and moves the ones they want done into **Pickup**. Today
they then have to explain the board to Claude and paste the card. Instead, a Claude session should
discover from the server itself what the board is and how to use it: read it, add ideas, take a
card from Pickup and work on it, without the user explaining anything.

Later, sessions in one hydraui should be able to talk to each other. That's why the server is one
shared localhost endpoint per hydraui process (not one per session): every session in the window
reaches the same server.

### Goals

- One MCP server per hydraui process, on localhost, shared by all Claude sessions in its
  terminals.
- A Claude session started in a hydraui terminal finds it with no setup by the user.
- The server's instructions and tool descriptions are enough for Claude to know the workflow.
- Tools for the session's own project board: read it, add to Backlog, take a card from Pickup,
  list the cards it has taken.
- A taken card records which terminal took it, in the file and on the board.

### Non-goals

- Waking an idle session when a card lands in Pickup. For now the user tells the session to check
  Pickup, and the tool descriptions tell Claude to check it after finishing a piece of work.
  hydraui does not type into terminals. (Revisit once there's a reliable way.)
- Sessions talking to each other (a later feature; this work only must not rule it out).
- Reaching another hydraui's server, or another project's board.

### Anti-goals

- A session making the user's decisions: putting a card into Pickup or Done, deleting or rewording
  cards, reordering. The server refuses these; it doesn't rely on Claude following descriptions.
- Losing a card or an edit: the server's writes follow the board's existing merge-on-write rules,
  so it never overwrites a change made in the Tasks tab or by hand, and vice versa.
- Another program on the machine using the server: it listens on loopback only and needs a token.

Accepted (the user's decision): Claude sessions outside hydraui show the server as failed (§3).

## 2. The server

- hydraui starts the server when it starts and stops it when it exits. It binds `127.0.0.1` on a
  port the OS picks (port 0), so several hydraui processes never clash.
- Transport: MCP's Streamable HTTP, at path `/mcp`. Plain JSON responses are enough (no
  server-sent events needed for these tools); the builder picks the crate or writes the small
  JSON-RPC handler by hand. hydraui has no tokio today; a server thread outside gpui's executor
  is fine (builder's call).
- Every request must carry `Authorization: Bearer <token>`, where the token is one hydraui gave a
  terminal (§3). Unknown or missing token: 401. Requests whose `Origin` header is present and isn't
  a localhost origin are refused (DNS rebinding guard, as the MCP spec advises).
- **The token identifies the terminal.** hydraui keeps a map token → terminal while the terminal
  lives; a closed terminal's token stops working. From the terminal the server knows, at the time
  of each call:
  - its **name**, used as the taker (§4): fixed when the terminal is spawned, so a `cd` doesn't
    change it mid-task. It's the terminal's title if the user set one, else the label the list
    shows for its start folder. If another live terminal in the window already has that name, a
    number is added (`hydra-2`, `hydra-3`), so names are unique within a window. When the user
    renames the terminal, its name becomes the new title. `]` and line breaks in a name are
    replaced (`]` → `)`), so it always fits `[name]`; a name `x` or `X`, which would read as a
    checkbox, becomes `x-term`. (Our call.)
  - its **project**: as the Tasks tab finds it (git work tree of the terminal's current folder,
    else the folder; `tasks_project::project_root`). A git repository root is a folder whose `.git`
    is a directory containing `HEAD`, or a file starting `gitdir:`; a `.git` folder of config files
    isn't a repository.
- The server's MCP `initialize` result includes `instructions` describing the workflow (§5), so
  Claude has it before it reads any tool.

## 3. How a session finds it

- Every terminal hydraui spawns gets two environment variables:
  - `HYDRA_MCP_URL` = `http://127.0.0.1:<port>/mcp`
  - `HYDRA_MCP_TOKEN` = a random token for that terminal (≥128 bits). It is per terminal, not
    per spawn: kept when the terminal's shell is restarted, and dropped when the terminal is
    closed. Tokens live only in hydraui's memory and the port is new each run, so a terminal in
    a restarted hydraui gets a new address and token.
- A `claude` run in the terminal inherits them, including through `hydra claude` / `hydra shell`:
  check that hydra's environment handling (`hydra-core/src/resolve.rs`) passes them through, and
  that they are not treated as Claude session markers.
- hydra registers the server once in each environment's Claude config: `sync_mcp`
  (`hydra-providers/src/claude_files.rs`) adds a server named `hydraui` to the `mcpServers` it
  writes into every environment's `.claude.json`, alongside those copied from `~/.claude.json`:

```json
"hydraui": {
  "type": "http",
  "url": "${HYDRA_MCP_URL}",
  "headers": { "Authorization": "Bearer ${HYDRA_MCP_TOKEN}" }
}
```

  hydra's entry wins over one of the same name in `~/.claude.json`.
- Only environments with their own `[claude]` get the server (the user's decision): hydra never
  writes the user's own `~/.claude.json`, so a terminal whose environment has no `[claude]` runs
  Claude without it. The variables are still set there; nothing reads them.
- hydra pre-approves the tools (the user's decision): the `settings.json` it generates for each
  environment (`merge_settings`) gains `permissions.allow` entries for the five tools by name
  (`mcp__hydraui__tasks_read`, `..._tasks_add`, `..._tasks_take`, `..._tasks_verify`,
  `..._tasks_mine`), added to
  whatever allow list the user's settings already have. By name rather than the whole server, so
  a tool added later is asked about until it's added here too. (Our call.)
- Claude Code expands `${HYDRA_MCP_URL}` and `${HYDRA_MCP_TOKEN}` from the session's environment,
  so a session in a hydraui terminal talks straight to that terminal's endpoint; there is no
  bridge process.
- **Outside hydraui** (no variables set): Claude Code warns "Missing environment variables:
  HYDRA_MCP_URL, HYDRA_MCP_TOKEN" and lists `hydraui` as failed. Accepted (the user's decision):
  one transport, no stdio bridge.
- `[claude] mcp.exclude = ["hydraui"]` opts an environment out of the registration.

## 4. The board file: who took a card

- A card in **InProgress**, **Verify** or **Done** may start with a name in square brackets:
  `- [claude] Ctrl+K should open the launcher from anywhere.` The name is who took it.
- Reading: in those three columns only, a leading `[name] ` (name non-empty, no `]`, not a checkbox
  `[ ]`/`[x]`/`[X]`) is the card's **taker**, not part of its text. In Backlog and Pickup a leading
  `[...]` is ordinary text, so a hand-written "[WIP] ..." there stays as typed.
- Writing: a card with a taker is written `- [<taker>] <text>`; the taker is never part of the
  card's text, so card identity for merging (column, text, occurrence; task board §6) is
  unchanged.
- Escaping: in those three columns, a card with no taker whose text would read back as a taker, a
  checkbox or an escape (e.g. "[WIP] fix login" moved to InProgress) is written with a `\` before
  it: `- \[WIP] fix login`. Reading there, a leading `\` before `[` is dropped and the rest is
  ordinary text, so no taker is split off. Backlog and Pickup never write or read the `\`.
- Moving in the Tasks tab: moving a taken card to Backlog or Pickup drops its taker; to Verify or
  Done keeps it; editing its text keeps it. Moves by drag and by the right-click menu behave the same.
- The Tasks tab shows a taken card's taker as a small badge on the card (look: builder's call).
  The taker isn't editable in the tab; editing the file by hand can change it.
- **Commit.** A card `tasks_verify` moved records the commit holding its work after the taker:
  `- [claude @5e9a827] Ctrl+K should open the launcher.` Reading, a taker ending in ` @` and 4 to
  40 hex digits splits into the name and the commit; anything else after ` @` stays part of the
  name. Moving the card to Backlog or Pickup drops the commit with the taker. The Tasks tab shows
  the commit beside the taker badge; clicking it opens what that commit changed (`git show`) as a
  diff tab, `<hash> (diff)`, in the environment's file pane, opening the pane if needed.

## 5. The tools

All tools act on the calling terminal's project board (§2), read fresh from `.hydra/tasks.md` at
each call. Every write goes through the same path as the Tasks tab (`tasks_project::commit`: re-read,
apply, atomic write). That path gains a cross-process lock on the board (e.g. an exclusive lock on
`.hydra/tasks.md.lock` held for the re-read/apply/write), used by the Tasks tab and the server
alike, so two sessions, two hydraui windows on the same repo, or a session and the tab can't both
take one card or lose each other's change. (Our call.) An open Tasks tab sees the change through its file watcher, as it does
for hand edits. The first card written to a new file in a git repository asks the commit/private
question in the window, as the Tasks tab does (task board §3), if that's simple to reach from the
server; otherwise the file is created and the question waits for the next change made in the tab
(builder's call).

Names below are a suggestion (builder's call); descriptions must carry the workflow.

- **`tasks_read`**: the whole board: each column's cards in order, each with its text, taker and
  commit (if any), plus the caller's own name and project folder. Read-only.
- **`tasks_add(text)`**: adds a card at the top of **Backlog**. Text is normalised as the tab does
  (one paragraph); empty is refused.
- **`tasks_take(text)`**: moves the Pickup card with exactly this text to the top of
  **InProgress**, with the caller's name as taker. If several Pickup cards have that text, the first.
  If none (already taken, moved or edited), an error saying so and suggesting `tasks_read`.
- **`tasks_verify(text, commit)`**: once the card's work is committed, moves the caller's own
  InProgress card with exactly this text (taker = the caller's name) to the top of **Verify**,
  keeping the taker and recording `commit` (the last commit, if the work took several) as its short
  hash. `commit` must name a commit in the project's repository (`git rev-parse`); otherwise an
  error asks for the work to be committed first. If no card matches, an error saying so and
  suggesting `tasks_mine`. Another session's card can't be moved.
- **`tasks_mine`**: the InProgress cards whose taker is the caller's current name. Read-only. (A
  renamed terminal no longer matches its older cards; accepted.)

There are no tools to move a card into Pickup or Done, out of Verify, back out of InProgress
other than to Verify, edit, reorder or delete. Those are the user's: only the user moves a card
from Verify to Done.

**What the instructions and descriptions tell Claude** (wording: builder's call):
- The board is the user's to-do list for this project; Pickup holds work the user has approved.
- Check Pickup when the user asks, and after finishing a piece of work.
- Take one card at a time with `tasks_take`, then do the work it describes.
- Once the card's work is committed, move it to Verify with `tasks_verify`, giving the commit,
  and tell the user;
  until then it stays in InProgress. The user checks it and moves it to Done. Never claim a card
  is done on the board.
- Ideas or follow-ups worth keeping go to Backlog with `tasks_add`, not into Pickup.
- Use `tasks_mine` to recall what this session is working on.

## 6. Testing

Unit tests: parsing and writing the taker (InProgress/Done only, checkbox not mistaken, round
trip, merge identity ignoring the taker); the tab's moves dropping or keeping it; each tool's
effect on a board file, including take when the card is gone and two takes of one card (one wins);
refused requests (no token, unknown token, closed terminal's token, foreign `Origin`); `sync_mcp`
adding the `hydraui` entry; `merge_settings` adding the five allow rules without dropping the
user's own; taker names made unique and bracket-safe; the board lock held across the
re-read/apply/write. An end-to-end test that drives the HTTP endpoint with JSON-RPC
(`initialize`, `tools/list`, `tools/call`). Checked by hand: a real `claude` in a hydraui terminal
lists the server, reads the board, takes a card, and the Tasks tab shows it with its badge.

## 7. Calls made without the user

Marked "our call" / "builder's call" above, plus: hydra registers the server in each environment's
Claude config automatically; the taker is the terminal's name; take moves to the top of
InProgress; add goes to the top of Backlog; Done keeps the taker and Backlog/Pickup drop it;
the `[name]` marker is read only in InProgress and Done; sessions can't delete or edit cards; a
terminal's name is fixed at spawn and made unique in the window; the board gets a cross-process
lock; the allow rules name each tool.

Decided by the user: Pickup and Done are the user's; sessions add to Backlog and take from Pickup
only; no waking of idle sessions for now; one localhost server per hydraui (for later
session-to-session messaging); takers are recorded; only `[claude]` environments get the server;
the tools are pre-approved.
