# hydraui: Projects

Status: design, 2026-11-03; revised the same day (the Projects tab, the new-project flow, the
milestone views and the local check). A new part of hydraui, beside the orchestrator rather than
built on it. Uses environments and their gh sign-in (`2026-10-03-hydra-design.md`), bindings (§7
there), and the MCP server (`2026-10-20-hydraui-mcp-server-design.md`). Everything not named here
stays as it is.

## 1. Why

Today an engineer writes a spec, makes milestones and tickets in Linear, then builds from the
tickets. After that, nothing keeps the spec and the tickets in step. Decisions get made in
meetings and threads and aren't written down next to the spec. A senior engineer can't see
where each person on a project is without asking them.

Projects puts everything about a project in one place: one GitHub repo for every project, and
one folder per project holding the spec, milestones, tickets, open questions, decisions made (by
whom and when), meeting notes and resources (files and links). hydraui reads and writes that
repo for the engineer.

During the day, hydraui keeps the engineer's own view current: tickets complete and milestones
fill up as they work, including work done in plain Claude Code outside hydra. Once or twice a day
it sends that progress to the project as a change whose description says what moved. The
project's lead approves other people's changes in hydraui. Nobody handles git, branches or pull
requests.

Decisions (Jordan, 2026-11-03):

- **Projects are not pipelines.** They are a feature of their own, called **Projects**, with a
  tab of their own in hydraui. The orchestrator and its pipelines don't change.
- **The repo replaces Linear for projects.** Tickets live only in the projects repo, and Linear
  doesn't mirror them. The orchestrator's Linear phase stays as it is for INTH/SLA work.
- **There are two speeds:**
  - **The local check** keeps the engineer's own view current all day. It never leaves the
    machine (§6).
  - **The repo sync** sends that progress to the projects repo once or twice a day, as the lead
    sets for the project (§7). The lead and others see an engineer's progress at the repo sync,
    not before.
- **The local check is cheap.** Every few minutes hydra checks git and GitHub itself, with no
  Claude, and moves what it can be sure of. Once an hour, if there was new activity, a short
  Claude pass reads the sessions too.
- **Both run in each engineer's hydraui**, with that environment's gh sign-in. They learn what
  happened from git activity and Claude Code sessions, and Claude works out which project each
  piece belongs to.
- **A repo sync covers anything that changed in the project**: ticket progress, spec edits, new
  milestones, bugs. It is one change per engineer, and its description makes the progress clear.
  It goes out automatically, with no preview. The engineer sees what was sent and can add a
  correction afterwards.
- **One lead per project**, who can hand the role over. The lead's changes merge at once.
  Everyone else's wait for the lead's approval in hydraui.
- **Engineers never deal with GitHub or PRs.** hydra does all of that behind the scenes, and
  hydraui shows it.
- **Starting a project**: the Projects tab asks only for a name, then opens a Claude session to
  fill in the details and attach resources. Once the project is created, its settings take the
  repos, members and sync times (§9.1).
- **The project view** shows milestones visualised. Inside a milestone are its tickets and their
  progress, and the overview shows outstanding and answered decisions at a high level (§8).
- **Project files are edited only through hydraui.** Plain Claude Code work feeds progress and
  never edits project files directly.
- **Open questions are answered in hydraui** by the person they're assigned to. hydraui
  surfaces them to that person.
- **Meeting notes are added in hydraui.** Claude pulls the decisions and questions out of them.
- **Everyone sees every project.** The lead additionally gets the controls to approve changes
  and change settings.
- **The projects repo is set per environment.**

### Goals

- A projects repo format (§2) that people and Claude can read on GitHub without hydra.
- The Projects tab (§8): the active projects, one project's milestones, each milestone's
  tickets, decisions at a glance, per-engineer progress, and **Waiting on you**.
- Starting a project with Claude, then configuring it (§9.1).
- Changing a project in hydraui (§4) and the change flow behind it (§5): the lead's changes merge
  at once, everyone else's wait for the lead.
- The local check (§6) and the repo sync (§7).
- Meeting notes with Claude (§9.2).

