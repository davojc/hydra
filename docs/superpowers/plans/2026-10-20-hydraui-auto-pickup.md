# Auto-pickup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A card the user moves into Pickup is typed into a free Claude session of that project; sessions are free when clearly finished (fresh, or just verified a card) or, with a Jev key, when Jev says so.

**Architecture:** Pure, unit-tested pieces (`jev.rs` client and key, `readiness.rs` per-terminal state machine, `pickup.rs` selection and prompt) wired into the existing `PaneView`, MCP server events, `TaskBoard`/`TaskBoardView` events and the `Workspace`. hydraui types the prompt into the chosen terminal's PTY.

**Tech Stack:** Rust, gpui / gpui-component, `keyring` 3 (Windows Credential Manager), `ureq` 2, `serde_json`, `tiny_http` (test servers).

**Spec:** `docs/superpowers/specs/2026-10-20-hydraui-auto-pickup-design.md`

## Global Constraints

- One Jev key for hydraui, in the Windows credential store, service `hydraui`, user `jev`; never in `config.toml`, a terminal's environment or a log.
- Jev: `POST https://api.typesafe.ai/v1/systemone`, `Authorization: Bearer <key>`, model `"jev-latest"`, questions of `"type": "noul"`; reply `answers.<id>.noul` in 0..1.
- Free by Jev: `finished ≥ 0.8`, `waiting ≤ 0.2`, `draft ≤ 0.2`; error, timeout (5 s) or bad reply → not free.
- Jev state: the terminal's last 60 rows as text.
- Reservation after a send: until the terminal's next hook state, or 30 s.
- Only user moves into Pickup on this window's board fire; reloads never do.
- Terminals in a linked worktree are never candidates.
- Copy: toasts "Sent to <terminal> in <env>", "No free Claude session for this card"; launcher "Set Jev API key…", "Remove Jev API key"; card menu "Send to a free session", "Send to".
- Run `cargo fmt`, `cargo clippy -p hydraui --all-targets` (no warnings) and `cargo test -p hydraui` before each commit.

## Review Focus

1. A card moved into Pickup while the only idle session is the focused terminal: nothing is typed there; toast instead. (Task 5 test `focused_terminal_is_never_a_candidate`.)
2. Jev slow or down: the board stays responsive and the session is not free. (Task 2 tests `a_timeout_is_an_error`, `a_bad_reply_is_an_error`.)
3. The card leaves Pickup (taken by hand, dragged on) while Jev is being asked: nothing is sent. (Task 5 test `send_needs_the_card_still_in_pickup`.)
4. A card whose text has line breaks or quotes: the prompt is one line, the text intact otherwise. (Task 5 test `prompt_is_one_line_and_quotes_the_card`.)
5. Two cards moved into Pickup in quick succession with one free session: the second gets the toast, not the same session. (Task 5 test `a_reserved_terminal_is_not_a_candidate`.)

---

### Task 1: hydraui's Jev key and its launcher entries

**Files:**
- Create: `crates/hydraui/src/jev.rs`
- Modify: `crates/hydraui/src/lib.rs` (add `pub mod jev;`), `crates/hydraui/Cargo.toml` (add `keyring.workspace = true`, move `ureq.workspace = true` to `[dependencies]`)
- Modify: `crates/hydraui/src/launcher.rs` (`Action::SetJevKey`, `Action::RemoveJevKey`; `entries` gains `jev_set: bool`)
- Modify: `crates/hydraui/src/workspace/launcher.rs` (dispatch; set/remove dialogs)

**Interfaces:**
- Produces: `pub struct JevKey { service: String }` with `JevKey::app() -> JevKey` (service `"hydraui"`), `JevKey::with_service(s: &str) -> JevKey` (tests), `get(&self) -> Result<Option<String>, String>`, `set(&self, key: &str) -> Result<(), String>` (blank refused, value trimmed), `remove(&self) -> Result<bool, String>`. Keyring entry user `"jev"`.
- Produces: `launcher::entries(envs, sign_ins, secrets, jev_set: bool) -> Vec<Entry>`.

- [ ] **Step 1: Write the failing tests**

