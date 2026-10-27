> The design handoff for the orchestrator (2026-10-22), as delivered with its clickable prototype (`Hydra Prototype.dc.html`, kept with the handoff bundle, not in this repo). The orchestrator spec, `2026-10-22-hydraui-orchestrator-core-design.md` §7, says how it maps onto hydraui; where the two differ, the spec wins.

# Handoff: hydraui Ticket Orchestrator

## Overview
Adds a **ticket orchestrator** to hydraui, the hydra desktop app. The orchestrator watches Linear for SLA tickets in the INTH team, one at a time. For each stage of a configurable pipeline (default: Understand & replicate → Develop → Review → Verify → Done) it starts a fresh Claude session with one skill. Every session ends with a confidence rating, and the next stage starts only if the rating clears that stage's bar. Below the bar, the ticket stops and a person is asked to step in.

The product requirements are in `hydra-spec.md` (included). Read it first. Its principles are fixed:
- No stage starts without a rating.
- Low confidence stops the line.
- Gates can't be skipped.
- The pipeline never merges.
- Everything fails closed.
- Windows and macOS have parity.

## About the Design Files
The files in this bundle are **design references created in HTML**. They are prototypes that show the intended look and behaviour, not production code. Recreate them inside hydraui's existing codebase and UI framework, using its established patterns: terminal panes, sidebar, MCP server, task board and session states. Do not ship the HTML.

The prototype's pipeline is simulated with a timer and canned transcripts (`script()` in the logic class). In the real app this is replaced by:
- the orchestrator session
- hydraui's MCP server
- real Claude sessions reporting `{ score, evidence, reasons }`

## Fidelity
**High-fidelity.** The colours, type, spacing and interactions are final in intent. The design keeps hydraui's current shell: dark UI, a left sidebar of environments, terminal panes, and a status bar. It refines hierarchy and spacing. Match the values below, adapting to hydraui's existing components where they already exist.

## Files
- `Hydra Prototype.dc.html` is the **primary reference**: a fully clickable prototype. Open it in a browser. The markup is inline-styled HTML; the logic is a JS class at the bottom of the file. The Tweaks props (speed, start state, Develop outcome) are demo controls only.
- `reference/Hydra Orchestrator.dc.html` holds static boards of the same screens (overview 1b, stopped 2b, settings 2d, history 2e). The settings board is older; the prototype supersedes it.
- `reference/HydraSidebar.dc.html` is the static sidebar used by the boards.
- `hydra-spec.md` is the feature spec.

---

## App shell (all views)
- **Window**: column flex, full viewport, background `#0d0f0f`.
- **Title bar**: 38px tall, background `#111313`, bottom border `1px #1d2221`.
  - Traffic lights.
  - Window title `iov42 · {orchestrator name}`, 13px, `#949c9a`, ellipsis.
  - **Pause/Resume orchestrator** button: 12px, padding 4×10, radius 6, border `1px #2a3030`, nowrap.
  - `hydra 0.7.0-dev`, 12px, `#646c6a`.
- **Status bar**: 26px tall, background `#111313`, top border.
  - Left: the environment name `iov42`, 11.5px, `#3fbfa6`.
  - Middle: orchestrator status text. It turns `#e5705f` when stopped.
  - Right: "Restart demo", prototype only.
- **Sidebar**: 232px, background `#121414`, right border `1px #252b2a`, padding 12×10, gap 20.
  - **+ New**: 32px tall, border `1px #2a3030`, radius 7.
  - **ENVIRONMENTS** label: 10.5px, uppercase, letter-spacing .08em, `#646c6a`.
    - Each row: a 9px ring dot (`#e5b53e` for the active env), the name at 13px, and the account at 11.5px in `#646c6a`.
  - **PIPELINES**: the orchestrator is a **special item alongside environments**. It is a card with padding 10 and radius 8.
    - Inactive: background `#161919`, border `#252b2a`. Active: background `#10302b`, border `#1f5a50`.
    - Line 1: an 8px teal diamond (rotated square, `#3fbfa6`) and the orchestrator name at 13px/500.
    - Line 2: `{ticket} · {stage first word}` in 11px mono `#949c9a`, plus a state pill at 10.5px, padding 2×6, radius 4:
      - Working: `#3fbfa6` on `#10302b`
      - Needs you: `#e5705f` on `#331916`
      - Paused: `#e5b53e` on `#2e2511`
      - Done: `#62c98d` on `#12291c`
      - Polling: `#949c9a` on `#1c2020`
    - Line 3: a segmented progress bar, one 3px segment per stage, gap 3. Passed is `#3fbfa6`, running is `#3fbfa6` at 53% alpha, stopped is `#e5705f`, pending is `#2a3030`.
  - Under the card, two sub-links (indent 26px, 12.5px): **Ticket history** and **Pipeline settings**. The active one has background `#1c2020`.
  - **Footer**: the Claude 7-day allowance meter.
    - 4px track `#252b2a`, fill `#62c98d`. It goes red below 30%.
    - A 1px tick marks the 30% floor.
    - Caption: "Pipeline stops below 30%".

