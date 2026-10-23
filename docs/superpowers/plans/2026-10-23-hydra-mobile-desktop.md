# Hydra mobile, plan 1 of 2: the desktop side — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** hydraui can be paired with a phone and serves it, over iroh, the live session list, alerts, task boards (add/move, Pickup starts work) and Remote Control; proven end to end with a command-line stand-in for the phone (`link-probe`).

**Architecture:** A new crate `hydra-link` holds the wire protocol, framing, pairing and both ends of the connection (a `Host` for hydraui, a `Client` for the phone), so the Tauri app in plan 2 reuses the client unchanged. hydraui runs the `Host` on its own thread with its own tokio runtime and bridges it into GPUI the way the MCP server is bridged (unbounded `futures` channel pumped by `cx.spawn_in`). Only one hydraui window owns the link, by a cross-process file lock.

**Tech Stack:** Rust 2024, iroh 1.x (1.4.1 at time of writing; check docs.rs/iroh for the exact 1.x API names: `Endpoint::builder`, `SecretKey`, `EndpointId`, `EndpointAddr`, `RelayMode`, `accept`/`connect`, bi-streams), tokio (iroh needs it; hydraui has none today), serde/serde_json, `qrcode` crate for the pairing QR, `keyring` (already used), GPUI + gpui-component.

**Spec:** `docs/superpowers/specs/2026-10-23-hydra-mobile-design.md`. Mockups: https://claude.ai/artifact/9Ziph74t33qtcWj4Z7Zt2d. Plan 2 (the Tauri Android app) is written after this one lands.

## Global Constraints