In `jev.rs`:
```rust
#[test]
fn a_key_is_set_read_and_removed() {
    let k = JevKey::with_service(&format!("hydraui-test-{}", std::process::id()));
    assert_eq!(k.get().unwrap(), None);
    k.set("  tsk-123  ").unwrap();
    assert_eq!(k.get().unwrap().as_deref(), Some("tsk-123"));
    assert!(k.remove().unwrap());
    assert_eq!(k.get().unwrap(), None);
    assert!(!k.remove().unwrap());
}
#[test]
fn a_blank_key_is_refused() {
    let k = JevKey::with_service("hydraui-test-blank");
    assert!(k.set("   ").is_err());
}
```
In `launcher.rs`: update both existing tests' `entries(...)` calls to pass `false` and add:
```rust
#[test]
fn jev_entries_follow_whether_a_key_is_set() {
    let labels = |set| entries(&[], &[], &[], set).into_iter().map(|e| e.label).collect::<Vec<_>>();
    assert!(labels(false).contains(&"Set Jev API key…".to_string()));
    assert!(!labels(false).contains(&"Remove Jev API key".to_string()));
    assert!(labels(true).contains(&"Remove Jev API key".to_string()));
}
```
Append `"Set Jev API key…"` to the fixed-entries list in `with_nothing_configured_only_the_fixed_entries_remain` (after "Keyboard shortcuts").

- [ ] **Step 2: Run, expect FAIL** — `cargo test -p hydraui jev launcher` → compile errors (`JevKey`, `entries` arity).

- [ ] **Step 3: Implement** `JevKey` on `keyring::Entry::new(&self.service, "jev")` (`NoEntry` → `None`/`false`); `entries` adds "Set Jev API key…" always and "Remove Jev API key" when `jev_set`. In `workspace/launcher.rs`, read `JevKey::app().get()` (treat `Err` as not set) where `entries` is called; `SetJevKey` opens a dialog like `secret_dialog` with one masked `Input` (title "Jev API key"), Save calls `set` and toasts "Jev API key saved" or the error; `RemoveJevKey` asks "Remove the Jev API key?" (Cancel / Remove, danger) and toasts "Jev API key removed".

- [ ] **Step 4: Run, expect PASS** — `cargo test -p hydraui`.

- [ ] **Step 5: Commit** — `feat: hydraui keeps a Jev API key, set from the launcher`

---

### Task 2: Jev client and the free decision

**Files:**
- Modify: `crates/hydraui/src/jev.rs`

**Interfaces:**
- Produces: `pub const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";`
- Produces: `#[derive(Debug, Clone, Copy, PartialEq)] pub struct Verdict { pub waiting: f64, pub finished: f64, pub draft: f64 }` and `impl Verdict { pub fn is_free(&self) -> bool }` (thresholds from Global Constraints).
- Produces: `pub fn ask(url: &str, key: &str, state: &str) -> anyhow::Result<Verdict>` (blocking; call off the UI thread; 5 s connect+read timeout).
- Produces: `pub fn request_body(state: &str) -> serde_json::Value` (the three questions, ids `waiting`, `finished`, `draft`, instructions copied from spec §5.3).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn free_needs_finished_and_no_waiting_or_draft() {
    let v = |w, f, d| Verdict { waiting: w, finished: f, draft: d };
    assert!(v(0.1, 0.9, 0.0).is_free());
    assert!(v(0.2, 0.8, 0.2).is_free());
    assert!(!v(0.3, 0.9, 0.0).is_free());
    assert!(!v(0.1, 0.7, 0.0).is_free());
    assert!(!v(0.1, 0.9, 0.5).is_free());
}
#[test]
fn the_request_asks_three_noul_questions() {
    let b = request_body("screen");
    assert_eq!(b["model"], "jev-latest");
    assert_eq!(b["state"], "screen");
    for id in ["waiting", "finished", "draft"] {
        assert_eq!(b["questions"][id]["type"], "noul");
    }
}
#[test]
fn a_reply_is_read_and_the_key_sent() { /* tiny_http server on 127.0.0.1:0: assert the request's Authorization is "Bearer k"; reply {"answers":{"waiting":{"type":"noul","noul":0.1},"finished":{"type":"noul","noul":0.9},"draft":{"type":"noul","noul":0.05}}}; ask(url,"k","s") == Verdict{0.1,0.9,0.05} */ }
#[test]
fn a_bad_reply_is_an_error() { /* server replies {"answers":{}} → ask is Err; HTTP 422 → Err */ }
#[test]
fn a_timeout_is_an_error() { /* server accepts and never answers → ask returns Err within ~6 s */ }
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test -p hydraui jev::`.
- [ ] **Step 3: Implement** with a `ureq::AgentBuilder` (`timeout_connect`/`timeout_read` 5 s), `send_string(&request_body(state).to_string())`, parse with `serde_json`; any missing `noul` number → error. Never include the key in an error message.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: hydraui asks Jev whether a Claude session is free`