### Non-goals (this spec)

- Importing an existing Linear project. A later, separate tool can do this if it's wanted.
- The orchestrator building a project's tickets. A later phase can let a pipeline take its
  tickets from a project rather than from Linear.
- Showing other engineers' progress between repo syncs.
- Slack or email notices. Questions and approvals are surfaced in hydraui only.
- Git LFS or large binaries. Resource files have a size limit (§2).
- Editing project files outside hydraui, in a local clone or on GitHub. hydra doesn't stop it, but
  it isn't a supported path (§11).
- Reports across projects (burn-down, velocity).

### Anti-goals (a failure even if it works)

- Through hydra, a change from someone other than the lead reaches the project without the
  lead's approval.
- Raw session text, file contents from Claude transcripts, or anything that looks like a secret
  ending up in the projects repo. Only summaries are sent.
- Progress from another environment, or from a folder bound to one, reaching this environment's
  projects.
- Anything from the local check leaving the machine before a repo sync or a **Send now**.
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
    project.toml                # name, key, lead, members, status, repos, sync times
    spec.md                     # the spec; history is git history
    milestones.toml             # ordered list: id, title, target date, status
    tickets/PG-12.md            # front matter + description
    decisions/D-7.md            # an open question or a decision made
    meetings/2026-11-03-kickoff.md
    resources/<file>            # attached files: images, PDFs, docs, at most 10 MB each
    resources/links.toml        # attached links (Figma, docs, threads), with titles
    progress/<login>/2026-11-03.md   # what the repo sync sent that day (§7)
```

```toml
# project.toml
name = "Product Groups"
key = "PG"                      # ticket prefix, unique in the repo; suggested from the name
lead = "jmiller-iov42"          # GitHub login
members = ["aza-iov42", "jmiller-iov42"]
status = "active"               # active | paused | done
repos = ["iov42/version2-frontend", "iov42/version2-backend"]   # a hint for the checks, optional