- Workspace `rust-version` goes from `1.89` to `1.91` (iroh 1.x needs it; CI uses stable, the dev machine has 1.98).
- hydraui's UI thread never blocks on the network.
- The MCP token, credentials and environment variables never go to the phone.
- Phone access is off by default; while off, hydraui starts no endpoint.
- Only the paired phone's endpoint id may connect; everything else is refused before any message is read. Pairing a new phone replaces the old one.
- A pairing offer lives two minutes.
- No queue: when no phone is connected, nothing is sent or kept for later.
- Never type into a session that is waiting (a question or permission prompt).
- On-demand Remote Control types `/remote-control` only when the session is idle with no draft. Refusal copy, exactly: `Remote Control is off for this session. Answer it at your PC.`
- Per-environment switch copy, exactly: `Claude sessions start with Remote Control` (off by default). No shell aliases or wrappers.
- One hydraui window owns the link. Settings › Phone copy: `Phone connects to this window` / `Phone connects to another hydraui window`.
- Dev builds of hydraui are stopped by PID or `scripts\stop-dev-hydraui.ps1`, never by name.
- Tests for protocol, pairing and board changes; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt`.

## Review Focus

1. **A phone request naming a project hydraui doesn't know** (any path, `..`, another repo): refused, nothing written. Test in Task 5.
2. **A stale card reference** (the card was moved or edited on the desktop meanwhile): refused with `Missing`, board unchanged, app gets the fresh board. Test in Task 5.
3. **A second hydraui window starting while the first owns the link**, and the first closing: the second must not start an endpoint, then must take over. Test in Task 6.
4. **The phone disconnecting mid-request or the app being killed**: hydraui keeps running, drops the connection, broadcasts go nowhere, no task leaks. Test in Task 3.
5. **Remote Control asked for a session that is waiting or has a draft**: nothing typed, the exact refusal returned. Test in Task 8.

---

## File map

- Create `crates/hydra-link/` — `Cargo.toml`, `src/lib.rs` (ALPN, re-exports), `src/protocol.rs` (wire types), `src/framing.rs` (length-prefixed JSON), `src/pairing.rs` (offer, secret check, word pair), `src/host.rs` (desktop end), `src/client.rs` (phone end), `src/bin/link-probe.rs` (CLI stand-in for the phone), `tests/link.rs` (host↔client over local iroh).
- Create `crates/hydraui/src/link/` — `mod.rs`, `store.rs` (key, settings, paired id), `owner.rs` (cross-process lock), `wire.rs` (workspace data → wire types, pure), `boards.rs` (phone board changes, pure + file).
- Create `crates/hydraui/src/workspace/phone.rs` — the GPUI wiring (start/stop host, pump events, answer requests, broadcast).
- Create `crates/hydraui/src/workspace/phone_page.rs` — Settings › Phone page.
- Modify `crates/hydraui/src/workspace/pickup.rs` (phone arrivals), `crates/hydraui/src/workspace/busy.rs` (forward alerts), `crates/hydraui/src/workspace/settings_tab.rs` (Phone section, env switch), `crates/hydraui/src/workspace.rs` (fields, startup), `crates/hydraui/src/pane_view.rs` (Remote Control flag), `crates/hydraui/src/config.rs` (`phone_access`, `phone`), `crates/hydra-core/src/config.rs` (`ClaudeConfig.remote_control`), `crates/hydra-providers/src/claude.rs` (write the Claude setting), `Cargo.toml` (workspace deps, rust-version).

---

### Task 1: `hydra-link` crate: protocol and framing

**Files:**
- Create: `crates/hydra-link/Cargo.toml`, `crates/hydra-link/src/lib.rs`, `src/protocol.rs`, `src/framing.rs`
- Modify: `Cargo.toml` (workspace: `rust-version = "1.91"`, add `hydra-link = { path = "crates/hydra-link" }`, `iroh = "1"`, `tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "io-util"] }`)

**Interfaces:**
- Produces (all `Serialize + Deserialize + Debug + Clone + PartialEq`):
  - `pub const ALPN: &[u8] = b"hydra/link/1";` `pub const PROTOCOL_VERSION: u32 = 1;`
  - `pub enum Column { Backlog, Pickup, InProgress, Verify, Done }` (hydra-link's own; hydraui maps to `tasks::Column`)
  - `pub struct WireCard { text: String, taker: Option<String>, commit: Option<String> }`
  - `pub struct CardRef { column: Column, text: String, nth: usize }`
  - `pub struct BoardInfo { project: String, name: String, columns: Vec<Vec<WireCard>> }` (5 entries, in `Column` order)
  - `pub enum Status { Working, NeedsYou, Finished, Idle, Ended }`
  - `pub enum Kind { Claude, Shell }`
  - `pub struct SessionInfo { id: u64, env: String, env_color: String /* "#rrggbb" */, label: String, cwd: String, project: Option<String>, kind: Kind, status: Status, card: Option<String>, remote_control: bool, changed_at: i64 /* unix secs */ }`
  - `pub enum AlertKind { NeedsYou, Finished }`
  - `pub enum ClientMsg { Hello { version: u32 }, Pair { secret: String }, Snapshot, Board { project: String }, AddCard { project: String, text: String, to: Column }, MoveCard { project: String, card: CardRef, to: Column }, RemoteControl { session: u64 }, Seen { session: u64 } }`
  - `pub struct Request { id: u64, msg: ClientMsg }`
  - `pub enum Outcome { Ok, Snapshot { sessions: Vec<SessionInfo>, projects: Vec<String> }, Board(BoardInfo), Refused(String), Missing, Paired { word: String } }`
  - `pub enum ServerMsg { Welcome { version: u32, window: String }, Reply { id: u64, outcome: Outcome }, Session(SessionInfo), SessionGone { id: u64 }, Board(BoardInfo), Alert { session: u64, kind: AlertKind }, Notice(String), TooOld { need: u32 } }`
  - `framing::write_msg<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()>` and `framing::read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>>` (`None` on clean EOF); frame = u32 big-endian length + JSON; `pub const MAX_FRAME: u32 = 1 << 20;`, a longer frame is `ErrorKind::InvalidData`.
- Serde: enums tagged `#[serde(tag = "type", rename_all = "snake_case")]`, fields snake_case (the Tauri web UI reads these as JSON).

- [ ] **Step 1: Write the failing tests** in `framing.rs` `#[cfg(test)]` (tokio `duplex`):
  - `round_trips_every_message`: write then read a `Request { id: 7, msg: ClientMsg::AddCard { project: "E:/p".into(), text: "x".into(), to: Column::Pickup } }` and a `ServerMsg::Session(..)`; read equals written.
  - `eof_between_frames_is_none`: read on a closed empty stream returns `Ok(None)`.
  - `oversized_frame_is_refused`: a header claiming `MAX_FRAME + 1` gives `InvalidData` without allocating it.
  - `json_shape_is_stable`: `serde_json::to_value(ClientMsg::Seen { session: 3 })` equals `json!({"type":"seen","session":3})`.
