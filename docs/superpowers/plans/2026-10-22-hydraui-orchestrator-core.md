# hydraui Orchestrator Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An orchestrator Claude session in a hydraui terminal takes one ticket through a pipeline's stages; each stage is a fresh Claude session hydraui opens and links; a stage ends with a confidence rating; hydraui refuses the next stage until the rating clears the bar, and stops the run for a person otherwise.

**Architecture:** Pure modules first: `pipeline` (TOML pipelines, prompts), `rating` (the report and its validation), `runs` (the run state machine and every gate rule, behind the MCP registry's mutex so a gate check and its state change are one step), `run_log` (JSON lines). The MCP server gains five tools; the tools that need the app (open a terminal, wake the orchestrator, show a stop) emit new `McpEvent`s, and the workspace acts on them: it opens the stage terminal without taking focus, links the new terminal's token back into the run, types the orchestrator's wake-up prompt when it is idle with no draft, raises the stop alert, and draws the design handoff's views (spec §7): the pipeline card at the top of the terminal list with the run's stage terminals under it, and a tab beside the orchestrator's terminal (as the Tasks tab is) showing the Pipeline view, Pipeline settings or Ticket history. hydra pre-approves the five tools.

**Tech Stack:** Rust 2024 (MSRV 1.89), gpui / gpui-component, serde + serde_json, toml (all already in hydraui), toml_edit (already a workspace dependency, new to hydraui, for Pipeline settings). No new crates.

**Spec:** `docs/superpowers/specs/2026-10-22-hydraui-orchestrator-core-design.md` (builds on `2026-10-20-hydraui-mcp-server-design.md` and `2026-10-20-hydraui-auto-pickup-design.md`).

## Global Constraints

- Tool names exactly `run_start`, `stage_start`, `run_status`, `stage_brief`, `stage_report`; allow rules `mcp__hydraui__<name>` by name, added to `HYDRAUI_TOOLS` (which grows from 5 to 10).
- The gate lives in `runs`, checked and applied under the registry lock in the server thread: a refused call changes nothing; an allowed `stage_start` marks the stage `Starting` before the event leaves, so two calls can't both start it.
- Roles by token (spec §3.3): orchestrator tools refuse stage tokens; stage tools refuse every token but the stage's own.
- Bars are integers 0–100; pipeline default 80; a stage's `bar` overrides. Usage line: `min_week_left` (default 30) percent of the seven-day window must be left; no report yet = allowed.
- Retries: three attempts per stage for `Failed`; `Stopped` is never retried by the orchestrator, only by a person.
- One run per orchestrator token that is `Running` or `NeedsYou`. Runs live in memory; a terminal's token dropping (terminal closed) ends what it held: an orchestrator's run becomes `Stopped` (final), a running stage becomes `Failed`.
- No gate can be skipped (spec anti-goals, the design): there is no override anywhere, not in `runs`, the tools or the views. A person re-runs a stage or stops the ticket.
- The views follow the design handoff (`docs/superpowers/specs/2026-10-22-hydraui-orchestrator-design-handoff.md`): its colours, sizes and copy; hydraui's shell keeps its look (spec §1 non-goals).
- Windows and macOS identical: no new `cfg`; paths through `Path::join`; the run log is UTF-8 JSON lines.
- Comments and doc comments match the surrounding code: short, plain, referencing spec sections as `§n`.

## Review Focus

1. Two `stage_start` calls racing for the same stage: exactly one opens a terminal (Task 3, Task 5).
2. A rating with a score in range but empty `reasons`, no evidence, or `outcome: "blocked"` with a high score never passes (Task 2, Task 3).
3. A stage terminal closed or its shell exited before reporting makes the stage `Failed`, never stuck `Running` (Task 6).
4. The wake-up prompt is never typed while the orchestrator has a draft or is working; the newest message waits and is sent once it is idle (Task 7).
5. `run_status` and the Pipeline view show the same state the gate uses: they read `runs`, never a copy.
6. A bar changed in Pipeline settings applies at the next gate, and a stage list change only to the next run (Task 4b, Task 13).

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
  - `pub enum Decided { Passed, Stopped(String), Failed(String), Overridden, Retrying, RunResumed, RunFinished, RunStopped(String) }` with the run id and stage index, as `pub struct Change { pub run: String, pub stage: Option<usize>, pub what: Decided }` — what the server turns into events.
  - `pub struct Refusal { pub why: String, pub changes: Vec<Change> }` — why a stage may not start; a usage refusal also stops the run, and its changes say so (spec §5.3).
  - `#[derive(Default)] pub struct Runs` with:
    - `pub fn start_run(&mut self, orchestrator: &str, pipeline: Pipeline, ticket: &str, summary: &str) -> Result<&Run, String>` (id `<ticket>-<n>`, n counting this process's runs of that ticket; refused while this orchestrator has a `Running` run).
    - `pub fn may_start(&self, orchestrator: &str, run: &str, stage: &str, min_week_left: u8) -> Result<usize, String>` — spec §5.1 rules 1–5 in order, first failure's reason; usage: refused when `week_used` is `Some(u)` and `100 - u < min_week_left`.
    - `pub fn begin(&mut self, orchestrator: &str, run: &str, stage: &str, brief: &str, min_week_left: u8) -> Result<StartStage, Refusal>` — the gate, then status `Starting`, `attempts += 1`, prompt rendered with the previous stage's `handoff` and a person's retry note; a usage refusal stops the run.
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

### Task 4b: The model follows the design handoff

Spec revision of 2026-10-22 (the design handoff). Reworks Tasks 2, 1 and 3 before the tools serialise them.

**Files:**
- Modify: `crates/hydraui/src/rating.rs`, `crates/hydraui/src/pipeline.rs`, `crates/hydraui/src/runs.rs`

**Interfaces:**
- `rating`: `reasons: Vec<Reason>` with `pub struct Reason { pub side: Side, pub text: String }`, `pub enum Side { For, Against }` (serde lowercase); at least one, at most `MAX_EVIDENCE`, each `text` at most `MAX_DETAIL` (spec §4).
- `pipeline`: `Stage { skill: Option<String>, prompt: Option<String>, .. }`; `pub fn prompt_for(stage: &Stage) -> String` — the stage's prompt or `Use the {skill} skill for the "<title>" stage of ticket {ticket}.` (no skill: `Do the "<title>" stage of ticket {ticket}.`) (spec §2). `EXAMPLE` names a skill per stage and the pipeline `INTH SLA tickets`.
- `runs`:
  - `StageStatus` loses `Overridden`; `cleared()` is `Passed` only.
  - `pub struct Attempt { pub label: String /* s-01 */, pub session: Option<String> /* token */, pub session_name: Option<String>, pub started: SystemTime, pub note: Option<String>, pub rating: Option<Rating> }`; `StageRun { status, attempts: Vec<Attempt>, note: Option<String> /* for the next attempt */ }`; `Run.sessions: u32` numbers labels across the run; `Run.started: SystemTime`; `Run.paused: bool`.
  - `RunState { Running, NeedsYou(String), Finished, Stopped(String) }` — `Stopped` is final (spec §3.5); `start_run` is refused while a `Running` or `NeedsYou` run is the orchestrator's.
  - `Choice { Retry { note: String }, Stop }`; `Retry` needs a non-empty note and works on a `NeedsYou` run (stopped, failed past the limit, or a usage stop); `Stop` works on a `Running` or `NeedsYou` run and ends it `Stopped("stopped by you at <stage>")`.
  - A re-run's prompt carries the previous attempt's report: `Your previous attempt (s-NN) reported <score>: <summary>` plus its reasons, then `Note from you: <note>`.
  - `pub fn pause(&mut self, run: &str, paused: bool) -> Result<Vec<Change>, String>`; gate rule 2 refuses while paused. `Decided` gains `Paused`, `Resumed`; loses `Overridden`.
  - `report(&mut self, token: &str, rating: Rating, current: Option<&Pipeline>) -> Result<Vec<Change>, String>` — the bar is `current`'s for the stage's id when it has one, else the run's (spec §5.1, last paragraph).
  - Only a stage's latest attempt's token has the stage role.
  - `status_json` gives `paused`, `state` (`running`, `needs_you`, `finished`, `stopped`), each stage's `skill`, `bar`, `status` and `attempts: [{label, session, live, started, note, rating}]`.
- [ ] **Step 1: Tests** (updated and new): `reasons_need_a_side_and_text`; `a_stage_without_a_prompt_gets_one_from_its_skill`; `a_person_retries_with_a_note_or_stops` (replaces the continue test; note required; stop is final and lets a new run start); `attempts_are_numbered_across_the_run_and_kept`; `an_earlier_attempt_cannot_report`; `a_paused_run_starts_nothing`; `a_bar_changed_since_the_start_applies_at_the_gate`; `a_rerun_prompt_carries_the_previous_report_and_the_note`.
- [ ] **Step 2: Run** red. **Step 3: Implement.** **Step 4: Run** `cargo test -p hydraui` green; mutation-check the no-override and the latest-attempt rules.
- [ ] **Step 5: Commit** `feat(hydraui): runs follow the orchestrator design`

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
  - `stage_report` reads the run's pipeline file again (`pipeline::load`) and passes it to `runs.report` for the bar (Task 4b); a file that no longer loads falls back to the run's own.

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
- Modify: `crates/hydraui/src/workspace/busy.rs` (`alert` gains a run form), `crates/hydraui/src/workspace/orchestrator.rs`

**Interfaces:**
- Produces: `pub fn escalation(run: &Run) -> Escalation { title, body, first_against }` — the design's toast: "Pipeline stopped · needs you", `<ticket> stopped at <stage>`, `Rated 64%, bar is 85%.` (or the failure / usage reason) and the first `against` reason; on `RunNeedsYou`, the orchestrator's pane gets `attention`, the window flashes in the background, and the toast (design: top right, 360 wide, `#171a1a` on `#4a2620`) stays until **Later**; **Open ticket** opens the Pipeline view with the stopped session selected (Task 10).
- [ ] **Step 1: Test** `escalations_name_the_ticket_stage_score_bar_and_reason` (low score, attempts used up, usage line).
- [ ] **Steps 2–4** red → implement → green. **Step 5: Commit** `feat(hydraui): a stopped run calls a person`

---

### Task 9: The pipeline card

**Files:**
- Modify: `crates/hydraui/src/workspace/terminal_list.rs`, `crates/hydraui/src/workspace/orchestrator.rs`, `crates/hydraui/src/workspace/usage.rs`

**Interfaces:**
- Produces: `pub enum CardState { Working, NeedsYou, Paused, Done }` and `pub fn card(run: &Run) -> Card { name, line /* INTH-482 · Develop */, state, segments: Vec<Segment> /* Passed, Running, Stopped, Pending */ }`; `pub fn stage_rows(runs: &Runs, orchestrator_token: &str) -> Vec<(String /* token */, String /* s-02 · Develop */)>`; in the list, the card at the top of the orchestrator's environment (design: padding 10, radius 8, `#161919`/`#10302b` with the teal diamond, the pill colours and 3px segments), **Ticket history** and **Pipeline settings** links under it, and the run's stage terminals grouped under it, indented; at the list's foot the seven-day allowance meter (4px track, green fill, red below `min_week_left`, 1px tick at the floor, "Pipeline stops below N%").
- [ ] **Step 1: Test** `the_card_names_the_ticket_and_current_stage`, `segments_follow_stage_statuses`, `stage_rows_follow_the_orchestrator` and `the_meter_turns_red_below_the_floor` (a pure `fn meter(week_used: f64, floor: u8) -> (f32, bool)`).
- [ ] **Steps 2–4**, **Step 5: Commit** `feat(hydraui): the pipeline card`

---

### Task 10: The Pipeline view

**Files:**
- Create: `crates/hydraui/src/pipeline_view.rs` (+ `pub mod pipeline_view;`)
- Modify: `crates/hydraui/src/workspace.rs` (`pipeline_tabs: HashMap<PaneId, (bool, PipelineTab)>` beside `tasks_tabs`, `PipelineTab { Pipeline, Settings, History }`; `render_env`'s match gains its arm), `crates/hydraui/src/workspace/tasks_tab.rs` (the strip draws the tab when open)

**Interfaces:**
- Produces: `PipelineView` (a gpui entity reading `server::get()`'s `runs` on render, re-rendered on `RunChanged` and session-state changes), as spec §7.2: the ticket header, the timeline (circles, skill, gate badge, attempts as session rows, dimmed stages after a stop, the Done row), and the right panel by the selected session — the session's own `PaneView` (working, ended), the rating report, the stopped banner with **Step into s-NN** / **Re-run <stage> with a note** / **Stop this ticket** (inline confirm), step-in (amber header, **Back to report**, **Hand back · re-run**), the re-run dialog (note required), done and idle. Choices call `runs.person(...)`, their changes handled as `RunChanged`. A newly started session is selected.
- Pure helpers tested: `pub fn gate_badge(rating: Option<&Rating>, bar: u8) -> (String, Tone)` (`92% ✓`, `64% · needs 85`, `needs ≥80%`); `pub fn session_line(a: &Attempt, status: StageStatus, latest: bool, live: Option<&str>, now: SystemTime) -> String` (`Working · 2m 15s`, `Waiting for you`, `Ended · 92%`, `Stopped · 64%`, `Ended · no report`); `pub fn panel_for(...) -> Panel`.
- [ ] **Step 1: Test** `gate_badges_follow_the_rating_and_bar`, `session_lines_cover_every_state` and `the_panel_follows_the_selected_session`.
- [ ] **Steps 2–4**; by hand against the prototype: open from the card, select sessions, stop a stage, step in, re-run with a note, stop the ticket.
- [ ] **Step 5: Commit** `feat(hydraui): the Pipeline view`

---

### Task 11: Pause and the orchestrator's status

**Files:**
- Modify: `crates/hydraui/src/workspace/title_bar.rs`, `crates/hydraui/src/workspace.rs` (`render_status`), `crates/hydraui/src/workspace/orchestrator.rs`

**Interfaces:**
- Produces: **Pause orchestrator** / **Resume orchestrator** in the title bar (design: 12px, padding 4×10, radius 6, border `#2a3030`) while the shown environment has an orchestrator with a run, calling `runs.pause`; `pub fn status_text(run: &Run) -> (String, bool /* red */)` in the middle of the status bar; wake-ups (Task 7) wait while paused.
- [ ] **Step 1: Test** `status_text_names_the_stage_or_why_it_waits` and, in Task 7's tests, `a_paused_run_wakes_nobody`.
- [ ] **Steps 2–4**, **Step 5: Commit** `feat(hydraui): pause the orchestrator`

---

### Task 12: Starting an orchestrator, editing pipelines, the usage setting

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

### Task 13: Pipeline settings

**Files:**
- Modify: `crates/hydraui/src/pipeline.rs` (`pub fn save(root: &Path, file_name: &str, edit: &Edit) -> io::Result<()>` with `toml_edit`), `crates/hydraui/Cargo.toml` (`toml_edit.workspace = true`), `crates/hydraui/src/pipeline_view.rs` (the Settings panel)
- Create: `crates/hydraui/src/skills_list.rs` (`pub fn installed(claude_config: Option<&Path>, project: &Path) -> Vec<String>`: the `skills/*/SKILL.md` names under each)

**Interfaces:**
- Produces: `pub struct Edit { name: String, bar: u8, stages: Vec<EditStage { id: Option<String> /* None = new */, title, skill: Option<String>, bar: Option<u8> }> }`; `pub fn validate(edit: &Edit) -> Result<(), &'static str>` ("Give the orchestrator a name", "Every stage needs a name", at least one stage); `save` keeps every key and comment it doesn't own, each kept stage's `prompt` and `worktree`, orders stages as given, gives new stages an id slugged from the title and unique; on save, running runs pick up the new bars at their next gate (Task 4b reads the file there). The view as spec §7.4 and the design (sliders 50–100, Reset, ↑/↓, ×, + Add stage, the Done row, the limits, the save bar).
- [ ] **Step 1: Tests:** `saving_keeps_prompts_and_comments`, `a_new_stage_gets_a_unique_id_from_its_title`, `reordering_and_removing_stages`, `validation_names_what_is_missing`, `installed_skills_are_listed_once_sorted`.
- [ ] **Steps 2–4**; by hand: change a bar mid-run and see the next gate use it. **Step 5: Commit** `feat(hydraui): pipeline settings`

---

### Task 14: Ticket history and the run's worktree

**Files:**
- Create: `crates/hydraui/src/run_history.rs` (`pub fn read(root: &Path) -> Vec<Entry>` from the run logs)
- Modify: `crates/hydraui/src/pipeline_view.rs` (the History panel), `crates/hydraui/src/workspace/orchestrator.rs` (worktree removal on `RunFinished` / `RunStopped`), `crates/hydraui/src/worktrees.rs` (`pub fn remove_if_clean(path: &Path) -> Result<bool, String>`)

**Interfaces:**
- Produces: `Entry { run, ticket, title, outcome: Outcome /* Done, Stopped, InProgress */, detail, took: Duration, stages: Vec<(String, Option<u8>)>, note }`, newest first; a log with no `run ended` line is In progress only for the live run, otherwise Stopped ("hydraui closed"). The History panel as spec §7.5 (filter chips, expanding rows, the current run pinned). On finish or stop, `remove_if_clean` the run's worktree (git worktree remove without `--force`, branch kept); a dirty one stays and the view says so.
- [ ] **Step 1: Tests:** `history_reads_outcomes_details_and_scores_from_the_logs`, `a_log_without_an_end_is_stopped_unless_live`, `a_dirty_worktree_is_kept` (temp git repo).
- [ ] **Steps 2–4**. **Step 5: Commit** `feat(hydraui): ticket history`

---

### Task 15: End to end, both platforms

**Files:**
- Modify: `crates/hydraui/src/mcp/server.rs` (HTTP test)

- [ ] **Step 1:** `a_two_stage_run_over_http` beside `serves_over_http`: real server, registry with an orchestrator and a stage token, the whole run through `ureq` posts, `StageStart` and `RunChanged` events read from `subscribe()`.
- [ ] **Step 2:** Full gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on macOS (pwsh on PATH, as CI) and on Windows CI.
- [ ] **Step 3: By hand (the phase's gate, spec §10)** on macOS and Windows: Start orchestrator… → a real ticket through `ticket.toml`; every stage opens beside, links, reports, shows in the Pipeline view and under the pipeline card; force one stage below its bar (a stage prompt saying to report 40) → the run needs you, the toast and badges appear, **Re-run with a note** starts attempt 2, which the orchestrator follows after its wake-up prompt; Pause holds the next stage; History lists the run; the views match the prototype.
- [ ] **Step 4:** Docs: the spec's open "builder's call" items answered in the spec; the website's hydraui section gains a short "Orchestrator" paragraph.
- [ ] **Step 5: Commit** `test(hydraui): a run end to end; docs`

## Order and size

Tasks 1–4 are pure and independent (they can run in parallel); Task 4b reworks them for the design and comes before Task 5. Task 5 needs 1–4b. Tasks 6–14 need 5 and are mostly independent of each other, except that 8 and 9 open the view built in 10 (stub the click until 10 lands), and 13 and 14 add panels to it. Task 15 is last. Each task is one commit with its tests; none needs a new crate.
