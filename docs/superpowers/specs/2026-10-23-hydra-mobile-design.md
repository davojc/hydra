# Hydra mobile: an Android companion app for hydraui

A phone app that talks straight to the user's desktop hydraui over iroh (peer to peer, no server
of ours). From it the user sees their sessions, gets told when one needs them or finishes, opens a
stuck Claude session in the Claude app through Remote Control, and drops cards onto a project's
task board, including straight into Pickup so work starts while they're away. Builds on the
[MCP server](2026-10-20-hydraui-mcp-server-design.md), [task board](2026-10-19-hydraui-task-board-design.md)
and [auto-pickup](2026-10-20-hydraui-auto-pickup-design.md) designs; everything not named here
stays as it is.

Screen mockups: https://claude.ai/artifact/9Ziph74t33qtcWj4Z7Zt2d (approved by the user as they
stand: Pair, Sessions, One session, Task board, Add a card).

## 1. Why

When the user is away from their desk they want to:

- see what their sessions are doing: running, finished, or waiting on them;
- unblock a session that's waiting on them;
- start new work.

hydraui already knows all of this (Claude's status hooks, the boards, auto-pickup). What's missing
is a way to reach it from another device. The user chose a dedicated app over chat bots: a live
session list, boards and buttons beat parsing chat messages. Talking to an individual session is
not rebuilt here: Claude Code's own Remote Control already does that, in the Claude app.

### Goals

- An Android app, sideloaded onto the user's own phone for now, built so it can later ship to the
  Play Store and App Store (one codebase for both).
- One phone paired with one desktop hydraui, by scanning a QR code once.
- A direct, encrypted connection over iroh: nothing of ours in the middle, no public URL, works
  across networks (home router, mobile data).
- Live while the app is open: the session list and boards update as they change, and an in-app
  banner appears when a session needs the user or finishes.
- Turn on / open Remote Control for a Claude session.
- Add cards to any project board, and move cards between columns, including into Pickup.

### Non-goals (v1)

- Notifications while the app is closed. No push service, no queue: when no phone is connected,
  hydraui sends nothing and keeps nothing for later. On connect the app just shows the current
  state.
- Several desktops per phone, or several phones per desktop.
- Several hydraui windows at once. Each window is its own process; the phone talks to one of them
  (§4, "Which window") and sees only that window's sessions and boards.
- Editing or deleting card text, reordering, clearing Done.
- Sending prompts or keystrokes to a session from the app. That goes through Remote Control.
- Showing what a waiting Claude is asking. The status hooks only report "waiting", not the
  question.
- Store releases (Play Store / App Store), iOS builds. Later; nothing here should rule them out.
- Pluggable chat channels (Telegram, Slack). Parked as a Backlog card.

### Anti-goals

- Anything other than the paired phone being able to connect, read sessions or touch boards.
- Typing into a session that is showing a question or permission prompt. A keystroke there would
  answer the prompt for the user.
- hydraui trying to reach the phone when the app isn't connected (no retries, no queued alerts).
- Turning on Remote Control for every Claude session. The user said no: it's per environment, or
  per session on request (§5).

## 2. Decisions the user made

- **A dedicated app over iroh**, not chat bots.
- **Tauri 2 (mobile)** for the app: a Rust core (shares iroh and Hydra's own types with hydraui)
  and a web UI; one codebase for Android now, iOS later.
- **Android first**, sideloaded onto the user's phone; stores later.
- **Live only.** No notifications when the app is closed, and hydraui doesn't even try.
- **One phone ↔ one desktop** for now, and **one hydraui window**: the phone doesn't gather
  sessions from several windows.
- **Talk to hydraui, not to sessions.** Session conversations happen in the Claude app via Remote
  Control.
- **The phone may put cards straight into Pickup**, which starts work through auto-pickup.
- **Remote Control is opt-in per environment** (a Settings switch), plus on demand from the phone
  for a session that isn't showing a prompt (§5). Never on for every session by default.
- **The app looks like hydraui**: the same dark zinc palette, Inter / JetBrains Mono, teal accent,
  env colour dots (as in the mockups).

## 3. Parts

1. **A shared crate, `hydra-link`** (my call on the name): the message types both sides speak and
   the pairing logic, built on iroh. Used by hydraui and by the app's Rust core.
2. **hydraui's side**: an iroh endpoint on its own thread with its own tokio runtime (hydraui has
   no tokio today; see §7), bridged into the UI the same way the MCP server is: events in over a
   `futures` mpsc channel, pumped into `Workspace` by a GPUI task.
3. **Settings › Phone** in hydraui's Settings tab: turn phone access on, pair, see the paired
   phone, unpair. Plus the per-environment Remote Control switch on each environment's page.
4. **The app** (`hydra-mobile`, Tauri 2): Rust core holds the iroh connection and the pairing
   secret; the web UI renders the five screens from the mockups.

## 4. Behaviour

### Pairing

- Phone access is **off by default** (my call). While it's off hydraui starts no endpoint.
- In Settings › Phone the user presses **Pair a phone**. hydraui shows a QR code for two minutes
  holding its iroh node id and a one-time pairing secret (and relay hints if needed).
- The app scans it, connects, and proves the secret. Both screens then show the same short word
  pair (e.g. `amber-otter`) derived from the session, so the user can see they reached their own PC
  (my call; it was in the approved mockup). hydraui stores the phone's node id as the one allowed
  peer; the app stores hydraui's node id. Secrets go in the OS keyring on the desktop (`keyring` is
  already a dependency) and the Android keystore on the phone.
- From then on hydraui accepts connections **only from that node id**; everything else is refused
  before any message is read. Pairing a new phone replaces the old one. **Unpair** forgets it.