## View 1: Pipeline (overview, based on board 1b)
There are two columns after the sidebar.

**Left column**: width 34%, min 280, max 380px, right border.
- **Ticket header**: padding 18×22, bottom border.
  - The ticket ID in 12.5px mono `#949c9a`, with an SLA pill at 11.5px: `#e5b53e` on `#2e2511`, padding 3×8, radius 5.
  - The title at 16px/500, line-height 1.35.
  - "Picked up {time} · Linear ↗" at 12px `#646c6a`.
- **Vertical timeline**: padding 18×22, scrolls. Each stage is a row.
  - Rail (left), a 22px circle:
    - passed: `#12291c` background with a `✓` in `#62c98d`
    - running: 2px `#3fbfa6` ring with a pulsing 7px dot
    - stopped: `#331916` background with a `!` in `#e5705f`
    - pending: 1px `#2f3635` ring with the stage number
    - A 1px connector runs to the next circle: `#3fbfa6` if passed, else `#2a3030`.
  - Body:
    - The stage name at 13.5px. Running is teal/500, stopped is red/500, pending is `#949c9a`.
    - The skill at 11.5px `#646c6a`.
    - A gate badge on the right in 11.5px mono: `92% ✓` (green), `64% · needs 85` (red), or `needs ≥80%` (muted).
  - **Sessions are nested under the stage that started them.** Each one is a clickable row: padding 7×10, radius 6.
    - It shows a dot, `{s-id} · {skill}`, and a status: `Working · 2m 15s`, `Waiting for you`, `Ended · 92%` or `Stopped · 64%`.
    - Selected rows get a tinted background and border: working `#10302b`/`#1f5a50`, waiting `#1c1513`/`#4a2620`, ended `#1c2020`/`#2f3635`.
  - For a stage whose skill is `iov42-agents`, the repo chips it chose are shown (11px mono, `#1c2020`, nowrap, wrap row).
  - Stages after a stopped stage are dimmed to 45% opacity.
- **Idle state** (no ticket): a pulsing dot with "Watching Linear", then "No ticket in progress" and an explanation.

**Right panel**: background `#0a0b0b`. Its content depends on the selected session:
1. **Live session** (status working):
   - Header: pulse dot, `s-02 · Develop`, and meta `{skill} · Working · {elapsed}`.
   - Terminal: Geist Mono 12.5/1.45, bottom-anchored so the newest line is at the bottom.
     - Line glyphs: `⏺` `#3fbfa6`, `✓` `#62c98d`, `›` `#e5b53e`, `−` `#e5705f`.
     - Last line: `✻ Working…` in `#d9785b` with a blinking block cursor.
     - Each new line fades and slides in (0.25s ease-out, translateY −6px → 0).
   - Footer: a "Confidence" track (6px, `#1c2020`) with a 1px marker at the bar %, and "Rated when s-02 ends · Review starts at ≥85%".
2. **Rating report** (status ended):
   - The score in 48px/500, green if it passed. Verdict line: "Cleared the 85% bar · Review started".
   - Two auto-fit columns (min 260): **Reasons** (`+` green / `−` red bullets) and **Evidence** (rows: padding 9×12, `#161919`, radius 7, mono name, muted kind).
   - Then the full transcript.
3. **Stopped** (board 2b): the stopped session is selected.
   - Banner: background `#1a1311`, padding 22×28.
     - Overline in 12px uppercase red: "Stopped at the Develop gate · 64% · bar 85%".
     - Headline at 20px/500.
     - "You were notified in hydraui and on Slack. Nothing downstream starts until you act."
   - Below: Reasons and Evidence, as in the report.
   - Action bar:
     - **Step into s-02** (primary): background `#3fbfa6`, text `#06201b`, 13px/500, padding 9×16, radius 7.
     - **Re-run Develop with a note** (secondary): bordered.
     - **Stop this ticket** (tertiary): muted text, red on hover.
     - On the right, the note "Gates can't be skipped".
   - "Stop this ticket" swaps the bar for an inline confirm on a red-tinted bar: "Stop INTH-482? It's recorded as stopped at Develop, its worktrees are removed and the next ticket is picked up." with Cancel and a red **Stop ticket** button.
