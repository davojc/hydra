# hydraui orchestrator, phase 2: tickets from Linear

Phase 1 (`2026-10-22-hydraui-orchestrator-core-design.md`) runs a ticket through a pipeline once
someone names it to the orchestrator. Phase 2 has the orchestrator find its own tickets in
Linear, claim them, and report back when they're done. The roadmap's gate for this phase: **a
real ticket goes from SLA to an open PR with no prompting.**

The stages are as phase 1 left them: QA-Skills replicates, iov42-agents develops (choosing the
repositories and making their worktrees and PRs itself, as its skills already do), QA-Skills
verifies. INTH-147, INTH-165 and INTH-166 went through them. This phase adds the Linear side
around a run.

Decisions (Jordan, 2026-10-23):

- **Which tickets.** INTH tickets in **Ready for Investigation**, with an SLA running, assigned to
  nobody. The one whose SLA breaches soonest goes first.
- **On pickup**, hydraui assigns the ticket to the Linear user the API key belongs to, moves it to
  **In Investigation**, and comments that hydraui's orchestrator is working it. Nobody else picks
  it up after that.
- **When a run ends**, hydraui comments the outcome, each stage's score and any PR links. When
  every stage passed, it also moves the ticket to **Ready for Peer Review**. A stopped run gets
  the comment with its reason and stays where it is.
- **Starting.** While **Watch Linear** is on and the orchestrator has no open run, hydraui checks
  every 5 minutes and starts the next ticket itself. **Check Linear now** looks at once.
- **How hydraui reads Linear.** It calls Linear's GraphQL API directly, with the environment's
  Linear API key. This is deterministic and spends no Claude tokens; the orchestrator session
  doesn't poll.

## 1. Linear as an environment tool