---

### Task 3: Per-terminal readiness (drafts, fresh, just verified)

**Files:**
- Create: `crates/hydraui/src/readiness.rs` (+ `pub mod readiness;` in lib.rs)
- Modify: `crates/hydraui/src/pane_view.rs`, `crates/hydraui/src/mcp/server.rs`, `crates/hydraui/src/mcp/tools.rs`, `crates/hydraui/src/workspace.rs` (pump), `crates/hydraui/src/workspace/busy.rs`

**Interfaces:**
- Produces in `readiness.rs`:
  ```rust
  pub enum Draft { Clear, Typed, MaybeCleared }
  pub struct Readiness { pub draft: Draft, pub finished: bool, last_escape: Option<Instant> }
  impl Readiness {
      pub fn typed(&mut self);                         // printable/edit key, IME text, paste
      pub fn enter(&mut self);                         // unmodified Enter: draft Clear
      pub fn escape(&mut self, now: Instant);          // second Esc within 500 ms → MaybeCleared (only if Typed)
      pub fn ctrl_c(&mut self);                        // Typed → MaybeCleared
      pub fn state(&mut self, before: Option<SessionState>, after: SessionState);
          // after==Idle && before==None → finished=true (fresh), draft=Clear
          // before==Some(Idle) && after==Working → draft=Clear, finished=false
          // after==Ended → reset to default
      pub fn verified(&mut self);                      // finished=true
      pub fn screen_says_empty(&mut self, empty: Option<bool>); // MaybeCleared + Some(true) → Clear
      pub fn has_draft(&self) -> bool;                 // Typed | MaybeCleared
  }
  ```
  Default: `draft: Clear, finished: false`.
- Produces: `pub fn claude_input_empty(rows: &[Vec<(char, bool)>]) -> Option<bool>` in `readiness.rs`: `(char, dim)` cells; finds Claude's input line (the last row whose first non-space char is `>` or `❯`, between two rows made of `─`), `Some(true)` when every non-dim char after the prompt char is a space, `Some(false)` when not, `None` when no such line.
- Produces on `PaneView`: `pub fn readiness(&self) -> &Readiness`, `pub fn verified(&mut self)`, `pub fn screen_text(&self, rows: usize) -> String` (last `rows` rows incl. scrollback, trailing spaces trimmed, `\n`-joined), `pub fn type_prompt(&mut self, text: &str, cx)` (Task 5 uses it; does not touch `Readiness`), and an internal refresh of `MaybeCleared` via `claude_input_empty` when `readiness()` is read through `pub fn check_draft(&mut self) -> bool` (returns `has_draft()` after the screen check).
- Produces: `McpEvent::Verified(String /* token */)`, emitted by `answer` when `tasks_verify` succeeds (add `pub verified: bool` to `Outcome`).

- [ ] **Step 1: Write the failing tests** (`readiness.rs`)

```rust
#[test] fn typing_sets_a_draft_and_enter_clears_it()
#[test] fn one_escape_keeps_the_draft_two_quick_ones_may_clear_it()   // 2nd Esc at +300 ms → MaybeCleared; at +900 ms → still Typed
#[test] fn maybe_cleared_needs_the_screen_to_say_empty()              // screen_says_empty(None) and Some(false) keep it; Some(true) clears
#[test] fn a_prompt_starting_clears_the_draft_and_finished()          // state(Some(Idle), Working)
#[test] fn stop_and_tool_use_keep_type_ahead()                        // typed(); state(Some(Working), Working) and state(Some(Working), Idle) keep Typed
#[test] fn a_new_session_is_fresh()                                   // state(None, Idle) → finished
#[test] fn verify_marks_finished_until_the_next_prompt()
#[test] fn the_input_line_is_read_from_the_screen()                   // rows: "────", "> ", "────" → Some(true); "> hello" → Some(false); "> " + dim placeholder "Try \"x\"" → Some(true); no prompt row → None
```
In `mcp/server.rs` tests: a successful `tasks_verify` call returns `Some(McpEvent::Verified(token))`; a failing one (unknown commit) returns `None`.

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** In `PaneView`: call `typed()` in `on_key_down` for keys that produce bytes other than unmodified `enter`, `escape` and `ctrl-c` (those call `enter()`, `escape(Instant::now())`, `ctrl_c()`), in `replace_text_in_range` and `paste_text`; not in mouse, focus or `insert_text`/`type_prompt` paths. In `set_claude`, call `readiness.state(self.claude, state)` before updating. `screen_text` and `claude_input_empty` read cells via `pane.with_term` (`Flags::DIM`). In the workspace pump, `McpEvent::Verified(token)` finds the pane by `mcp_token()` and calls `verified()`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: hydraui tracks each Claude terminal's draft and whether it has finished`