4. **Step-in**: the terminal takes over the panel.
   - Header: amber dot, "s-02 · you're in control", "The orchestrator waits while you work in this session", and a "Back to report" link.
   - A prompt input (`❯`, amber). Enter sends a message, and the session acknowledges it.
   - **Hand back · re-run Develop** is enabled once at least one message exists. It starts a new session (attempt 2), whose transcript begins with `› Note from Jordan: …`.
5. **Re-run modal**: overlay `rgba(5,6,6,.72)`, card 540px wide, background `#141717`, border `#2a3030`, radius 12.
   - Textarea, 110px tall.
   - "Re-run Develop" is disabled until text is entered.
   - Copy: "A fresh session starts with the previous transcript and your note. It still has to clear the 85% bar."
6. **Done**:
   - A green check, "INTH-482 is verified fixed", and the PR note: "approved and passed QA, waiting for a person to merge, worktrees removed".
   - Chips showing each stage's score.
   - Buttons: **Pick up next ticket** and **View history**.
7. **Idle**: "Next check in 4m 12s" and a **Check Linear now** button.

**Escalation toast**: shown whenever a gate fails.
- Absolute position: top 50, right 16, width 360, background `#171a1a`, border `#4a2620`, radius 10.
- Overline in red: "Pipeline stopped · needs you".
- Title: "INTH-482 stopped at Develop".
- Body: "Rated 64%, bar is 85%." plus the first reason.
- "Also sent to you as a Slack DM".
- Buttons: **Open ticket** and Later.
- The Slack DM carries the same three facts: ticket, stage, reason. See the spec and the old board 2f.

## View 2: Pipeline settings (user-configurable orchestrator)
A scrolling body, padding 32 × clamp(20px, 4vw, 48px), gap 34, with a sticky save bar at the bottom.
- **Orchestrator name**: a text input (padding 10×12, radius 8, background `#0d0f0f`, border `#2a3030`, red border when empty). Helper text: "Shown in the sidebar, notifications and Slack messages". The name updates the sidebar card and the window title after Save.
- **Default confidence bar**: a card (`#161919`, border `#252b2a`, radius 10) with a range slider (50–100, accent `#3fbfa6`) and the value in mono.
- **Stages list** ("Run top to bottom · N stages …"):
  - Grid columns: `24px 52px minmax(0,1.3fr) minmax(0,1fr) minmax(120px,1.4fr) 28px`, gap 10.
  - Each row is a card: `#131616`, border `#1f2524`, radius 10, padding 12×14.
  - Columns, left to right:
    1. Number circle.
    2. **↑ / ↓** reorder buttons, disabled at the ends.
    3. **Stage name** input. Red border if empty.
    4. **Skill** dropdown: a custom chevron SVG, `appearance:none`, right padding 28.
    5. **Confidence bar** slider, the value (amber when overriding the default), and a fixed 36px slot for **Reset**, so every slider is the same length.
    6. **×** remove button: red on hover, disabled when only one stage is left.
- **+ Add stage**: a full-width button: dashed border `#2f7d6f`, background `#0f2622`, text `#3fbfa6` 13.5/500, with a solid teal 20px "+" circle. On hover the background becomes `#133630`. It appends a "New stage" using the first skill and the default bar.
- **Done row**: fixed and always last. "Done · PR left for a person to merge", with "Always last · can't be removed".
- **Limits** (read-only cards): "Retries before escalating: 3" and "Claude allowance floor: 30%".
- **Save bar**: shown only when the draft differs from the saved config.
  - Amber dot and the status "Unsaved changes · N stages · apply from the next ticket".
  - **Discard** and **Save changes** buttons.
  - Validation: the name must be non-empty, every stage needs a name, and there must be at least one stage. When invalid, Save is disabled and the message turns red: "Give the orchestrator a name" or "Every stage needs a name".
  - When nothing is unsaved, a note explains: "Stage changes apply from the next ticket. Bars apply at the next gate."
- The skill list is a placeholder: `QA-Skills, iov42-agents, Review · Slack, Security review, Release notes, Docs update`. In the real app, populate it from the skills installed in the environment.

