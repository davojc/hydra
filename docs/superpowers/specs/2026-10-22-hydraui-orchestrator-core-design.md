# hydraui: orchestrator core

Status: design, 2026-10-22. Phase 1 of the ticket pipeline (the vision: tickets that finish
themselves, with a person called in whenever a stage isn't confident). Builds on the MCP server
and session hooks (`2026-10-20-hydraui-mcp-server-design.md`), auto-pickup's sending
(`2026-10-20-hydraui-auto-pickup-design.md` §5.1, §6) and "Take in new worktree" (task board).
Everything not named here stays as it is.

## 1. Why

The pipeline the user wants: a Claude session in a hydraui terminal, the **orchestrator**, takes
one ticket at a time through stages (understand and replicate, develop, review, verify). Each stage
is its own fresh Claude session. A stage ends by reporting a **confidence rating**, and the next
stage starts only when that rating clears the bar. Below the bar, everything stops for a person.
The user wants to click the orchestrator and see which stage it is on, every session it started,
and what each one is doing.

This phase builds the machinery every stage needs and nothing ticket-specific: runs, stages,
ratings, gates, linked sessions and the view. Linear, the QA-Skills and iov42-agents stages, the
Slack review loop and Slack escalation come in later phases on top of it.

### Goals

- A run: one ticket going through a pipeline's stages, owned by one orchestrator terminal.
- The orchestrator starts each stage through the MCP server. hydraui opens the stage's terminal
  (in a fresh worktree when the stage asks for one), runs Claude in it with the stage's prompt,
  and links it to the run.
- A stage session ends by reporting a rating (§4) over the MCP server.
- **hydraui enforces the gate** (§5): it refuses to start a stage until the previous stage's rating
  has cleared the bar. A low rating, a blocked stage or a session that dies without reporting stops
  the run and calls a person.
- The orchestrator hears when a stage reports, without the user prompting it (§6).
- The orchestrator view (§7): the run's stages, their sessions, live states and ratings, one click
  from the orchestrator's terminal.

### Non-goals (this phase)

- Polling Linear, choosing tickets, the INTH/SLA filter (phase 2). A run's ticket is whatever the
  orchestrator names (e.g. `INTH-123`).
- The real stage prompts for QA-Skills and iov42-agents (phase 2) and the Slack review loop
  (phase 3). This phase ships an example pipeline with plain prompts, enough to walk a ticket
  through by hand (the phase's gate).
- Slack escalation (phase 3): this phase calls a person through hydraui only (§5.3).
- Worktrees in several repositories. The develop stage will decide which repos a ticket needs
  (the user's decision), but this phase makes one worktree, of the orchestrator's project (§3.2).
- More than one run per orchestrator, or runs that survive a hydraui restart (§8).

### Anti-goals (a failure even if it works)

- A stage starting without the previous stage's rating, or after a rating below the bar, unless a
  person said so in hydraui. The server refuses; it doesn't rely on Claude following descriptions.
- A rating with no evidence or no reasons counting as a pass.
- A stage session acting as another stage, another run, or the orchestrator.
- Typing into the orchestrator's terminal while the user has a half-written prompt there.

## 2. Pipelines

A pipeline is a TOML file, `<HYDRA_HOME>/hydraui/pipelines/<name>.toml`, read when a run starts (so
an edit applies to the next run):

```toml
name = "ticket"
bar = 80                      # default bar, 0-100; 80 when left out

[[stage]]
id = "replicate"
title = "Understand and replicate"
worktree = false              # true: a fresh worktree of the project per run (§3.2)
prompt = """
Read ticket {ticket}. Decide whether it has enough information to act on, then reproduce the
problem it describes. ...
"""

[[stage]]
id = "develop"
title = "Develop"
bar = 85                      # this stage's own bar
worktree = true
prompt = "..."
```

- `id` is unique in the file, letters, digits and `-`. `title` is what the view shows.
- `bar` on a stage overrides the pipeline's for that stage (the user's decision: configurable per
  pipeline and per stage, 80 by default).
- `prompt` is the stage session's first prompt. `{ticket}`, `{run}`, `{stage}` and `{handoff}` (the
  previous stage's handoff, §4) are filled in. Every prompt gets the same closing paragraph from
  hydraui, telling the session to finish with `stage_report` (§3.3), so a pipeline file can't leave
  it out.
- A file that doesn't parse, or has no stages, is refused by `run_start` with the reason.
- hydraui writes `ticket.toml`, the example pipeline with the four stages above and plain prompts,
  when the folder is empty (our call). The launcher gains **Edit pipelines** (opens the folder).

## 3. Runs and stages

### 3.0 Starting an orchestrator

The launcher gains **Start orchestrator…**: pick an environment (one with `[claude]`, since the
MCP server is registered only there) and a pipeline. hydraui opens a terminal in that environment
titled `orchestrator`, running `claude` with a first prompt that gives it the orchestrator's role
(§9) and the pipeline's name. Any Claude session in a hydraui terminal can also become an
orchestrator by calling `run_start` itself (our call: the launcher is a convenience, not a gate).

### 3.1 State

hydraui keeps, per run (in memory, §8):
- the run id (`<ticket>-<n>`, e.g. `INTH-123-1`), its pipeline (as read at start), the ticket, and a
  one-line summary the orchestrator gives;
- the **orchestrator**: the terminal token that started it. A terminal has at most one run that
  isn't finished (the user's decision: one ticket at a time);
- per stage: its status (§3.4), its attempts, the terminal running it, and its rating.

### 3.2 Starting a stage

The orchestrator calls `stage_start(run, stage, brief)`. hydraui:
1. checks the gate (§5): refused with the reason, and nothing opens, unless the stage may start;
2. when the stage has `worktree = true` and the run has no worktree yet, makes one the way "Take in
   new worktree" does (`worktrees::create`), named after the run, and records it. Later worktree
   stages of the run reuse it. Its merge stays the person's, as for cards (the user's decision: a
   person merges);
