# hydraui: orchestrator core

Status: design, 2026-10-22; revised the same day to follow the design handoff
(`2026-10-22-hydraui-orchestrator-design-handoff.md`). Phase 1 of the ticket pipeline (the vision:
tickets that finish themselves, with a person called in whenever a stage isn't confident). Builds
on the MCP server and session hooks (`2026-10-20-hydraui-mcp-server-design.md`), auto-pickup's
sending (`2026-10-20-hydraui-auto-pickup-design.md` §5.1, §6) and "Take in new worktree" (task
board). Everything not named here stays as it is.

## 1. Why

The pipeline the user wants: a Claude session in a hydraui terminal, the **orchestrator**, takes
one ticket at a time through stages (understand and replicate, develop, review, verify). Each stage
is its own fresh Claude session. A stage ends by reporting a **confidence rating**, and the next
stage starts only when that rating clears the bar. Below the bar, everything stops for a person.
The user wants to click the orchestrator and see which stage it is on, every session it started,
and what each one is doing.

This phase builds the machinery every stage needs and nothing ticket-specific: runs, stages,
ratings, gates, linked sessions and the views. Linear, the QA-Skills and iov42-agents stages, the
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
- The views of the design handoff (§7): the pipeline card, the Pipeline view with every stage and
  session, Pipeline settings, and Ticket history.

### Non-goals (this phase)

- Polling Linear, choosing tickets, the INTH/SLA filter (phase 2). A run's ticket is whatever the
  orchestrator names (e.g. `INTH-123`). So the design's idle "Watching Linear" state, **Check Linear
  now**, the SLA pill, the Linear link and the History SLA column come with phase 2.
- The real stage prompts for QA-Skills and iov42-agents (phase 2) and the Slack review loop
  (phase 3). This phase ships an example pipeline with plain prompts, enough to walk a ticket
  through by hand (the phase's gate).
- Slack escalation (phase 3): this phase calls a person through hydraui only (§5.3). The toast's
  "Also sent to you as a Slack DM" line appears once phase 3 sends it.
- Worktrees in several repositories, and the design's repo chips on the develop stage. The develop
  stage will decide which repos a ticket needs (the user's decision), but this phase makes one
  worktree, of the orchestrator's project (§3.2).
- More than one run per orchestrator, or runs that survive a hydraui restart (§8).
- The design's restyle of hydraui's shell (title and status bars, the environment list, the Geist
  fonts). The orchestrator's own views use the design's colours, sizes and copy; the rest of
  hydraui keeps its look (our call: restyling every user's hydraui is davojc's decision, and a PR
  of its own).

### Anti-goals (a failure even if it works)

- A stage starting without the previous stage's passing rating. Nothing skips a gate: no tool, no
  setting and no button. A person can re-run a stage or stop the ticket, never wave it through (the
  design: "Gates can't be skipped"). The server refuses; it doesn't rely on Claude following
  descriptions.
- A rating with no evidence or no reasons counting as a pass.
- A stage session acting as another stage, another run, or the orchestrator.
- Typing into the orchestrator's terminal while the user has a half-written prompt there.

## 2. Pipelines

A pipeline is a TOML file, `<HYDRA_HOME>/hydraui/pipelines/<name>.toml`. Its stage list is copied
when a run starts (an added, removed or reordered stage applies to the next ticket); its name, bars
and each stage's own settings (title, skill, folder, prompt) are read again on Save and at every
gate (§5.1), so a new name shows on the running ticket's card at once, a changed bar applies at its
next gate, and a stage's settings apply when it next starts, running ticket included (the design,
extended after the walkthrough: a folder set before stage 1 started didn't reach it). Run ids skip
ones already in the run logs, so a run after a restart never shares a log.

```toml
name = "INTH SLA tickets"     # the orchestrator's name in the views, notifications and Slack
bar = 80                      # default bar, 0-100; 80 when left out

[[stage]]
id = "replicate"
title = "Understand and replicate"
skill = "QA-Skills"           # the skill the stage's session uses; shown in the views
worktree = false              # true: a fresh worktree of the project per run (§3.2)
prompt = """
Read ticket {ticket}. Decide whether it has enough information to act on, then reproduce the
problem it describes. ...
"""

[[stage]]
id = "develop"
title = "Develop"
skill = "iov42-agents"
bar = 85                      # this stage's own bar
worktree = true
prompt = "..."
```

- `id` is unique in the file, lower-case letters, digits and `-`. `title` is what the views show.
- `bar` on a stage overrides the pipeline's for that stage (the user's decision: configurable per
  pipeline and per stage, 80 by default).
- `folder` is optional: the folder the stage's session starts in, such as `~/Projects/QA-Skills`,
  so the session loads that project's skills and instructions as when you start Claude there
  yourself (the user's way of choosing them, added during the walkthrough). It takes the place of
  the orchestrator's folder and the run's worktree; a folder that doesn't exist fails the start.
- `skill` is optional. `prompt` is the stage session's first prompt, as short as you would type
  it yourself (`replicate {ticket}`): the skill knows what to do. When left out it is `Do the
  "{title}" stage of ticket {ticket}.`. `{ticket}`, `{ticket_lower}`, `{run}`, `{stage}`,
  `{handoff}` (the previous stage's handoff, §4) and `{feature_env}` are filled in.
- `feature_env` (pipeline-wide, optional) is where a ticket's fix runs, as a pattern with
  `{ticket}`/`{ticket_lower}`, e.g. `https://fev2-solutions-{ticket_lower}.dev.iov42.net`.
- After the prompt, hydraui adds one short note of its own, so a pipeline file can't leave it
  out: the pipeline, ticket and stage (n of m), the stage's skill, which stages came before (with
  their scores) and which come after, and to finish with `stage_report` (`stage_brief` has what
  earlier stages handed over). It replaced a longer preamble and closing after the walkthrough,
  where the user found the prompts too large and rigid for skills that need only a line.
- A file that doesn't parse, or has no stages, is refused by `run_start` with the reason.
- hydraui writes `ticket.toml`, the example pipeline with the four stages above and plain prompts,
  when the folder is empty (our call). Pipeline settings (§7.4) edit the file; the launcher's
  **Edit pipelines** opens the folder for prompts, which the settings view doesn't show.

## 3. Runs and stages

### 3.0 Starting an orchestrator

The launcher gains **Start orchestrator…**: pick an environment (one with `[claude]`, since the
MCP server is registered only there) and a pipeline. hydraui opens a terminal in that environment
titled `orchestrator`, running `claude` with a first prompt that gives it the orchestrator's role
(§9) and the pipeline's name. Any Claude session in a hydraui terminal can also become an
orchestrator by calling `run_start` itself (our call: the launcher is a convenience, not a gate).

### 3.1 State

hydraui keeps, per run (in memory, §8):
- the run id (`<ticket>-<n>`, e.g. `INTH-123-1`), its pipeline (stage list as read at start), the
  ticket, a one-line summary the orchestrator gives (the ticket's title in the views), and when it
  started;
- the **orchestrator**: the terminal token that started it. A terminal has at most one run that
  is going or waiting for a person (the user's decision: one ticket at a time);
- whether a person paused it (§5.4);
- per stage: its status (§3.4) and its **attempts**, each one a session: its label (`s-01`,
  `s-02`, … numbered across the run, as the design shows them), the terminal running it, when it
  started, the person's note it was started with, and its rating.

### 3.2 Starting a stage

The orchestrator calls `stage_start(run, stage, brief)`. hydraui:
1. checks the gate (§5): refused with the reason, and nothing opens, unless the stage may start;
2. when the stage has `worktree = true` and the run has no worktree yet, makes one the way "Take in
   new worktree" does (`worktrees::create`), named after the run, and records it. Later worktree
   stages of the run reuse it. Its merge stays the person's, as for cards (the user's decision: a
   person merges);
3. opens a terminal in the **orchestrator's environment**, in the worktree or else the
   orchestrator's folder, titled `<label> · <stage title>` (e.g. `s-02 · Develop`), running
   `claude "<prompt>"` (as `open_worktree` does), with `brief` added after the stage's prompt;
4. registers the new terminal's token as that attempt's session (§3.3) and marks the stage
   `running`.

Opening a terminal does not take focus from the one the user is in (our call: the pipeline runs
beside the user, not in their face). `open_terminal` shows and focuses the new terminal today, so
the stage path needs a variant that adds it without `focus_soon` and without changing which
terminal is shown (builder's call how). The Pipeline view, when open, selects the new session
(the design).

### 3.3 Which terminal may do what

The MCP server already knows the terminal behind every call (its token). This phase adds a role
per token:
- **orchestrator**: the token that called `run_start`, for that run;
- **stage session**: a token hydraui registered in step 4 above, for that run and stage only, and
  only while it is the stage's latest attempt.

`run_*` and `stage_start` are refused for a stage session. `stage_report` and `stage_brief` are
refused for anything but the stage's latest session. A stage session that starts a run of its own
is refused too (our call: stages don't nest).

### 3.4 Stage status

`waiting` (not started) → `running` → one of:
- `passed`: reported, outcome `done`, score at or above the bar;
- `stopped`: reported below the bar, or outcome `blocked`;
- `failed`: the session ended (`SessionEnd`, the terminal closed, or its shell exited) without
  reporting.

Nothing else clears a stage: there is no override (anti-goals). A `stopped` or `failed` stage goes
back to `waiting` only when it is re-run (§5.2, §5.3).

### 3.5 Run state

- `running`;
- `needs you`, with the reason: waiting for a person (§5.3);
- `finished`: the last stage passed;
- `stopped`, with the reason: a person stopped the ticket, or the orchestrator's terminal closed.
  Final, as `finished` is; the orchestrator may then start the next ticket.

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
  "reasons": [
    {"side": "for",     "text": "Fails the same way on a clean checkout, three runs out of three."},
    {"side": "against", "text": "Not tried with the mobile client."}
  ],
  "handoff": "Fix where DeliveryService.save reads lines; the failing e2e test is the check."
}
```

- `outcome`: `done`, or `blocked` (the stage couldn't do its job: not enough information in the
  ticket, no access, a dependency down). A `blocked` stage stops the run whatever its score.
- `score`: an integer 0 to 100, the session's confidence that the stage is done well.
- `summary`: one or two sentences: the stage's row, and the headline of a stopped stage's banner.
- `evidence`: at least one item. `kind` is `test`, `command`, `file`, `link` or `note`; `detail` is
  free text, up to ~500 characters each, up to 20 items (our call).
- `reasons`: why this score, at least one: `for` what makes it this high, `against` what keeps it
  from higher (the design's `+` and `−` bullets). `text` up to ~500 characters, up to 20 (our
  call).
- `handoff`: what the next stage needs to know; empty allowed for the last stage. Filled into the
  next stage's `{handoff}`.

A report missing reasons or evidence, with a score outside 0 to 100 or an unknown outcome, is
refused with what's wrong, so the session can fix it and report again. Once a report is accepted,
the attempt is decided; a second report is refused (our call). Re-running a stage (§5.2, §5.3)
opens a new session that reports afresh; the earlier attempts keep their ratings.

The rating is the user's decision: a score, the evidence and the reasons.

**The stage's skill is enforced** (added after the walkthrough, the user's requirement: "absolutely
sure that it uses the skills set in the pipeline settings"):
- before a stage with a `skill` starts, hydraui checks the skill is installed where its session
  will look (the stage's folder's `.claude/skills`, the environment's skills, claude.ai's synced
  skills, or a plugin's); if not, the start fails with that reason;
- its prompt opens with an instruction to use that skill, put there by hydraui whatever the
  prompt's own text says;
- hydra's Claude hooks report every skill a session invokes (`PreToolUse` on the Skill tool runs
  `hydra hook skill`, which sends `hydraui/skill`), and `stage_report` is refused until the
  stage's latest session has used its skill (by name, ignoring case; a plugin's `name:skill`
  counts). Each use is written to the run log.

## 5. The gate

### 5.1 When a stage may start

`stage_start(run, stage)` is allowed only when all of these hold; otherwise it is refused with the
first one that fails:
1. the caller is the run's orchestrator and the run is `running`;
2. a person hasn't paused it (§5.4);
3. no stage of the run is `running`;
4. `stage` is the first stage, or every stage before it is `passed`;
5. `stage` itself is `waiting`, or `failed` and being retried within its attempts (§5.2);
6. **usage**: the latest Claude report from one of the run's sessions shows at least 30% of the
   seven-day window left (the user's decision). With no report yet, it is allowed (our call: the
   first stage's own session produces one). The share is configurable: `min_week_left = 30` in
   `hydraui/config.toml`.

A stage's bar is the pipeline file's as it is at the moment the stage reports (§2), falling back
to the bar the run started with when the file no longer has that stage or doesn't parse.

### 5.2 Retries

A stage that is `failed` (its session died, or its terminal couldn't open) may be started again by
the orchestrator, up to **three attempts** in all (the user's decision: retry three times, then
wait for a person). Each attempt is a new session; the views keep the earlier ones. A `stopped`
stage (a low score, or blocked) is not retried by the orchestrator: it waits for a person (our
call: a low score is a judgement, not a glitch).

### 5.3 Calling a person

When a stage becomes `stopped`, or `failed` with its attempts used up, or the usage check (§5.1.6)
refuses a start, the run **needs you** and hydraui:
- marks the pipeline card **Needs you** and the stage red in the views, with the reason;
- alerts as for "Claude needs you" (MCP server spec §5.1 Alerts): badges, the window flashing in
  the background, and the design's escalation toast ("Pipeline stopped · needs you", the ticket and
  stage, the score against the bar and the first reason against), which stays until closed; **Open
  ticket** shows the Pipeline view with the stopped session selected.

The Pipeline view then offers the person (the design's action bar):
- **Step into s-NN**: shows that session's terminal, live, for the person to work in it (§7.2).
  The orchestrator waits. **Hand back** re-runs the stage, as below.
- **Re-run <stage> with a note**: a new attempt, past the limit if need be. Its prompt carries the
  previous attempt's report (score, summary, reasons) and the note, which is required. The run is
  `running` again and the orchestrator is told (§6); it still has to clear the bar. For a usage
  stop (the stage never started) this is how the person resumes the run.
- **Stop this ticket**, after an inline confirm: the run ends `stopped` ("stopped by you at
  <stage>"), its worktree is removed (§8) and the orchestrator may start the next ticket. Its
  terminals stay open for the person.

The Slack message to Jordan (the user's decision) is added in phase 3, beside the same alert.

### 5.4 Pause

**Pause orchestrator** (the design's title-bar button) pauses its current run: no stage starts
(§5.1.2) and no wake-up prompt is typed (§6) until **Resume**. A stage session already running
keeps working and may report; its gate is decided as usual, and the next stage waits for Resume
(our call: hydraui can't freeze a Claude session mid-work, and killing it would lose the work).
The pipeline card shows **Paused**.

## 6. Telling the orchestrator

When a stage is decided (passed, stopped, failed) or a person chooses in the view, hydraui types a
one-line prompt into the orchestrator's terminal, e.g.

```
Run INTH-123-1: stage replicate passed (86, bar 80). Call run_status, then start the next stage.
```

using auto-pickup's sending (its §6: typed input then a separate Enter, a draft flag, the
reservation) and only when the orchestrator is idle with no draft (its §5.1 checks 3 and 6) and the
run isn't paused. Until then the message waits, newest kept, and goes when it next can (our call).
The wording is the builder's call; it must name the run, the stage and the result, and send the
orchestrator to `run_status`.

The orchestrator may also call `run_status` at any time (its own polling). Typing is the reliable
wake-up; Claude Code's Channels were set aside for the reasons in auto-pickup §1.

## 7. The views

The look and behaviour follow the design handoff and its prototype: its colours, sizes, copy and
states, for everything in this section. Where hydraui already has the piece (terminals, toasts,
the usage numbers), the view uses it rather than drawing a copy. Mapping the design onto hydraui:

### 7.1 Where things go

- The design's sidebar is hydraui's **terminal list**. The pipeline card sits at the top of the
  orchestrator's environment's list, above its terminals; a run's stage terminals are grouped under
  the card, indented, named `s-NN · <stage title>`, with the usual activity dot.
- The design's main area is the **middle**: the Pipeline, Pipeline settings and Ticket history
  views open as a tab in the strip above the orchestrator's terminal, as the Tasks tab opens
  (✕ closes it); the sub-links switch which of the three it shows.
- The design's title-bar button (**Pause/Resume orchestrator**) goes in hydraui's title bar while
  the shown environment has an orchestrator; its status-bar text (the orchestrator's state, red
  when it needs you) goes in the middle of hydraui's status bar.
- The design's allowance meter goes at the foot of the terminal list, from the environment's latest
  usage report (the numbers the status bar already shows), with its tick at `min_week_left` and
  "Pipeline stops below N%".

### 7.2 The pipeline card and the Pipeline view

- **Card**: the diamond and the pipeline's `name`; `{ticket} · {current stage}` and a state pill
  (Working, Needs you, Paused, Done; Polling in phase 2); a segmented bar, one segment per stage
  (passed, running, stopped, pending). Under it, **Ticket history** and **Pipeline settings**.
  Clicking the card opens the Pipeline view.
- **Left column**: the ticket and its summary, "Started {time}", then the timeline: a row per stage
  with its circle (passed / running / stopped / pending), title, skill and gate badge (`92% ✓`,
  `64% · needs 85`, `needs ≥80%`), and its attempts nested under it as session rows
  (`s-02 · iov42-agents`, then `Working · 2m 15s`, `Waiting for you`, `Ended · 92%`,
  `Stopped · 64%`, or `Ended · no report` for a failed attempt). Stages after a stopped one are
  dimmed; a fixed **Done** row closes the list ("PR left for a person to merge").
- **Right panel**, by the selected session:
  1. *working*: the session's own terminal (its pane, not a copy) under the design's header, with
     the confidence footer: the stage's bar on the track, "Rated when s-NN ends · <next stage>
     starts at ≥N%";
  2. *ended*: the rating report (score, verdict, Reasons with `+`/`−`, Evidence with its kind),
     then the session's terminal for its transcript;
  3. *stopped*: the banner ("Stopped at the <stage> gate · score · bar N", the summary, "You were
     notified in hydraui…"), Reasons and Evidence, and the action bar of §5.3 with its inline
     confirm for **Stop this ticket** and "Gates can't be skipped". A stage failed three times
     shows the same bar with "<stage> failed 3 times" and the last failure;
  4. *step-in*: the session's terminal, focused, under the amber "s-NN · you're in control" header,
     with **Back to report** and **Hand back · re-run <stage>**, which opens the re-run dialog;
  5. the **re-run dialog**: the note (required) and "A fresh session starts with the previous
     attempt's report and your note. It still has to clear the N% bar.";
  6. *done*: "<ticket> is done", the stages' score chips, "waiting for a person to merge",
     **Start next ticket** (tells the orchestrator, §6) and **View history**;
  7. *idle*: "No ticket in progress" (phase 2 adds Watching Linear).

### 7.3 What the views read

The views read hydraui's run state and the sessions' live state, re-rendered when a run changes
(the `RunChanged` event) or a session's state does. They never write ratings. The person's choices
(§5.3), Pause and Resume, and Pipeline settings are the only writes.

### 7.4 Pipeline settings

As the design: the orchestrator's name (the pipeline's `name`), the default bar (50–100), the
stages (reorder, title, skill, bar with Reset to the default, remove; **+ Add stage**), the fixed
Done row, the read-only limits (retries 3, the allowance floor `min_week_left`), and the save bar
with its validation. **Save** rewrites the pipeline file, keeping its prompts, comments and
anything the view doesn't show (`toml_edit`); an added stage gets an `id` from its title (unique
in the file) and no prompt (§2). The skill list is the skills installed for the environment's
Claude (its config folder's and the project's `skills`), plus whatever the file already names
(builder's call how to find them); the design's list is a placeholder. "Stage changes apply from
the next ticket. Bars apply at the next gate." (§2).

### 7.5 Ticket history

As the design, read from the run logs (§8): a row per run, newest first, the current one pinned on
top (clicking it opens the Pipeline view). Columns: Ticket, Title (the summary), Outcome (Done,
Stopped, In progress), Detail (where and why it stopped, e.g. "Review · 3 retries, Slack
unreachable", or the last stage's summary), Time (how long the run took), SLA (phase 2). Filter
chips All / Done / Stopped. A row expands to each stage's score chip and the note.

## 8. Lifetime and records

- Runs live in hydraui's memory, as terminal tokens do. A hydraui restart ends them: terminals get
  new tokens, so their links can't be kept (our call for phase 1; resuming runs across restarts is
  a later phase).
- Every change of a run is also appended to `<HYDRA_HOME>/hydraui/runs/<run>.jsonl`: run started
  (ticket, summary, pipeline), stage started (label, terminal name), report (the whole rating),
  person's choice (with note), paused and resumed, run ended (its outcome, reason and each stage's
  latest score). That's the audit trail, and Ticket history (§7.5) reads it.
- A run is **finished** when its last stage is `passed`. The orchestrator may then `run_start` the
  next ticket.
- When a run finishes or is stopped, its worktree is removed, keeping its branch (the design). A
  worktree with uncommitted changes is kept, and the view says so (our call: never throw work
  away).
- Stage terminals stay open after their stage is decided, so a person can read them; they close
  like any terminal. The views keep showing a closed session's rating (our call).

## 9. The tools

Added to the MCP server's list; descriptions carry the workflow (wording: builder's call). hydra
pre-approves them by name in each environment's generated `settings.json`, as it does the task
tools (MCP server spec §3).

Orchestrator:
- **`run_start(pipeline, ticket, summary)`**: starts a run; returns its id, the stages with their
  skills and bars, and the first stage to start. Refused while the caller has a run that is
  `running` or `needs you`, or the pipeline can't be read.
- **`stage_start(run, stage, brief)`**: §3.2, behind the gate (§5).
- **`run_status(run?)`**: the run (default: the caller's current one): its state and reason,
  whether it is paused, each stage's status, bar and attempts (label, session name and live state,
  note, rating), plus whether a stage may start now and, if not, why. Read-only.

Stage session:
- **`stage_brief`**: the run, ticket, this stage's title, skill and bar, the attempt, and the
  previous stage's summary and handoff. Read-only.
- **`stage_report(outcome, score, summary, evidence, reasons, handoff)`**: §4.

There are no tools to clear a stage, change a bar, skip a stage, pause, or end a run. Those are
the person's, in the views.

**What the instructions tell each role** (wording: builder's call):
- The orchestrator: you coordinate and never do a stage's work yourself; start stages in order with
  `stage_start`; wait to be told a stage is decided (or check `run_status`); when a run needs a
  person, tell the user why and wait.
- A stage session: do this stage only; finish with `stage_report`, with honest evidence and reasons
  for and against; report `blocked` rather than guess when the stage can't be done.

## 10. Testing

Unit tests: pipeline parsing (bars, defaults, skills, the default prompt, unknown keys, duplicate
ids, the closing paragraph always added, placeholders filled) and rewriting a pipeline from the
settings view without losing prompts or comments; every gate rule (§5.1) including pause and the
usage line with and without a report; bars read at the gate; retries counting attempts and
stopping at three; attempts numbered across the run and kept; `stopped` never retried by the
orchestrator; roles (§3.3) refusing each forbidden call, including an earlier attempt's session;
rating validation (each missing or bad field, reasons for and against); one report per attempt; a
session ending without a report making the stage `failed`; the person's re-run (note required,
past the limit, carrying the previous report) and stop (final, next run allowed); the run log's
lines and history read back from them. An end-to-end test drives the HTTP endpoint through a
two-stage run with JSON-RPC, as the task tools' test does. Checked by hand (the phase's gate): one
real ticket walked through the example pipeline in hydraui on Windows and on macOS, every stage
rated and visible in the Pipeline view, one stage stopped on purpose and re-run with a note by a
person, and the views checked against the prototype.

## 11. Calls made without the user

Marked "our call" / "builder's call" above, plus: the orchestrator is a Claude session driving MCP
tools, with hydraui holding the run state and enforcing the gate; pipelines are TOML files in
`HYDRA_HOME`, which Pipeline settings edit; stage terminals open in the orchestrator's environment
without taking focus; one worktree per run, shared by its worktree stages; the orchestrator is
woken by a typed prompt; runs end with hydraui; the audit log is JSON lines and History reads it;
the design's views live in hydraui's terminal list and a tab above the orchestrator's terminal;
hydraui's shell keeps its look.

Decided by the user: one ticket at a time; a rating is a score, evidence and reasons; the bar is
configurable per pipeline and per stage, 80 by default; three retries, then a person; stop when
less than 30% of the seven-day Claude window is left; a person is called through hydraui and
Slack (Slack in phase 3); a person merges; the orchestrator view shows the current stage and every
linked session with what it is doing; the views look and behave as the design handoff shows, and
no gate can be skipped.

## 12. As built

The builder's calls above, answered (plan Tasks 1-15, branch `orchestrator-core`):
- **Views (§7.1).** The Pipeline tab shares the Tasks tab's strip above the orchestrator's
  terminal; Pipeline, Ticket history and Pipeline settings are its three views, reached from the
  card and the two links under it. A newly opened stage is followed; clicking a session pins it.
- **Opening a stage quietly (§3.2).** `EnvTabs::add_terminal_quietly` adds the terminal without
  changing what is shown; its name is `s-NN · <stage>`. A shell that exits, or a Claude session
  that ends, before reporting fails its stage.
- **Wake-up wording (§6).** One line naming the run, the stage and the result, then what to do
  (`Call run_status, then start the next stage.`; `Tell the user why and wait`). Sent through
  `PaneView::type_prompt` with the reservation; a waiting one is retried each second.
- **Step-in and re-run (§5.3).** Step-in focuses the stopped session's own terminal; **Hand back**
  opens the re-run dialog. The note is a single-line field for now (the design's is taller).
- **Skills (§7.4).** Folders holding a `SKILL.md` in `state/<env>/claude/skills` and the
  project's `.claude/skills`, plus any skill the file already names.
- **Look.** The design's colours are `orchestrator_theme`; gpui-component's sliders keep its
  own colours, not the design's teal. hydraui's shell keeps its look (§1).
- **History (§7.5).** The detail is the stop's reason or the last report's summary; a log with no
  end whose run isn't live reads "hydraui closed before the run ended".

Checked by hand on macOS: the card, the allowance meter, the escalation toast, the Pipeline
tab's held banner, Pause and the status text, Pipeline settings and Ticket history, in a hydraui
built from the branch. The live, report, step-in and done panels, and a ticket walked through
every stage, wait for the end-to-end walkthrough on macOS and Windows.

## 13. Picking a run back up after a restart

Closing hydraui used to end its runs: the orchestrator and the stage sessions die with their
terminals, and only the log was left (§8, "hydraui closed before the run ended"). Jordan's
decisions (2026-10-22): a run that was open when hydraui closed comes back as a card with
**Resume** (nothing starts until it's clicked); Resume reopens the orchestrator's own
conversation and the interrupted stage's own conversation (`claude --resume`), so neither
starts over and the stage doesn't use up an attempt.

- **What the log gains.** The hooks pass Claude's `session_id` with each state
  (`hydraui/status`), and hydraui writes `claude session {who, id}` when a run's orchestrator or
  stage session first reports one (`who` is `orchestrator` or the attempt's label). When a run
  starts, the app writes `orchestrator {env, folder, pid}`; when a stage's terminal opens,
  `stage folder {label, folder}`; when a worktree is made, `worktree {path}`.
- **Which runs come back.** At launch hydraui reads every log whose run never ended, whose
  `orchestrator` line names a hydraui that is no longer alive (so a second window never takes
  another's live run), and that recorded the orchestrator's Claude session. Older logs without
  these lines stay as they are in Ticket history.
- **Rebuilding a run.** The log is replayed through the same `Runs` calls that made it
  (`start_run`, `begin` + `link`, `skill_used`, `report`, `session_gone`, `person`, `pause`),
  with the pipeline as its file is now, so the gate's rules and the card come out as they would
  have live.
- **The card.** An interrupted run shows in its environment's list as its pipeline's card,
  pill **Interrupted**, with **Resume** and **Stop ticket**. Stop ticket writes `run ended`
  (stopped, "hydraui closed") and the card goes.
- **Resume.** Opens a terminal titled `orchestrator` in the logged folder running
  `claude --resume <its session> "<note>"`, the note saying hydraui restarted, where the run
  is, and what to do next (wait for the stage, or start the next one). The run is moved to the
  new terminal's token. A stage that was running reopens the same way in its logged folder,
  told to carry on and call `stage_report`, and its latest attempt moves to the new token. A
  stage whose Claude session wasn't logged ends as if its session had gone (§5.2).

## 14. Going back to an earlier stage

Jordan (2026-10-24): INTH-166's Verify failed twice because Develop had pushed the wrong branch,
and once a stage passed there was no way back to its session to correct it. A person whose run
waits for them can now choose **Go back…** in the Pipeline tab's banner:

- **The choice.** Pick an earlier stage that passed, and write a note (required).
  `Choice::Back { stage, note }`.
- **Same conversation.** That stage's own session reopens with the note: `claude --resume` of its
  last attempt's Claude session, in that attempt's folder, both read from the run's log. It's
  kept as a new attempt so its earlier report stays in the timeline. It keeps the skills its
  conversation already used, so the skill rule doesn't make it start over. It reports again and
  must clear its bar again, and the branch rule too.
- **Later stages.** They wait for it again, and their earlier attempts no longer count toward
  their three tries (`StageRun::counted_from`). Their history stays visible.
- **The orchestrator** is told the run went back and to wait for the report. Then it starts the
  next stage as usual.
- **No session in the log.** Without a logged session (runs from before session ids were logged)
  the stage fails as if its session had gone, so the orchestrator starts it afresh.
- **Logged** as `person {choice: "back", stage, note}`, `went back`, and `stage reopened {stage,
  label}`. A restart replays it.

## 15. Asking product instead of deciding (added 2026-10-26)

INTH-166's Develop stage removed the browser's automatic translation: a call for product, not
for a stage. Every stage is now told so, and has a way to ask.

- **The rule**, in every stage's note: a change beyond the ticket that alters what users see or
  do (removing or changing a feature, copy or a flow) is a product decision; ask before making
  it, and wait.
- **Where it asks.** A pipeline's `[product] channel = "#…"` (Pipeline settings → Product
  questions): the stage posts the question there itself (the ticket, the options, its pick)
  with the environment's Slack tools, then calls **`stage_ask`** with the question and the
  message's link. Without a channel it calls `stage_ask` alone and the user answers in hydraui.
- **The ticket waits** (the user's choice: one ticket at a time, nothing else starts): the run
  is needs-you with its question kept on the run (`Run.asking`); the card shows **Asked
  product** in amber with the line `<ticket> · <stage> · paused for product`, then (since simplified, Jordan: too much text) one line, **⏸ Waiting for product · N min · see
  question**, which opens a dialog with with the question, where and when it was
  asked, when it next checks for a reply, **Open thread ↗**, **Check now** and **Answer…**; the
  Automatic line says the ticket is paused for a product answer, and a notification says so. The orchestrator is told to do nothing until it's answered. A person can't re-run or go
  back while it asks (the question would be lost), only stop the ticket.
- **The answer.** Every 5 minutes (or at once with **Check now**) hydraui types to the waiting session: look in the thread; if
  it's answered, call **`stage_answer`** with it and carry on. Or the user's **Answer…** records
  it and types it to the session. A stage that reports or fails while asking drops the question.
- **Logged and resumed**: `asked` (stage, question, link) and `answered` (answer) in the run's
  log; a restart brings a waiting question back.
- **No go-ahead needed** (Jordan, 2026-10-26): a stage asks rather than asking the user whether
  to ask. With a channel, its Claude starts with `--allowedTools=` posting a Slack message and
  reading a thread (`pipeline::PRODUCT_TOOLS`), so neither waits on a permission prompt.
- **Questions already open count.** INTH-165's stages carried a product-owner question from the
  ticket along as "pending PO" (and marked its test `@fixme`), so nothing on the run showed it.
  The rule now says to register an open question with `stage_ask` and its link as well.
- **A stray "needs you".** Claude can send a permission-prompt notification just after its turn
  ends; an orchestrator waiting for its stage then read "needs you" for nothing. A `waiting`
  within 10 s of the session going idle is ignored (`busy.rs`, `STRAY_WAIT`).

## 16. Must address (added 2026-10-26)

A stage's reasons against are often the real problems (Jordan, 2026-10-26), yet a stage that
clears its bar goes on regardless. A person can now hold a stage to them.

- **Marking.** In a stage's report (the Pipeline tab's report panel, or the stopped banner), each
  reason against has a **Must address** toggle. Only the stage's latest report can be marked,
  in a run that's still open and not waiting on a product answer. The marks are kept on the
  attempt (`Attempt.must`).
- **Send back.** With at least one marked, **Send back to <stage> · fix N reasons** appears
  under the reasons. The stage's session is told the marked reasons, to deal with each and to
  say in its next report how each was dealt with (`runs::must_note`).
  - A stage that waits for the person (stopped below its bar) re-runs with that note.
  - An earlier stage that passed reopens its own session (§14), and the stages after it run
    again.
  - While another stage is at work, the send-back waits (`Run.fix_after`). Once that stage
    reports or fails, the run goes back instead of going on or finishing. The report shows
    **↩ Going back to <stage> once <stage at work> reports**, with **Cancel send back**.
- **The orchestrator** is told the run went back, as with **Go back** (that message now wins
  over a pass in the same wake-up).
- **Logged and resumed** as `marked {stage, reason, on}`, `sent back {stage}` and `send-back
  cancelled {stage}`. A restart replays them, a waiting send-back included.

## 17. New pipeline (added 2026-10-26)

Until now a pipeline could be edited in **Pipeline settings** but not made in the app: another
meant copying a TOML file by hand.

- **Where.** **▶ Start pipeline** and **+ New pipeline** under the sidebar's **+ New**, for the
  selected environment (shown only where Claude is set up); **New pipeline…** in the launcher, and as the last entry of **Start orchestrator…**'s
  pipeline list (which now always shows, even with one pipeline).
- **Start from.** **Blank** (one stage, `first`, to rename and fill in) or **Copy of <pipeline>**:
  its stages, bars, prompts, branch and product channel, comments kept.
- **Name.** The name the views show; its file is `<name>` in lowercase with words joined by `-`
  (`pipeline::file_name_for`). An existing file is never overwritten.
- **A copy takes no tickets yet.** Its `[linear] watch` is set false and `[autostart]` dropped, so
  it can't race its original for the same tickets until you turn them on in its settings.
- **Then** its orchestrator starts (in the environment already chosen, else one picked) and its
  Pipeline tab opens on **Settings**.

## 18. Pull requests from Slack (added 2026-10-26)

A pipeline's work can come from a Slack channel instead of Linear: Jordan's **Review PR's**
pipeline reviews each pull request posted in #pull-requests.

```toml
[slack]
channel = "C0C217CER6G"     # the channel's id (names can be renamed)
label = "#pull-requests"    # how the card shows it
reaction = "eyes"           # the mark that a message is taken (default)
every_minutes = 2           # default
lookback_hours = 24         # how far back a look goes (default)
watch = true                # start in Automatic
```

- **One trigger.** A pipeline has `[linear]` or `[slack]`, not both. The card's Automatic switch,
  its line and its failure handling are shared (`linear::Watch`, `linear_watch::trigger`).
- **The token** is the secret `<env>/slack` (launcher → Set secret: other…), a Slack **user**
  token (Jordan's choice, so the 👀 shows as them) with `channels:history`, `reactions:read` and
  `reactions:write`. A missing, revoked or under-scoped token stops Automatic at once and says
  why; other failures retry, three in a row stop it.
- **A look** (`slack_watch::find_and_mark`) reads the channel's people-posted messages from the
  last `lookback_hours`, oldest first. A message with the reaction already, from anyone, is
  someone's (Jordan's choice); one without a GitHub pull request link isn't for it. Each message
  it takes gets the reaction first, then each of its pull requests not run before is queued, so
  a message with three stacked PRs brings three runs.
- **One at a time.** The queue (kept in `<HYDRA_HOME>/hydraui/slack-queue/<pipeline>.json`, so a
  restart loses nothing) starts its next pull request whenever no run is open; looks happen only
  when the queue is empty. The orchestrator is told the ticket (`<repo>-<number>`, e.g.
  `svc-usage-62`), a title (`svc-usage #62`) and the link, and passes the link to `run_start`
  (`link`, new); stage prompts get it as `{pr}` (or `{link}`). One it hasn't started within 5
  minutes is skipped with a notification.
- **Take next PR** (Manual) starts the next waiting pull request, else looks now.
- **Your own pull requests are skipped** (Jordan, 2026-10-26): the first real one, version2-frontend
  #5548, was Jordan's own, and GitHub won't let an author approve. A look asks GitHub, as the
  environment's `gh` login (`hydra run <env> -- gh`), who you are (once) and who opened each
  candidate. Yours aren't queued, and a message carrying only yours gets no reaction, so someone
  else can take it. Passed-over PRs are remembered while hydraui runs, so GitHub isn't asked
  again. A failed lookup fails the look, like any other.
- **In Pipeline settings** (added later the same day; since folded into one **Trigger** section, Jordan: a heading, a dropdown of None, Linear or Slack, then that trigger's options): a **Slack** section beside **Linear**, with
  the channel id, how it's shown, how often to check, how far back to look and the reaction.
  Turning one trigger on turns the other off, and saving refuses both at once. The table keeps
  its comments, and `label` is dropped when it's empty or the channel id.
- **Connect Slack** (Jordan, 2026-10-26: "if they all have to make a bot it's a bit crap"). One team
  Slack app; each person signs in to it, and nobody makes an app. Slack only hands a token to
  whoever holds the app's client secret, so the team's Canopy (`slack_connect_url` in
  hydraui's config.toml) finishes the sign-in: hydraui listens on `127.0.0.1:<random port>`,
  opens `<canopy>/api/slack/connect?port&state=<48-hex nonce>`, Canopy (signed in) sends the
  browser through Slack's consent screen and answers with a page that posts the user token to
  `http://127.0.0.1:<port>/slack` with the nonce. hydraui keeps only a post with its nonce and
  an `xoxp-` token, stores it as `<env>/slack`, and puts Slack watchers in that environment that
  had stopped back on Automatic. Canopy stores nothing (iov42/pw-dashboard branch
  `slack-connect`). Reached from the launcher (**Connect Slack…**) or the card's **Connect
  Slack** while its Slack watcher is stopped.

## 19. A colour per pipeline (added 2026-10-26)

With INTH and Review PR's side by side, their cards looked the same. A pipeline may set
`color = "#rrggbb"`: its card's ◆, the selected card's tint and border, its progress segments, and
the frame and guide line round its sessions take it. Without one it's the default teal. Pipeline
settings → **Colour** offers eight (`pipeline::COLORS`: Teal, Blue, Violet, Pink, Orange, Yellow,
Green, Slate); choosing Teal removes the key. The card reads the file each time it's drawn, so a
change shows at once, even mid-run. State colours (the pill: working, needs you, asked product,
done) are unchanged.

## 20. Runs at once (added 2026-10-26)

A pipeline may work on several tickets together: `runs_at_once = N` (1 to 6; Pipeline settings →
**Runs at once**). The one-ticket-at-a-time rule stays per orchestrator; a pipeline simply gets N
of them (Jordan chose "one card per run"):

- **Starting** (Jordan, revised: "spun up once the first picks up a ticket") a pipeline opens
  one orchestrator. When work arrives and every orchestrator is busy, another starts, up to
  `runs_at_once` (`slot_for_work`); the ticket waits for it to be ready. So **Tickets at once**
  (Pipeline settings → Advanced) reads **One at a time** or **Up to 2…6**. Extra orchestrators stay
  open once started and take later work. Names are `orchestrator 1`, `2`, … when the pipeline
  allows more than one.
- **A new orchestrator gets its ticket after its first turn** (bug, 2026-10-26: orchestrator 2 was
  handed pw-dashboard #58 the instant its session started; Claude submitted its startup prompt
  0.3 s later and the typed hand-over was lost). Wake-ups wait until a just-opened orchestrator's
  first turn ends (`starting_up`, Working → Idle), or 90 s. A pull request an orchestrator was
  handed but didn't start within 5 minutes goes back to the front of the queue once
  (`Queued::tries`), then is skipped with a notification.
- **In the sidebar** (Jordan, revised the same day: not separate cards): one block per pipeline,
  with one card (its name, "1 of 2 running", the most urgent state), its trigger row, then
  `orchestrator 1`, `orchestrator 2`, … each with its ticket line (ticket · stage · state, its SLA
  and PR links; click it for that run's Pipeline tab), any product question, and its sessions
  nested under it (`pipeline_card::group_card`).
- **One trigger.** The first orchestrator (its *lead*) holds the Linear or Slack switch; the
  others say "Run 2 of 3: takes work from the first card's Slack". A look happens while any run
  is free, and what it finds goes to a free one (`free_slot`: no run open, nothing on its way to
  it). A run told to start that doesn't within 5 minutes frees its slot again.
- **The line** says "Next PR after all 2 running" only when every run is busy.
- **Limits.** Each run is its own Claude orchestrator and stage session, so more runs use the
  Claude week faster. Only the first run hydraui closed on is resumed at launch; any other shows
  as Interrupted to resume by hand.

## 21. A simpler Pipeline settings page (added 2026-10-26)

Jordan: "so much text … it needs to be super simple and straightforward for non-technical people".
The page is now four parts, with explanations moved from paragraphs to an ⓘ beside each heading:

1. **Name**: the name, and the colour swatches under it.
2. **Where work comes from**: the dropdown (None, Linear, Slack), then that trigger's fields with
   plain labels (Linear: Team, Pick tickets in, Whose tickets, Then move it to, When done, move it
   to; Slack: Channel name, Channel ID, Mark taken with), hints as placeholders.
3. **Steps**: the stages and **+ Add step**. The fixed "Done" row and the prompt-placeholder
   paragraph are gone (the ⓘ names `{ticket}`, `{pr}`, `{handoff}`).
4. **▸ Advanced**, folded by default: Runs at once, Pass mark (the default bar), Branch name,
   Product questions channel, and one line on retries and the Claude-week floor.

## 22. A ticket put back is taken again (added 2026-10-26)

Phase 2 skipped any ticket hydraui had a run log for, so a ticket could never loop. INTH-166 ran
and finished on 24 Oct, was moved back to Ready for Investigation, and the watcher skipped it
("run here before") while Jordan could see it waiting. hydraui never moves a ticket *into* the
trigger state (it claims out of it, and moves a passed one on), so a ticket that's there again
was put back by a person. The watcher now skips only tickets with a run still open here (running,
or interrupted and waiting to be resumed); the card and notification say "still running here".

## 23. Score again (added 2026-10-26)

A stage stopped below its bar; the person stepped into its session and fixed the problem there,
but nothing could ask that session to score its work again (Jordan, 2026-10-26). Re-run started
a new session, losing the fix's context and spending a try.

- **Where.** The stopped banner shows **Score again** beside **Step into**, while the stage's
  session is still open.
- **What it does.** The same attempt goes back to `Running` (not counted as another try), the run
  goes on, and the session is told a person worked on it, to check the stage's work as it is now
  and call `stage_report` again (`runs::rescore`). Its new report passes or stops the stage as any
  report does. The orchestrator is told to wait for it.
- **Logged and resumed** as `scoring again {stage}`; a restart replays it.

## 24. How product questions are written (added 2026-10-26)

Different pipelines ask different readers (Jordan, 2026-10-26): one wants product questions short,
casual and about the purpose; another wants them technical and data-driven.

- **Setting.** `[product] style` in the pipeline file: what each stage is told about writing a
  product question, appended to the product rule in its note (`How to write a product question:
  …`). A style needs no channel; `[product]` now takes either key.
- **Pipeline settings.** A **Product questions** section (no longer under Advanced) has the two
  ready-made styles from `pipeline::PRODUCT_STYLES` (**Short and plain**, **Technical, with
  data**; the chosen one is filled), a box for the person's own words, and the channel.

## 25. Every orchestrator of a pipeline in one tab row (added 2026-10-26)

With two orchestrators under **Review PR's**, the row above the terminal showed only the one on
screen and its Pipeline tab (Jordan, 2026-10-26). Now, for an orchestrator whose pipeline runs
tickets at once, the row has each orchestrator of that pipeline in turn, its terminal tab and its
**Pipeline N** tab, and clicking one shows it. Opening one orchestrator's Pipeline tab (the card,
a run line) gives the others theirs too, behind it. The row starts with the pipeline's name (◆ in its
colour), fixed and not a tab, so it's clear which pipeline these tabs are. Then the Pipeline tab(s), always
first, then each **Orchestrator N** (capitalised), then the terminal's Tasks.

## 26. Sidebar actions, colour everywhere, one pipeline's history (added 2026-10-26)

- **Sidebar actions** (Jordan, 2026-10-26: "more organised"). **Open session** and **Create
  pipeline** side by side, outlined; under them **▶ Start pipeline**, filled and full width, as
  it's what most people come for.
- **A pipeline's colour reaches everything it tints**, not only the frame and ◆: the state pill
  while working or done, the trigger's **Automatic** pill and its next-look bar, the ticket and PR
  links, and in the Pipeline tab the selected view, the running stage's ring, row and panel
  header, the passed connectors, **Go back here**, and History's chips and in-progress pill.
  Red, amber and grey keep their meaning; green stays for a pass or a reason for.
  The sidebar rows under a pipeline (each orchestrator and session: the selected row and its
  dot) take its colour too, and its card stays grey: the colour is on the frame, pills and bars.
  `orchestrator_theme::blend` lays the colour over the dark surface for tints and edges.
  A colour changed in Pipeline settings reaches a ticket already in progress at once
  (`Runs::refresh_from_file`, with the product channel and style).
- **History shows one pipeline.** The run log's `run started` names its pipeline
  (`run_history::Entry.pipeline`); an orchestrator's History lists only its own pipeline's runs.

## 27. Motion (added 2026-10-26)

The once-a-second redraw made the next-look bar step and the working dot blink on and off
(Jordan, 2026-10-26: "so stuttery").

- **Next look** counts down to the second (`next look in 3:59`), and its bar slides every frame
  from the clock (a repeating gpui animation, at most 30 frames a second).
- **The Pipeline tab with no ticket** shows the same: **No ticket (PR) in progress**, what its
  trigger is doing with the countdown, and the bar (`linear::look_bar`, shared with the card;
  `Workspace::trigger_view` is the trigger's state for both).
  Under it: give the Orchestrator a ticket number or link (a PR link) to start one now,
  otherwise it keeps looking.
- **The bar never disappears while it waits for work**, so the row doesn't jump when a wait
  ends: full and breathing while it looks, then empty again as the next wait starts.
- **Pipeline settings** lost its **← Back to pipeline** (the view bar's tabs do it), and the
  trigger's dropdown is a **Source** row lined up with the options under it.
- **What's at work breathes** (`busy::pulse`: a 1.6 s pulse between 15% and full, in step
  everywhere, at most 30 frames a second): a working terminal's dot in the sidebar (a stage's
  session's for as long as its stage is in progress, even while it waits idle on a deploy) and the
  environment tab, the card's segment for the stage at work, and that stage's name in the
  Pipeline tab's timeline (its ring's dot stays steady).

## 28. End a pipeline (added 2026-10-26)

There was no way to end a pipeline short of closing its terminals one by one (Jordan,
2026-10-26). The Pipeline tab's view bar has **■ End pipeline** at its right, red and outlined
(a plain "End" on the card read as a label, not a button); the card's right-click menu has **End
pipeline…** too. It asks first, saying how many
tickets are in progress; **End pipeline** then stops each of its orchestrators' open tickets (as
**Stop this ticket**: logged as stopped by you) and closes its orchestrators and every session
under them, so its trigger stops looking. **Start pipeline** starts it again; one with
`[autostart]` also starts when hydraui does.


## 29. A check by hand (added 2026-10-27)

INTH-166's Verify stage stopped as **blocked** at 55%: everything it could run passed, but the
last proof was real Chrome's own "Always translate", which automated Chrome can't drive. It
wasn't blocked; it needed a person to try one thing (Jordan, 2026-10-27). `stage_report` now
takes a third outcome, **check**, with a `check` field saying what to try and what to see.
Like blocked it never passes by itself: the run needs you, the gate badge reads *check by
hand*, and the stopped banner shows **What to check** in amber. Its choices add **✓ I checked
it, it works**, which asks for an optional note and passes the stage (logged as `person`
`checked`, then `checked`), so the next stage starts or the ticket finishes; re-run is labelled
**Doesn't work: re-run … with a note**. This is the one choice that lets a stage through
(§5.3), and only a stage that asked for a check.

## 30. A finished ticket closes its sessions; toasts name their pipeline (added 2026-10-27)

A finished run's last stage session stayed open and idle, and a Slack pipeline that reviews one
PR per run gathered one idle **1. Review PR** session per PR (Jordan, 2026-10-27). When a run
finishes (every stage passed), hydraui now closes each of its stage sessions (every attempt's,
logged as `run finished: closed …`); its orchestrator stays for the next ticket. A run that's
stopped keeps its sessions, so a person can look at what went wrong. Toasts about a pipeline's
terminals say which: **Claude finished** and **Claude needs you** are titled
`<pipeline> · <ticket> · …`, the stopped-ticket toast's overline is `<PIPELINE> STOPPED · NEEDS
YOU`, and the wake-up, Automatic and Check Linear toasts carry the pipeline's name too.

## 31. One orchestrator, several tickets; Sessions and Pipelines apart (added 2026-10-27)

Three things from using the Review PR's pipeline (Jordan, 2026-10-27):

- **Tickets under one orchestrator.** `runs_at_once` used to start another orchestrator per
  ticket ("Orchestrator 1, 2…", each with its sessions). Now one orchestrator holds up to
  `runs_at_once` open runs, each with its own stage sessions, so related pull requests are seen
  by the one conversation that can relate them. `run_start` refuses a ticket already open, or
  once that many are open; `run_status` with no run returns every open run (`open_runs`);
  `stage_start` with no run asks which when several are open. Its card reads *N tickets in
  progress*; under its row, each ticket's line (click opens its Pipeline tab) then that
  ticket's sessions; the tab row has a Pipeline tab per ticket, by ticket. Wake-ups for
  different tickets are joined, not replaced (`join_wake`). After a restart, tickets of the
  same orchestrator conversation come back under one terminal. A second orchestrator is only
  ever started by hand.
- **Every PR in a message.** A message with several PR links gets one 👀 and a run per link,
  each told the others it was posted with. The watcher keeps looking while the orchestrator is
  full: what it takes waits in the queue, already marked, so nobody else picks it up (before,
  it stopped looking while a review ran and others took those messages).
- **Sessions · Pipelines.** The left list has two tabs: Sessions (Open session and your own
  terminals) and Pipelines (Start pipeline, Create pipeline, interrupted runs and each
  pipeline's block). It opens on Pipelines when any run here, and switches to whichever holds
  a terminal shown from a toast or link.

## 32. A merged PR ends its ticket; one Pipeline tab (added 2026-10-27)

svc-usage #66's review approved the PR on GitHub, then reported 92 against its 95 bar, so the
run stopped for a person; a re-run then reviewed it again after it had merged and posted five
more comments (Jordan, 2026-10-27). The score is the stage's confidence; approving was a step
it took before its gate. Two changes:

- **Settled outside hydraui.** Every two minutes hydraui asks GitHub (as the run's
  environment) for the state of each open run's pull request (its `link`). Merged or closed:
  the run ends as done (`Runs::settle`, logged as `run ended` with the reason), whatever its
  gates said; its sessions close and the orchestrator is told not to touch it again.
- **No outward step below the bar.** Stage sessions are told (server instructions and
  `stage_report`) not to approve or merge a pull request, or take any step outside hydraui
  that can't be undone, unless they will report at or above their bar; below it they report
  and leave that to the person.

The tab row now has one **Pipeline** tab per pipeline, not one per orchestrator or ticket.
When the pipeline holds more than one ticket, the Pipeline tab's Ticket view opens with a row
of ticket tabs (a dot in the ticket's state colour, then the ticket); clicking one shows it.

## 33. Checks with screenshots; a check isn't a failure (added 2026-10-27)

- **Screenshots.** `stage_report` takes `screenshots`: up to six absolute paths to .png, .jpg,
  .gif or .webp images the session captured. hydraui copies them beside the run's log
  (`runs/<run>/shots/<label>-<n>.<ext>`) and records the copies, so they outlive the session's
  temp files; a missing file refuses the report, naming it. The stopped banner's **What to
  check** box shows them as thumbnails (click: full size), beside **✓ I checked it, it works**.
  Stages are told to attach them whenever a check needs a person's eyes.
- **Amber, not red.** PRE-830's Test scored 86 against an 85 bar and asked for a check by
  hand; it read as failed (Jordan, 2026-10-27). A stage waiting for a check is now amber
  everywhere: the card's pill reads **Check it**, its segment, the timeline's mark (`?`) and
  the badge (`86% · check by hand`).
- **Must address** reads as a button: outlined **+ Mark must address**, then red **✓ Marked:
  must address**.

## 34. A message that didn't take doesn't silence a session (added 2026-10-27)

INTH-170's Plan fix waited on product. hydraui asked it every 5 minutes to look in the Slack
thread for the answer, but from 13:49 none of those messages reached it, and product's reply
went unread (Jordan, 2026-10-27). The 13:49 message didn't start the session working (a Claude
Code question was likely in the way). A prompt hydraui types counts as a draft until the
session starts on it, so every later message waited for a draft that was never cleared, and
the "isn't taking messages" toast never came because only one try was made.

- A wake-up that didn't take (`recheck_wakes`) now lets the screen decide whether the input
  still holds it (`Readiness::prompt_not_taken`): an empty input frees the session for the
  next try.
- A wake-up that has waited 3 minutes while its session sits idle without taking it (a draft,
  or a Claude Code question) tells the person once, with a toast naming the session that
  stays until closed; clicking it shows that terminal.

Once INTH-170's session read its thread again, it found no decision, rightly: product's reply
was a question back ("where does this solution apply if we're allowing larger ruleset
saves?"). The check told it only to look for an answer, so nobody would ever have replied.
The 5-minute check now also has the stage reply in the thread to a question or request to
explain that nobody has answered yet, then wait for the decision as before.

A finished ticket no longer stays on its orchestrator's card (Jordan, 2026-10-27: Review PR's
kept saying "version-2-backend-1840 · Done" after the review ended). Once the last ticket there
is finished, the card goes back to its pipeline's ready state and drops the ticket's link; a
stopped one stays so someone can look (`pipeline_card::stays_on_card`). Ticket history keeps
every finished ticket.

## 35. Not needed (added 2026-10-28)

Beside each reason against's **+ Mark must address** there is **Not needed** (Jordan,
2026-10-28): a concern the person doesn't want acted on. A reason is one or the other, never
both (`Runs::mark_as`, logged as `not needed`). Marked, its text is struck through and muted;
the stage's next session (a re-run, a send-back, or **Score again**) is told those concerns are
not needed and not to count them against its score again (`waived_note`).

## 36. Tickets without an SLA (added 2026-10-28)

The Hydra Triage pipeline watches PRE's *Next up (prioritised)* for unassigned tickets and found
none, though Linear had four (Jordan, 2026-10-28): the pick only ever took tickets with an SLA
running (§3), right for SLA Tickets, but PRE's tickets have none. `[linear] sla_only` (default
true, so existing pipelines are unchanged) set to false also takes tickets without an SLA: those
with one still first, by breach; then by priority, Urgent first and no priority last; then the
oldest. Linear's `priority` and `createdAt` are now read with each ticket. Pipeline settings show
it as **Which tickets: With an SLA · All, by priority**.

## 37. Labels (added 2026-10-28)

A Linear trigger can be narrowed to labels: `labels = ["Bug", "Frontend"]` in `[linear]`, or "With one of the labels" in Pipeline settings (comma-separated). A ticket is picked only when it carries at least one of them, in any case; no labels means any ticket, as before. Labels are trimmed and a repeat in another case is dropped when read and saved. The check right before a claim asks again, so a ticket whose label was taken off since the list is passed over.

## 38. Open pipelines stay open across a restart (added 2026-10-28)

hydraui saves the pipelines that are open (`~/.hydra/hydraui/open-pipelines.toml`: each one's name, the environment of its first orchestrator, and whether that orchestrator is in Automatic) whenever the set changes, checked each second. At launch each saved pipeline opens again, Automatic or Manual as it was, and its interrupted runs carry on, as an `[autostart]` pipeline's do; `[autostart]` pipelines that weren't open start as before. End pipeline (or closing its orchestrator) takes a pipeline off the list. Nothing is saved before launch has reopened the saved ones, nor while the window is closing, so a quit or a crash both leave the list as it was. `HYDRAUI_NO_AUTOSTART` turns reopening off too.
