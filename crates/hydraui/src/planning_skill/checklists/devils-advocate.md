# Devil's advocate: review your own plan before the lead does

Run this on every batch you propose: part A after milestones, part B after a milestone's
tickets. Be the sceptical lead who has to defend this plan to the team. The aim is to find
what's wrong, not to confirm it's right.

## How to run it

1. Call `plan_show` and read what you proposed as the lead will see it.
2. Go through every question below for every item. Write down each **fail**, with the item's
   id and why.
3. Score: the share of questions that pass, across all items. **The bar is 90%.**
4. Fix every fail you can with `plan_revise` (or `plan_withdraw` and propose again). A fail
   only the lead can settle becomes a question (`plan_add_question`).
5. Run it again on what changed. Repeat until you're at the bar.
6. Tell the lead, in two or three lines: the score before and after, what you changed, and
   what you're still unsure of.

Never hand over below the bar, and never mark something a pass because fixing it is work.

## Part A: milestones

For each milestone:

1. **Outcome.** Would someone outside engineering notice it's done? If the honest answer is
   "the code is merged", it's a task, not a milestone.
2. **Plain words.** Could CS or a customer read the title and every point without asking
   what a word means? No component, class, endpoint or ticket names.
3. **One thing.** Is it one outcome? An "and" joining two outcomes means two milestones.
4. **Not a layer.** Is it a slice users can see end to end, rather than "the backend for X"
   or "the UI for X"?
5. **Done is checkable.** Does a point say how we'll know it's done, in a way two people would
   agree on?
6. **Size.** One to three weeks of work for the people on the project? A milestone you
   can't picture the tickets for is too big.
7. **Order.** Does it come after what it depends on, and leave the product working?
8. **Details earn their place.** Does every point's detail explain the point (what, why,
   edge cases, the decisions it rests on), rather than repeat it or slip in implementation?
9. **Grounded.** Does everything in it come from the resources, the spec, an answered
   question or `references/product.md`? Mark anything you assumed, and ask about it.
10. **Dates.** Is its target date justified by the work and the order, or left empty?

Then, across all milestones:

11. **Coverage.** Does every goal in the spec land in a milestone? List each goal and where.
12. **No passengers.** Does every milestone carry real weight? Merge or drop one that
    doesn't.
13. **Non-goals respected.** Does any milestone build what the spec says is out of scope?

## Part B: a milestone's tickets

For each ticket:

1. **One repo**, named, and the right one per `references/product.md`.
2. **A few days** for one engineer. Bigger: split it by what the user or the system does.
3. **Clear title**: says what changes, in one line.
4. **Done is testable**: the description says what a reviewer would check.
5. **Where**: names the area or files when you know them.
6. **Dependencies stated**: "after MUE-3", and proposed after what it needs.

Then, across the milestone's tickets:

7. **The milestone is reachable.** If every ticket is done, are all the milestone's points
   true? Walk each point to the tickets that make it true.
8. **Layers in order.** API design before backend before frontend; nothing builds on what
   another ticket hasn't made yet.
9. **Nothing forgotten.** Go through the "easy to forget" list in `SKILL.md`: feature flag,
   migration, permissions, translatable copy, empty, loading and error states, emails,
   sharing, tests. Each is either covered by a ticket or not needed. Say which.
10. **No duplicates.** No two tickets do the same work, and none redoes something that
    already exists.
11. **No gold-plating.** Nothing beyond what the milestone needs.

## What to tell the lead

> Reviewed: 31 of 36 checks passed (86%). Fixed: M2 split into "Partners see shared groups"
> and "Partners can't edit shared groups" (it was two outcomes); M4's title had an endpoint
> name. Now 36 of 36. Unsure: M3's date assumes D-2 goes the backend way; asked as D-7.
