# hydraui MCP Server (tasks) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each hydraui process hosts one localhost MCP server that Claude sessions in its terminals find by themselves, with four tools for the calling terminal's project task board.

**Architecture:** The board model learns a card's *taker* (`- [name] text` in InProgress/Done) and a `Take` change; board writes take a cross-process lock. A new `hydraui::mcp` module holds a token→terminal registry, the tool logic (pure, over `tasks_project`), and a tiny_http JSON-RPC server on `127.0.0.1:0`. Each terminal gets `HYDRA_MCP_URL`/`HYDRA_MCP_TOKEN`; hydra registers a stdio server `hydra mcp` in every `[claude]` environment, which relays to that URL (and serves no tools when the variables are unset), and pre-approves the four tools.

**Tech Stack:** Rust 2024 (rustc 1.98, MSRV 1.89), gpui, serde_json, `tiny_http` 0.12 (new), `getrandom` 0.3 (new), `ureq` 2 (workspace; new to hydra-cli and as hydraui dev-dep).

**Spec:** `docs/superpowers/specs/2026-10-20-hydraui-mcp-server-design.md` (builds on `docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md`).

## Finding that fixes the registration (spec §3, checked 2026-10-20)

`${VAR}` expansion works in a user-scope `http` entry when the variables are set, but with them
unset `claude mcp list` prints `[Warning] [hydraui] mcpServers.hydraui: Missing environment
variables: HYDRA_MCP_URL, HYDRA_MCP_TOKEN` and the server shows as failed. That is the noise the
spec's anti-goal rules out, so this plan takes the spec's fallback: a stdio bridge, `hydra mcp`.
The registered entry is:

```json
"hydraui": { "type": "stdio", "command": "hydra", "args": ["mcp"] }
```

`command` is the bare `hydra` (found on PATH, as hydraui itself finds it): `materialise` also runs
inside hydraui.exe, where `current_exe` isn't hydra. (Our call.)

## Global Constraints

- Server binds `127.0.0.1` port 0; path `/mcp`; plain JSON responses, no SSE.
- `Authorization: Bearer <token>` required; unknown/missing → 401. `Origin` present and not `http://127.0.0.1[:port]` / `http://localhost[:port]` → 403.
- Variables: `HYDRA_MCP_URL` = `http://127.0.0.1:<port>/mcp`, `HYDRA_MCP_TOKEN` = ≥128 random bits, hex.
- MCP server name `hydraui`; tool names exactly `tasks_read`, `tasks_add`, `tasks_take`, `tasks_mine`.
- Allow rules exactly `mcp__hydraui__tasks_read`, `mcp__hydraui__tasks_add`, `mcp__hydraui__tasks_take`, `mcp__hydraui__tasks_mine`, by name (never `mcp__hydraui` or a wildcard).
- Only environments with `[claude]` get the entry; hydra never writes `~/.claude.json`.
- Sessions can't move into Pickup or Done, move out of InProgress, edit, reorder or delete. No tool exists for those.
- Taker marker `[name] ` is read only in InProgress and Done; written `- [<taker>] <text>`.
- A terminal's name: user title if set, else `label_for(None, start cwd)`; unique among live terminals in the process (`hydra`, `hydra-2`, …); `]` → `)`, whitespace runs → one space; follows renames, not `cd`.
- Board lock: exclusive lock held across re-read/apply/write in `tasks_project::commit`, shared by tab and server, in the temp folder (never inside the project).
- Comments and doc comments match the surrounding code: short, plain, referencing spec sections as `§n`.

## Review Focus

1. A Claude-supplied card text that differs from the file only by wrapping/whitespace (Claude copies a wrapped card) must still be taken: `Take` matches on `normalise`d text. Test in Task 1.
2. A hand-written Backlog card starting `[WIP] ` stays text, and an InProgress card `- [ ] foo` is a checkbox, not taker ` `. Test in Task 1.
3. `tasks_read`/`tasks_mine` on a project with no `.hydra/tasks.md` return an empty board and create nothing; `tasks_take` there is an error and creates nothing. Test in Task 4.
4. Requests with no token, a revoked token (terminal closed), or a browser `Origin` are refused; an unknown tool or bad arguments return a JSON-RPC/tool error, never panic the server thread. Tests in Task 5.
5. A user's own `permissions.allow` / `deny` in settings.json are kept and our four rules are added once, also when `materialise` runs repeatedly. Test in Task 7.

---

### Task 1: Board model: takers and Take

**Files:**
- Modify: `crates/hydraui/src/tasks.rs` (model, parse, write, apply, tests)
- Modify: `crates/hydraui/src/task_board.rs`, `crates/hydraui/src/task_board_view.rs`, `crates/hydraui/src/tasks_project.rs` (callers and tests, to `Card`)