- [ ] **Step 2: Run** `cargo test -p hydra-link` — expect compile failure (types missing).
- [ ] **Step 3: Implement** the types and `write_msg` / `read_msg`.
- [ ] **Step 4: Run** `cargo test -p hydra-link` — expect PASS; `cargo build --workspace` still builds with the new rust-version.
- [ ] **Step 5: Commit** `feat(hydra-link): wire protocol and framing for the phone link`

### Task 2: Pairing: offer, secret check, word pair

**Files:**
- Create: `crates/hydra-link/src/pairing.rs`

**Interfaces:**
- Produces:
  - `pub struct PairingOffer { pub endpoint: String /* EndpointId as its string form */, pub secret: String /* 32 random bytes, lowercase hex */, pub relay: Option<String> }`
  - `impl PairingOffer { pub fn new(endpoint: String, relay: Option<String>) -> PairingOffer; pub fn encode(&self) -> String /* "hydra-pair:" + base64url(no pad) of its JSON */; pub fn decode(s: &str) -> Option<PairingOffer> }`
  - `pub fn secret_matches(expected: &str, given: &str) -> bool` (constant-time over bytes; different lengths false)
  - `pub fn word_pair(secret: &str, desktop: &str, phone: &str) -> String` — `"<adjective>-<animal>"` chosen from two fixed 64-word lists by bytes 0 and 1 of SHA-256(`secret` ‖ `desktop` ‖ `phone`) mod 64. Lists are lowercase single words, no duplicates.
  - `pub const OFFER_LIFETIME: Duration = Duration::from_secs(120);`

- [ ] **Step 1: Write the failing tests**:
  - `an_offer_round_trips`: `PairingOffer::decode(&o.encode()) == Some(o)`; starts with `"hydra-pair:"`.
  - `garbage_is_not_an_offer`: `decode("hello")`, `decode("hydra-pair:!!")` are `None`.
  - `secrets_are_64_hex_chars_and_differ`: two `new` calls give different 64-char hex secrets.
  - `secret_matches_only_the_same_secret`: same → true; one char off → false; empty → false.
  - `both_ends_get_the_same_word`: `word_pair(s, d, p)` is deterministic, matches `^[a-z]+-[a-z]+$`, and changes when `p` changes (pick inputs where it does).
