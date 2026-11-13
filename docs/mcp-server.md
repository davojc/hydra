# hydraui's MCP server

Each hydraui window runs a small MCP server called `hydraui`. Every Claude session in that window's terminals connects to it on its own, with nothing for you to set up. The server is how Claude works with hydraui itself: the task board, pipelines, Projects and swarm claims.

This page is the reference. The original design is in [`superpowers/specs/2026-10-20-hydraui-mcp-server-design.md`](superpowers/specs/2026-10-20-hydraui-mcp-server-design.md), and each group of tools has its own spec (linked below). Where a spec and the code differ, the code is right: the tool list here comes from `crates/hydraui/src/mcp/`.

## How sessions find it

- hydraui starts the server when it opens and stops it when it closes. It listens on `127.0.0.1` only, on a port the OS picks, so several hydraui windows never clash. The path is `/mcp`, using MCP's Streamable HTTP transport (plain JSON responses).
- Each terminal hydraui opens gets two environment variables: `HYDRA_MCP_URL` (where the window's server is) and `HYDRA_MCP_TOKEN` (a token for that terminal only).
- Every hydra environment's Claude config lists the server as:

  ```json
  "hydraui": {
    "type": "http",
    "url": "${HYDRA_MCP_URL}",
    "headers": { "Authorization": "Bearer ${HYDRA_MCP_TOKEN}" }
  }
  ```

  hydra adds this to the environment's `.claude.json` at each launch, together with your own servers from `~/.claude.json` (minus any `--mcp-exclude` patterns). See [Claude Code](../README.md#claude-code-separate-accounts-shared-setup) in the README.
- **Outside hydraui** (a plain terminal in a hydra environment) the two variables aren't set, so Claude shows `hydraui` as failed to connect. That's expected.

## Security

- **Loopback only, and a token on every request.** A request without a known token gets `401`. A request whose `Origin` header isn't a localhost origin gets `403` (this guards against DNS rebinding). Anything other than `POST /mcp` is refused.
- **The token says which terminal is calling.** A closed terminal's token stops working. Each tool acts only for that terminal: its project's board, its own pipeline run, its own Projects ticket, its own claims.
- **The server enforces the rules itself rather than trusting Claude to follow them.** For example, nothing can move a card into Pickup or Done, a stage can't start before the one before it clears its bar, and plan tools refuse any session that isn't a planning session.

## Permissions

hydra adds `mcp__hydraui__<tool>` to every environment's `permissions.allow` for 30 of the 34 tools, so Claude doesn't ask before using them. The four `claims_*` tools are left out: they follow the session's permission mode, because nothing undoes `claims_done` and it holds the key for every teammate.

## What Claude is told

When a session connects, the server's `instructions` explain the task board workflow, `hydra_access`, how orchestrator and stage sessions behave in a pipeline, and how to use claims. The text is `INSTRUCTIONS` in `crates/hydraui/src/mcp/tools.rs`. Each tool's description adds the details for that tool. Together they're meant to be enough for Claude without you explaining anything.

## Tools

### Task board and environment

The project's Tasks tab, stored in the project's `.hydra/tasks.md`. Spec: [task board](superpowers/specs/2026-10-19-hydraui-task-board-design.md).

| Tool | What it does |
|---|---|
| `tasks_read` | The whole board: each column's cards in order, who took each InProgress, Verify or Done card, the calling session's own name, and the other Claude sessions working in the project (`other_sessions`). |
| `tasks_add` | Adds an idea or follow-up to the top of Backlog. It never goes into Pickup. |
| `tasks_take` | Takes one card from Pickup into InProgress, with the session's name on it. |
| `tasks_verify` | Moves one of the session's InProgress cards to Verify, recording the commit that holds the work. |
| `tasks_mine` | Lists the InProgress cards this session took. |
| `hydra_access` | What the terminal's hydra environment gives agents: the tools it has signed in (github, aws, claude, ...) and the credentials you allow agents to use, each with what it's for. Credentials are environment variables already set in the shell. Their values are never returned. |

Only you move cards into Pickup or Done, or edit, reorder or delete them. Writes follow the board's merge-on-write rules, so they never overwrite a change made in the Tasks tab or by hand.

### Pipelines

These take one ticket through a pipeline's stages (the Pipeline view). The orchestrator session coordinates the run, and each stage is a fresh Claude session that ends with a confidence rating. Pipelines are TOML files in `<HYDRA_HOME>/hydraui/pipelines`. Spec: [orchestrator core](superpowers/specs/2026-10-22-hydraui-orchestrator-core-design.md).

| Tool | Who uses it | What it does |
|---|---|---|
| `run_start` | Orchestrator | Starts a run of `ticket` through `pipeline` and returns the run's id, its stages (skills and confidence bars) and the first stage. It's refused once `runs_at_once` runs are still going or waiting for a person. |
| `stage_start` | Orchestrator | Opens a fresh Claude session for the next stage, with the stage's prompt and an optional brief. It's refused (with the reason) unless every earlier stage cleared its bar, no stage is running, you haven't paused the run and enough of the seven-day Claude limit is left. |
| `run_status` | Orchestrator | Shows the run's state (running, needs_you with the reason, finished, stopped), whether it's paused, each stage's attempts and ratings, and whether the next stage may start. |
| `stage_brief` | Stage | Shows what this stage is: the ticket, its skill and bar, this attempt, a person's note if they re-ran it, and the previous stage's summary and handoff. |
| `stage_report` | Stage | Ends the stage, once: `done`, `blocked` or `check`, a 0–100 score, a summary, evidence, reasons for and against, a handoff, and optional screenshots. Below the bar, or blocked or check, a person is asked to step in. |
| `stage_ask` | Stage | The stage waits on someone: a product decision (`who: "product"`) or a pull request author (`who: "author"`, with the PR link). The run waits but isn't marked as needing you. |
| `stage_answer` | Stage | Records the answer to a `stage_ask` so the stage can carry on. |

Which role a session has comes from its token: hydraui knows which terminal it opened for each stage.

### Projects

Projects are planned and then worked in hydraui's Projects tab. hydraui starts a planning, work or testing session for a project, and each session sees only its own project or ticket. Spec: [Projects](superpowers/specs/2026-11-03-hydraui-projects-design.md). A session of the wrong kind gets a refusal that says which session to start instead.

**Planning sessions** write the project's spec and propose milestones and tickets for the lead to review:

| Tool | What it does |
|---|---|
| `plan_show` | Shows the project as it stands: spec, milestones and tickets with their review state, open questions and resources. |
| `plan_set_spec` | Replaces the spec (Markdown). This only works before any milestone is approved. |
| `plan_propose_milestone` | Proposes a milestone: a plain-words outcome (title at most 60 characters, 1–4 points of at most 80, each with a detail). |
| `plan_propose_ticket` | Proposes a ticket in an approved milestone, in one repo, with how to tell it's done. |
| `plan_suggest_repo` | Tells the lead about a repo the project will probably change, and why. |
| `plan_revise` | Revises a milestone or ticket that's still to review or had changes asked. A ticket's repo can be set even after approval. |
| `plan_propose_change` | Proposes a change to (or `drop` of) an approved milestone or ticket after new requirements. The approved version stays until the lead accepts the change. |
| `plan_propose_spec` | Proposes a new version of the agreed spec, for the lead to accept. |
| `plan_withdraw` | Withdraws a proposal that hasn't been approved. |
| `plan_add_question` | Asks the lead (or someone else) to decide something. It shows in hydraui with the options as buttons, and the answer comes back to the session. |
| `plan_add_link` | Attaches a link to the project's resources. |
| `plan_read_resource` | Reads an attached file: its text, or its path if it isn't text. |
| `plan_done` | Ends the job. hydraui closes the session and shows the lead its summary. |

**Work and testing sessions** do one ticket:

| Tool | Who uses it | What it does |
|---|---|---|
| `ticket_show` | Work, testing | Shows the ticket, its milestone, the project's spec and its open questions. |
| `ticket_ask` | Work, testing | Asks a question someone must decide. It shows as a decision on the ticket. |
| `ticket_report` | Work | Finishes the ticket with its pull requests (none if no code changed) and a summary. It can also list tasks only a person can do, which become tasks in the milestone. |
| `ticket_checked` | Testing | Reports the check of the ticket's pull requests. A pass puts the ticket in review. The first fail sends it back to a work session, and the second stops the run for a person. |

### Swarm claims

Claims stop two teammates' sessions from working on the same shared job. A claim is a `key` within a `context` (the kind of work, as your team names it), in one of your swarms. Claims go through whichever hydraui window is running your swarms, so they can take a few seconds. Spec: [swarm claims](superpowers/specs/2026-11-04-swarm-claims-design.md).

| Tool | What it does |
|---|---|
| `claims_take` | Claims a job before starting it. Only an answer saying the session holds the claim means go ahead. hydraui keeps the claim alive while the terminal is open and tells the session if it's lost. |
| `claims_release` | Gives up a claim without finishing, so someone else can take the job. |
| `claims_done` | Marks the job finished, so nobody works on it again. This can't be undone. |
| `claims_list` | Shows a context: its settings, who holds which key and what was finished recently. |

## Notifications from hydra

The server also accepts three JSON-RPC notifications (no `id`, answered `202`) from hydra's own commands. That's how hydraui knows what each session is doing:

| Method | Sent by | Carries |
|---|---|---|
| `hydraui/status` | `hydra hook <state>` (Claude Code hooks) | The session's state (`working`, `waiting`, `idle`, `ended`, or `resumed`) and its Claude session id |
| `hydraui/usage` | `hydra statusline` | Claude's status JSON: five-hour and seven-day limits, and how full the context is |
| `hydraui/skill` | `hydra hook` | The skill the session just used |

## Adding a tool

1. Add its schema and handler in the right module under `crates/hydraui/src/mcp/`: `tools.rs` (board and access), `run_tools.rs`, `project_tools.rs` or `claims_tools.rs`.
2. Add its name to `HYDRAUI_TOOLS` in `crates/hydra-providers/src/claude_files.rs`. If Claude may use it without asking, add it to `HYDRAUI_ALLOWED` too.
3. If sessions need to know when to use it, update `INSTRUCTIONS` in `tools.rs`.
4. Update the tables on this page.
