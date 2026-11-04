# The projects repo

One folder per project. hydraui writes every file; you change them only through the plan tools
(`plan_*`), which also check the rules in `../SKILL.md`. The lead sends the changes to GitHub
from hydraui, as a pull request they merge; other people's changes wait for the lead's approval.

```
<slug>/                   e.g. mapping-ux-enhancements
  project.toml            name, key (ticket prefix), lead, members, status, repos, sync times
  spec.md                 the spec, Markdown
  milestones.toml         the milestones, in order
  tickets/<KEY>-<n>.md    one ticket each: TOML front matter between +++ lines, then Markdown
  decisions/D-<n>.md      a question, and once answered, the decision
  meetings/<date>-<name>.md
  resources/<file>        attached files (at most 10 MB each)
  resources/links.toml    attached links, with titles
  progress/<login>/<date>.md
.claude/skills/project-planning/   this skill: SKILL.md, references/, checklists/
```

## Milestones (`milestones.toml`)

```toml
[[milestone]]
id = "M1"
title = "Partners can see shared groups"
target = 2026-12-04
status = "planned"            # planned | active | done
review = "proposed"           # proposed | changes | approved | rejected
description = """
- Partners see the groups we share with them
  The full explanation, indented under its point.
- Done when a partner opens a shared group in Interu
  ..."""
```

The description is the points: each a `- ` line, its detail indented two spaces under it.
`plan_propose_milestone` takes them as `points: [{point, detail}]` and writes this for you.

## Tickets (`tickets/PG-1.md`)

```
+++
title = "Show shared groups in the partner's group list"
kind = "feature"              # feature | bug | chore
status = "todo"               # todo | in progress | in review | done | dropped
milestone = "M1"
assignee = ""
review = "proposed"
+++

What to do, where, and how to tell it's done.
```

## Decisions (`decisions/D-1.md`)

A question (`question`, `asked_by`, `assigned_to`, `options`, optional `ticket`), which becomes
a decision when answered (`answer`, `decided_by`, `decided_at`). Never deleted; a reversal is a
new decision with `replaces = "D-1"`.