- [ ] **Step 2: Run** `cargo test -p hydra-link pairing` — FAIL.
- [ ] **Step 3: Implement** (`getrandom`, `sha2`, `base64` — add `base64 = "0.22"` to workspace deps).
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat(hydra-link): pairing offers and matching words`

### Task 3: Host and Client over iroh

**Files:**
- Create: `crates/hydra-link/src/host.rs`, `crates/hydra-link/src/client.rs`, `crates/hydra-link/tests/link.rs`

**Interfaces:**
- Consumes: Task 1 types and framing; Task 2 pairing.
- Produces (host, desktop end; runs its own tokio runtime on a thread named `hydra-link`, so callers need no tokio):
  - `pub struct HostConfig { pub secret_key: [u8; 32], pub paired: Option<String> /* phone EndpointId */, pub window: String, pub relays: bool /* false in tests: RelayMode::Disabled */ }`
  - `pub enum HostEvent { Request { id: u64, msg: ClientMsg, reply: Replier }, Paired { phone: String, word: String }, Connected, Disconnected }`
  - `pub struct Replier` with `pub fn send(self, outcome: Outcome)` (dropping it unsent replies `Refused("hydraui closed the request")`).
  - `pub struct Host` with `pub fn start(cfg: HostConfig) -> io::Result<(Host, futures::channel::mpsc::UnboundedReceiver<HostEvent>)>`, `pub fn endpoint_id(&self) -> String`, `pub fn addr_for_tests(&self) -> iroh::EndpointAddr`, `pub fn offer_pairing(&self) -> PairingOffer` (replaces any live offer; expires after `OFFER_LIFETIME`), `pub fn unpair(&self)` (forgets the phone, closes its connection), `pub fn broadcast(&self, msg: ServerMsg)` (to the connected phone; dropped when none), `pub fn stop(self)` (also on `Drop`).
  - Rules: one phone connection at a time (a new one from the paired phone replaces the old). A connection from an endpoint id that is not `paired` is closed with no message read **unless** an offer is live and its first frame is `ClientMsg::Pair { secret }` with a matching secret; then the host replies `Outcome::Paired { word }` (id 0), emits `HostEvent::Paired`, records that id as paired, and keeps the connection. A failed `Pair` closes it and does not consume the offer. After `Hello { version }` the host sends `Welcome`, or `TooOld { need: PROTOCOL_VERSION }` and closes when `version < PROTOCOL_VERSION`. Each `Request` becomes one `HostEvent::Request`; its `Reply` goes back on the same connection. Phone→desktop and desktop→phone use one bi-stream each way, framed.
- Produces (client, phone end; async, caller supplies tokio):
  - `pub struct Client` with `pub async fn pair(secret_key: [u8; 32], offer: &PairingOffer, relays: bool) -> Result<(Client, String /* word */), LinkError>`, `pub async fn connect(secret_key: [u8; 32], desktop: &str, relays: bool) -> Result<Client, LinkError>`, `pub async fn request(&self, msg: ClientMsg) -> Result<Outcome, LinkError>`, `pub fn events(&self) -> tokio::sync::broadcast::Receiver<ServerMsg>` (everything that isn't a `Reply`), `pub fn endpoint_id(&self) -> String`.
  - `pub enum LinkError { Refused, NotPaired, TooOld(u32), Closed, Io(String) }`
  - For tests, `Client::pair_at(secret_key, offer, addr: iroh::EndpointAddr)` and `connect_at(secret_key, addr)`, which dial an address directly with relays off.

- [ ] **Step 1: Write the failing tests** in `tests/link.rs` (tokio test runtime for the client; relays off):
  - `pairing_with_the_right_secret_pairs`: host with no `paired`; `offer_pairing()`; `Client::pair_at` succeeds; both words equal; host emits `Paired { phone }` equal to the client's `endpoint_id()`.
  - `a_wrong_secret_is_refused_and_the_offer_survives`: tamper the secret → `Err(Refused)`; then the real offer still pairs.
  - `an_expired_offer_is_refused`: construct the host with a test-only offer lifetime of 50 ms (`HostConfig` gains `#[doc(hidden)] pub offer_lifetime: Duration`, default `OFFER_LIFETIME`); pair after 100 ms → `Err(Refused)`.
  - `a_stranger_is_refused_once_paired`: after pairing, a client with another key `connect_at` → `Err(Refused)` and no `HostEvent::Request` is ever emitted.
  - `requests_get_their_replies`: paired client sends `ClientMsg::Snapshot`; test answers the `HostEvent::Request` with `Outcome::Snapshot { sessions: vec![], projects: vec!["E:/p".into()] }`; client gets exactly that.
  - `broadcast_without_a_phone_is_dropped_and_later_messages_arrive`: `broadcast(Notice("lost"))` with no client; connect; `broadcast(Notice("seen"))`; the client's first event is `Notice("seen")`.
  - `an_old_app_is_told_to_update`: a raw connection sending `Hello { version: 0 }` gets `TooOld { need: 1 }`.
  - `a_vanished_phone_does_not_stop_the_host` (Review Focus 4): connect, drop the client mid-request (request sent, no reply yet); host emits `Disconnected`; answering the stale `Replier` doesn't panic; a new connection from the same phone works.
  - `unpair_closes_and_refuses`: after `unpair()`, the old client's next request is `Err(Closed)` and reconnecting is `Err(Refused)`.
