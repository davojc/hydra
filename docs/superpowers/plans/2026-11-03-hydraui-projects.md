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

- **Done (`project_send.rs`, 7 tests against a bare repo and a fake gh):**
  - Send now: the project's pending files are rebuilt on the latest default branch in a
    separate index, pushed to `hydra/<login>/<slug>/<date>` and opened as a pull request with
    the environment's gh. Its title and body say what changed in words.
  - The lead's change merges at once (squash). Other people's go to the lead's Overview and
    Waiting on you, with Approve and Send back…; the lead's note shows on the author's
    project page until they send again.
  - Questions and answers are sent at once, quietly.
  - Clashes: a text file both sides changed merges three ways; the same lines changed two
    ways asks the engineer Keep mine or Use theirs, then sends again.
  - Fetching settles the checkout onto the repo: a pending file that's now the same in the
    repo (sent and merged) is dropped first, so it doesn't clash with itself.
- **Different from the spec, for now:**
  - Steps aren't replayed (§5.3): the pending change is still the working tree, rebuilt file
    by file. Structured files (milestones.toml, a ticket's front matter) merge as text, so two
    edits to the same milestone line clash rather than merging field by field.
  - One open pull request per engineer per project, not per day: while one waits for the
    lead, the next send updates it. A second one would carry the first one's changes too,
    since both come from the same working tree.
  - An empty repo gets its first commit (a README on `main`) from whoever sends first.
  - The repo setting stays in `hydraui/projects.toml` (see Global constraints).

### Slice 3: the local check and the repo sync

- **Done (`project_check.rs`, 6 tests):**
  - **The check (§6.1)** runs after each 5-minute fetch, for every project you're on. For each
    code repo it reads:
    - your pull requests, through the environment's gh (`--author @me`): open means in review,
      merged means done;
    - in the local clone, when there is one: its branch names, and your last 14 days of commit
      subjects, matched by the environment's git email (in progress).

    Ticket keys in any of them move tickets forward only; pull request links are recorded on
    the ticket. The moves are local edits, so they're part of the pending change. The
    projects page says when the check last ran, and what it couldn't read.
  - **The repo sync (§7)** runs at each project's sync times, and once on start for a time
    missed today. It writes `progress/<login>/<date>.md`: what moved, then what else changed;
    a second sync that day adds a section. Then it sends the pending change, quietly. A project
    hydraui sees for the first time starts from its next sync time.
- **Different from the spec, for now:**
  - The write-up is built from the pending change, not written by Claude. That comes with the
    hourly pass.
  - Only the latest missed time today runs: a time missed yesterday is caught up by today's
    first sync.
  - The check is stateless rather than "since the last check": moves only go forward, so
    seeing the same pull request again changes nothing.
- **Next:** the hourly Claude pass and the check tools (§6.2), with the 30% usage rule.

### Slice 4: meeting notes

- Notes with Claude's suggestions, each one ticked before it's added (§9.2).

### Milestone runs (spec §14.5), added 2026-11-04

- **Done:**
  - `project_run.rs` (5 tests): the next step of a run. Work the next approved ticket by
    number, check an in-review one with the testing skill, pause, ask for a clone, or finish.
    It also works out the branch a ticket stacks on.
  - Testing sessions (`Session::Check`) report with `ticket_checked`.
  - Work settings per project, on this machine.
  - Folder pickers for every skill.
  - Start milestone, Carry on, Open session and Stop on the milestone page and its row.
  - Per-ticket Start work is gone.
- **Not yet:** a run lives only in memory. After hydraui restarts, Start milestone picks it
  back up from the tickets' state. A ticket that was in progress starts again in its existing
  worktree.