- iroh's public relays (n0's) are used when a direct path can't be made; traffic is end-to-end
  encrypted either way (my call: the defaults, no self-hosted relay in v1).

### Which window

Each hydraui window is its own process (`claim.rs`), and two processes can't both run an endpoint
with the same iroh key. My calls:

- Phone access, the pairing and the paired phone's node id are stored once per user (hydraui's
  config plus the keyring), not per window, so pairing survives restarts and window changes.
- The first window that starts with phone access on takes a cross-process lock and runs the
  endpoint; other windows don't. If that window closes, the next window to notice (at start, or
  on a periodic check) takes over. The phone sees whichever window holds it, and Settings › Phone
  says which one that is ("Phone connects to this window" / "…to another hydraui window").

### What the app sees and does

- **Sessions** (Sessions screen): every terminal, grouped by environment (env name and colour),
  each with a label (`label_for`), its folder, and its state. Claude terminals use the hook state
  (`SessionState`: working / waiting = "Needs you" / idle / ended); other terminals use the
  activity heuristic. A "Finished" state is shown for a session that went Working → Idle (the
  existing `alert_for` rule) until the user opens it or it changes again (my call). If a terminal
  holds a card (`taker` on a board), its text is shown.
- **Alerts**: hydraui forwards the same `NeedsYou` / `Finished` alerts it computes today
  (`mcp/mod.rs` `alert_for`) to the connected app, which shows them as a banner. Unlike the desktop
  toast, these go to the phone whether or not the terminal is visible on the desktop.
- **One session** screen: state, environment, project, current card, last state change, and the
  Remote Control button (§5).
- **Boards**: a project picker listing the projects hydraui knows (those of open terminals and boards
  it has loaded), the five columns with counts, cards with taker and commit. **Move…** moves a card
  to another column; **Add card** adds to Backlog or Pickup. All changes go through
  `tasks_project::commit` exactly as the MCP tools do, so the desktop board reloads by itself.
- **A card the phone puts into Pickup must start work** (a user requirement). Today it wouldn't:
  auto-pickup fires only on `TaskBoardEvent::ArrivedInPickup`, emitted by a move on an open Tasks
  tab in that window, and changes arriving by file reload are deliberately ignored (auto-pickup
  spec §4) so two windows don't both send a card. So the window that holds the phone link must
  call the auto-pickup path itself after a phone commit into Pickup, whether or not a Tasks tab has
  that board loaded, and only that window does so. The candidate rules need one adjustment: the
  user is away, so "the focused session" shouldn't decide anything; otherwise the existing rules
  stand (my call). This amends the auto-pickup spec. If no session can take it, the card stays in
  Pickup and the app says so.
- **Connection**: a pill shows connected / reconnecting. On (re)connect the app asks for a full
  snapshot, then receives changes. Terminal ids are only stable while hydraui runs; after a restart
  the snapshot simply replaces everything.

### Identifying things on the wire

- A session is referred to by an id hydraui hands out for this connection (e.g. the `PaneId`), not
  by its MCP token, which must never leave the machine.
- A card is referred to as the board does it today: project root + `CardRef { column, text, nth }`.
  A stale reference (the card changed meanwhile) is refused with a message, and the app refreshes.

## 5. Remote Control

Claude Code (checked: 2.1.309) supports Remote Control at launch (`claude --remote-control`) and
from inside a running session (`/remote-control`).

- **Per environment**: a switch on each environment's Settings page, "Claude sessions start with
  Remote Control" (off by default). When on, Claude sessions in that environment start with Remote
  Control on, so a stuck one is already reachable from the Claude app. Claude Code has a
  `remoteControlAtStartup` setting (found in the 2.1.309 binary, not yet tried), and each
  environment already has its own Claude config (`CLAUDE_CONFIG_DIR`), so the switch writes that
  setting into the environment's config. The builder confirms which file Claude reads it from and
  that a plain `claude` honours it. If nothing works for a plain `claude`, drop the switch and keep
  on-demand only (my call); no shell aliases or wrappers.
- **On demand from the phone**: for a session whose Remote Control is off, the button turns it on
  by typing `/remote-control` into the session (the way auto-pickup types prompts), **only when it's
  safe to type**: the session is idle with no draft (`readiness`). While it's showing a question or
  permission prompt, the button doesn't type; the app says "Remote Control is off for this session.
  Answer it at your PC." Whether typing is also safe while Claude is *working* is for the builder to
  check; if not, idle only.
- hydraui needs to know which sessions have Remote Control on (env switch, or it typed the command)
  so the button reads **Open in Claude** instead of **Turn on**. Opening launches the Claude app
  (or claude.ai/code) on the phone; deep-linking to the exact session is a nice-to-have if Claude
  Code exposes the session's URL (e.g. in its terminal output).

## 6. Constraints

- Windows desktop is the primary host (hydraui also builds on macOS; nothing here should be
  Windows-only).
- hydraui's UI thread must never block on the network.
- The MCP token, credentials and environment variables never go to the phone.
- Follows the repo's existing standards: tests for the protocol, pairing and board changes;
  clippy clean.
- Dev builds of hydraui must be stopped by PID or `scripts\stop-dev-hydraui.ps1`, never by name
  (see CLAUDE.md).

## 7. Left to the builder

- The wire protocol's shape (request/response plus an event stream over iroh QUIC streams, JSON
  or a binary encoding) and versioning, as long as an older app gets a clear "update hydraui/app"
  message rather than garbage.
- Where the app lives in the repo (e.g. `apps/hydra-mobile`) and whether it joins the Cargo
  workspace (`members = ["crates/*"]` today); the Android build setup and signing for sideloading.
- The web UI framework inside Tauri.
- How the tokio runtime for iroh is hosted inside hydraui.
- The QR payload format and the word-pair derivation.
- Exact copy on screens beyond what the mockups show.
