# Swarm claims: one hydra takes a job

A general hydra service that lets one machine in a swarm **claim** a job for itself, so that
teammates' hydras watching the same source don't both work it. A job is named by a **context**
(say, an email pipeline) and a **key** within it (say, the email's Message-ID). An
email-triggered pipeline is what prompted it; Claude sessions (through the MCP server) use it
first (§8), and Linear and Slack pipelines and other hydraui features can use the same service
later.

Builds on [Swarms](2026-10-27-swarms-design.md): the roster, the swarm document, gossip and the
roster gate.

## 1. Why

A pipeline triggered by a shared source (a Google Group's mail, a Linear team, a Slack channel)
runs on several teammates' machines, and any of them may be on or off. Each new item must be
picked up by **one** hydra only. Linear and Slack give pipelines a mark to leave today (state
and assignee, the 👀 reaction), but a Google Group has none that a program can set: the
collaborative inbox's take/assign/complete exists only in the Groups web page, with no API. So
hydra needs a way to agree on "who has this" among its own machines, and it should be one
service anything in hydra can use, not something built into each pipeline.

### Goals

- **Claim a key in a context**, and be told it's yours, or who has it.
- **Peer to peer.** No server, nothing that has to be always on. Teammates' machines come and
  go.
- **Jobs move on** when the machine holding them disappears.
- **A finished key isn't taken again** for as long as its context says.
- **Usable by anything in hydra**: pipelines, future hydraui features, and Claude sessions
  through hydraui's MCP server.
- **People can see** who holds what, release their own claims, and owners can release anyone's.

### Non-goals

- **A guarantee of exactly one holder.** The user's call: during a split (two groups of
  machines that can't see each other) both sides may claim the same key; this is settled when
  they meet again (§4.3). No quorum, no consensus protocol, no server.
- Claims while hydraui isn't running (as for swarms: no background service).
- What a pipeline does with work that was stopped part-way (§4.5 only says it is told).
- The email pipeline itself (reading the group's mail, labels); a separate design.
- Claims that outlive what took them, or hand-offs to a named person (§5.2).

### Anti-goals

- Two teammates' agents working the same key while their machines **can** see each other.
- A key held forever by a machine that is never coming back.
- Claims appearing as files in the swarm folder, or mixed in with people's files.

## 2. Contexts

- A **context** belongs to **one swarm**: only that swarm's members take part in it. Its name
  is unique within the swarm (e.g. `inbox-support`, `linear-inth`).
- A context's **settings** are stored in the swarm document, so every member uses the same
  values:
  - **`renew_secs`**: how often a live claim is renewed. Default 60 *(our call)*.
  - **`lapse_after`**: missed renewals before a claim lapses. Default 3 *(our call)*; the
    lapse time is `renew_secs × lapse_after` plus some slack for clock differences between
    machines, so it should be well above any likely skew.
  - **`max_per_machine`**: live claims one machine may hold at once in this context. A
    positive number is the limit; **-1** is no limit; **0** pauses the context: nobody can
    claim, and claims already held run on until done, released or lapsed *(our reading of
    "disabled")*. Done keys never count towards it. Default 1 *(our call)*: work spreads across
    teammates' machines, and an owner raises it for a context that can run more at once.
  - **`keep_done`**: how long a done key is remembered before it is forgotten, from minutes to
    days. Default 30 days *(our call)*: forgetting too soon means an item gets worked twice,
    while keeping too long only costs a little space. A caller that rescans its source (a mail
    watcher looking back N days) must keep done keys longer than it looks back, or an old item
    can be claimed again; the context's settings page says so.
- All four settings are per context. A new context starts with the defaults above. Owners create
  contexts and change their settings (as with other swarm settings). A pipeline
  names the swarm and context it claims in.

## 3. Life of a key

| State | Meaning | Counts towards the machine's limit | Claimable by others |
|---|---|---|---|
| **Claimed** | live, renewed every `renew_secs` | yes | no |
| **Lapsed** | missed `lapse_after` renewals | no | yes |
| **Released** | given up unfinished (by its holder, its person, or an owner) | no | yes |
| **Done** | finished | no | no, until `keep_done` passes and it is forgotten |

- A key nobody has claimed, or whose done record has been forgotten, is free.
- Marking a key done ends its claim and frees the machine's slot in one step.

## 4. Rules

### 4.1 Claiming

A hydra may claim a key when, as far as it knows (its copy of the document plus what gossip
has told it): the key is free, lapsed or released; the context isn't paused; and it is under
`max_per_machine`. The claim then has three steps (the user's call: ask, don't wait):

1. **Write.** It writes its claim to the document and announces it on gossip.
2. **Ask.** It asks every member it can reach, directly over iroh on the claims' own ALPN behind
   the roster gate: here is my claim, what do you know about this key? Each answers with any
   claim or done record it holds, its own included.
3. **Decide.** Once all have answered, or a short timeout has passed, it applies the fixed rule
   (§4.2) to everything it now knows. It starts work only if it won. A member that doesn't answer
   in time counts as unreachable.

Because each claimer writes before it asks, two claimers that can reach each other each see the
other's claim, whatever the timing: if one's question arrives before the other has written, the
other sees that claim and doesn't claim; if after, the answer carries the other's claim. Both
then apply the same rule and get the same winner. This isn't a vote: nobody grants anything, and
a member that is off has no say.

**If it can reach no other member, it assumes it is alone and claims.** It still respects every
claim and done record it has already synced: a sleeping teammate's live claim is still in its
copy and still holds until it lapses.

### 4.2 Two claims at once

When two live claims on one key meet, **one fixed rule** picks the winner, and every machine
applies it the same way: **the earliest claim wins** (the user's call), by the time stamped on
the claim by its claimer; on equal times, **the lowest machine id** *(our call)*. The loser gives
the key up (§4.5).

The time is the claimer's own clock, so a machine with a slow clock tends to win when claims
meet. That only decides between claims that really do meet, and every machine still picks the
same winner.

### 4.3 A split heals

The same rule settles it: when machines that couldn't see each other sync again, two live
claims on one key are decided by §4.2. **Done beats any live claim**: if one side finished the
key, the other side's holder loses it.

### 4.4 A holder goes away

A claim is renewed only while its holder is alive (§5.2). A closed laptop stops renewing; after
the lapse time its key is free and another hydra may claim it. When the laptop wakes, it finds
its claim lapsed (and perhaps someone else holding the key) and is told it lost it (§4.5).

### 4.5 "You lost this"

Whenever a machine's claim stops being its own, by the fixed rule, a lapse, a done record from
elsewhere, or an owner's release, the service tells whatever holds it, with the reason, so it
can stop its work. This is part of the service's interface, not an afterthought.

### 4.6 Limits are kept by the claimer

Each machine keeps within `max_per_machine` itself. If a machine's live claims in a context go
over it (a bug, an older hydra), the others ignore its **newest** claims beyond the limit, so
they don't block anyone *(our call)*. "Over the limit" is judged against the limit in force when
each claim was made: an owner lowering `max_per_machine` (to 0 or a smaller number) stops new
claims but never takes away claims already held, which run on until done, released or lapsed
*(our call)*.

### 4.7 Picking up after a restart

When hydraui restarts and a pipeline run or session resumes, it claims its key afresh. If this
machine's own claim on that key is still live (the restart was within the lapse time), the claim
is simply taken up again: it is this machine's. If the key has lapsed and someone else has it,
or it is done, the resume is told so, the same as a lost claim (§4.5), and the person sees it
*(our call)*.

## 5. How it's exposed

### 5.1 The core

Claims live in the swarms service (`crates/hydra-swarms`), beside the roster and folder sync,
because they use the same document, gossip and roster gate. Its interface, in outline:

- claim `(swarm, context, key)` → yours (with a handle), or held by whom, or done, or paused,
  or at the limit;
- release, and mark done, through the handle;
- list a context's claims and done keys;
- create a context and change its settings (owners);
- release someone else's claim (its person, or an owner);
- events: lost (with the reason), released by someone else, context settings changed.

Nothing in it knows about pipelines, email or MCP.

### 5.1a Every window can claim

One hydraui window per machine runs the swarms (it holds `swarms.lock`; the others retry every
10 s and one takes over when it closes), but every window has its own pipelines, terminals and
MCP server. **Claims work from every window** (the user's call): a window that doesn't run the
swarms passes its claims, releases and done marks to the one that does over a local channel
between windows on this machine, and receives its events back. When the window running the
swarms closes, the window that takes over keeps renewing the claims whose holders are still
alive in the other windows: each window re-registers its live holders with the new one, well
inside the lapse time. Claims whose holder was in the closed window end with it. The channel's
form is the builder's call.

### 5.2 Claims live as long as their holder

Callers don't renew. **The service renews a claim for as long as its handle is held**, and stops
when the holder ends: a pipeline run when the run closes, a session's claim when its terminal
closes, any caller when it drops the handle. A crashed or closed holder therefore lapses on its
own. There are no free-standing claims and no hand-offs.

### 5.3 Callers

- **Inside hydraui**: pipelines and any later feature call the core directly. A new user is a
  new caller, with no change to the service.
- **Claude sessions**, through hydraui's MCP server, beside `tasks_*` and `stage_*`:
  `claims_take(context, key, swarm?)`, `claims_release(key, context?)`,
  `claims_done(key, context?)`, `claims_list(context, swarm?)`. `swarm` is optional, an id or a
  name; without it the call uses the one swarm that has the context, and fails if there is none or
  more than one. `context` on a release or done is needed only if the session holds the same key in
  two contexts. A session's claims are tied to its terminal (§5.2). When a session loses a claim,
  hydraui types it into the session's prompt, as it already tells an orchestrator that a stage
  reported.
- **Later, maybe**: a `hydra claim …` command for scripts. Not in this work.

### 5.4 In hydraui

On a swarm's page, a **Claims** section per context:

- every live claim: the key, the person and machine holding it, when it was last renewed, and
  what holds it (pipeline run, session);
- recent done keys;
- **Release** on claims held by your own machines, and on anyone's if you're an owner;
- the context's settings, editable by owners.

Layout is the builder's call, in the HydraUI look.

## 6. Permissions

| Who | Can |
|---|---|
| Any member | see every context's claims and done keys |
| Owners and writers | claim, release and mark done their own claims *(our call: readers only see)* |
| A person | release claims held by any of **their** machines, from any of them |
| Owners | release anyone's claim; create contexts and change their settings |

*(Our call: any owner may release any claim, whatever their seniority. Releasing only frees a
key; it takes nobody's standing, so the roster's ranking doesn't apply.)*

A release of someone else's claim is a **signed entry** of its own (a machine can't delete
another's entries), honoured by every member like a roster change. A claim, release or done
written by someone not entitled to it when it was written is ignored, as the roster ignores
changes from someone without standing.

## 7. Storage

- Claims, releases and done records are entries in the **swarm document** under their own
  prefix (`claims/<context>/…`), beside `log/`, `files/` and `parents/`.
  The document is stored in hydra's state folder (`<state root>/swarms/<id>/docs/`), on every
  member machine; folder sync ignores this prefix, so nothing shows in the swarm folder.
- Context settings are not under that prefix. They are signed changes in the roster's `log/`
  (like the file types), so who may change them, and when, is judged as for any roster change,
  and they survive the owner who set them being removed.
- Each machine writes its own entries; renewing overwrites its own claim entry, so renewals
  don't pile up.
- Gossip announces claims, releases and done at once; the document is what lasts and what a
  machine coming online catches up from.
- Done records are dropped once `keep_done` has passed; each machine prunes what it can, and
  every machine ignores done records older than `keep_done` whoever wrote them.
- Exact key layout, entry encoding and pruning are the builder's call.

## 8. Scope and later users

**This build** (the user's call): the core service (§5.1), claims from every window (§5.1a), the
MCP tools for sessions (§5.3) and the Claims section on a swarm's page (§5.4). Sessions are the
only callers at first; "pipeline run" is a holder kind the core supports but nothing uses yet.

**Later, each its own piece of work** with its own decisions (what a "held by someone else" item
shows on the card, what a run does when it loses its claim):

- **Email (Google Group)**: the context per pipeline, the key the Message-ID. Its design covers
  reading the mail and any Gmail label it leaves for people.
- **Linear and Slack pipelines**: claiming tickets and pull requests as a team-wide "who's on it"
  alongside their existing marks. On one machine a pipeline's `runs_at_once` still applies; the
  lower of it and the context's `max_per_machine` wins.

## 9. Testing

Unit tests on the rules, as pure functions over a set of entries: the fixed rule, equal times,
done beating
live claims, lapse timing with clock slack, limits (including -1 and 0), the newest-over-limit
claims ignored, release permissions, a split healing. Service tests with several in-process
nodes, as the swarms tests do: two machines claiming at once settle on one, with gossip held
back so only the asking can tell them; a member that doesn't answer in time is passed over; a
machine alone
claims; a holder that stops renewing lapses and another claims; a woken machine is told it
lost. These are network tests and run in the `network` nextest group.

## 10. Calls made without the user

Marked "our call" above: defaults of `renew_secs` 60, `lapse_after` 3, `max_per_machine` 1 and
`keep_done` 30 days; lowest machine id on equal claim times; 0 pauses new
claims but lets held ones run on; over-limit claims beyond the newest ignored, judged against the
limit when each was made, so lowering it takes nothing away; a resumed run or session takes up
its machine's still-live claim; readers can't claim; any owner can release any claim. Left to
the builder: the key layout and encoding, the timeout on asking, how a session's context finds its
swarm, the channel between windows, and the Claims section's layout.

The builder's choices, as built:

- **Settings** are a signed `SetClaimContext` change in `log/` (§7). Older hydras skip it.
- **Records** are `claims/<context>/claim/<base64url(key)>\0`,
  `claims/<context>/done/<base64url(key)>\0` and
  `claims/<context>/release/<base64url(key)>/<base64url(target author)>\0`, one each per author.
  Keys are encoded since Message-IDs hold `/` and `<`, and the closing zero is there because a
  delete clears every entry its key starts, and one job's key can start another's. The plain key
  is inside the record too. A machine releasing its own claim deletes its claim entry; only
  someone else's release writes a release record. A release ends only the claim it names, and is
  kept for `keep_done` or the lapse time, whichever is longer.
- **Limits**: a key is up to 1024 bytes; a context name is 1 to 64 of `a-z 0-9 - _`.
- **Lapse time** is `renew_secs` x `lapse_after` + 30 s of clock slack, so 210 s by default.
- **Asking** all members at once waits at most 3 s.
- **An answer** also carries the answering machine's own records across the context: all its
  claims, its newest done marks up to 1 MiB, and how far that list reaches. Whether a claim counts
  depends on that machine's other claims (§4.6), so the key asked about isn't enough. A claim of
  its made by then and not on the list has ended, so the asker stops counting it, as it would on
  syncing the tombstone. The asker takes this only from the machine it asked.
- **Who is entitled** is judged by the roster now, not as it stood when a record was written, as
  §6 has it. So a demoted writer's claims stop counting.
- **A loss is typed when the session next goes idle**, so a session in the middle of a turn works
  on until then. One that doesn't take is typed again, and after 3 tries the person is told what it
  says, so they can tell the session by hand, while hydraui goes on trying.
- **Names in typed prompts** are cleaned of control characters and capped, but a member could still
  put plain words there, and the session reads them as part of the prompt.
- **A done key can't be undone**: it stays done until `keep_done` passes.
- **Finding the swarm**: `claims_take` takes an optional `swarm` (§5.3).
- **Two sessions on one machine** can't share a key: a key this machine holds through a live
  handle answers "held" (by this machine). Taking up (§4.7) applies only when no live handle
  exists.
- **Between windows** the window running the swarms listens on loopback TCP, with JSON lines and
  a secret, found in `<running_dir>/swarms-claims.json`. A dropped connection tells the host its
  holders ended. If that window closes, another takes over and renews the holders without
  telling them they lost anything.
- **A re-take** after the window running the swarms changes never claims afresh. If the claim
  has gone, the holder is told it lost it.
- **Done from a removed member** still counts; their claims don't, so removing a teammate
  doesn't make their last 30 days of work claimable again.
- **Limit in force** is carried in each claim record, since settings history isn't kept. It is
  self-reported, which is acceptable: the rule catches bugs, it isn't a security check.
- **Settings changed** reaches the page through `SwarmView`; there is no separate event.
- **The `claims_*` tools** are pre-approved like the other hydraui tools, so sessions call them
  without a permission prompt. A claim only frees or holds a key. This is still for the user to
  confirm.