3. opens a terminal in the **orchestrator's environment**, in the worktree or else the
   orchestrator's folder, titled `<run> · <stage title>`, running `claude "<prompt>"` (as
   `open_worktree` does), with `brief` added after the stage's prompt;
4. registers the new terminal's token as that stage's session (§3.3) and marks the stage
   `running`.

Opening a terminal does not take focus from the one the user is in (our call: the pipeline runs
beside the user, not in their face). `open_terminal` shows and focuses the new terminal today, so
the stage path needs a variant that adds it without `focus_soon` and without changing which
terminal is shown (builder's call how).

### 3.3 Which terminal may do what

The MCP server already knows the terminal behind every call (its token). This phase adds a role
per token:
- **orchestrator**: the token that called `run_start`, for that run;
- **stage session**: a token hydraui registered in step 4 above, for that run and stage only.

`run_*` and `stage_start` are refused for a stage session. `stage_report` and `stage_brief` are
refused for anything but the stage's own session. A stage session that starts a run of its own is
refused too (our call: stages don't nest).

### 3.4 Stage status

`waiting` (not started) → `running` → one of:
- `passed`: reported, outcome `done`, score at or above the bar;
- `stopped`: reported below the bar, or outcome `blocked`;
- `failed`: the session ended (`SessionEnd`, the terminal closed, or its shell exited) without
  reporting.

A person can turn `stopped` or `failed` into `overridden` (§5.3), which counts as passed and records
who decided and why.

## 4. The rating

A stage session reports once, at its end, with `stage_report`:

```json
{
  "outcome": "done",
  "score": 86,
  "summary": "Reproduced: saving a delivery with no lines returns 500.",
  "evidence": [
    {"kind": "test",    "detail": "e2e deliveries.spec.ts: 1 failing, reproduces the 500"},
    {"kind": "command", "detail": "curl -X POST .../deliveries -> 500, body attached below"},
    {"kind": "file",    "detail": "backend: DeliveryService.java:142, null lines not handled"}
  ],
  "reasons": "Fails the same way on a clean checkout, three runs out of three.",
  "handoff": "Fix where DeliveryService.save reads lines; the failing e2e test is the check."
}
```

- `outcome`: `done`, or `blocked` (the stage couldn't do its job: not enough information in the
  ticket, no access, a dependency down). A `blocked` stage stops the run whatever its score.
- `score`: an integer 0 to 100, the session's confidence that the stage is done well.
- `summary`: one or two sentences, shown on the stage's row.
- `evidence`: at least one item. `kind` is `test`, `command`, `file`, `link` or `note`; `detail` is
  free text, up to ~500 characters each, up to 20 items (our call).
- `reasons`: why this score: what makes it this high and what keeps it from higher. Required.
- `handoff`: what the next stage needs to know; empty allowed for the last stage. Filled into the
  next stage's `{handoff}`.

A report missing `reasons` or evidence, with a score outside 0 to 100 or an unknown outcome, is
refused with what's wrong, so the session can fix it and report again. Once a report is accepted,
the stage is decided; a second report is refused (our call). Retrying a stage (§5.2) opens a new
session that reports afresh.

The rating is the user's decision: a score, the evidence and the reasons.

## 5. The gate

### 5.1 When a stage may start

`stage_start(run, stage)` is allowed only when all of these hold; otherwise it is refused with the
first one that fails:
1. the caller is the run's orchestrator and the run isn't finished or stopped;
2. no stage of the run is `running`;
3. `stage` is the first stage, or every stage before it is `passed` or `overridden`;
4. `stage` itself is `waiting`, or `failed`/`stopped` and being retried within its attempts
   (§5.2);
5. **usage**: the orchestrator environment's latest Claude report (§5.2 of the MCP server spec)
   shows at least 30% of the seven-day window left (the user's decision). With no report yet, it is
   allowed (our call: the first stage's own session produces one). The share is configurable:
   `min_week_left = 30` in `hydraui/config.toml`.

### 5.2 Retries

A stage that is `failed` (its session died, or a service it needed was down and it reported
`blocked` with evidence saying so) may be started again by the orchestrator, up to **three
attempts** in all (the user's decision: retry three times, then wait for a person). Each attempt is
a new session; the view keeps the earlier ones. A `stopped` stage (a low score) is not retried by
the orchestrator: it waits for a person (our call: a low score is a judgement, not a glitch).

### 5.3 Calling a person

When a stage becomes `stopped`, or `failed` with its attempts used up, or the usage check (§5.1.5)
refuses a start, the run becomes **stopped** and hydraui:
- marks the orchestrator's row and the run in the view amber, with the reason (the stage, its score
  against the bar, its summary);
- alerts as for "Claude needs you" (MCP server spec §5.1 Alerts): tab and row badges, the window
  flashing in the background, a toast that stays until closed and shows the run when clicked.

The run's view (§7) then offers the person three choices:
- **Continue anyway**: the stage becomes `overridden`; the person may add a note. The orchestrator
  is told (§6) and may start the next stage.
- **Retry stage**: one more attempt, past the limit if need be; optionally with a note added to
  the stage's prompt.
- **Stop run**: the run ends as `stopped`; its terminals stay open for the person.

The Slack message to Jordan (the user's decision) is added in phase 3, beside the same alert.

## 6. Telling the orchestrator

When a stage is decided (passed, stopped, failed) or a person chooses in the view, hydraui types a
one-line prompt into the orchestrator's terminal, e.g.

```
Run INTH-123-1: stage replicate passed (86, bar 80). Call run_status, then start the next stage.
```

using auto-pickup's sending (its §6: typed input then a separate Enter, a draft flag, the
reservation) and only when the orchestrator is idle with no draft (its §5.1 checks 3 and 6). While
it is working or has a draft, the message waits, newest kept, and goes when it next becomes idle
(our call). The wording is the builder's call; it must name the run, the stage and the result, and
send the orchestrator to `run_status`.

The orchestrator may also call `run_status` at any time (its own polling). Typing is the reliable
wake-up; Claude Code's Channels were set aside for the reasons in auto-pickup §1.

## 7. The orchestrator view

- **In the terminal list**, a terminal with a run shows a small run badge after its name: the
  ticket and the current stage, e.g. `INTH-123 · 2/4 Develop`, amber when the run is stopped. The
  run's stage terminals are listed directly under it, indented, each with its stage title and the
  usual activity dot and state (working / needs you / idle). (Look: builder's call.)
- **Clicking the badge** opens a **Run** tab in the strip above the orchestrator's terminal, as the
  Tasks tab opens (beside the terminal's own tab; ✕ closes it). It shows:
  - the ticket, summary, pipeline and bar, and the run's state (running, stopped with its reason,
    finished);
  - one row per stage, in order: title, status, attempts, the score drawn against that stage's bar,
    the summary. A row expands to the full evidence, reasons and handoff, and each attempt's
    session;
  - for a running stage, its session's live state from the hooks. Clicking a session shows its
    terminal;
  - for a stopped run, the three choices of §5.3.
- **From any stage terminal**, the same badge leads back to its run.

The view reads hydraui's run state and updates as it changes; it never writes ratings.

## 8. Lifetime and records

- Runs live in hydraui's memory, as terminal tokens do. A hydraui restart ends them: terminals get
  new tokens, so their links can't be kept (our call for phase 1; resuming runs across restarts is
  a later phase).
- Every change of a run is also appended to `<HYDRA_HOME>/hydraui/runs/<run>.jsonl`: run started,
  stage started (terminal name), report (the whole rating), person's choice (with note), run
  ended. That's the audit trail; the view doesn't read it (our call).
- A run is **finished** when its last stage is `passed` or `overridden`. The orchestrator may then
  `run_start` the next ticket.
- Stage terminals stay open after their stage is decided, so a person can read them; they close
  like any terminal. A run's view keeps showing a closed session's rating (our call).

## 9. The tools

Added to the MCP server's list; descriptions carry the workflow (wording: builder's call). hydra
pre-approves them by name in each environment's generated `settings.json`, as it does the task
tools (MCP server spec §3).

Orchestrator:
- **`run_start(pipeline, ticket, summary)`**: starts a run; returns its id, the stages with their
  bars, and the first stage to start. Refused while the caller has a run that isn't finished or
  stopped, or the pipeline can't be read.
- **`stage_start(run, stage, brief)`**: §3.2, behind the gate (§5).
- **`run_status(run?)`**: the run (default: the caller's current one): each stage's status,
  attempts, rating and session name and live state, plus whether a stage may start now and, if
  not, why. Read-only.

Stage session:
- **`stage_brief`**: the run, ticket, this stage's title and bar, and the previous stage's
  summary and handoff. Read-only.
- **`stage_report(outcome, score, summary, evidence, reasons, handoff)`**: §4.

There are no tools to override a stage, change a bar, skip a stage or mark a run finished. Those
are the person's, in the view.

**What the instructions tell each role** (wording: builder's call):
- The orchestrator: you coordinate and never do a stage's work yourself; start stages in order with
  `stage_start`; wait to be told a stage is decided (or check `run_status`); when a run stops, tell
  the user why and wait.
- A stage session: do this stage only; finish with `stage_report`, with honest evidence and reasons;
  report `blocked` rather than guess when the stage can't be done.

## 10. Testing

Unit tests: pipeline parsing (bars, defaults, unknown keys, duplicate ids, the closing paragraph
always added, placeholders filled); every gate rule (§5.1) including the usage line with and
without a report; retries counting attempts and stopping at three; `stopped` never retried by the
orchestrator; roles (§3.3) refusing each forbidden call; rating validation (each missing or bad
field); one report per stage; a session ending without a report making the stage `failed`; the
person's three choices; the run log's lines. An end-to-end test drives the HTTP endpoint through a
two-stage run with JSON-RPC, as the task tools' test does. Checked by hand (the phase's gate): one
real ticket walked through the example pipeline in hydraui on Windows and on macOS, every stage
rated and visible in the view, one stage stopped on purpose and continued by a person.

## 11. Calls made without the user

Marked "our call" / "builder's call" above, plus: the orchestrator is a Claude session driving MCP
tools, with hydraui holding the run state and enforcing the gate; pipelines are TOML files in
`HYDRA_HOME`; stage terminals open in the orchestrator's environment without taking focus; one
worktree per run, shared by its worktree stages; the orchestrator is woken by a typed prompt; runs
end with hydraui; the audit log is JSON lines.

Decided by the user: one ticket at a time; a rating is a score, evidence and reasons; the bar is
configurable per pipeline and per stage, 80 by default; three retries, then a person; stop when
less than 30% of the seven-day Claude window is left; a person is called through hydraui and
Slack (Slack in phase 3); a person merges; the orchestrator view shows the current stage and every
linked session with what it is doing.