**Interfaces:**
- Produces:
  - `pub struct Card { pub text: String, pub taker: Option<String> }` (`Debug, Clone, PartialEq, Eq`), `Card::new(text: impl Into<String>) -> Card` (no taker).
  - `Board.columns: [Vec<Card>; 4]`; `Board::cards(&self, Column) -> &[Card]`. `card_ref` / `find` compare `card.text` only (identity unchanged, task board §6).
  - `Change::Take { text: String, taker: String }`: the first Pickup card whose text equals `normalise(text)` moves to index 0 of InProgress with `taker: Some(taker)`; none → `Err(Missing)`.
  - `Change::Move` to Backlog or Pickup sets `taker = None`; to InProgress or Done keeps it. `Change::Edit` keeps it.

- [ ] **Step 1: Write the failing tests** in `tasks.rs`'s test module:

```rust
#[test]
fn takers_are_read_in_inprogress_and_done_only() {
    let b = parse("## Backlog\n- [WIP] still text\n## Pickup\n- [x] boxed\n## InProgress\n- [claude] doing it\n- [ ] boxed too\n## Done\n- [api-2] did it\n");
    assert_eq!(b.cards(Column::Backlog), [Card::new("[WIP] still text")]);
    assert_eq!(b.cards(Column::Pickup), [Card::new("boxed")]);
    assert_eq!(b.cards(Column::InProgress)[0], Card { text: "doing it".into(), taker: Some("claude".into()) });
    assert_eq!(b.cards(Column::InProgress)[1], Card::new("boxed too"));
    assert_eq!(b.cards(Column::Done)[0].taker.as_deref(), Some("api-2"));
}

#[test]
fn takers_round_trip() {
    let text = "# Tasks\n\n## Backlog\n\n## Pickup\n\n## InProgress\n- [my term] doing it\n\n## Done\n";
    assert_eq!(write(&parse(text)), text);
}

#[test]
fn take_moves_the_first_match_to_the_top_of_inprogress() {
    let mut b = parse("## Pickup\n- a\n- the  card\n  wrapped\n- the card wrapped\n## InProgress\n- [x1] old\n");
    apply(&mut b, &Change::Take { text: "the card\nwrapped ".into(), taker: "claude".into() }).unwrap();
    assert_eq!(b.cards(Column::Pickup), [Card::new("a"), Card::new("the card wrapped")]);
    assert_eq!(b.cards(Column::InProgress)[0], Card { text: "the card wrapped".into(), taker: Some("claude".into()) });
    assert_eq!(apply(&mut b, &Change::Take { text: "gone".into(), taker: "c".into() }), Err(Missing));
}

#[test]
fn moving_back_drops_the_taker_and_done_keeps_it() {
    let mut b = parse("## InProgress\n- [c] one\n- [c] two\n");
    let one = b.card_ref(Column::InProgress, 0).unwrap();
    apply(&mut b, &Change::Move { card: one, to: Column::Pickup, index: 0 }).unwrap();
    assert_eq!(b.cards(Column::Pickup), [Card::new("one")]);
    let two = b.card_ref(Column::InProgress, 0).unwrap();
    assert_eq!(two.text, "two");
    apply(&mut b, &Change::Move { card: two, to: Column::Done, index: 0 }).unwrap();
    assert_eq!(b.cards(Column::Done)[0].taker.as_deref(), Some("c"));
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p hydraui tasks::tests`
Expected: compile errors (`Card`, `Change::Take` not defined).

- [ ] **Step 3: Implement**

In `tasks.rs`: `Card`, the column type change, `Change::Take`, and the move/edit rules above. Parsing: `flush` (which knows the column) splits a leading `[name] ` off the normalised text for InProgress/Done via `fn split_taker(text: &str) -> (Option<String>, &str)`: name is up to the first `]`, non-empty, not `" "`, `"x"`, `"X"`, and must be followed by a space and non-empty text; otherwise the whole text is the card. (Checkboxes are already stripped before this, so `[ ] boxed too` never reaches it.) Writing: `write_card(out, &format!("[{taker}] {text}"))` when there is a taker. Update every caller in `task_board.rs`, `task_board_view.rs` and `tasks_project.rs` to use `card.text` (tests compare against `Card::new(..)` or map to text).

- [ ] **Step 4: Run all hydraui tests**

Run: `cargo test -p hydraui`
Expected: all pass (the 284 existing plus the 4 new).

- [ ] **Step 5: Commit**

```bash
git add crates/hydraui/src/tasks.rs crates/hydraui/src/task_board.rs crates/hydraui/src/task_board_view.rs crates/hydraui/src/tasks_project.rs
git commit -m "feat: hydraui's task board records who took a card"
```

