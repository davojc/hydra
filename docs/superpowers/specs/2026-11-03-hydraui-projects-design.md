# hydraui: Projects

Status: design, 2026-11-03. A new part of hydraui, beside the orchestrator rather than built on
it. Uses environments and their gh sign-in (`2026-10-03-hydra-design.md`), bindings (§7 there),
and the MCP server (`2026-10-20-hydraui-mcp-server-design.md`). Everything not named here stays
as it is.

## 1. Why

Today an engineer writes a spec, makes milestones and tickets in Linear, then builds from the
tickets. After that, nothing keeps the spec and the tickets in step. Decisions get made in
meetings and threads and aren't written down next to the spec. A senior engineer can't see
where each person on a project is without asking them.

Projects puts everything about a project in one place: one GitHub repo for every project, and
one folder per project holding the spec, milestones, tickets, open questions, decisions made (by
whom and when), meeting notes and designs. hydraui reads and writes that repo for the engineer.
Twice a day it works out what the engineer did, including work done in plain Claude Code outside
hydra, and sends it to the project as a change whose description says what moved. The project's
lead approves other people's changes in hydraui. Nobody handles git, branches or pull requests.

Decisions (Jordan, 2026-11-03):

- **Projects are not pipelines.** They are a feature of their own, called **Projects** in
  hydraui. The orchestrator and its pipelines don't change.
- **The repo replaces Linear for projects.** Tickets live only in the projects repo, and Linear
  doesn't mirror them. The orchestrator's Linear phase stays as it is for INTH/SLA work.
- **The sync runs in each engineer's hydraui**, twice a day, with that environment's gh sign-in.
- **The sync learns what happened** from the engineer's git activity and Claude Code sessions,
  and Claude works out which project each piece belongs to.
- **A sync covers anything that changed in the project**: ticket progress, spec edits, new
  milestones, bugs. It is one change per engineer, and its description makes the day's progress
  clear.
- **The sync goes out automatically**, with no preview. The engineer sees what was sent and can
  add a correction afterwards.
- **One lead per project**, who can hand the role over. The lead's changes merge at once.
  Everyone else's wait for the lead's approval in hydraui.
- **Engineers never deal with GitHub or PRs.** hydra does all of that behind the scenes, and
  hydraui shows it.
- **Projects are created in hydraui**: "New project" drafts the spec, milestones and tickets
  with Claude.
- **Project files are edited only through hydraui.** Plain Claude Code work feeds the progress
  write-up and never edits project files directly.
- **Open questions are answered in hydraui** by the person they're assigned to. hydraui
  surfaces them to that person.
- **Meeting notes and designs are added in hydraui.** Claude pulls the decisions and questions
  out of meeting notes.
- **Everyone sees every project.** The lead additionally gets the controls to approve changes.
- **The projects repo is set per environment.**

### Goals

- A projects repo format (§2) that people and Claude can read on GitHub without hydra.
- The Projects views in hydraui (§7): the list, one project's spec, tickets, decisions, notes,
  designs and per-engineer progress, plus **Waiting on you**.
- Changing a project in hydraui (§4) and the change flow behind it (§5): the lead's changes merge
  at once, everyone else's wait for the lead.
- The twice-daily sync (§6).
- New project with Claude (§8.1), and meeting notes with Claude (§8.2).

### Non-goals (this spec)

- Importing an existing Linear project. A later, separate tool can do this if it's wanted.
- The orchestrator building a project's tickets. A later phase can let a pipeline take its
  tickets from a project rather than from Linear.
- Slack or email notices. Questions and approvals are surfaced in hydraui only.
- Git LFS or large binaries. Designs have a size limit (§2).
- Editing project files outside hydraui, in a local clone or on GitHub. hydra doesn't stop it, but
  it isn't a supported path (§10).
- Reports across projects (burn-down, velocity).

### Anti-goals (a failure even if it works)

- Through hydra, a change from someone other than the lead reaches the project without the
  lead's approval.
- Raw session text, file contents from Claude transcripts, or anything that looks like a secret
  ending up in the projects repo. The sync writes summaries only.
- The sync sending work from another environment, or from a folder bound to one, to this
  environment's projects repo.
- An engineer being shown git, branches, PRs or a merge conflict, apart from the single case in
  §5.3.

