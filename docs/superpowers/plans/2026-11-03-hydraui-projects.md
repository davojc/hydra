# hydraui Projects Implementation Plan

**Goal:** The Projects feature of `docs/superpowers/specs/2026-11-03-hydraui-projects-design.md`,
built in slices that each run in hydraui on their own.

**Architecture:**
- A new crate, `hydraui-projects`, holds everything that has no UI: the repo's file formats,
  milestone progress, numbering and the check's rules. It is pure and fully unit-tested, like
  `hydraui-sets`.
- hydraui wires it into GPUI the way the orchestrator does: pure rules in a module, with the
  view in `workspace/projects_tab.rs`.
- State is a `projects: Option<ProjectsTab>` on `Workspace`, opened from a button in the tab
  strip next to Settings, and ticked from `lane_tick`.
- MCP tools follow `run_tools.rs`: names, schemas, and `call(reg, token, ..)`, with the role
  taken from a token → draft map in `Registry`.

## Global constraints

- **Where the repo is set:** `EnvConfig` denies unknown fields, so a hydra built before
  Projects refuses an env.toml with `[projects]`. That would break the installed hydra and
  hydraui the moment the prototype wrote it. Until a release that knows `[projects]` is
  installed everywhere, the setting lives in `<HYDRA_HOME>/hydraui/projects.toml`
  (`[repos] <env> = "owner/name"`). It moves to env.toml as the spec says (§3) when sending ships.
- hydra's checkout lives at `state/<env>/projects/<owner>-<name>/`. Engineers never open it.
- No GUI automation or synthetic keys from subagents. The user checks the UI by running the app.
- CI trio: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.

## Slices

### Slice 1: see, start and edit projects (local)

- **Done:** the `hydraui-projects` crate: front matter, model, load and write, ids, progress,
  late, and the ticket-key rules (20 tests).
- **Config:** the env's repo in `hydraui/projects.toml`, set from a Projects row on the Settings env page. The
  value is an `owner/name` or a local path (for trying it out without GitHub).
- **Checkout:** clone the repo if it's missing, then `git pull --ff-only` on open and every
  5 minutes, through `hydra run <env> -- git …`.
- **Projects tab:**
  - active-project cards, each with one bar per milestone and its open questions;
  - a project overview with milestones, decisions (outstanding and answered) and progress;
  - a milestone board with tickets by status, where a ticket's menu moves it, and New ticket;
  - Waiting on you: questions assigned to me, answered in place;
  - project Settings: repos, members, sync times and status.
- **Pending change** (prototype shortcut, stated in the PR): edits are written to the checkout
  and left uncommitted, so the pending change is the working-tree diff, and "not sent yet" comes
  from `git status`. The spec's step replay (§5.3) comes with slice 2.
- **New project:**
  - a name dialog makes a draft folder outside the checkout;
  - a Claude terminal opens with the draft tools (`draft_*`, scoped by token) and the draft's
    preview;
  - files dropped on the preview and links are added as resources;
  - **Create** moves the draft into the checkout and opens its Settings.

### Slice 2: send and approve (GitHub)

- Send now: branch, push and PR with the environment's gh.
- The lead's change merges at once. Other people's changes go to the lead's Waiting on you
  (Approve / Send back).
- Questions and answers are sent at once.
- Replaying steps and settling clashes (§5.3).

### Slice 3: the local check and the repo sync

- The 5-minute git/gh check (§6.1), using `progress::ticket_ids_in` and `move_for`.
- The hourly Claude pass and the check tools (§6.2), with the 30% usage rule.
- The repo sync at the project's times, with the write-up in `progress/` (§7).

### Slice 4: meeting notes

- Notes with Claude's suggestions, each one ticked before it's added (§9.2).