- [ ] **Step 2: Run** `cargo test -p hydra-link --test link` — FAIL.
- [ ] **Step 3: Implement** `host.rs` and `client.rs`. Host checks the remote endpoint id from the accepted connection before reading any frame; the client sends `Hello { version: PROTOCOL_VERSION }` first on every connection (after `Pair` when pairing).
- [ ] **Step 4: Run** — PASS. Also `cargo clippy -p hydra-link --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** `feat(hydra-link): host and client over iroh, paired-only`

### Task 4: `link-probe`, a command-line phone

**Files:**
- Create: `crates/hydra-link/src/bin/link-probe.rs`

**Interfaces:**
- Consumes: `Client`, `PairingOffer`.
- Produces the CLI (clap, already a workspace dep): `link-probe --state <dir> pair <offer-string>`, `link-probe --state <dir> watch` (prints every `ServerMsg` as one JSON line), `link-probe --state <dir> send '<ClientMsg JSON>'` (prints the `Outcome` JSON). `<dir>/key` holds the probe's secret key (created on first use), `<dir>/desktop` the paired desktop id. Relays on.

- [ ] **Step 1: Implement** (no unit test: it is a thin shell over Task 3, exercised by hand in Task 10).
- [ ] **Step 2: Run** `cargo run -p hydra-link --bin link-probe -- --help` — prints the three commands.
- [ ] **Step 3: Commit** `feat(hydra-link): link-probe, a command-line stand-in for the phone`

### Task 5: hydraui: wire mapping and phone board changes (pure)

**Files:**
- Create: `crates/hydraui/src/link/mod.rs`, `crates/hydraui/src/link/wire.rs`, `crates/hydraui/src/link/boards.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod link;`), `crates/hydraui/Cargo.toml` (`hydra-link.workspace = true`)

**Interfaces:**
- Consumes: `crate::tasks::{Board, Card, Change, Column, CardRef}`, `crate::tasks_project::{commit, load, CommitError}`, `crate::mcp::SessionState`, `crate::activity::Activity`, `hydra_link::*`.
- Produces:
  - `wire::column(c: tasks::Column) -> hydra_link::Column` and `wire::from_column(c: hydra_link::Column) -> tasks::Column`.
  - `wire::status(claude: Option<SessionState>, activity: Activity, finished: bool) -> (Kind, Status)` — Claude: Waiting→NeedsYou, Working→Working, Idle→`Finished` if `finished` else Idle, Ended→Ended; no Claude: Kind::Shell with Activity Working→Working, else Idle.
  - `wire::board_info(project: &Path, board: &Board) -> BoardInfo` (`name` = the folder's file name).
  - `boards::apply(known: &[PathBuf], project: &str, change: PhoneChange) -> Result<BoardInfo, Outcome>` where `pub enum PhoneChange { Add { text: String, to: hydra_link::Column }, Move { card: hydra_link::CardRef, to: hydra_link::Column } }`. `project` must equal (after `tasks_project::project_root`) one of `known`, else `Err(Outcome::Refused("hydraui doesn't know that project"))`. Add to Backlog = `Change::Add`; Add to any other column = `Change::Add` then `Change::Move { card: CardRef { column: Backlog, text, nth: 0 }, to, index: 0 }`. Move = `Change::Move { to, index: 0 }`. Empty or whitespace text → `Err(Refused("A card needs some text"))`. `CommitError::Missing` → `Err(Outcome::Missing)`. Taking cards (InProgress with a taker) and Done are allowed as plain moves, matching the desktop board.
  - `boards::arrived_in_pickup(change: &PhoneChange) -> Option<String>` — the card text when the change put a card into Pickup.

- [ ] **Step 1: Write the failing tests** (tempdir repos as `tasks_project` tests do):
  - `claude_states_map_to_what_the_phone_shows`: every row of the `status` mapping above.
  - `a_card_added_to_pickup_lands_on_top_of_pickup`: `apply(.., Add { text: "fix paste", to: Pickup })` → board's Pickup[0] is `"fix paste"`, Backlog unchanged.
  - `an_unknown_project_is_refused_and_nothing_written` (Review Focus 1): `project` = another tempdir, and `"<known>/../x"` → `Refused`, no `.hydra/tasks.md` created in either.
  - `a_stale_card_is_missing` (Review Focus 2): Move a `CardRef` whose text isn't on the board → `Err(Outcome::Missing)`, file unchanged.
  - `blank_text_is_refused`.
  - `only_pickup_arrivals_are_reported`: `arrived_in_pickup` is `Some` for Add/Move to Pickup, `None` otherwise.
- [ ] **Step 2: Run** `cargo test -p hydraui --lib link::` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat(hydraui): map sessions and boards for the phone link`

### Task 6: hydraui: link settings, key and window ownership

**Files:**
- Create: `crates/hydraui/src/link/store.rs`, `crates/hydraui/src/link/owner.rs`
- Modify: `crates/hydraui/src/config.rs` (`UiConfig.phone_access: bool` key `phone_access`, default false; `UiConfig.phone: Option<String>` key `phone`, the paired endpoint id)