## 2. The projects repo

There is one repo per organisation, such as `iov42/projects`, with one folder per project. Every
file is either Markdown with TOML front matter between `+++` lines, or plain TOML. TOML matches
the rest of hydra, and GitHub renders the Markdown.

```
projects/
  README.md                     # written by hydra on first use: what this repo is
  <slug>/                       # e.g. product-groups
    project.toml                # name, key, lead, members, status, repos
    spec.md                     # the spec; history is git history
    milestones.toml             # ordered list: id, title, target date, status
    tickets/PG-12.md            # front matter + description
    decisions/D-7.md            # an open question or a decision made
    meetings/2026-11-03-kickoff.md
    designs/<file>              # images or PDFs, at most 10 MB each
    designs/links.toml          # Figma and other links, with titles
    progress/<login>/2026-11-03.md   # what the sync sent that day (§6)
```

```toml
# project.toml
name = "Product Groups"
key = "PG"                      # ticket prefix, unique in the repo
lead = "jmiller-iov42"          # GitHub login
members = ["aza-iov42", "jmiller-iov42"]
status = "active"               # active | paused | done
repos = ["iov42/version2-frontend", "iov42/version2-backend"]   # a hint for the sync, optional
```

A **ticket** has `title`, `kind` (`feature`, `bug` or `chore`), `status` (`todo`,
`in progress`, `in review`, `done` or `dropped`), `milestone`, `assignee`, `created_by` and
`created`, with its description underneath.

A **decision** file is one of two kinds:

- An **open question**: `question`, `asked_by`, `asked_at`, `assigned_to`, an optional
  `options` list and an optional `ticket`.
- A **decision made**: the same fields plus `answer`, `decided_by` and `decided_at`.

A question becomes a decision in the same file, so the question and its answer stay together.
Decisions are never deleted. A decision that is reversed gets a new decision that names the one
it replaces (`replaces = "D-7"`).

**Who is who.** People are named by GitHub login, which is what the environment's gh sign-in
gives hydra. The lead check, "assigned to me" and "decided by" all rest on that login. This is
our call: the login is the only identity hydra already has for everyone.

**Numbers.** Ticket and decision numbers are per project. A new item takes the next number free
across the default branch and every open change for that project. If two changes still collide
when hydra rebuilds one (§5.3), the change that merges second is renumbered, and the references
inside it are rewritten. This is our call: it keeps short, readable ids like `PG-12` without a
server to hand out numbers.

## 3. Setting it up

The environment's env.toml gets an optional `[projects]` table:

```toml
[projects]
repo = "iov42/projects"
sync = ["12:30", "17:30"]       # local times; these are the defaults
```

This is a new optional table, so existing environments need no migration. hydraui's environment
settings get a **Projects** section with these fields. An environment without the table shows
nothing new.

hydra keeps its own checkout of the repo at `state/<env>/projects/` and uses it for every read
and write. Engineers never open it. hydraui fetches the repo when it starts, every 5 minutes
while it's open, and when the Projects view comes to the front. This is our call: it's cheap,
and questions reach their person within minutes.

## 4. Changing a project in hydraui

Every edit goes through hydraui's views (§7): editing the spec, adding or moving a ticket, adding
a milestone, asking or answering a question, adding meeting notes or a design, and editing
`project.toml` (members, lead hand-over, status). hydraui records each edit as a step in the
engineer's **pending change** for that project and shows it at once as "not sent yet".

A pending change is sent:

- by the sync (§6), with the day's progress;
- at once, for asking or answering a question, so it reaches its person or the lead without
  waiting for the next sync (our call);
- when the engineer presses **Send now**.

## 5. The change flow, behind the scenes

### 5.1 Sending

When a change is sent, hydra:

1. builds a branch `hydra/<login>/<slug>/<date>` from the latest default branch by replaying the
   change's steps;
2. pushes the branch with the environment's gh sign-in;
3. opens a PR, or updates today's PR if one is already open. The description comes from the
   steps and, for a sync, the progress write-up (§6.3).

There is one PR per engineer, per project, per day. If yesterday's PR is still waiting for the
lead, today's change is a separate PR, and the lead sees both.

### 5.2 Merging