## View 3: Ticket history (board 2e)
- Title "Ticket history" and filter chips: All / Verified / Stopped.
- Table columns: `80px minmax(120px,1.4fr) 100px minmax(140px,1fr) 64px 60px`. Headers are Ticket, Title, Outcome, Detail, Time and SLA.
- Rows: padding 13×14, top border `#1d2221`, hover background `#141717`.
- Outcome pills: Verified green, Stopped red, In progress teal.
- SLA: Met is green, Missed is red.
- The in-progress ticket is pinned on top. Clicking it goes to the Pipeline view.
- Clicking any other row expands it to show each stage's score chip and a note. Every stop names its stage and reason, for example "Review · 3 retries, Slack unreachable".

## Interactions & behaviour summary
- **Navigation**: clicking the sidebar card opens Pipeline; the sub-links open History and Settings. Clicking a session row selects it and shows it in the right panel. When a new stage starts, its session is auto-selected.
- **Stage engine**:
  - Each session streams output, then reports a score.
  - If `score ≥ bar(stage)`, the stage passes and the next stage's session starts.
  - Otherwise the session goes to `waiting`, the stage to `stopped`, and the run records `stopped = {sessionId, stage, score, bar}`. The toast fires, along with the Slack DM in the real app.
  - After the last configured stage passes, Done passes, a history entry is added and the worktrees are removed.
- **Bar resolution**: the stage's own bar if set, otherwise the pipeline default. It is read live at each gate.
- **Config snapshot**: a run copies the stage list when its ticket is picked up. Editing stages affects the next ticket only. Bars apply at the next gate.
- **Pause**: no new output or stage transitions while paused. The sidebar pill shows Paused (amber).
- **Fail closed** (from the spec, not simulated): a dead session or unreachable service gets 3 retries, then counts as a stop. A missing sign-in, or less than 30% allowance left, stops at once.
- **Never offered**: skipping a gate, self-approval, or merging.

## State (from the prototype logic)
- `config: { name, def, stages: [{ id, label, skill, bar|null }] }` and `draft` (a copy edited in Settings).
- `run: { ticket, stages (snapshot + Done), status{stageId: pending|running|passed|stopped}, scores{}, sessions[], stopped|null, done, attempt{} }`
- `session: { id, stage, attempt, status: working|waiting|ended|stopped, ticks, score, transcript }`
- UI state: `view`, `selected`, `toast`, `paused`, `modal`, `noteDraft`, `stepIn`, `stepLog`, `confirmStop`, `filter`, `openRow`, `history[]`.
- The real data comes from: Linear (tickets, SLA), GitHub (PRs, approvals), Slack (review thread, DMs), the hydraui MCP server (session state and ratings), and hydra environment sign-ins.

## Design tokens
| Token | Hex |
|---|---|
| Canvas / window | `#070808` / `#0d0f0f` |
| Terminal background | `#0a0b0b` |
| Sidebar | `#121414` |
| Bars (title/status) | `#111313` |
| Surface / raised / hover | `#161919` / `#1c2020` / `#141717` |
| Borders | `#1d2221`, `#252b2a`, `#2a3030`, `#2f3635` |
| Text / secondary / muted | `#e6e9e8` / `#949c9a` / `#646c6a`; terminal text `#cfd4d2` |
| Teal (primary, running) | `#3fbfa6`, tint `#10302b`, border `#1f5a50`, on-teal text `#06201b` |
| Green (passed) | `#62c98d`, tint `#12291c` |
| Red (stopped) | `#e5705f`, tint `#331916`, border `#4a2620`, banner `#1a1311` |
| Amber (SLA, override, paused) | `#e5b53e`, tint `#2e2511` |
| Claude orange (working spinner) | `#d9785b` |

- **Type**: Geist (UI) and Geist Mono (IDs, scores, terminal), from Google Fonts. Sizes 10.5 / 11.5 / 12.5 / 13 / 13.5 / 14 / 16 / 20 / 22 / 48. Weights 400 and 500.
- **Radius**: 4 (chips), 5–7 (pills, buttons, inputs), 8–10 (cards), 12 (modal).
- **Shadows**: toast `0 20px 50px rgba(0,0,0,.6)`; modal `0 30px 80px rgba(0,0,0,.6)`.
- **Motion**: pulse 1.4s ease-in-out infinite, opacity 1 → .35; cursor blink 1s step-end; enter animation 0.2–0.25s ease-out.

## Assets
None. All icons are text glyphs (✓ ! ↑ ↓ × ⏺ ✻ ❯) or CSS shapes. The dropdown chevron is an inline SVG data-URI.
