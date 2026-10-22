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
when a run starts (an added, removed or reordered stage applies to the next ticket); its name and
bars are read again on Save and at every gate (§5.1), so a new name shows on the running ticket's
card at once and a changed bar applies at its next gate (the design).

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
- `skill` is optional. `prompt` is the stage session's first prompt; when left out (a stage added in
  Pipeline settings has none), it is `Use the {skill} skill for the "{title}" stage of ticket
  {ticket}.` (our call). `{ticket}`, `{run}`, `{stage}` and `{handoff}` (the previous stage's
  handoff, §4) are filled in. Every prompt gets the same closing paragraph from hydraui, telling the
  session to finish with `stage_report` (§3.3), so a pipeline file can't leave it out.
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
