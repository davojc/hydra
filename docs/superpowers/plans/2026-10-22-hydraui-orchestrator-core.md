# hydraui Orchestrator Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An orchestrator Claude session in a hydraui terminal takes one ticket through a pipeline's stages; each stage is a fresh Claude session hydraui opens and links; a stage ends with a confidence rating; hydraui refuses the next stage until the rating clears the bar, and stops the run for a person otherwise.

**Architecture:** Pure modules first: `pipeline` (TOML pipelines, prompts), `rating` (the report and its validation), `runs` (the run state machine and every gate rule, behind the MCP registry's mutex so a gate check and its state change are one step), `run_log` (JSON lines). The MCP server gains five tools; the tools that need the app (open a terminal, wake the orchestrator, show a stop) emit new `McpEvent`s, and the workspace acts on them: it opens the stage terminal without taking focus, links the new terminal's token back into the run, types the orchestrator's wake-up prompt when it is idle with no draft, raises the stop alert, and draws the run badge, the indented stage rows and a **Run** tab beside the terminal (as the Tasks tab is). hydra pre-approves the five tools.

**Tech Stack:** Rust 2024 (MSRV 1.89), gpui / gpui-component, serde + serde_json, toml (all already in hydraui). No new crates.

**Spec:** `docs/superpowers/specs/2026-10-22-hydraui-orchestrator-core-design.md` (builds on `2026-10-20-hydraui-mcp-server-design.md` and `2026-10-20-hydraui-auto-pickup-design.md`).

## Global Constraints

- Tool names exactly `run_start`, `stage_start`, `run_status`, `stage_brief`, `stage_report`; allow rules `mcp__hydraui__<name>` by name, added to `HYDRAUI_TOOLS` (which grows from 5 to 10).
- The gate lives in `runs`, checked and applied under the registry lock in the server thread: a refused call changes nothing; an allowed `stage_start` marks the stage `Starting` before the event leaves, so two calls can't both start it.
- Roles by token (spec §3.3): orchestrator tools refuse stage tokens; stage tools refuse every token but the stage's own.
- Bars are integers 0–100; pipeline default 80; a stage's `bar` overrides. Usage line: `min_week_left` (default 30) percent of the seven-day window must be left; no report yet = allowed.
- Retries: three attempts per stage for `Failed`; `Stopped` is never retried by the orchestrator, only by a person.
- One unfinished run per orchestrator token. Runs live in memory; a terminal's token dropping (terminal closed) ends what it held: an orchestrator's run becomes `Stopped`, a running stage becomes `Failed`.
- Windows and macOS identical: no new `cfg`; paths through `Path::join`; the run log is UTF-8 JSON lines.
- Comments and doc comments match the surrounding code: short, plain, referencing spec sections as `§n`.

## Review Focus

1. Two `stage_start` calls racing for the same stage: exactly one opens a terminal (Task 3, Task 5).
2. A rating with a score in range but empty `reasons`, no evidence, or `outcome: "blocked"` with a high score never passes (Task 2, Task 3).
3. A stage terminal closed or its shell exited before reporting makes the stage `Failed`, never stuck `Running` (Task 6).
4. The wake-up prompt is never typed while the orchestrator has a draft or is working; the newest message waits and is sent once it is idle (Task 7).
5. `run_status` and the Run tab show the same state the gate uses: they read `runs`, never a copy.

---

### Task 1: Pipelines

**Files:**
- Create: `crates/hydraui/src/pipeline.rs` (+ `pub mod pipeline;` in `lib.rs`)

**Interfaces:**
- Produces:
  - `pub struct Pipeline { pub name: String, pub bar: u8, pub stages: Vec<Stage> }` and `pub struct Stage { pub id: String, pub title: String, pub bar: Option<u8>, pub worktree: bool, pub prompt: String }` (`Debug, Clone, PartialEq, Eq`).
  - `impl Pipeline { pub fn bar_for(&self, stage: usize) -> u8 }`.
  - `pub fn parse(text: &str) -> Result<Pipeline, String>`: `bar` default 80; stage `worktree` default false; errors (with the stage or key named) for unknown keys, a bar over 100, an empty or non-`[a-z0-9-]` id, a duplicate id, no stages.
  - `pub fn folder(hydra_root: &Path) -> PathBuf` = `<root>/hydraui/pipelines`; `pub fn load(hydra_root: &Path, name: &str) -> Result<Pipeline, String>` (`<name>.toml`; the name must match `[a-z0-9-]+`).
  - `pub const EXAMPLE: &str` (the `ticket.toml` of spec §2, four stages, plain prompts) and `pub fn ensure_example(hydra_root: &Path) -> io::Result<()>` (writes it only when the folder has no `.toml`).
  - `pub struct PromptVars<'a> { pub ticket: &'a str, pub run: &'a str, pub stage: &'a str, pub handoff: &'a str }` and `pub fn render(prompt: &str, vars: &PromptVars, brief: &str) -> String`: placeholders filled, `brief` after a blank line when non-empty, then `CLOSING` (spec §2: finish with `stage_report`; report `blocked` rather than guess).

- [ ] **Step 1: Write the failing tests** (`pipeline.rs` `mod tests`):

```rust
const TWO: &str = "name = \"t\"\n[[stage]]\nid = \"replicate\"\ntitle = \"Replicate\"\nprompt = \"Read {ticket} ({run}, {stage}). {handoff}\"\n[[stage]]\nid = \"develop\"\ntitle = \"Develop\"\nbar = 85\nworktree = true\nprompt = \"Fix it.\"\n";

#[test]
fn bars_default_to_80_and_a_stage_can_set_its_own() {
    let p = parse(TWO).unwrap();
    assert_eq!((p.bar, p.bar_for(0), p.bar_for(1)), (80, 80, 85));
    assert!(p.stages[1].worktree && !p.stages[0].worktree);
}

#[test]
fn mistakes_are_named() {
    for (text, says) in [
        ("name = \"t\"\n", "no stages"),
        ("name = \"t\"\nbar = 101\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n", "bar"),
        ("name = \"t\"\n[[stage]]\nid = \"A b\"\ntitle = \"A\"\nprompt = \"p\"\n", "A b"),
        ("name = \"t\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n[[stage]]\nid = \"a\"\ntitle = \"B\"\nprompt = \"q\"\n", "twice"),
        ("name = \"t\"\nspeed = 2\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\n", "speed"),
    ] {
        let e = parse(text).unwrap_err();
        assert!(e.contains(says), "{e}");
    }
}

#[test]
fn a_prompt_gets_its_values_the_brief_and_always_the_closing() {
    let p = parse(TWO).unwrap();
    let vars = PromptVars { ticket: "INTH-1", run: "INTH-1-1", stage: "replicate", handoff: "see log" };
    let text = render(&p.stages[0].prompt, &vars, "Focus on the API.");
    assert!(text.starts_with("Read INTH-1 (INTH-1-1, replicate). see log"), "{text}");
    assert!(text.contains("\n\nFocus on the API.\n"), "{text}");
    assert!(text.ends_with(CLOSING), "{text}");
}

#[test]
fn the_example_parses_and_is_written_once() {
    assert_eq!(parse(EXAMPLE).unwrap().stages.len(), 4);
    let d = tempfile::tempdir().unwrap();
    ensure_example(d.path()).unwrap();
    let file = folder(d.path()).join("ticket.toml");
    std::fs::write(&file, "edited").unwrap();
    ensure_example(d.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited");
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p hydraui pipeline::`
Expected: compile errors (module not defined).

- [ ] **Step 3: Implement** with `#[derive(Deserialize)] #[serde(deny_unknown_fields)]` file structs mapped to the public ones, then the checks above. `render` replaces the four `{…}` placeholders literally (no other braces are special).

- [ ] **Step 4: Run** `cargo test -p hydraui pipeline::` — all pass.

- [ ] **Step 5: Commit** `feat(hydraui): pipelines for the orchestrator`

---

### Task 2: Ratings

**Files:**
- Create: `crates/hydraui/src/rating.rs` (+ `pub mod rating;`)

**Interfaces:**
- Produces:
  - `pub enum Outcome { Done, Blocked }`; `pub enum EvidenceKind { Test, Command, File, Link, Note }`; `pub struct Evidence { pub kind: EvidenceKind, pub detail: String }`; `pub struct Rating { pub outcome: Outcome, pub score: u8, pub summary: String, pub evidence: Vec<Evidence>, pub reasons: String, pub handoff: String }` (all `Debug, Clone, PartialEq, Eq, Serialize`).
  - `pub fn from_args(args: &serde_json::Value) -> Result<Rating, String>`: every problem in one message (spec §4): score not an integer 0–100, unknown outcome or evidence kind, empty `summary` or `reasons`, no evidence, more than 20 items, an item's detail over 500 characters (trimmed text). `handoff` defaults to empty.
  - `pub fn passes(r: &Rating, bar: u8) -> bool` = `r.outcome == Done && r.score >= bar`.

- [ ] **Step 1: Write the failing tests:**

```rust
fn good() -> serde_json::Value {
    json!({"outcome": "done", "score": 86, "summary": "Reproduced.",
           "evidence": [{"kind": "test", "detail": "e2e: 1 failing"}],
           "reasons": "Fails three runs out of three.", "handoff": "Fix save()."})
}

#[test]
fn a_full_report_reads_and_passes_at_or_above_the_bar() {
    let r = from_args(&good()).unwrap();
    assert!(passes(&r, 86) && !passes(&r, 87));
}

#[test]
fn blocked_never_passes() {
    let mut a = good();
    a["outcome"] = json!("blocked");
    a["score"] = json!(100);
    assert!(!passes(&from_args(&a).unwrap(), 0));
}

#[test]
fn every_missing_piece_is_named_at_once() {
    let e = from_args(&json!({"outcome": "maybe", "score": 101, "summary": " ", "evidence": [], "reasons": ""})).unwrap_err();
    for says in ["outcome", "score", "summary", "evidence", "reasons"] {
        assert!(e.contains(says), "{says}: {e}");
    }
}

#[test]
fn evidence_is_bounded() {
    let mut a = good();
    a["evidence"] = json!((0..21).map(|_| json!({"kind": "note", "detail": "x"})).collect::<Vec<_>>());
    assert!(from_args(&a).unwrap_err().contains("20"));
    a["evidence"] = json!([{"kind": "note", "detail": "x".repeat(501)}]);
    assert!(from_args(&a).unwrap_err().contains("500"));
    a["evidence"] = json!([{"kind": "rumour", "detail": "x"}]);
    assert!(from_args(&a).unwrap_err().contains("rumour"));
}
```

- [ ] **Step 2: Run** `cargo test -p hydraui rating::` — compile errors.
- [ ] **Step 3: Implement** by reading fields by hand from the `Value` (not `serde` derive), collecting every problem into a `Vec<String>` joined with `; `.
- [ ] **Step 4: Run** — all pass.
- [ ] **Step 5: Commit** `feat(hydraui): stage ratings`

---

### Task 3: Runs and the gate

**Files:**
- Create: `crates/hydraui/src/runs.rs` (+ `pub mod runs;`)

**Interfaces:**
- Consumes: `pipeline::{Pipeline, PromptVars, render}`, `rating::{Rating, passes}`.
- Produces:
  - `pub enum StageStatus { Waiting, Starting, Running, Passed, Stopped, Failed, Overridden }`; `pub struct StageRun { pub status: StageStatus, pub attempts: u8, pub session: Option<String> /* token */, pub session_name: Option<String>, pub rating: Option<Rating>, pub note: Option<String> }`.
  - `pub enum RunState { Running, Stopped(String) /* why */, Finished }`; `pub struct Run { pub id: String, pub ticket: String, pub summary: String, pub pipeline: Pipeline, pub orchestrator: String /* token */, pub stages: Vec<StageRun>, pub state: RunState, pub worktree: Option<PathBuf>, pub week_used: Option<f64> }`.
  - `pub enum Role<'a> { Orchestrator(&'a Run), Stage(&'a Run, usize), Other }`; `impl Runs { pub fn role(&self, token: &str) -> Role<'_> }`.
  - `pub struct StartStage { pub run: String, pub stage: usize, pub orchestrator: String, pub worktree: bool, pub title: String, pub prompt: String }` (what the workspace needs to open the terminal).
  - `pub enum Decided { Passed, Stopped(String), Failed(String), RunFinished, RunStopped(String) }` with the run id and stage index, as `pub struct Change { pub run: String, pub stage: Option<usize>, pub what: Decided }` — what the server turns into events.
  - `#[derive(Default)] pub struct Runs` with:
    - `pub fn start_run(&mut self, orchestrator: &str, pipeline: Pipeline, ticket: &str, summary: &str) -> Result<&Run, String>` (id `<ticket>-<n>`, n counting this process's runs of that ticket; refused while this orchestrator has a `Running` run).
    - `pub fn may_start(&self, orchestrator: &str, run: &str, stage: &str, min_week_left: u8) -> Result<usize, String>` — spec §5.1 rules 1–5 in order, first failure's reason; usage: refused when `week_used` is `Some(u)` and `100 - u < min_week_left`.
    - `pub fn begin(&mut self, orchestrator: &str, run: &str, stage: &str, brief: &str, min_week_left: u8) -> Result<StartStage, String>` — `may_start`, then status `Starting`, `attempts += 1`, prompt rendered with the previous passed/overridden stage's `handoff`.
    - `pub fn link(&mut self, run: &str, stage: usize, token: &str, name: &str)` — `Starting` → `Running`.
    - `pub fn start_failed(&mut self, run: &str, stage: usize, why: &str) -> Vec<Change>` — terminal or worktree couldn't open: `Failed`.
    - `pub fn report(&mut self, token: &str, rating: Rating) -> Result<Vec<Change>, String>` — only the stage's own `Running` session; `Passed` if `passes`, else `Stopped` and the run `Stopped(reason)`; the last stage passing finishes the run.
    - `pub fn session_gone(&mut self, token: &str) -> Vec<Change>` — a `Running` stage's token → `Failed`, and the run `Stopped` once attempts reach 3; an orchestrator's token → its `Running` run `Stopped("the orchestrator's terminal closed")`.
    - `pub fn usage(&mut self, token: &str, week_used: f64)` — sets `week_used` on the run the token belongs to (orchestrator or stage).
    - `pub fn person(&mut self, run: &str, choice: Choice) -> Result<Vec<Change>, String>` with `pub enum Choice { Continue { note: String }, Retry { note: String }, Stop }` — spec §5.3; `Continue` makes the stopped/failed stage `Overridden` and the run `Running` (or `Finished` if it was the last); `Retry` puts it back to `Waiting` past the attempt limit with `note` added to its next prompt; `Stop` ends the run `Stopped("stopped by you")`.
    - `pub fn run(&self, id: &str) -> Option<&Run>`, `pub fn current(&self, orchestrator: &str) -> Option<&Run>`, `pub fn all(&self) -> impl Iterator<Item = &Run>`.
    - `pub fn status_json(&self, run: &Run, min_week_left: u8, live: impl Fn(&str) -> Option<&'static str>) -> serde_json::Value` (what `run_status` returns; `live(token)` gives the session's hook state word).

- [ ] **Step 1: Write the failing tests** — one per rule, using `pipeline::parse` of a two- or three-stage pipeline and `rating::from_args`:
  - `a_run_starts_its_first_stage_and_links_its_session`
  - `the_next_stage_waits_for_a_passing_rating` (rule 3: `begin` of stage 2 refused while stage 1 is `Running`, then after `report` below the bar, allowed after `Passed`)
  - `only_one_stage_runs_at_a_time` (rule 2, including `Starting`: two `begin`s of the same stage → the second refused)
  - `a_stage_token_cannot_orchestrate_and_others_cannot_report` (roles)
  - `a_low_score_stops_the_run_and_the_orchestrator_cannot_retry_it`
  - `a_failed_stage_is_retried_three_times_then_stops`
  - `less_than_the_weekly_share_left_refuses_a_start` (with `week_used` `None` allowed; 71.0 with `min_week_left` 30 refused; 70.0 allowed)
  - `the_handoff_reaches_the_next_prompt`
  - `a_person_continues_retries_or_stops` (each choice's status and the run's state)
  - `the_last_stage_passing_finishes_the_run_and_a_new_one_may_start`
  - `a_closed_orchestrator_stops_its_run`
  - `one_report_per_stage` (second `report` from the same token refused)

```rust
fn pipeline(stages: &[&str]) -> Pipeline {
    let mut t = String::from("name = \"t\"\n");
    for s in stages {
        t += &format!("[[stage]]\nid = \"{s}\"\ntitle = \"{s}\"\nprompt = \"do {s}. {{handoff}}\"\n");
    }
    pipeline::parse(&t).unwrap()
}

fn rated(score: u8) -> Rating {
    rating::from_args(&json!({"outcome": "done", "score": score, "summary": "s",
        "evidence": [{"kind": "note", "detail": "d"}], "reasons": "r", "handoff": format!("after {score}")})).unwrap()
}

#[test]
fn the_next_stage_waits_for_a_passing_rating() {
    let mut runs = Runs::default();
    let id = runs.start_run("orch", pipeline(&["a", "b"]), "INTH-1", "x").unwrap().id.clone();
    runs.begin("orch", &id, "a", "", 30).unwrap();
    assert!(runs.begin("orch", &id, "b", "", 30).unwrap_err().contains("running"));
    runs.link(&id, 0, "tok-a", "INTH-1-1 · a");
    runs.report("tok-a", rated(80)).unwrap();
    let next = runs.begin("orch", &id, "b", "", 30).unwrap();
    assert!(next.prompt.contains("after 80"), "{}", next.prompt);
}
```

- [ ] **Step 2: Run** `cargo test -p hydraui runs::` — compile errors.
- [ ] **Step 3: Implement.** Keep every rule in `may_start` (one place the gate lives); `begin` and the person's `Retry` are the only ways to `Starting`/`Waiting`.
- [ ] **Step 4: Run** — all pass.
- [ ] **Step 5: Commit** `feat(hydraui): runs, stages and the gate between them`

---

### Task 4: The run log

**Files:**
- Create: `crates/hydraui/src/run_log.rs` (+ `pub mod run_log;`)

**Interfaces:**
- Produces: `pub fn file_for(hydra_root: &Path, run: &str) -> PathBuf` (`<root>/hydraui/runs/<run>.jsonl`); `pub fn append(file: &Path, event: &str, fields: serde_json::Value) -> io::Result<()>` — one line `{"at": "<local time>", "event": ..., ...fields}`, creating the folder; best effort at call sites (a failed write is logged with `action_log::write`, never stops a run).

- [ ] **Step 1: Test** `each_event_is_one_json_line` (two appends → two lines, each parsing as JSON with `event` and the fields) and `a_run_id_is_a_safe_file_name` (`file_for` of `INTH-1-1` stays inside `runs/`).
- [ ] **Step 2: Run** red. **Step 3: Implement** (timestamp via `action_log`'s formatter). **Step 4: Run** green.
- [ ] **Step 5: Commit** `feat(hydraui): an append-only log per run`

---

### Task 5: The tools

**Files:**
- Modify: `crates/hydraui/src/mcp/registry.rs` (holds `pub runs: Runs` and `pub min_week_left: u8`)
- Modify: `crates/hydraui/src/mcp/server.rs` (`reply_to` routes the five tools; `McpEvent` variants; usage feeds `runs.usage`; token revocation feeds `runs.session_gone`)
- Modify: `crates/hydraui/src/mcp/tools.rs` (schemas in `list()`, descriptions, `INSTRUCTIONS` gains the orchestrator/stage paragraph)
- Modify: `crates/hydra-providers/src/claude_files.rs` (`HYDRAUI_TOOLS: [&str; 10]` and its test)

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `McpEvent::StageStart(runs::StartStage)`, `McpEvent::RunChanged(Vec<runs::Change>)`.
  - `fn run_tool(reg: &mut Registry, token: &str, hydra_root: &Path, name: &str, args: &Value) -> Option<(Outcome, Option<McpEvent>)>` in `tools.rs` — `None` for any other name (so `call_with` keeps the task tools). Called from `reply_to` under the registry lock, before `call_with`.
  - `Registry::revoke` returns the `Vec<Change>` from `runs.session_gone(token)`; `McpTerminal`'s `Drop` sends them as `RunChanged` through the server (`Server::broadcast(McpEvent)`, a new `pub fn`).
  - The server needs the hydra root for pipelines and logs: `Server::start` gains `hydra_root: PathBuf` (from `main.rs`, where `hydra.paths` is known), stored on the server and passed to `run_tool`.

- [ ] **Step 1: Write the failing tests:**
  - In `tools.rs`: `the_list_offers_exactly_the_pre_approved_tools` keeps passing once both lists hold the ten names (update its expected array).
  - In `server.rs` (in-process, as `speaks_json_rpc`): `a_run_goes_through_two_stages_over_json_rpc` — write a two-stage pipeline into a temp hydra root; `run_start` from the orchestrator token; `stage_start` → reply ok and a `StageStart` event; a second `stage_start` → `isError` naming "running"; simulate the workspace's link with `registry.lock().runs.link(...)` for a stage token registered with `register`; `stage_report` from that token → `RunChanged` with `Passed`; `stage_report` from the orchestrator's token → `isError`; `run_status` shows stage 1 passed and stage 2 may start.
  - `a_report_missing_reasons_is_refused_and_changes_nothing`.
  - `usage_reports_feed_the_weekly_check` — a `hydraui/usage` notification with `rate_limits.seven_day.used_percentage: 75` from a stage token, then `stage_start` refused naming the share left.
  - `closing_a_stage_terminal_fails_its_stage` — `revoke(stage_token)` returns a `Failed` change.
  - Update `speaks_json_rpc`'s tool count from 5 to 10.
  - In `claude_files.rs`: `hydraui_tools_are_allowed_once_keeping_the_users_rules` expects the ten `mcp__hydraui__…` rules.
- [ ] **Step 2: Run** `cargo test -p hydraui mcp::` and `cargo test -p hydra-providers claude_files` — red.
- [ ] **Step 3: Implement.** Each tool: role check (`runs.role(token)`), then the `runs` call, then `run_log::append`, then the `Outcome` text (a short human line plus the JSON the spec names for `run_status`/`stage_brief`). `run_start` loads the pipeline (`pipeline::load`) and calls `pipeline::ensure_example` first. Descriptions carry the workflow of spec §9 (wording: builder's call).
- [ ] **Step 4: Run** `cargo test -p hydraui -p hydra-providers` — all pass.
- [ ] **Step 5: Commit** `feat(hydraui): run and stage tools on the MCP server`

---

### Task 6: Opening and linking stage terminals

**Files:**
- Modify: `crates/hydraui/src/workspace.rs` (`_mcp_pump` match: `StageStart`, `RunChanged`; `open_terminal_beside`)
- Modify: `crates/hydraui/src/envs.rs` (`add_terminal_quietly`)
- Create: `crates/hydraui/src/workspace/orchestrator.rs` (`mod orchestrator;` in `workspace.rs`)
- Modify: `crates/hydraui/src/worktrees.rs` (`create` accepts a run id as its card text — it already slugs any text; add a test that a run id makes `<repo>-worktrees/inth-1-1` on `task/inth-1-1`)

**Interfaces:**
- Produces:
  - `EnvTabs::add_terminal_quietly(&mut self, env: &str, id: PaneId) -> bool` — adds the terminal to `env`'s tab without changing `shown` or `active`.
  - `Workspace::open_terminal_beside(&mut self, env: &str, request: PaneRequest, window, cx) -> PaneId` — `open_terminal` without `focus_soon` and with `add_terminal_quietly`.
  - `Workspace::start_stage(&mut self, s: StartStage, window, cx)` in `workspace/orchestrator.rs`: finds the orchestrator's pane by token (as `session_state` does), its environment and folder; when `s.worktree` and the run has no worktree, `worktrees::create(repo, &s.run)` in `background_spawn` and record it on the run; opens `claude "<prompt>"` titled `<run> · <title>` with `open_terminal_beside`; then `registry.runs.link(run, stage, token, name)` with the new pane's `mcp_token()` and `taker_name()`. Any failure → `registry.runs.start_failed(...)` and its changes handled as `RunChanged`.
  - A `PaneEvent::Exited` for a stage pane → `registry.runs.session_gone(token)` (the shell died even if the terminal is still open), and so does a stage token's `McpEvent::Session { state: Ended, .. }` (Claude ended without reporting; spec §3.4).
  - `PaneRequest` has no title: the stage pane is named with `PaneView::set_name(Some(stage_title(..)))` right after it opens, before `link` reads `taker_name()`.
- [ ] **Step 1: Write the failing tests:** `EnvTabs` — `adding_quietly_keeps_what_is_shown` (in `envs.rs`, beside the existing `add_terminal` tests); `worktrees` — the run-id test above; `orchestrator.rs` — `the_stage_terminal_title_names_run_and_stage` (a pure `fn stage_title(run: &str, title: &str) -> String`).
- [ ] **Step 2: Run** red. **Step 3: Implement.** **Step 4: Run** `cargo test -p hydraui` green.
- [ ] **Step 5: Commit** `feat(hydraui): stage terminals open beside the user and link to their run`

---

### Task 7: Waking the orchestrator

**Files:**
- Modify: `crates/hydraui/src/workspace/orchestrator.rs`, `crates/hydraui/src/pickup.rs` (a shared pure check)

**Interfaces:**
- Produces:
  - `pub fn ready_for_prompt(s: &pickup::Session) -> bool` = `claude == Some(Idle) && !reserved && !draft` (auto-pickup's checks 3 and 6 without its project rule).
  - `pub fn wake_message(run: &str, change: &Decided, stage_id: Option<&str>, score_and_bar: Option<(u8, u8)>) -> String` — e.g. `Run INTH-1-1: stage replicate passed (86, bar 80). Call run_status, then start the next stage.`; for stopped: `… stopped (62, bar 80). Tell the user why and wait.`; finished: `… finished. Tell the user, then start the next ticket.`
  - `Workspace.pending_wakes: HashMap<PaneId, String>` (newest kept); sent from the `RunChanged` handler and from `session_state` when the orchestrator becomes idle, through `PaneView::type_prompt` and the reservation, as `send_card` does.
- [ ] **Step 1: Write the failing tests** in `pickup.rs`: `ready_for_prompt_needs_idle_no_draft_not_reserved` (each field flipped once); in `orchestrator.rs`: `wake_messages_name_run_stage_and_result` (passed / stopped / failed / finished).
- [ ] **Step 2: Run** red. **Step 3: Implement.** **Step 4: Run** green.
- [ ] **Step 5: Commit** `feat(hydraui): tell the orchestrator when a stage is decided`

---

### Task 8: A stopped run calls a person

**Files:**
- Modify: `crates/hydraui/src/workspace/busy.rs` (`alert` gains a run-stopped form), `crates/hydraui/src/workspace/orchestrator.rs`

**Interfaces:**
- Produces: `pub fn stop_reason(run: &Run) -> String` (`"INTH-1-1 · Develop: 62, bar 85. <summary>"`, or the failure / usage reason); on `RunStopped`, the orchestrator's pane gets `attention`, the window flashes in the background, and a toast titled **Run stopped** (stays until closed) shows the reason; clicking it opens the run's Run tab (Task 10).
- [ ] **Step 1: Test** `stop_reasons_name_the_stage_score_bar_and_summary` (three cases: low score, attempts used up, usage line).
- [ ] **Steps 2–4** red → implement → green. **Step 5: Commit** `feat(hydraui): a stopped run calls a person`

---

### Task 9: The run badge and stage rows

**Files:**
- Modify: `crates/hydraui/src/workspace/terminal_list.rs`, `crates/hydraui/src/workspace/orchestrator.rs`

**Interfaces:**
- Produces: `pub fn run_badge(run: &Run) -> String` — `INTH-123 · 2/4 Develop` (current = the running stage, else the next waiting one, else the last); `pub fn stage_rows(runs: &Runs, orchestrator_token: &str) -> Vec<(String /* stage token */, String /* title */)>` for the running and finished stages' sessions; in the list, the orchestrator's row shows the badge (amber when stopped) and its stage terminals are drawn directly under it, indented, with the usual dot and state; clicking the badge opens the Run tab.
- [ ] **Step 1: Test** `the_badge_counts_stages_and_names_the_current_one` and `stage_rows_follow_the_orchestrator` (`grouped_terminals` order keeps a run's stages under its orchestrator).
- [ ] **Steps 2–4**, **Step 5: Commit** `feat(hydraui): the orchestrator's row shows its run and its stages`

---

### Task 10: The Run tab

**Files:**
- Create: `crates/hydraui/src/run_view.rs` (+ `pub mod run_view;`)
- Modify: `crates/hydraui/src/workspace.rs` (`run_tabs: HashMap<PaneId, bool>` beside `tasks_tabs`; `render_env`'s match gains the Run tab arm), `crates/hydraui/src/workspace/tasks_tab.rs` (`render_tasks_strip` draws **Run** too when open)

**Interfaces:**
- Produces: `RunView` (a gpui entity reading `server::get()`'s `runs` on render, re-rendered on `RunChanged` and session-state changes): the run's header (ticket, summary, pipeline, bar, state), a row per stage (title, status word, attempts, a small bar showing the score against the stage's bar, summary; expands to evidence, reasons, handoff and each attempt's session — clicking a session calls `show_terminal`), and for a stopped run three buttons: **Continue anyway**, **Retry stage** (each with an optional note field) and **Stop run**, calling `runs.person(...)` and handling its changes as `RunChanged`.
- Pure helpers tested: `pub fn status_word(s: StageStatus) -> &'static str`; `pub fn score_fraction(score: u8, bar: u8) -> (f32, f32)` (positions on the bar track).
- [ ] **Step 1: Test** `status_words_cover_every_status` and `the_score_and_bar_sit_on_one_track`.
- [ ] **Steps 2–4**; by hand: open the tab from the badge, expand a stage, click a session.
- [ ] **Step 5: Commit** `feat(hydraui): the Run tab`

---

### Task 11: Starting an orchestrator, editing pipelines, the usage setting

**Files:**
- Modify: `crates/hydraui/src/launcher.rs` (`Action::StartOrchestrator`, `Action::EditPipelines`; entries test), `crates/hydraui/src/workspace/launcher.rs` (`run` arms), `crates/hydraui/src/config.rs` (`min_week_left`), `crates/hydraui/src/workspace/orchestrator.rs` (`pub fn orchestrator_prompt(pipeline: &str) -> String`)

**Interfaces:**
- Produces:
  - **Start orchestrator…**: `choose_env` (only environments whose `[claude]` is configured, as `sign_ins` checks it), then a pipeline picked from `pipeline::folder` (after `ensure_example`), then `open_terminal(env, request)` titled `orchestrator` running `claude "<orchestrator_prompt(name)>"` (spec §3.0, §9 orchestrator role).
  - **Edit pipelines**: `ensure_example`, then `cx.open_with_system(&folder)` as `OpenConfigFolder` does.
  - `UiConfig.min_week_left: u8` (default 30, 0–100); copied into the registry at start and on Reload config.
- [ ] **Step 1: Tests:** `entries_cover_environments_sign_in_secrets_and_config` gains the two labels; `config` — `reads_every_setting` with `min_week_left = 25`, `one_bad_setting_keeps_the_rest` with `min_week_left = 101` (bump its expected count); `the_orchestrator_prompt_names_the_pipeline_and_its_role`.
- [ ] **Steps 2–4**. **Step 5: Commit** `feat(hydraui): start an orchestrator from the launcher`

---

### Task 12: End to end, both platforms

**Files:**
- Modify: `crates/hydraui/src/mcp/server.rs` (HTTP test)

- [ ] **Step 1:** `a_two_stage_run_over_http` beside `serves_over_http`: real server, registry with an orchestrator and a stage token, the whole run through `ureq` posts, `StageStart` and `RunChanged` events read from `subscribe()`.
- [ ] **Step 2:** Full gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on macOS (pwsh on PATH, as CI) and on Windows CI.
- [ ] **Step 3: By hand (the phase's gate, spec §10)** on macOS and Windows: Start orchestrator… → a real ticket through `ticket.toml`; every stage opens beside, links, reports, shows in the Run tab and under the orchestrator's row; force one stage below its bar (a stage prompt saying to report 40) → the run stops, the toast and badges appear, **Continue anyway** lets the orchestrator start the next stage after its wake-up prompt.
- [ ] **Step 4:** Docs: the spec's open "builder's call" items answered in the spec; the website's hydraui section gains a short "Orchestrator" paragraph.
- [ ] **Step 5: Commit** `test(hydraui): a run end to end; docs`

## Order and size

Tasks 1–4 are pure and independent (they can run in parallel). Task 5 needs 1–4. Tasks 6–11 need 5 and are mostly independent of each other, except that 8 and 9 link to the tab built in 10 (stub the click until 10 lands). Task 12 is last. Each task is one commit with its tests; none needs a new crate.