**Interfaces:**
- Produces:
  - `store::LinkKey` like `jev::JevKey` (service `"hydraui"`, user `"phone-link-key"`, `with_service` for tests): `pub fn get_or_create(&self) -> Result<[u8; 32], String>` (hex in the keyring).
  - `store::set_paired(paths: &HydraPaths, phone: Option<&str>) -> Result<(), String>` and `store::set_enabled(paths, on: bool) -> Result<(), String>` via `config::set`.
  - `owner::try_own(paths: &HydraPaths) -> Option<OwnerLock>` — `File::try_lock` on `<claim::running_dir(paths)>/phone-link.lock`, writing this process's pid into `phone-link.pid` beside it; `OwnerLock` releases on drop. `owner::held_elsewhere(paths) -> bool`.
- [ ] **Step 1: Write the failing tests**:
  - `config_reads_phone_settings`: `parse("phone_access = true\nphone = \"abc\"")` → both set; absent → false/None; `phone_access = "yes"` → a problem naming `phone_access`.
  - `one_owner_at_a_time` (Review Focus 3): `try_own` → Some; second `try_own` on the same paths → None and `held_elsewhere` true; drop the first → a new `try_own` → Some.
  - `the_key_is_created_once`: `LinkKey::with_service("hydraui-test-<random>")` — two `get_or_create` calls return the same 32 bytes (delete the entry after; skip with a message when the platform has no keyring, as `jev` tests do).
- [ ] **Step 2: Run** `cargo test -p hydraui --lib link::store link::owner config::` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat(hydraui): phone link settings, key and window ownership`

### Task 7: hydraui: run the host and serve the phone

**Files:**
- Create: `crates/hydraui/src/workspace/phone.rs`
- Modify: `crates/hydraui/src/workspace.rs` (fields `phone: Option<PhoneLink>`, `phone_finished: HashSet<PaneId>`, `phone_cards: HashSet<(PathBuf, String)>`; start in `startup`), `crates/hydraui/src/workspace/busy.rs` (`session_state` and `alert` forward), `crates/hydraui/src/workspace/pickup.rs` (phone arrivals)

**Interfaces:**
- Consumes: Tasks 3, 5, 6; `Workspace::card_arrived`, `offer_card`, `sessions`/`fresh_sessions`, `pane_name`.
- Produces on `Workspace`:
  - `pub(super) fn phone_start(&mut self, window, cx)` — if `ui.phone_access` and `owner::try_own` succeeds: `Host::start(HostConfig { secret_key: LinkKey::get_or_create, paired: ui.phone, window: <layout file name>, relays: true, .. })`, pump `HostEvent`s with `cx.spawn_in` into `phone_event`. If another window owns it, retry `try_own` every 10 s (`background_executor().timer`) so this window takes over when the owner closes.
  - `pub(super) fn phone_stop(&mut self)`.
  - `fn phone_event(&mut self, event: HostEvent, window, cx)` answering: `Snapshot` → `Outcome::Snapshot` (one `SessionInfo` per pane, id = `PaneId`, env/colour from `envs` and `palette::env_tint`, `card` = text of a card whose taker equals the pane's `taker_name()` in its project's InProgress, `remote_control` from Task 8's flag; `projects` = distinct `project_root` of pane cwds plus loaded boards); `Board { project }` → `tasks_project::load` + `wire::board_info` (refused if unknown); `AddCard`/`MoveCard` → `boards::apply` on a background thread, then broadcast `ServerMsg::Board`, then if `arrived_in_pickup` → `phone_card_arrived(project, text)`; `Seen { session }` → remove from `phone_finished`, broadcast that session; `RemoteControl` → Task 8; `Paired` → `store::set_paired` and show the word on the Phone page (Task 9).
  - `fn phone_broadcast_session(&self, id: PaneId, cx)` — called from `session_state` after `set_claude`; on `Alert::Finished` insert into `phone_finished`, on any other state change remove; on an alert also broadcast `ServerMsg::Alert` **regardless of `in_sight`** (spec §4: unlike the desktop toast). A pane closing broadcasts `SessionGone`.
  - Boards changed elsewhere (desktop, a Claude session, another window): when a loaded `TaskBoard` reloads or commits, broadcast `ServerMsg::Board` for its project. Boards not loaded in this window are fresh whenever the app asks with `ClientMsg::Board`.
  - Amend `docs/superpowers/specs/2026-10-20-hydraui-auto-pickup-design.md` with a short section: a card the phone puts into Pickup is offered by the window holding the phone link, with no session counted as focused.
  - In `pickup.rs`: `fresh_sessions(&mut self, away: bool, window, cx)` — `away` forces `focused = None`; existing callers pass `false`. `pub(super) fn phone_card_arrived(&mut self, project: PathBuf, text: String, window, cx)` records `(project, text)` in `phone_cards` and runs the `card_arrived` path with `away = true`. Where `deliver` / `no_free_session` conclude, a card in `phone_cards` also broadcasts `ServerMsg::Notice` — `Sent to <terminal>` or `No free Claude session in <project name>: the card stays in Pickup` — and leaves `phone_cards`.
- [ ] **Step 1: Write the failing test** in `pickup.rs` tests: `away_means_no_session_is_focused` — `focused_pane` is not consulted when away: build sessions where the shown terminal is free and idle; with `away` it is a candidate, without it is not. (Express as a pure helper `pickup::focused_for(away: bool, window_active: bool, shown: Option<PaneId>, board_shown: impl Fn(PaneId) -> bool) -> Option<PaneId>` that `fresh_sessions` calls.)
- [ ] **Step 2: Run** `cargo test -p hydraui --lib pickup::` — FAIL.
- [ ] **Step 3: Implement** the helper and the wiring above. Network and file work never runs on the UI thread: `boards::apply` and `load` go through `cx.background_spawn`.
- [ ] **Step 4: Run** `cargo test -p hydraui --lib` and `cargo clippy --workspace --all-targets -- -D warnings` — PASS.
- [ ] **Step 5: Commit** `feat(hydraui): serve sessions, alerts and boards to the paired phone`

### Task 8: Remote Control: the env switch and on demand

**Files:**
- Modify: `crates/hydra-core/src/config.rs` (`ClaudeConfig.remote_control: bool`, `#[serde(default)]`), `crates/hydra-providers/src/claude.rs` (write the setting), `crates/hydraui/src/pane_view.rs` (flag), `crates/hydraui/src/workspace/phone.rs` (request), `crates/hydraui/src/workspace/settings_tab.rs` (env page switch)