### Task 2: Board lock

**Files:**
- Modify: `crates/hydraui/src/tasks_project.rs`

**Interfaces:**
- Produces: `pub fn lock_path(project: &Path) -> PathBuf`: `std::env::temp_dir().join("hydraui-locks").join(<hex sha256 of the project path, lowercased, `/`→`\`>).lock`. `commit` holds an exclusive `std::fs::File::lock()` on it (created as needed) from before the re-read until after the write. Public behaviour of `commit` is otherwise unchanged.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn commits_from_many_threads_lose_nothing() {
    let d = tempfile::tempdir().unwrap();
    let project = d.path().to_path_buf();
    let threads: Vec<_> = (0..8).map(|i| {
        let p = project.clone();
        std::thread::spawn(move || {
            for j in 0..10 {
                commit(&p, &Change::Add { text: format!("t{i}-{j}") }).unwrap();
            }
        })
    }).collect();
    threads.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(load(&project).unwrap().0.cards(Column::Backlog).len(), 80);
}

#[test]
fn the_lock_lives_outside_the_project() {
    let d = tempfile::tempdir().unwrap();
    assert!(!lock_path(d.path()).starts_with(d.path()));
    assert_eq!(lock_path(d.path()), lock_path(d.path()));
    assert_ne!(lock_path(d.path()), lock_path(&d.path().join("other")));
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p hydraui tasks_project::tests::commits_from_many_threads_lose_nothing`
Expected: FAIL (fewer than 80 cards, or a write error from the racing renames).

- [ ] **Step 3: Implement** `lock_path` and the lock in `commit` (`File::lock` is stable since 1.89; the guard is the open `File`, dropped at the end of `commit`).

- [ ] **Step 4: Run** `cargo test -p hydraui tasks_project` and expect PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hydraui/src/tasks_project.rs
git commit -m "fix: hydraui's task board writes hold a lock so two writers can't lose a card"
```

### Task 3: Taker badge in the Tasks tab

**Files:**
- Modify: `crates/hydraui/src/task_board_view.rs` (`render_card`)

**Interfaces:**
- Consumes: `Card.taker` (Task 1).

- [ ] **Step 1: Render** a taken card's taker as a small muted badge above or beside its text (look: builder's call; reuse gpui-component's `Tag`/badge style if the board already uses one, else a small rounded `div` in `theme.muted`/`muted_foreground`). The editor still edits only `card.text`.
- [ ] **Step 2: Build and test**: `cargo test -p hydraui` passes and `cargo clippy -p hydraui --all-targets` is clean.
- [ ] **Step 3: Check by running**: put `- [claude] try me` under `## InProgress` in a project's `.hydra/tasks.md`, open its Tasks tab: the card shows text `try me` with a `claude` badge; drag it to Pickup: the badge goes and the file line reads `- try me`.
- [ ] **Step 4: Commit**

```bash
git add crates/hydraui/src/task_board_view.rs
git commit -m "feat: hydraui's task board shows who took a card"
```

### Task 4: MCP registry and tools

**Files:**
- Create: `crates/hydraui/src/mcp/mod.rs`, `crates/hydraui/src/mcp/registry.rs`, `crates/hydraui/src/mcp/tools.rs`
- Modify: `crates/hydraui/src/lib.rs` (`pub mod mcp;`), `crates/hydraui/Cargo.toml` (`getrandom = "0.3"`), root `Cargo.toml` if deps are workspace-declared (they are: add `getrandom = "0.3"` and `tiny_http = "0.12"` under `[workspace.dependencies]`)

**Interfaces:**
- Produces (`registry.rs`):
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct Terminal { pub name: String, pub cwd: PathBuf }`
  - `#[derive(Default)] pub struct Registry` with `register(&mut self, label: &str, cwd: PathBuf) -> String` (returns a new 32-hex-char token from `getrandom`), `rename(&mut self, token: &str, label: &str)`, `set_cwd(&mut self, token: &str, cwd: PathBuf)`, `revoke(&mut self, token: &str)`, `get(&self, token: &str) -> Option<Terminal>`.
  - `pub fn clean_name(label: &str) -> String`: whitespace runs → one space, trimmed, `]` → `)`, empty → `terminal`.
  - Names unique among registered terminals: `register`/`rename` use `clean_name(label)`, and if another token holds that name, the first free of `name-2`, `name-3`, ….
- Produces (`tools.rs`):
  - `pub const INSTRUCTIONS: &str`: the workflow, spec §5 "What the instructions and descriptions tell Claude", every point.
  - `pub fn list() -> serde_json::Value`: the `tools` array: each `{name, description, inputSchema}`; `tasks_add` and `tasks_take` take `{ "text": string }` (required), the others `{}`. Each description states its part of the workflow (e.g. `tasks_take`: take one approved card from Pickup before starting it; leave it in InProgress when done, the user moves it to Done).
  - `pub struct Outcome { pub text: String, pub is_error: bool, pub created: Option<PathBuf> }` (`created`: the project whose board file this call created).
  - `pub fn call(terminal: &Terminal, name: &str, args: &serde_json::Value) -> Option<Outcome>`: `None` for an unknown tool name. Project = `tasks_project::project_root(&terminal.cwd)`.
    - `tasks_read` → `is_error: false`, text = pretty JSON `{"you": name, "project": "<path>", "columns": {"Backlog": [{"text": ..}], "Pickup": [..], "InProgress": [{"text": .., "taker": ..}], "Done": [..]}}` (`taker` only when set).
    - `tasks_add {text}` → `commit(Change::Add)`; empty after `normalise` → `is_error: true` "text is empty".
    - `tasks_take {text}` → `commit(Change::Take { text, taker: name })`; `Missing` → `is_error: true`, text says no Pickup card has that text (taken, moved or edited) and to call `tasks_read`.
    - `tasks_mine` → JSON array of the texts of InProgress cards whose taker == `terminal.name`.
    - Missing/non-string `text` → `is_error: true` naming the argument. I/O errors → `is_error: true` with the error.
- Produces (`mod.rs`): `pub mod registry; pub mod tools; pub mod server;` (server in Task 5).

- [ ] **Step 1: Write the failing tests**

```rust
// registry.rs
#[test]
fn names_are_unique_clean_and_follow_renames() {
    let mut r = Registry::default();
    let a = r.register("hydra", "C:\\w".into());
    let b = r.register("hydra", "C:\\w".into());
    assert_eq!(r.get(&b).unwrap().name, "hydra-2");
    let c = r.register(" odd ] \n name ", "C:\\w".into());
    assert_eq!(r.get(&c).unwrap().name, "odd ) name");
    r.revoke(&a);
    assert!(r.get(&a).is_none());
    assert_eq!(r.get(&r.register("hydra", "C:\\w".into())).unwrap().name, "hydra");
    r.rename(&b, "api");
    assert_eq!(r.get(&b).unwrap().name, "api");
    assert_eq!(a.len(), 32);
    assert_ne!(a, b);
}

// tools.rs
#[test]
fn a_session_adds_takes_and_finds_its_cards() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join(".git")).unwrap();
    let t = Terminal { name: "claude".into(), cwd: d.path().join("src") };
    std::fs::create_dir(&t.cwd).unwrap();
    std::fs::create_dir(d.path().join(".hydra")).unwrap();
    std::fs::write(d.path().join(".hydra/tasks.md"), "## Pickup\n- do the thing\n").unwrap();
    let take = call(&t, "tasks_take", &json!({"text": "do the thing"})).unwrap();
    assert!(!take.is_error);
    assert!(call(&t, "tasks_take", &json!({"text": "do the thing"})).unwrap().is_error);
    assert!(!call(&t, "tasks_add", &json!({"text": "follow-up"})).unwrap().is_error);
    let mine: serde_json::Value = serde_json::from_str(&call(&t, "tasks_mine", &json!({})).unwrap().text).unwrap();
    assert_eq!(mine, json!(["do the thing"]));
    let file = std::fs::read_to_string(d.path().join(".hydra/tasks.md")).unwrap();
    assert!(file.contains("## Backlog\n- follow-up\n"));
    assert!(file.contains("## InProgress\n- [claude] do the thing\n"));
}

#[test]
fn reading_a_missing_board_creates_nothing() {
    let d = tempfile::tempdir().unwrap();
    let t = Terminal { name: "c".into(), cwd: d.path().into() };
    let read = call(&t, "tasks_read", &json!({})).unwrap();
    assert!(!read.is_error && read.text.contains("\"you\": \"c\""));
    assert!(call(&t, "tasks_take", &json!({"text": "x"})).unwrap().is_error);
    assert!(!call(&t, "tasks_mine", &json!({})).unwrap().is_error);
    assert!(!d.path().join(".hydra").exists());
}

#[test]
fn bad_calls_are_errors_not_panics() {
    let d = tempfile::tempdir().unwrap();
    let t = Terminal { name: "c".into(), cwd: d.path().into() };
    assert!(call(&t, "tasks_move", &json!({})).is_none());
    assert!(call(&t, "tasks_add", &json!({"text": 5})).unwrap().is_error);
    assert!(call(&t, "tasks_add", &json!({"text": "  "})).unwrap().is_error);
}

#[test]
fn the_list_offers_exactly_the_four_tools() {
    let names: Vec<_> = list().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["tasks_read", "tasks_add", "tasks_take", "tasks_mine"]);
}

#[test]
fn creating_the_board_is_reported() {
    let d = tempfile::tempdir().unwrap();
    let t = Terminal { name: "c".into(), cwd: d.path().into() };
    assert_eq!(call(&t, "tasks_add", &json!({"text": "x"})).unwrap().created.as_deref(), Some(d.path()));
    assert_eq!(call(&t, "tasks_add", &json!({"text": "y"})).unwrap().created, None);
}
```

- [ ] **Step 2: Run** `cargo test -p hydraui mcp::` and expect compile failure (module missing).
- [ ] **Step 3: Implement** `registry.rs`, `tools.rs`, `mod.rs` per the Interfaces block.
- [ ] **Step 4: Run** `cargo test -p hydraui mcp::` and expect PASS.
- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hydraui/Cargo.toml crates/hydraui/src/lib.rs crates/hydraui/src/mcp
git commit -m "feat: hydraui's MCP tools for a terminal's task board"
```

### Task 5: The HTTP server

**Files:**
- Create: `crates/hydraui/src/mcp/server.rs`
- Modify: `crates/hydraui/Cargo.toml` (`tiny_http.workspace = true`; dev-dep `ureq.workspace = true`)

**Interfaces:**
- Consumes: `Registry`, `tools::{INSTRUCTIONS, list, call, Outcome}` (Task 4).
- Produces:
  - `pub enum McpEvent { BoardCreated(PathBuf) }`
  - `pub struct Server { url: String, registry: Arc<Mutex<Registry>>, events: Mutex<Vec<futures::channel::mpsc::UnboundedSender<McpEvent>>> }` with `pub fn start() -> std::io::Result<Arc<Server>>` (binds `127.0.0.1:0`, serves on a named background thread), `url(&self) -> &str`, `registry(&self) -> &Mutex<Registry>`, `subscribe(&self) -> UnboundedReceiver<McpEvent>`.
  - Global: `pub fn init() -> Option<&'static Arc<Server>>` (starts once into a `OnceLock`; logs nothing, returns `None` if binding fails) and `pub fn get() -> Option<&'static Arc<Server>>`.
  - Pure core for tests: `pub struct Reply { pub status: u16, pub body: Option<String> }` and `pub fn handle(registry: &Mutex<Registry>, method: &str, path: &str, auth: Option<&str>, origin: Option<&str>, body: &str) -> (Reply, Option<McpEvent>)`.
- Rules for `handle`, in order: path ≠ `/mcp` → 404; `origin` present and not `http://127.0.0.1` / `http://localhost` (optionally `:port`) → 403; auth not `Bearer <registered token>` → 401; method `GET`/`DELETE` → 405; not POST → 405. Body: one JSON-RPC message (an array → JSON-RPC error `-32600`; unparsable → `-32700`). No `id` (notification) → 202, no body. `initialize` → `{"protocolVersion": <the client's params.protocolVersion>, "capabilities": {"tools": {}}, "serverInfo": {"name": "hydraui", "version": env!("CARGO_PKG_VERSION")}, "instructions": INSTRUCTIONS}`; `ping` → `{}`; `tools/list` → `{"tools": list()}`; `tools/call` → `{"content": [{"type": "text", "text": ..}], "isError": ..}` from `call` with the token's `Terminal` (unknown tool → JSON-RPC error `-32602`); other methods → `-32601`. 200 replies carry `Content-Type: application/json`. A `created` outcome yields `McpEvent::BoardCreated(project)`, which the serving thread sends to every subscriber (dropping closed ones).

- [ ] **Step 1: Write the failing tests** (unit, on `handle`):

```rust
#[test]
fn refuses_strangers() {
    let reg = Mutex::new(Registry::default());
    let tok = reg.lock().unwrap().register("c", std::env::temp_dir());
    let ok = format!("Bearer {tok}");
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    assert_eq!(handle(&reg, "POST", "/mcp", None, None, init).0.status, 401);
    assert_eq!(handle(&reg, "POST", "/mcp", Some("Bearer nope"), None, init).0.status, 401);
    assert_eq!(handle(&reg, "POST", "/mcp", Some(&ok), Some("https://evil.example"), init).0.status, 403);
    assert_eq!(handle(&reg, "POST", "/mcp", Some(&ok), Some("http://localhost:5173"), init).0.status, 200);
    assert_eq!(handle(&reg, "POST", "/other", Some(&ok), None, init).0.status, 404);
    assert_eq!(handle(&reg, "GET", "/mcp", Some(&ok), None, "").0.status, 405);
    reg.lock().unwrap().revoke(&tok);
    assert_eq!(handle(&reg, "POST", "/mcp", Some(&ok), None, init).0.status, 401);
}

#[test]
fn speaks_json_rpc() {
    let reg = Mutex::new(Registry::default());
    let tok = format!("Bearer {}", reg.lock().unwrap().register("c", std::env::temp_dir()));
    let rpc = |body: &str| handle(&reg, "POST", "/mcp", Some(&tok), None, body).0;
    let init: Value = serde_json::from_str(&rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#).body.unwrap()).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("Pickup"));
    assert_eq!(rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).status, 202);
    let list: Value = serde_json::from_str(&rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).body.unwrap()).unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 4);
    let bad: Value = serde_json::from_str(&rpc(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tasks_delete","arguments":{}}}"#).body.unwrap()).unwrap();
    assert_eq!(bad["error"]["code"], -32602);
    let junk: Value = serde_json::from_str(&rpc("{not json").body.unwrap()).unwrap();
    assert_eq!(junk["error"]["code"], -32700);
}

#[test]
fn serves_over_http() {
    let server = Server::start().unwrap();
    let tok = server.registry().lock().unwrap().register("c", std::env::temp_dir());
    let resp = ureq::post(server.url())
        .set("Authorization", &format!("Bearer {tok}"))
        .set("Content-Type", "application/json")
        .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.into_string().unwrap().contains("tasks_take"));
    assert!(server.url().starts_with("http://127.0.0.1:") && server.url().ends_with("/mcp"));
}
```

- [ ] **Step 2: Run** `cargo test -p hydraui mcp::server` and expect compile failure.
- [ ] **Step 3: Implement** `server.rs` per the Interfaces block. One serving thread is enough (requests are short); a panic inside a handler must not end the thread (`catch_unwind` around `handle`, replying 500).
- [ ] **Step 4: Run** `cargo test -p hydraui mcp::` and expect PASS.
- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/hydraui/Cargo.toml crates/hydraui/src/mcp/server.rs crates/hydraui/src/mcp/mod.rs
git commit -m "feat: hydraui serves its MCP tools on localhost"
```

### Task 6: hydraui wiring: terminals get the server

**Files:**
- Modify: `crates/hydraui/src/main.rs` (start the server), `crates/hydraui/src/hydra.rs` (`PaneRequest.extra_env`, `Hydra::start`), `crates/hydraui/src/pane_view.rs` (register, cwd, rename, revoke), `crates/hydraui/src/workspace/tasks_tab.rs` and `crates/hydraui/src/workspace.rs` (BoardCreated → commit/private question), `crates/hydraui/src/mcp/mod.rs` (`McpTerminal`)

**Interfaces:**
- Consumes: `mcp::server::{init, get, McpEvent}`, `Registry` (Tasks 4–5).
- Produces:
  - `PaneRequest.extra_env: Vec<(String, String)>` (empty in `PaneRequest::new`); `Hydra::start` appends it to `inherited` before `Pane::spawn`, so the shell and everything it runs inherit it.
  - `pub struct McpTerminal { token: String }` in `mcp/mod.rs`: `McpTerminal::register(label: &str, cwd: PathBuf) -> Option<McpTerminal>` (`None` when no server), `env(&self) -> Vec<(String, String)>` (`HYDRA_MCP_URL`, `HYDRA_MCP_TOKEN`), `set_cwd(&self, PathBuf)`, `rename(&self, &str)`; `Drop` revokes the token.
  - `PaneView` holds `mcp: Option<McpTerminal>`, registered in `PaneView::new` with `label_for(None, &request.cwd)`. `start` (also restart) sets `request.extra_env` from it, so a restart keeps the terminal's token. `TermEvent::CwdChanged` calls `set_cwd`; `set_name` calls `rename(&self.label())`. (Token per terminal rather than per spawn: a restart is the same terminal. Our call.)
  - `Workspace` subscribes once (`mcp::server::get()?.subscribe()`) in its constructor and, on `BoardCreated(project)` where `tasks_project::is_repo(&project) && privacy(&project) == Privacy::Committed`, asks the same commit/private question as the Tasks tab. Refactor `ask_git` to take the project path (`fn ask_git(&mut self, project: PathBuf, window, cx)`) and do Keep private through `tasks_project::keep_private(&project)`, toasting a failure as the board's `Problem` toast does; the tab path calls it with its board's project.

  - `pub fn pane_inherited(req: &PaneRequest, vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)>` in `hydra.rs`: `inherited_env(vars)` followed by `req.extra_env`; `Hydra::start` uses it for `SpawnSpec.inherited`.

- [ ] **Step 1: Write the failing test** in `hydra.rs`'s tests:

```rust
#[test]
fn extra_env_is_inherited_by_the_pane() {
    let mut req = PaneRequest::new("work", PathBuf::from("C:\\w"), ShellKind::Pwsh);
    req.extra_env = vec![("HYDRA_MCP_TOKEN".into(), "t0k".into())];
    let vars = pane_inherited(&req, [(OsString::from("HOME"), OsString::from("x"))]);
    assert!(vars.contains(&("HYDRA_MCP_TOKEN".into(), "t0k".into())));
    assert!(vars.contains(&("HOME".into(), "x".into())));
}
```

(If `inherited_env` filters `HOME`, use any variable it keeps.)

And in `mcp/mod.rs`: `McpTerminal` registers, exposes both variables with the server's URL and the token, and its drop revokes (use `server::init()`).

- [ ] **Step 2: Run** `cargo test -p hydraui extra_env_is_inherited_by_the_pane` and `cargo test -p hydraui mcp::` and expect failure.
- [ ] **Step 3: Implement** the Interfaces block; `main.rs` calls `hydraui::mcp::server::init()` before opening the window.
- [ ] **Step 4: Run** `cargo test -p hydraui` and `cargo clippy -p hydraui --all-targets`; expect all pass, no warnings.
- [ ] **Step 5: Check by running** (with Task 7–8 not yet done, by hand): in a hydraui terminal `$env:HYDRA_MCP_URL; $env:HYDRA_MCP_TOKEN` print a URL and a 32-hex token; a second terminal in the same folder shows a different token.
- [ ] **Step 6: Commit**

```bash
git add crates/hydraui/src
git commit -m "feat: hydraui gives each terminal its MCP server address and token"
```

### Task 7: hydra registers and pre-approves the server

**Files:**
- Modify: `crates/hydra-providers/src/claude_files.rs` (`sync_mcp`, new `add_hydraui_allow`), `crates/hydra-providers/src/claude.rs` (`materialise`), `crates/hydra-providers/src/claude.rs` README text (mention the `hydraui` entry)

**Interfaces:**
- Produces:
  - `pub const HYDRAUI_SERVER: &str = "hydraui";`
  - `pub const HYDRAUI_TOOLS: [&str; 4] = ["tasks_read", "tasks_add", "tasks_take", "tasks_mine"];`
  - `sync_mcp` inserts `"hydraui": {"type": "stdio", "command": "hydra", "args": ["mcp"]}` into the servers copied from the base, replacing a base entry of that name, **before** `drop_excluded` (so `[claude] mcp.exclude = ["hydraui"]` opts out). It now writes `.claude.json` even when the base has no servers.
  - `pub fn add_hydraui_allow(settings_json: &str) -> Result<String, String>`: appends each missing `mcp__hydraui__<tool>` to `permissions.allow` (creating `permissions`/`allow`), keeping every other key and rule; idempotent. `materialise` applies it to `merged.text` (before the guard-hook step) unless `exclude` contains `hydraui`; an error is a warning, as with the guard hook.

- [ ] **Step 1: Write the failing tests** in `claude_files.rs`:

```rust
#[test]
fn every_environment_gets_the_hydraui_server() {
    let out = sync_mcp(None, None, &[]).unwrap().unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["mcpServers"]["hydraui"], json!({"type": "stdio", "command": "hydra", "args": ["mcp"]}));
    let base = r#"{"mcpServers":{"hydraui":{"command":"old"},"codegraph":{}}}"#;
    let v: Value = serde_json::from_str(&sync_mcp(None, Some((Path::new("b"), base)), &[]).unwrap().unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["hydraui"]["command"], "hydra");
    assert!(v["mcpServers"]["codegraph"].is_object());
    assert_eq!(sync_mcp(None, None, &["hydraui".into()]).unwrap(), None);
}

