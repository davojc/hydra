# Plan: orchestrator phase 2, tickets from Linear

Spec: `docs/superpowers/specs/2026-10-23-hydraui-orchestrator-linear-design.md`. Branch `linear`,
stacked on `orchestrator` (davojc/hydra#2). Every task is test-first (red, then green), ends with
the narrow checks (`cargo test -p <crate> <filter>`, `cargo clippy -p <crate> --all-targets -- -D
warnings`, `cargo fmt --all --check`), and is one commit. The full gate runs before each push.

## Task 1: Linear as an environment tool (spec §1)

Follow `db55ff0` (jev) exactly.

- `hydra-core/config.rs`: `pub linear: Option<ApiKeyConfig>`; `validate` requires
  `[linear] api_key` to be a secret. Test: `linear_key_must_be_a_secret`.
- `hydra-core/envedit.rs`: `"linear"` in `TOOLS`; the `api_key` arms and messages become
  "gemini, jev and linear". Test: `linear_api_key_validates`. Update the jev test's message.
- `hydra-providers/linear.rs`: the `Linear` provider, `LINEAR_API_KEY` from the secret, with
  `check`. Tests as `jev.rs`. Registered in `all()`.
- `hydra-cli/cli.rs`: help text.
- `hydraui/accounts.rs`: `linear` in `SETUP_TOOLS`, `form_fields`, `form_values`. The workspace's
  `ApiKey` handling must already cover it. Check that no `"jev"` arm is missing `"linear"`.

## Task 2: the pipeline's `[linear]` table (spec §2)

- `pipeline.rs`: `Pipeline.linear: Option<LinearSettings{team, pick, claim, passed,
  every_minutes}>`, with defaults for all but `team`. A `[linear]` without `team` is an error.
  `Edit` and `save` carry it, and an absent table stays absent. Tests: parse with defaults, parse
  overrides, the missing-team error, the save round trip.

## Task 3: the GraphQL client (spec §6)

- `hydraui/linear.rs`:
  - `trait LinearApi` with `viewer()`, `states(team)`, `pickable(team, state_id)` (unassigned,
    `slaBreachesAt` set, ordered), `claim(issue_id, user_id, state_id)`, `comment(issue_id,
    body)`, `set_state(issue_id, state_id)`.
  - `Ticket{id, identifier, title, url, sla_breaches_at, sla_high_risk_at, assignee}`.
  - `Http` implements it on `ureq` (POST `https://api.linear.app/graphql`, `Authorization: <key>`).
  - Request building and response parsing are pure functions, tested with recorded JSON. That
    includes the GraphQL `errors` array and a 401 reading "Linear rejected the API key".

## Task 4: the watcher's decisions (spec §3, §4)

- `hydraui/linear_watch.rs`, pure:
  - `pick(tickets) -> Option<&Ticket>`: soonest breach first, unassigned only.
  - `due(last_check, now, every, has_open_run, watching) -> bool`.
  - `Failures`: three in a row stops watching; a rejected key stops at once.
  - `claim_comment(pipeline, run)`, and `end_comment(run)` with stage scores, summaries, and PR
    URLs extracted from the ratings (`https://github.com/<o>/<r>/pull/<n>`, deduped).
  - `watcher_conflict(team, watchers)`: only one per team.
- Unit tests for each, including the claim race (the first is no longer unassigned → the next
  one).

## Task 5: watching in the app (spec §3, §4)

- Workspace state `linear_watch: HashMap<PaneId, Watch{on, last, next, failures}>`, a timer
  task (every 30 s; acts when `due`), and **Watch Linear** / **Check Linear now** on the card.
- A check:
  1. Off the UI thread: read the environment's `LINEAR_API_KEY` (as `prepare` resolves secrets),
     run `states` + `pickable` through `Http`, and claim the first ticket.
  2. Then type the start prompt into the orchestrator (the existing `pending_wakes` path).
  3. Log `linear claim`.
- End writes on `RunFinished` / `RunStopped` for runs whose ticket matches the team, logged
  `linear comment` / `linear state`. `run_resume` and History ignore these events except to show
  them.
- Escalation on the third failure (existing `escalate` path, reason `Linear: …`).

## Task 6: the views (spec §5)

- Card: the SLA pill, the Linear link and the "Watching Linear · looked 2m ago" idle line.
- The Pipeline tab header gets the SLA and the link. History gets the SLA column. Settings gets
  the Linear section.

## Task 7: docs and the gate

README's Orchestrator line, the site's app page, spec §9 "As built". Full gate, push, and stack a
draft PR on #2 when the user says so.

## Needs from Jordan

A Linear personal API key for the iov42 environment (Linear → Settings → Security & access →
Personal API keys). Jordan types it into hydraui's **Set up linear…** or `hydra add linear`;
never into chat.