- **The lead's own change**: hydra merges it straight away (squash), so the lead never waits.
- **Anyone else's change**: hydra asks the lead to review it on GitHub. It then shows in the
  lead's **Waiting on you** (§7.2) as a list of what the change does, not as a diff. **Approve**
  approves and merges it with the lead's gh sign-in. **Send back** with a note turns it into an
  item on the author's **Waiting on you**. The author edits in hydraui, and their next send
  updates the same PR.

hydra makes these checks before it merges. GitHub can't enforce them by itself, because a
required-review rule would also block the lead's own PRs. Whether to add a GitHub rule as well
is open (§10).

### 5.3 Clashes

Each send rebuilds the branch from the latest default branch by replaying steps, not by merging
git history. Most steps are structured (a field on a ticket, a new file), so they can't clash in
practice. The spec, meeting notes and descriptions are free text and are merged three ways. A
change hydra can't settle on its own (the same lines changed two ways) is the one case where the
engineer sees a clash: hydraui shows both texts side by side and asks them to pick or edit.

## 6. The sync

### 6.1 When

Sync runs at each time in `sync`, in every environment with a `[projects]` table, while hydraui
is open. If a time passed while hydraui was closed, the sync runs once when hydraui next starts,
and that run covers everything since the last sync. A sync that finds no activity and no pending
change sends nothing.

### 6.2 What it reads

hydra gathers the evidence itself. It doesn't let Claude roam the disk.

- **Claude Code sessions since the last sync**:
  - the environment's own transcripts (`state/<env>/claude/projects/`);
  - transcripts in the shared base (`~/.claude/projects/`) whose working folder is **bound to
    this environment**.

  Sessions run in plain `claude` outside hydra land in the base, so the second source is how
  outside work is seen. A folder bound to another environment, or not bound at all, is never
  read (the anti-goal in §1).
- **Git**: commits since the last sync by the environment's git author, in repos under folders
  bound to this environment, plus their branch names.
- **GitHub**: PRs the engineer's login opened, updated, reviewed or merged since the last sync,
  through the environment's gh.
- **The project list**: every project the engineer is lead or a member of, with its tickets,
  milestones and open questions.

### 6.3 What Claude does

hydraui starts a Claude session in the environment, in a quiet terminal as the orchestrator's
stage sessions are. It gives the session a prompt and a folder holding the evidence above. The
session has the project tools only (§9) and no file-write or shell tools. For each project the
engineer touched, it reports:

- a short **progress write-up** in plain words: what moved, what's blocked, and what's next;
- **ticket moves** with their evidence (for example "PG-12 → in review: PR iov42/backend#1712
  opened"), only where the evidence supports the move;
- **new tickets** for bugs or work found along the way, marked as found by the sync;
- **questions the engineer was stuck on**, as open questions assigned to the lead, marked as
  raised by the sync.

Work that matches none of the engineer's projects is dropped. It isn't written down anywhere. The
session says when it is guessing which project something belongs to, and the write-up shows that
as "probably".

hydra then adds these as steps to the engineer's pending change for that project. It writes the
write-up to `progress/<login>/<date>.md` (a second sync that day adds to the same file) and sends
the change (§5).

These are our calls: the sync can move tickets and add tickets and questions, because that is
"what happened", but it never edits the spec or milestones, which are only changed through
hydraui by a person. The write-up never quotes transcripts or code.

### 6.4 After it's sent

The engineer's project view shows each sync, what it sent and whether the lead has approved it.
**Add a correction** adds a note, or reverses a ticket move, in the next change. The sync uses the
environment's Claude account, so it counts against that account's usage like any session. A sync
that would start while less than 30% of the seven-day window is left waits and says so, the same
rule as the orchestrator (our call).

## 7. The views

Projects is a new entry in each environment that has a `[projects]` table. It sits beside the
terminals and the task board and uses the orchestrator's colours and type.

### 7.1 Projects and one project

- **Projects**: every project in the repo, with its name, lead, status, milestone progress,
  open questions, and the time of its last change.
- **One project**, in tabs:
  - **Spec**: rendered, with Edit.
  - **Tickets**: a board by status, grouped by milestone, with New ticket and New bug.
  - **Decisions**: open questions (who they're waiting on, and for how long) and decisions made
    (who decided, and when). Ask a question here.
  - **Meetings**: notes, newest first. Add meeting notes here (§8.2).
  - **Designs**: images and PDFs shown inline, plus links.
  - **Progress**: one row per engineer with their latest write-up, their tickets in progress, and
    any change waiting for the lead. Opening a row shows their day-by-day history.
  - **Changes**: this engineer's sent and pending changes and their state (sent, waiting for the
    lead, merged, sent back). The lead also sees everyone's here.

### 7.2 Waiting on you

A list across all projects, with a count in the environment list:

- questions assigned to you;
- changes waiting for your approval, if you're a lead;
- your changes the lead sent back;
- clashes to settle (§5.3).

A new item also shows a hydraui toast once. Answering a question gives a text box, plus the
`options` as buttons when the question has them. The answer is recorded with your login and the
time, and it is sent at once (§4).

## 8. Claude in the views

### 8.1 New project

**New project** asks for a name, a key and the code repos. It then opens a Claude session in a
terminal beside a live preview of the draft. The engineer talks the project through with Claude,
which writes the spec, milestones, tickets and first open questions through the draft tools
(§9). **Create** sends the draft as one change, with the creator as lead, so it merges at once.
Until then nothing leaves the machine, and **Discard** drops the draft.

### 8.2 Meeting notes

Paste or type notes and press **Save**. Claude reads them and suggests decisions made, new open
questions (with whom they're probably for) and new tickets. Each suggestion is a row the engineer
ticks or edits before it's added. Nothing from the notes becomes a decision on its own. The notes
themselves are always saved.

## 9. The tools

These are added to the MCP server and pre-approved in each environment's settings, as the task
tools are. Each one is limited to the session hydraui started it for: a draft session sees only
its draft, and a sync session sees only its evidence and the engineer's projects (as roles are in
the orchestrator spec, §3.3).

- **Draft**: `draft_show`, `draft_set_spec`, `draft_add_milestone`, `draft_add_ticket`,
  `draft_add_question`, `draft_edit(id, …)`, `draft_remove(id)`.
- **Sync**: `sync_evidence`, `sync_projects`, `sync_report(project, writeup, moves, tickets,
  questions)`. There is one report per project and the session ends after the last one. A
  report with a move and no evidence is refused.
- **Meeting notes**: `notes_suggest(decisions, questions, tickets)`.

No tool merges, approves, or edits a project directly. Those actions belong to a person, in the
views.

## 10. Open questions

- **GitHub-side enforcement.** Should the repo also have a rule, such as rulesets or CODEOWNERS
  per project folder, so that editing on GitHub can't get around the lead? It would need a way
  for the lead's own changes to merge, either as a bypass actor or through a bot account. As
  written, only hydra enforces the lead's approval.
- **Ticket links from code.** Should "Start ticket" in hydraui make a branch named with the ticket
  key in a code repo? It would give the sync firmer evidence. Left out, so that how engineers
  work stays as it is.
- **Answering from outside hydra.** People who don't run hydra, such as product, can only answer
  questions on GitHub today.

## 11. Testing

**Unit tests:**

- parsing and writing every file kind, keeping comments and unknown keys;
- numbering, and renumbering on a collision;
- replaying steps onto a moved default branch, including the three-way merge of text and a clash
  it can't settle;
- the lead and non-lead merge rules;
- the sync's evidence gathering:
  - only bound folders;
  - never another environment's;
  - the time window since the last sync;
  - a missed time run once at start;
- `sync_report` validation;
- the tools' per-session limits.

**End-to-end test**, against a fake GitHub (as hydra-update's `fake` feature does): a lead
creates a project, a member's sync sends a change, the lead approves it, and it merges.

**Checked by hand (the gate):** a real project with two engineers over two days on macOS and
Windows. One engineer works only in plain Claude Code outside hydra and their progress still
shows. A question asked by one person is answered by another in hydraui. The lead sends one
change back, and the author fixes it and resends.

## 12. Calls made without the user

These are marked "our call" above. In addition:

- the file formats (Markdown with TOML front matter, `milestones.toml`, `designs/links.toml`);
- the folder layout, and keeping progress write-ups in the repo so the Progress view works from
  the repo alone;
- branch names under `hydra/`, and squash merges;
- the 10 MB design limit;
- fetching every 5 minutes;
- the default sync times of 12:30 and 17:30;
- questions raised by the sync going to the lead.