#[test]
fn hydraui_tools_are_allowed_once_keeping_the_users_rules() {
    let user = r#"{"permissions":{"allow":["Bash(git status)"],"deny":["Read(.env)"]}}"#;
    let once = add_hydraui_allow(user).unwrap();
    let twice = add_hydraui_allow(&once).unwrap();
    assert_eq!(once, twice);
    let v: Value = serde_json::from_str(&once).unwrap();
    assert_eq!(v["permissions"]["deny"], json!(["Read(.env)"]));
    assert_eq!(v["permissions"]["allow"], json!(["Bash(git status)", "mcp__hydraui__tasks_read", "mcp__hydraui__tasks_add", "mcp__hydraui__tasks_take", "mcp__hydraui__tasks_mine"]));
    assert!(add_hydraui_allow("{}").unwrap().contains("mcp__hydraui__tasks_take"));
}
```

Update existing `sync_mcp` tests that expected `None` for an empty base (they now get the `hydraui` entry), and the hydra-cli tests in `crates/hydra-cli/tests/claude_cli.rs` that compare `.claude.json`/`settings.json` contents.

- [ ] **Step 2: Run** `cargo test -p hydra-providers` and expect the new tests to fail.
- [ ] **Step 3: Implement** the Interfaces block.
- [ ] **Step 4: Run** `cargo test --workspace` and expect PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/hydra-providers crates/hydra-cli/tests
git commit -m "feat: hydra registers hydraui's MCP server in each claude environment and allows its tools"
```