**Interfaces:**
- Produces:
  - `PaneView::remote_control(&self) -> bool`, `PaneView::set_remote_control(&mut self, on: bool)`. Set at pane creation from the env's `claude.remote_control`; set by the on-demand path.
  - `link::remote_control_reply(claude: Option<SessionState>, draft: bool, on: bool) -> RcAction` with `pub enum RcAction { AlreadyOn, Type, Refuse(&'static str) }`: `on` → AlreadyOn; `claude == Some(Idle) && !draft` → Type; otherwise Refuse(`"Remote Control is off for this session. Answer it at your PC."`). (Not a Claude session at all → Refuse(`"This terminal isn't running Claude."`).)
  - On `Type`: `type_prompt("/remote-control")`, `set_remote_control(true)`, reply `Outcome::Ok`, broadcast the session.
- [ ] **Step 1: Verify the Claude setting by hand** (no code yet). In a scratch config dir: `CLAUDE_CONFIG_DIR=<tmp>` with `{"remoteControlAtStartup": true}` first in `<tmp>/settings.json`, then (if that does nothing) in `<tmp>/.claude.json`; run plain `claude` and check whether Remote Control is on at start (it says so in the session and the session appears in claude.ai/code). Record which file works in the commit message of step 5. **If neither works:** skip the env switch (hydra-core, hydra-providers and settings-tab parts of this task) per the spec, keep the on-demand path, and note it in the commit.
- [ ] **Step 2: Write the failing tests**:
  - `link::remote_control_reply` table (Review Focus 5): Waiting → Refuse with the exact copy; Idle + draft → Refuse; Working → Refuse; Idle no draft → Type; `on` → AlreadyOn; None → the not-Claude refusal.
  - hydra-core: `remote_control_is_off_unless_set` (`[claude]\nremote_control = true` parses; absent is false).
  - hydra-providers: `remote_control_env_writes_the_setting` — an env with `remote_control = true` gets `"remoteControlAtStartup": true` in the file found in step 1; without it, the key is absent (and an existing one from the user's base settings is left as the user had it).
- [ ] **Step 3: Run** `cargo test -p hydra-core -p hydra-providers -p hydraui` — FAIL.
- [ ] **Step 4: Implement**, plus the env page row `Claude sessions start with Remote Control` (a switch writing `[claude] remote_control` through `hydra_core::envedit` — add `set_claude_remote_control(text, on) -> Result<String, String>` beside `set_top`, comment-preserving, with a test that other keys and comments survive).
- [ ] **Step 5: Run** the tests and clippy — PASS. **Commit** `feat: Remote Control per environment and on demand from the phone`

### Task 9: Settings › Phone

**Files:**
- Create: `crates/hydraui/src/workspace/phone_page.rs`
- Modify: `crates/hydraui/src/workspace/settings_tab.rs` (`Section::Phone`, nav item `Phone` under HYDRAUI after General), `crates/hydraui/Cargo.toml` (`qrcode = "0.14"`, default-features off)

**Interfaces:**
- Consumes: Tasks 3, 6, 7 (`Host::offer_pairing`, `Host::unpair`, `store::*`, `owner::held_elsewhere`).
- Produces the page, in hydraui's existing settings style (`page_title`, sections as on the env page):
  - A switch `Let my phone connect` → `store::set_enabled` + `phone_start`/`phone_stop`.
  - A status line: `Phone connects to this window` or `Phone connects to another hydraui window` (or nothing while off).
  - When no phone is paired, or to replace it: button `Pair a phone` → `offer_pairing()`, render `offer.encode()` as a QR (`qrcode::QrCode::new(..)?.to_colors()` drawn as a grid of square divs, dark on white with a quiet zone), with `Scan this with the Hydra app. It expires in 2 minutes.` and a countdown; on `HostEvent::Paired` replace it with `Paired. Check your phone shows <word>.`
  - When paired: the phone's endpoint id shortened to its first 10 chars, `Connected` / `Not connected`, and `Unpair` (asks to confirm, then `unpair()` + `store::set_paired(None)`).
- [ ] **Step 1: Write the failing test**: `qr_grid_matches_the_offer` — a pure helper `phone_page::qr_cells(text: &str) -> Vec<Vec<bool>>` returns a square grid whose side equals `QrCode::new(text).width()`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** tests + clippy — PASS.
- [ ] **Step 5: Commit** `feat(hydraui): Settings › Phone for pairing and unpairing the phone`

### Task 10: End-to-end check with `link-probe`

**Files:** none new (fixes go in the task they belong to).

- [ ] **Step 1:** Build and start a dev hydraui (`cargo run -p hydraui`), note its PID. Settings › Phone → turn on → `Pair a phone`; copy the offer text (temporarily add a "Copy" button beside the QR if there isn't one: keep it, it's useful for desktop testing).
- [ ] **Step 2:** `cargo run -p hydra-link --bin link-probe -- --state <scratch>/probe pair "<offer>"` — prints the same word the page shows.
- [ ] **Step 3:** `link-probe ... watch` in one terminal; in hydraui start `claude` in a terminal, give it a prompt that asks a question → watch shows `Session` with `needs_you` and an `Alert`; answer it → `working`, then `finished` when done.
- [ ] **Step 4:** `link-probe ... send '{"type":"add_card","project":"E:\\AI\\davojc\\hydra","text":"probe card","to":"pickup"}'` with an idle Claude open in that project → `Ok`, the desktop board shows it, the Claude session receives the card, watch shows `Notice` `Sent to …`. Delete the card afterwards.
- [ ] **Step 5:** `send '{"type":"remote_control","session":<id of a waiting session>}'` → the exact refusal; on an idle session → `Ok` and `/remote-control` runs in it.
- [ ] **Step 6:** Open a second hydraui window → its Phone page says `Phone connects to another hydraui window`; close the first (by PID) → within ~10 s the second takes over and `link-probe watch` reconnects.
- [ ] **Step 7:** Stop the dev hydraui by PID. Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` — all pass. Commit any fixes with their owning task's prefix.
