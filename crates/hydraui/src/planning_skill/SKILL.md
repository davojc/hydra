---
name: project-planning
description: Plan a project in the projects repo through hydraui - what a milestone is and isn't, what a good ticket and question look like, the repo's files, and what we build and where. Use in every hydraui planning session, before proposing anything.
---

# Project planning

You're planning a project that lives in this repo, one folder per project. The lead reviews
everything you propose in hydraui's Projects tab: they approve it, edit it, ask you for changes
or reject it. Engineers read the result to know what to build. So write for both: plain words
up front, detail one click away.

Read these before you propose anything:

- `references/format.md`: the repo's files. You never write them yourself; the plan tools do.
- `references/product.md`: what we build, for whom, and which repo holds what.

## The order

1. `plan_show`, then every resource (`plan_read_resource`; Read for PDFs and images).
2. Questions for anything you'd otherwise guess (see Questions). Carry on with what doesn't
   depend on the answer.
3. The spec (`plan_set_spec`).
4. The milestones (`plan_propose_milestone`), in the order they'll be done.
5. Stop. Tickets come later, one milestone at a time, when the lead asks.

## Milestones

A milestone is **an outcome someone outside engineering would notice**: a customer, CS, sales,
the lead. It's done when that outcome is true, not when code is merged.

**It is:**

- one outcome, named in plain words: "Partners can see shared groups";
- small enough to finish in one to three weeks;
- something you could demo, or check by looking at the product;
- in order: each one leaves the product working and better than before.

**It isn't:**

- a technical task: "Implement group ACL service", "Refactor the stepper";
- a layer or a repo: "Backend work", "Frontend for groups";
- a list of tickets, or a phase with no outcome: "Groundwork", "Polish", "Misc";
- QA or release on its own, unless the release is the outcome people wait for.

**How it's written** (the tools refuse anything else):

- title: the outcome, at most 60 characters, no jargon, no ticket or component names;
- 1 to 4 points, each at most 80 characters: what it's for, and how we'll know it's done;
- each point's detail: the full explanation behind it, shown when the lead clicks the point:
  what it means, why, edge cases, decisions it rests on (D-n), up to 1000 characters;
- a target date when you can justify one; otherwise none.

Good:

> **Partners can see shared groups** (target 2026-12-04)
> - Partners see the groups we share with them
>   *detail: Today partners ask us by email for group membership. ...*
> - They can't change or reshare them
> - Done when a partner opens a shared group in Interu

Bad: "Groups backend + API + UI (PRE-2155..2160)": three layers, ticket numbers, no outcome.

If the lead's note says a milestone is too big, split it into two outcomes, not two layers.

## Tickets

Proposed per approved milestone, when the lead asks. A ticket is the engineering detail the
milestone left out.

- One piece of work one engineer can finish in a few days, in **one repo**: say which.
- The title says what changes, in one line: "Show shared groups in the partner's group list".
- The description says what to do, where (the repo, and the area or files when you know
  them), and **how to tell it's done**: the checks a reviewer would make.
- `kind`: feature, bug or chore. A chore changes nothing a user sees.
- Work that needs another team or repo first gets its own ticket, first.
- Don't propose what already exists: check `references/product.md` and ask when unsure.

## Questions

Ask with `plan_add_question`, never in the terminal: each question shows in hydraui as a
decision for one person, and the answer comes back to you.

- Ask when the answer changes what you'd propose. Don't ask what the resources already say.
- One decision per question, in one sentence, with the context in the description.
- Give 2 to 4 options as short labels, your recommendation first with "(recommended)".
- It goes to the lead unless someone else owns the call.
- Record what was decided in the spec, with the decision's id (D-n).

## The spec

Short numbered sections in plain words: why, goals, non-goals, decisions and their reasons
(with D-n), open questions. Not a design doc: the tickets carry the how.

## Changing your mind

- `plan_revise` a proposal the lead hasn't approved; read their note first (`plan_show`).
- Never propose again what was rejected; the reason is in `plan_show`.
- Approved items are the lead's: suggest a change as a question.