Like `gemini` and `jev`: `hydra add linear` (and hydraui's **Set up linear…**) stores a Linear
personal API key as the secret `<env>/linear` and writes `[linear] api_key = "secret:<env>/linear"`
to env.toml. Terminals get `LINEAR_API_KEY`. `hydra doctor` and the accounts list show whether
it's set. So the key belongs to an environment, like every other sign-in, and the pipeline
acts as that environment's Linear user.

hydraui reads the key the way it reads any secret of the orchestrator's environment. The
orchestrator's environment needs Claude (phase 1) and, for a pipeline with a `[linear]` table,
Linear: without it, **Watch Linear** says what to set up and stays off.

## 2. The pipeline's `[linear]` table

```toml
[linear]
team = "INTH"                          # the team's key
pick = "Ready for Investigation"       # the state that triggers a pickup
assignee = "unassigned"                # "unassigned", or "any" to take assigned tickets too
claim = "In Investigation"             # the state a picked ticket moves to
passed = "Ready for Peer Review"       # where a ticket goes when every stage passed
every_minutes = 5
```

All but `team` have these defaults. Another pipeline can trigger off another state (say `pick =
"Ready for Review"`) and take tickets whoever they're assigned to (`assignee = "any"`), as
Jordan asked on 2026-10-23. A pipeline without the table works as in phase 1: tickets
are named to the orchestrator by hand. State names are looked up in the team's workflow once
per check; a name the team doesn't have stops watching and says which.

Pipeline settings gets a **Linear** section with these fields.

## 3. Watching

- **The switch.** Each orchestrator's card has **Watch Linear** (off when it starts). It is the
  orchestrator's, not the pipeline's: two orchestrators on one pipeline would race for tickets,
  so only one at a time may watch a team. A second one says which orchestrator already watches.
- **A check.** The team's issues in the `pick` state (unassigned only, unless `assignee =
  "any"`), with `slaBreachesAt` set, ordered by `slaBreachesAt`. It runs every `every_minutes` while watching and the orchestrator
  has no open run, and at once on **Check Linear now** or when a run ends.
- **Picking.** The first ticket is claimed (§4). If someone else took it meanwhile (it has left
  the `pick` state, or, with `unassigned`, gained an assignee), hydraui takes the next one. Then hydraui types into the
  orchestrator: `Linear has INTH-170 "<title>" (SLA breaches <date>). Start it: call run_start
  with pipeline "inth", ticket INTH-170 and that title.` The run is the ticket's from then on,
  as in phase 1.
- **The idle card.** While watching with nothing to run, the card says **Watching Linear** and
  when it last looked and looks next. The design's idle "Watching Linear" state, **Check Linear
  now**, the SLA pill and the Linear link come in here (phase 1 §0 deferred them).
- **Failing closed.** A check that fails (network, a 4xx/5xx, a bad key) is retried at the next
  interval. After three failures in a row hydraui stops watching and calls a person (phase 1
  §5.3, reason `Linear: <error>`). A missing or rejected key stops at once.

## 4. Writing to Linear

hydraui makes three writes, all as the environment's Linear user:

1. **Claim**, before the orchestrator is told: assign to the key's user (`viewer`) when the
   ticket has no assignee (an assigned ticket keeps its assignee), move to `claim`, and comment *"hydraui's orchestrator (pipeline "INTH SLA tickets") is working on this:
   replicate → develop → verify. Run INTH-170-1."*
2. **Passed**, when the run finishes with every stage passed: move to `passed` and comment the
   outcome. The comment lists each stage with its score and summary, and any PR links found in
   the stages' reports (evidence, summary and handoff, matched as GitHub pull request URLs).
3. **Stopped**, when the run is stopped (by a person, or by the run's own rules): the same
   comment, saying why and at which stage. The state stays as it is, for a person.

Each write is logged in the run's log (`linear claim`, `linear comment`, `linear state`), so
History shows it and a restart doesn't repeat it. A write that fails is retried once. If it still
fails, a person is told (toast). The run itself isn't stopped by a failed comment.

A run started by hand for a ticket that matches the team key (an INTH ticket named to the
orchestrator) gets the same claim and end writes. A ticket from another team is left alone.

## 5. What the views gain

- **Card.** An SLA pill: time left until `slaBreachesAt`, amber inside the SLA's high-risk window
  (`slaHighRiskAt`), red once breached. The ticket id links to Linear.
- **Pipeline tab.** The header's ticket links to Linear, with the SLA beside it.
- **History.** An SLA column: whether the run ended inside the SLA. Plus the Linear writes in a
  run's detail.
- **Settings.** The `[linear]` fields (§2).

## 6. Where it lives

- `hydra-providers`: `linear.rs`, the `Linear` provider (like `jev.rs`).
- `hydraui`: `linear.rs`, a small GraphQL client on `ureq`, as `jev.rs` uses. It has three
  queries (viewer, the team's states, pickable issues) and three mutations (update issue state
  and assignee, create comment). It's behind a trait, so the watcher is tested with a fake.
  `linear_watch.rs` holds the watcher's pure decisions (when to check, what to pick, what to
  write when).
- Run logs: the events above. `run_resume` (phase 1 §13) skips them, since they don't change the
  run.

## 7. Testing

- Unit: the provider (key from the secret), query building and response parsing (from recorded
  JSON), the pick order and filter, the claim race (taken meanwhile → next one), the end-comment
  text and PR-link extraction, the three-failures rule, and only one watcher per team.
- By hand: the phase 2 gate. With Watch Linear on and a real INTH ticket in Ready for
  Investigation, hydraui picks it up, claims it in Linear, runs replicate and develop, and an open
  PR is commented back on the ticket with no prompting.

## 8. Out of scope

- Phase 3: the Slack review loop, the review stage, and Slack DMs on escalation.
- Several tickets at once (still one per orchestrator).
- Writing anything to Linear beyond the claim, the end comment and the passed state.

## 9. As built

- **Modules.** `linear.rs` (the client; requests and replies are pure, `Http` sends them),
  `linear_watch.rs` (the decisions, and `find_and_claim` / `claim_named` / `end_writes` over the
  `LinearApi` trait, tested against a Linear in memory), and `workspace/linear.rs` (the switch,
  the checks off the UI thread, the card row, the ticket link and SLA pill).
- **The claim comment** names the pipeline and its stages. It doesn't name the run, since the
  run starts only after the claim.
- **Re-check before claiming.** `find_and_claim` reads the ticket again just before it claims
  it. A ticket that has left the trigger state, or gained an assignee when only unassigned ones
  count, is passed over.
- **A run's ticket** comes from the watcher's pick, or is read from Linear when a run starts by
  hand. Either way it's logged as `linear ticket` (link and SLA times), which is what a resumed
  run reads back. An ended run logs `linear sla` (`inside`, from the time it ended), which
  History shows as Inside / Missed.
- **Tickets hydraui already ran are skipped.** That covers any ticket a run log names, whether
  its run finished, stopped or was interrupted. Such a ticket is waiting on a person (a PR to
  review, a stop to look at, an Interrupted card to resume), and the status line names it. Found
  on 2026-10-23: INTH-165 (done, PR open) and INTH-166 (interrupted) were still unassigned in
  Ready for Investigation, because they ran before phase 2 claimed anything. To run one again,
  start it by hand.
- **The watcher's switch isn't saved.** It's off after a restart, and so is a resumed run's
  orchestrator.
- **Times** are read by a small RFC 3339 parser (UTC only, as Linear sends them). The workspace
  has no date crate.
- **Not yet checked by hand** against a live Linear workspace. The queries follow Linear's
  documented schema (`IssueFilter` with `team.key`, `state.id`, `assignee.null`; `issue(id:)`
  accepts an identifier; personal keys go in `Authorization` without `Bearer`).

## 10. The branch rule (added 2026-10-23)

Jordan: INTH-166's Develop stage pushed `jordan/inth-166-…` instead of `INTH-166`, so its feature
environment didn't come up under the expected name. The fix is a hard rule:

- **Setting.** A pipeline's `branch = "{ticket}"` (`{ticket_lower}` works too), also **Branch
  name** in Pipeline settings. For PRE-151 every repository's branch is `PRE-151`.
- **Told.** Every stage's hydraui note says it: *Name every branch you create or push exactly
  PRE-151, in each repository: no prefix, no suffix, no other name. Put each pull request's link
  in your report.*
- **Enforced.** `stage_report` is refused (nothing recorded) while a GitHub pull request named in
  the report's summary, evidence or handoff is on another branch. hydraui asks GitHub as the
  stage's environment (`hydra run <env> -- gh pr view <url> --json headRefName`), outside the
  registry lock, with a 30 s limit. A branch it can't read refuses too (fail closed), saying why.
- **Not covered.** Work pushed without a pull request in the report isn't checked. The orchestrator
  and the person see the report, and Verify works against the feature environment anyway.

## 11. Automatic and Manual (added 2026-10-26)

**Watch Linear** read as a button, and nothing said whether it was on: a card could sit off with
the orchestrator asking "which ticket?" while its log said it was watching. The card now shows a
mode:

- **A switch and a pill**: **● Automatic** (hydraui picks), **○ Manual** (only what you give it)
  or **■ Stopped** (it switched itself off; the line says why). **Pick now** picks once in
  either mode, and is hidden while a ticket runs.
- **One line** on what happens next: `Nothing new · next look in 4 min`, `Looking in Linear…`,
  `Next ticket after INTH-169`, `Only runs tickets you give it`. What Automatic picks is the
  switch's tooltip; the full check results stay in `hydraui.log`, with every switch logged.
- **The orchestrator waits.** Its first prompt, for a pipeline with `[linear]`, says tickets come
  from hydraui or from you and not to ask; a finished run waits "for the next ticket, from
  hydraui or from them". Switching mode needs no new prompt.
- **A secret store that can't be read just now** (a keychain that can't show its prompt) counts
  as a failed check, not a rejected key: only a missing key stops Automatic at once.
- The list is 240 px wide by default (was 200).