### Task 8: `hydra mcp`, the stdio bridge

**Files:**
- Create: `crates/hydra-cli/src/commands/mcp.rs`
- Modify: `crates/hydra-cli/src/commands/mod.rs`, the clap command enum (where `guard` is declared), `crates/hydra-cli/Cargo.toml` (`ureq.workspace = true`, `serde_json.workspace = true` if missing)
- Test: `crates/hydra-cli/tests/mcp_cli.rs`

**Interfaces:**
- Produces: `hydra mcp` (hidden from `--help`, like `guard`): reads newline-delimited JSON-RPC from stdin until EOF, writes one compact JSON line per reply to stdout, flushing each.
  - `HYDRA_MCP_URL` and `HYDRA_MCP_TOKEN` both set: POST each line to the URL (`Content-Type: application/json`, `Accept: application/json, text/event-stream`, `Authorization: Bearer <token>`). 200 → write the body as one line; 202 → write nothing; any other status or a connection error → for a request (has `id`), write a JSON-RPC error `{"code": -32000, "message": "hydraui isn't reachable (closed?)"}` with that `id`; for a notification, nothing.
  - Either variable unset: answer locally as a server with no tools: `initialize` → `{"protocolVersion": <client's>, "capabilities": {"tools": {}}, "serverInfo": {"name": "hydraui", "version": <hydra's VERSION>}, "instructions": "Not running inside hydraui: no tools."}`; `tools/list` → `{"tools": []}`; `ping` → `{}`; other requests → `-32601`; notifications → nothing.
  - Pure core: `fn local_reply(line: &str) -> Option<String>` for the unset case, unit-tested.