---

### Task 4: Board events for Pickup arrivals and send menu items

**Files:**
- Modify: `crates/hydraui/src/task_board.rs`, `crates/hydraui/src/task_board_view.rs`

**Interfaces:**
- Produces: `TaskBoardEvent::ArrivedInPickup(String /* card text */)`, emitted by `TaskBoard::change` after a written `Change::Move { to: Column::Pickup, .. }` whose `card.column != Column::Pickup`. Never from `reload`.
- Produces: `pub struct SendTarget { pub pane: PaneId, pub label: String }`; `TaskBoardView::set_send_targets(&mut self, f: impl Fn(&App) -> Vec<SendTarget> + 'static)`.
- Produces: `TaskBoardViewEvent::SendToFree(String)` and `TaskBoardViewEvent::SendTo { text: String, pane: PaneId }`.

- [ ] **Step 1: Write the failing tests** (`task_board.rs`, gpui `TestAppContext` as existing board tests do, on a temp project)

```rust
#[gpui::test] fn moving_a_card_into_pickup_says_it_arrived()   // Backlog→Pickup emits ArrivedInPickup("idea")
#[gpui::test] fn moves_within_pickup_or_elsewhere_say_nothing() // Pickup reorder, Backlog→Done: no ArrivedInPickup
#[gpui::test] fn a_reload_never_says_a_card_arrived()           // write the file with a new Pickup card; reload; no event
```
(If the existing tests don't use `TestAppContext`, extract `fn arrived(change: &Change) -> Option<String>` and test that instead.)

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** In the Pickup branch of the card context menu (beside "Take in new worktree", shown in and out of repos): "Send to a free session" emits `SendToFree(text)`; a "Send to" submenu built from the targets closure, each item emitting `SendTo { text, pane }`, disabled item "No idle Claude session" when empty.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: task board reports cards moved into Pickup and offers sending them`

---

### Task 5: Picking a session and sending the card

**Files:**
- Create: `crates/hydraui/src/pickup.rs` (pure), `crates/hydraui/src/workspace/pickup.rs` (wiring; `mod pickup;` in workspace.rs)
- Modify: `crates/hydraui/src/workspace/tasks_tab.rs` (handle the Task 4 events; call `set_send_targets` in `new_board_view`), `crates/hydraui/src/workspace/busy.rs` (clear reservation on any hook state)

**Interfaces:**
- Consumes: `JevKey`, `jev::ask`, `JEV_URL`, `Verdict::is_free` (Tasks 1–2); `PaneView::{readiness, check_draft, screen_text, type_prompt, claude, cwd}` (Task 3); Task 4 events.
- Produces in `pickup.rs`:
  ```rust
  pub struct Session { pub pane: PaneId, pub project_root: Option<PathBuf> /* git_root(cwd) */, pub cwd: PathBuf,
                       pub claude: Option<SessionState>, pub focused: bool, pub reserved: bool,
                       pub draft: bool, pub finished: bool }
  pub enum Pick { Certain(Vec<PaneId>), AskJev(Vec<PaneId>) }
  pub fn idle_in_project(sessions: &[Session], project: &Path) -> Vec<PaneId>;  // facts 1–3 (Send to submenu)
  pub fn pick(sessions: &[Session], project: &Path, jev: bool) -> Pick;      // facts 1–6, then §5.2/§5.3/§5.4
  pub fn prompt(card: &str) -> String;
  ```
  `pick`: candidates pass all facts; if any has `finished` → `Certain(those)`; else if `jev` → `AskJev(the rest)`; else `Certain(vec![])`.
- Produces on `Workspace`: `fn offer_card(&mut self, project: PathBuf, text: String, window, cx)` (auto and "Send to a free session"), `fn send_card_to(&mut self, pane: PaneId, project: PathBuf, text: String, window, cx)` ("Send to"), reservations `HashMap<PaneId, Instant>`.

- [ ] **Step 1: Write the failing tests** (`pickup.rs`)

```rust
#[test] fn finished_sessions_are_picked_without_jev()
#[test] fn uncertain_sessions_go_to_jev_only_when_there_is_a_key()   // jev=false → Certain([]); jev=true → AskJev([..])
#[test] fn focused_terminal_is_never_a_candidate()
#[test] fn a_reserved_terminal_is_not_a_candidate()
#[test] fn drafts_working_and_waiting_sessions_are_skipped()
#[test] fn linked_worktrees_and_other_projects_are_skipped()        // project_root != project → skipped; outside git: cwd == project counts
#[test] fn prompt_is_one_line_and_quotes_the_card()                 // "a\nb \"c\"" → contains "a b \"c\"", no '\n'; >120 chars truncated with "…"
#[test] fn send_needs_the_card_still_in_pickup()                    // pure helper still_in_pickup(board, text) -> bool
```
Prompt copy: `A card was moved to Pickup: "<text>". Take it with tasks_take and do it.`

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
  - `offer_card`: build `Session`s from `self.panes` (`focused` = shown terminal of the active env while `window.is_window_active()`, `draft` via `check_draft`), call `pick` with `jev = JevKey::app().get()` is `Ok(Some)`. `Certain(v)` non-empty → random pick (`getrandom`) → send. `AskJev(v)` → for each, `screen_text(60)`, then `cx.background_spawn` all `jev::ask` calls in parallel, back on the UI thread keep `is_free` ones, re-check facts (`pick` again) and that the card is still in Pickup (`still_in_pickup`), random pick → send. Nothing free → warning toast "No free Claude session for this card" (with the project's folder name when the window has terminals in more than one project), auto-hides.
  - send (shared by both paths): `type_prompt` writes the prompt wrapped in bracketed paste (`\x1b[200~`…`\x1b[201~`) when the terminal's mode has it, else plain, then after 150 ms a separate `\r`; reserve the pane; info toast "Sent to <label> in <env>" that shows the terminal on click (as `alert` does in busy.rs).
  - Reservations expire after 30 s and are removed in `session_state` for that pane.
  - `tasks_tab.rs`: `ArrivedInPickup(text)` → `offer_card`; `SendToFree(text)` → `offer_card`; `SendTo{text,pane}` → `send_card_to`. `set_send_targets` closure: `idle_in_project` mapped to `SendTarget { pane, label: "<label> (<env>)" }`.
- [ ] **Step 4: Run, expect PASS** — `cargo test -p hydraui`; clippy clean.
- [ ] **Step 5: Commit** — `feat: a card moved into Pickup goes to a free Claude session`

---

### Task 6: Hands-on check and docs

**Files:**
- Modify: `docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md` (§5 menu: Send to a free session, Send to), `site/app.html` (task board section: auto-pickup; launcher: Jev key), `crates/hydraui/src/mcp/tools.rs` `INSTRUCTIONS` only if the hands-on check shows Claude needs a hint for typed card prompts.

- [ ] **Step 1:** Debug build; in a scratch project with two Claude terminals: one fresh, one that just answered a question with "Should I go ahead?". Move a card to Pickup → it goes to the fresh one and Claude calls `tasks_take`. Move a second → toast "No free Claude session for this card" (without Jev).
- [ ] **Step 2:** With a Jev key set: a session idle after plain finished work gets the card; the one that asked a question doesn't; one with a typed draft doesn't. Record the three Jev verdicts; tune thresholds only if real screens fall on the wrong side.
- [ ] **Step 3:** Confirm the prompt submits (no stuck multi-line paste) and the Jev key appears nowhere in `env`/`set` output of a terminal.
- [ ] **Step 4:** Update the docs listed above.
- [ ] **Step 5: Commit** — `docs: auto-pickup and the Jev key`