[sync]
times = ["12:30", "17:30"]      # once or twice a day, local times; set by the lead
```

A **milestone** has `id`, `title`, `target` (a date), `status` (`planned`, `active` or `done`)
and an optional `description`. Its progress is worked out from its tickets (§8.2), not stored.

A **ticket** has `title`, `kind` (`feature`, `bug` or `chore`), `status` (`todo`,
`in progress`, `in review`, `done` or `dropped`), `milestone`, `assignee`, `created_by` and
`created`, with its description underneath.

A **decision** file is one of two kinds:

- An **open question**: `question`, `asked_by`, `asked_at`, `assigned_to`, an optional
  `options` list, and an optional `ticket` or `milestone`.
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

The environment's env.toml gets an optional `[projects]` table naming the repo:

```toml
[projects]
repo = "iov42/projects"
```

This is a new optional table, so existing environments need no migration. hydraui's environment
settings get a **Projects** section with this field. Sync times belong to each project (§2), not
to the environment.

hydraui gets a **Projects** tab. It lists the projects of every environment that has a
`[projects]` table, grouped by environment when there is more than one. With no environment set
up, the tab says how to set one up.

hydra keeps its own checkout of each repo at `state/<env>/projects/` and uses it for every read
and write. Engineers never open it. hydraui fetches the repo when it starts, every 5 minutes
while it's open, and when the Projects tab comes to the front. This is our call: it's cheap, and
other people's merged changes and questions reach you within minutes.

## 4. Changing a project in hydraui

Every edit goes through hydraui's views (§8): editing the spec, adding or moving a ticket, adding
a milestone, asking or answering a question, adding meeting notes or a resource, and changing the
project's settings (repos, members, sync times, lead hand-over, status). hydraui records each
edit, and each ticket move from the local check (§6), as a step in the engineer's **pending
change** for that project, and shows it at once.

A pending change is sent:

- by the repo sync (§7), with the day's progress;
- at once, for asking or answering a question, so it reaches its person or the lead without
  waiting for the next repo sync (our call);
- when the engineer presses **Send now**.

## 5. The change flow, behind the scenes

### 5.1 Sending

When a change is sent, hydra:

1. builds a branch `hydra/<login>/<slug>/<date>` from the latest default branch by replaying the
   change's steps;
2. pushes the branch with the environment's gh sign-in;
3. opens a PR, or updates today's PR if one is already open. The description comes from the
   steps and, for a repo sync, the progress write-up (§7).

There is one PR per engineer, per project, per day. If yesterday's PR is still waiting for the
lead, today's change is a separate PR, and the lead sees both.

### 5.2 Merging

- **The lead's own change**: hydra merges it straight away (squash), so the lead never waits.
- **Anyone else's change**: hydra asks the lead to review it on GitHub. It then shows in the
  lead's **Waiting on you** (§8.4) as a list of what the change does, not as a diff. **Approve**
  approves and merges it with the lead's gh sign-in. **Send back** with a note turns it into an
  item on the author's **Waiting on you**. The author edits in hydraui, and their next send
  updates the same PR.

hydra makes these checks before it merges. GitHub can't enforce them by itself, because a
required-review rule would also block the lead's own PRs. Whether to add a GitHub rule as well
is open (§11).

### 5.3 Clashes

Each send rebuilds the branch from the latest default branch by replaying steps, not by merging
git history. Most steps are structured (a field on a ticket, a new file), so they can't clash in
practice. The spec, meeting notes and descriptions are free text and are merged three ways. A
change hydra can't settle on its own (the same lines changed two ways) is the one case where the
engineer sees a clash: hydraui shows both texts side by side and asks them to pick or edit.

## 6. The local check

The local check runs all day while hydraui is open, for every project the engineer is lead or a
member of. Its results are steps in the engineer's pending change (§4). They show in the
engineer's own views at once, marked "not sent yet", and nothing leaves the machine.

### 6.1 Every 5 minutes: git and GitHub, no Claude

hydra checks, since the last check:

- commits by the environment's git author, in repos under folders bound to this environment,
  and the branches they're on;
- PRs the engineer's login opened, updated, reviewed or merged, through the environment's gh.

When a branch name, commit message or PR title names a ticket key (such as `PG-12`), hydra moves
that ticket:

- **todo → in progress** on the first commit or branch;
- **→ in review** when a PR is opened;
- **→ done** when the PR is merged.

These rules are fixed, so they cost no tokens, and they only move a ticket forward. A ticket that
nothing names is left for the hourly pass. This is our call: a ticket key is the only thing hydra
can be sure of without Claude.

### 6.2 Every hour: a short Claude pass

If there was new activity since the last pass, hydraui starts a Claude session in the
environment, in a quiet terminal as the orchestrator's stage sessions are. hydra gathers the
evidence itself and doesn't let Claude roam the disk:

- **Claude Code sessions since the last pass**:
  - the environment's own transcripts (`state/<env>/claude/projects/`);
  - transcripts in the shared base (`~/.claude/projects/`) whose working folder is **bound to
    this environment**.

  Sessions run in plain `claude` outside hydra land in the base, so the second source is how
  outside work is seen. A folder bound to another environment, or not bound at all, is never
  read (the anti-goal in §1).
- **Git and GitHub**: what §6.1 saw since the last pass.
- **The projects**: each of the engineer's projects, with its milestones, tickets and open
  questions.

The session has the check tools only (§10) and no file-write or shell tools. For each project it
reports:

- **ticket moves** with their evidence, only where the evidence supports the move;
- **new tickets** for bugs or work found along the way, marked as found by the check;
- **questions the engineer was stuck on**, as open questions assigned to the lead, marked as
  raised by the check;
- **notes**: one or two lines per ticket it worked on, shown on the ticket as its latest
  activity.

Work that matches none of the engineer's projects is dropped. It isn't written down anywhere. The
session says when it is guessing which project something belongs to, and the views show that as
"probably".

### 6.3 Cost

The hourly pass uses the environment's Claude account, so it counts against that account's usage
like any session. It skips an hour with no new activity. It also waits, and says so, while less
than 30% of the seven-day window is left, the same rule as the orchestrator (our call).

## 7. The repo sync

The repo sync runs at each of the project's `[sync] times`, in the hydraui of every member and
the lead. If a time passed while hydraui was closed, the sync runs once when hydraui next starts.

For each project, it:

1. runs a last check (§6.1, then §6.2 if there was activity since the last pass);
2. has the same Claude session write a short **progress write-up** for the period since the last
   repo sync, in plain words: what moved, what's blocked, and what's next. The write-up is built
   from the pending change's steps and the check notes, never from quoting transcripts or code;
3. writes the write-up to `progress/<login>/<date>.md` (a second sync that day adds to the same
   file);
4. sends the pending change (§5).

A sync with no pending change sends nothing.

The sync can move tickets and add tickets and questions, because that is "what happened". It
never edits the spec, milestones or settings, which only a person changes, through hydraui (our
call).

The engineer's project view shows each repo sync, what it sent and whether the lead has
approved it. **Add a correction** adds a note, or reverses a ticket move, in the next change.

## 8. The Projects tab

The tab uses the orchestrator's colours and type. Everyone sees every project. Settings and
approvals are the lead's.

### 8.1 Active projects

The tab opens on the active projects, as cards. Each card shows:

- the name, the lead, and your role;
- one bar per milestone, filled by its done tickets;
- the number of open questions, and how many are waiting on you;
- when the project last changed.

**New project** (§9.1) is here. Paused and done projects are under a **Show all** switch.

### 8.2 A project

The project's overview shows:

- **Milestones**, visualised: a row of milestones in order, each with its target date, a bar
  split by ticket status (done, in review, in progress, todo), and "late" when the target has
  passed and the milestone isn't done. Clicking a milestone opens it (§8.3).
- **Decisions** at a high level:
  - the outstanding questions, each with who it's waiting on and for how long;
  - the most recent answers, each with who decided and when.

  Clicking one opens it. **Ask a question** is here.
- **Progress**: one row per engineer with their latest write-up and their tickets in progress.
  Opening a row shows their history, day by day.
- **Waiting on you** for this project, if anything is.

Beside the overview are **Spec** (rendered, with Edit), **Meetings** (newest first, with Add
meeting notes, §9.2), **Resources** (files shown inline where they can be, plus links, with Add),
**Changes** (your sent and pending changes and their state: sent, waiting for the lead, merged,
sent back; the lead sees everyone's), and **Settings** (§9.1).

### 8.3 A milestone

A milestone shows its tickets as a board by status, with New ticket and New bug. Each ticket
shows:

- its assignee and kind;
- its latest activity from the check (§6.2), such as "PR opened 40 min ago" or "probably: working
  on the share dialog";
- any open questions that name it.

Your unsent moves show as "not sent yet". Opening a ticket shows its description, its activity
history and its questions, and lets you edit it.

### 8.4 Waiting on you

A list across all projects, with a count on the Projects tab:

- questions assigned to you;
- changes waiting for your approval, if you're a lead;
- your changes the lead sent back;
- clashes to settle (§5.3).

A new item also shows a hydraui toast once. Answering a question gives a text box, plus the
`options` as buttons when the question has them. The answer is recorded with your login and the
time, and it is sent at once (§4).

## 9. Claude in the tab

### 9.1 Starting a project

1. **New project** asks for the project's name, and the environment when there is more than one.
2. hydraui opens a Claude session in a terminal, beside a live preview of the draft project. The
   engineer talks the project through with Claude, which writes the spec, milestones, tickets
   and first open questions through the draft tools (§10).
3. **Resources** are attached in the same view. Dropping files on the preview, or picking them,
   copies them into the draft. Pasting a link, or giving one to Claude, adds it to `links.toml`
   with a title. Claude can read attached text files and PDFs to inform the draft.
4. **Create** sends the draft as one change, with the creator as lead, so it merges at once. The
   key is suggested from the name. Until **Create**, nothing leaves the machine, and **Discard**
   drops the draft.
5. The project's **Settings** open next, for the lead to set:
   - the code repos (a hint for the checks);
   - members, picked by GitHub login;
   - the sync times (once or twice a day, at what times);
   - the key, which is fixed once the first ticket exists, and the status.

   Saving is a change like any other, so it merges at once for the lead. Until the lead saves,
   the project has no members but the lead, and syncs at the defaults (12:30 and 17:30).

### 9.2 Meeting notes

Paste or type notes and press **Save**. Claude reads them and suggests decisions made, new open
questions (with whom they're probably for) and new tickets. Each suggestion is a row the engineer
ticks or edits before it's added. Nothing from the notes becomes a decision on its own. The notes
themselves are always saved.

## 10. The tools

These are added to the MCP server and pre-approved in each environment's settings, as the task
tools are. Each one is limited to the session hydraui started it for: a draft session sees only
its draft, and a check session sees only its evidence and the engineer's projects (as roles are
in the orchestrator spec, §3.3).

- **Draft**: `draft_show`, `draft_set_spec`, `draft_add_milestone`, `draft_add_ticket`,
  `draft_add_question`, `draft_add_link`, `draft_read_resource(name)`, `draft_edit(id, …)`,
  `draft_remove(id)`.
- **Check**: `check_evidence`, `check_projects`, `check_report(project, moves, tickets,
  questions, notes)`, and at a repo sync `sync_writeup(project, text)`. There is one report per
  project, and the session ends after the last one. A report with a move and no evidence is
  refused.
- **Meeting notes**: `notes_suggest(decisions, questions, tickets)`.

No tool merges, approves, changes settings or edits a project directly. Those actions belong to a
person, in the views.

## 11. Open questions

- **GitHub-side enforcement.** Should the repo also have a rule, such as rulesets or CODEOWNERS
  per project folder, so that editing on GitHub can't get around the lead? It would need a way
  for the lead's own changes to merge, either as a bypass actor or through a bot account. As
  written, only hydra enforces the lead's approval.
- **Start ticket.** Should hydraui have a "Start ticket" button that makes a branch named with
  the ticket key in a code repo? It would make the 5-minute check (§6.1) catch far more without
  Claude. Left out, so that how engineers work stays as it is.
- **Answering from outside hydra.** People who don't run hydra, such as product, can only answer
  questions on GitHub today.

## 12. Testing

**Unit tests:**

- parsing and writing every file kind, keeping comments and unknown keys;
- numbering, and renumbering on a collision;
- replaying steps onto a moved default branch, including the three-way merge of text and a clash
  it can't settle;
- the lead and non-lead merge rules;
- the 5-minute check's rules: ticket keys in branches, commits and PRs, moving forward only;
- evidence gathering:
  - only bound folders;
  - never another environment's;
  - the window since the last pass;
- the hourly pass skipped with no activity;
- the repo sync:
  - at the project's times;
  - a missed time run once at start;
  - nothing sent when nothing is pending;
- milestone progress worked out from tickets;
- `check_report` validation;
- the tools' per-session limits.

**End-to-end test**, against a fake GitHub (as hydra-update's `fake` feature does): a lead
creates a project and sets its members, a member's check moves a ticket, their repo sync sends
it, the lead approves it, and it merges.

**Checked by hand (the gate):** a real project with two engineers over two days on macOS and
Windows:

- One engineer works only in plain Claude Code outside hydra, and still sees their tickets and
  milestone move during the day. The lead sees that progress after the repo sync.
- A question asked by one person is answered by another in hydraui.
- The lead sends one change back, and the author fixes it and resends.

## 13. Calls made without the user

These are marked "our call" above. In addition:

- the file formats (Markdown with TOML front matter, `milestones.toml`, `resources/links.toml`);
- the folder layout, and keeping progress write-ups in the repo so the Progress view works from
  the repo alone;
- branch names under `hydra/`, and squash merges;
- the 10 MB resource limit;
- fetching every 5 minutes, a 5-minute check and an hourly Claude pass;
- the default sync times of 12:30 and 17:30;
- questions raised by the check going to the lead;
- milestone progress counted by tickets done, with no weighting;
- the key suggested from the name, and fixed once tickets exist.

## 14. Planning by review (added 2026-11-03, after trying the prototype)

Jordan tried slice 1 and changed how a project is planned and built. The changes replace §9.1
(drafts and Create) and add work sessions for tickets. The rest of the spec stands.

Decisions (Jordan, 2026-11-03):

- **Starting.** New project asks for a name and makes the project at once, in hydra's
  checkout. The user attaches resources (files and links), then presses **Start project**.
- **Planning session.** Start project opens a Claude session that works on that project's
  folder. It reads the resources, writes the spec and **proposes** milestones. Nothing it
  proposes counts until a person approves it.
- **Review.** Every proposed milestone and ticket can be:
  - **approved**;
  - **updated**, in either of two ways: edit its fields in hydraui, or write a note that goes
    back to the Claude session, which revises it and proposes it again;
  - **rejected**, with a reason.

  A rejected item stays in the repo, marked rejected with its reason and hidden by default, so
  Claude sees it and doesn't propose it again.
- **Tickets per milestone.** On an approved milestone, **Create tickets** has the planning
  session propose that milestone's tickets, which are reviewed the same way.
- **Who.** Only the lead approves, updates and rejects. Any member can start work.
- **What needs review.** The project page always shows how many milestones and tickets are
  waiting for review, and where they are.
- **Work.**
  - Once its milestone is approved, an approved ticket has **Start work**. It asks for a
    skill, a model and an effort level, and which of the project's code repos to work in.
  - hydra makes a fresh worktree of the local clone of that repo, on a branch named after the
    ticket (`pg-12-share-dialog`), and starts Claude there with that skill, model and effort.
  - The session finishes by reporting its pull request URLs. They show on the ticket, and the
    ticket moves to **in review**.
- **Local clones.** The first time a repo is used, hydraui asks where it is cloned on this
  machine and remembers it.

Format additions (§2):

- Milestones and tickets get these fields:
  - `review`: `proposed`, `changes`, `approved` or `rejected`. Missing means `approved`, so
    hand-written files keep working.
  - `review_note`: the update note or the rejection reason.
  - `reviewed_by` and `reviewed_at`.
- Tickets also get `branch`, `prs`, `started_by`, `started_at`, `skill`, `model` and `effort`.

Tools (§10). These replace the draft tools:

- **Planning session**: `plan_show`, `plan_set_spec`, `plan_propose_milestone`,
  `plan_propose_ticket`, `plan_revise`, `plan_withdraw`, `plan_add_question`, `plan_add_link`
  and `plan_read_resource`.
  - It can propose and revise. It can never approve.
  - It proposes tickets only for approved milestones.
- **Work session**: `ticket_show` and `ticket_report(prs, summary)`.

Calls made without the user:
- Notes are typed into the planning session, so it acts on them at once.
- A rejection isn't typed into the session. `plan_show` shows it with its reason.
- Dropped tickets and rejected items don't count towards milestone progress.
- Local clones are remembered per machine in `hydraui/projects.toml`, under `[clones]`.

### 14.1 Claude's questions are decisions (added 2026-11-03)

Jordan: people work in hydraui, not in the sessions, though they can type to a session if they
want.

- **Asking.** A planning session asks with `plan_add_question` and a work session with
  `ticket_ask`.
  - Each question becomes an open decision, with a short context and options, assigned to the
    lead unless the session names someone else.
  - A question from a work session names its ticket.
  - Both sessions start with Claude Code's own question tool turned off
    (`--disallowedTools=AskUserQuestion`), so nothing is asked only in a terminal.
- **Surfacing.** The question shows under Waiting on you and on the project page. A toast
  appears once when it arrives, even with the Projects tab closed, while a session is running.
- **Answering.** The answer is recorded with who and when, like any decision, and typed into the
  session that asked:
  - a ticket's question goes to its work session;
  - every other question goes to the planning session.

  If that session isn't running, the answer waits in the repo, where `plan_show` and
  `ticket_show` list it.