- [ ] **Step 1: Write the failing tests**: unit tests on `local_reply` (initialize echoes the version and has empty tools; notification → `None`; unknown → `-32601`), and in `mcp_cli.rs` an `assert_cmd` test that pipes `initialize` + `tools/list` into `hydra mcp` with both variables removed and checks two JSON lines, the second with `"tools":[]`; and one with `HYDRA_MCP_URL=http://127.0.0.1:9/mcp`, `HYDRA_MCP_TOKEN=x` checking the `-32000` reply.
- [ ] **Step 2: Run** `cargo test -p hydra-cli mcp` and expect failure.
- [ ] **Step 3: Implement** the Interfaces block.
- [ ] **Step 4: Run** `cargo test --workspace` and `cargo clippy --workspace --all-targets`; expect all pass, no warnings.
- [ ] **Step 5: Commit**

```bash
git add crates/hydra-cli
git commit -m "feat: hydra mcp relays a Claude session to its hydraui's MCP server"
```

### Task 9: End to end, docs

**Files:**
- Modify: `site/app.html` (the hydraui guide's task board section), `docs/superpowers/specs/2026-10-20-hydraui-mcp-server-design.md` §3 (record the finding above: the bridge is the registration; the token is per terminal, kept across a restart, not fresh per spawn)

- [ ] **Step 1: Check by hand** with a dev build of both exes on PATH ahead of installed ones, in an environment with `[claude]`:
  1. Start hydraui; open a terminal in a git repo; run `claude mcp list` there: `hydraui` is connected.
  2. Outside hydraui (`hydra shell <env>` in Windows Terminal): `claude mcp list` shows `hydraui` connected with no warning.
  3. In hydraui, put a card in Pickup via the Tasks tab; ask Claude "check the task board": it calls `tasks_read`/`tasks_take` with no permission prompt; the Tasks tab shows the card in InProgress with the terminal's badge.
  4. Ask Claude to note an idea: it lands at the top of Backlog. Ask it to move a card to Done: it says it can't.
  5. Close the terminal and confirm its token no longer works (a fresh `claude` in another terminal still connects).
- [ ] **Step 2: Docs**: add a short "Claude and the board" part to the guide's task board section: sessions in hydraui terminals find the board themselves; they add to Backlog and take from Pickup; Pickup and Done stay yours; the `[name]` badge; works in environments with `[claude]`. Update spec §3 as above.
- [ ] **Step 3: Run** `cargo test --workspace` and expect PASS.
- [ ] **Step 4: Commit**

```bash
git add site/app.html docs/superpowers/specs/2026-10-20-hydraui-mcp-server-design.md
git commit -m "docs: hydraui guide covers Claude working the task board"
```
