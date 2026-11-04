# Swarms (plan B: linking your own machines) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A person's machines are linked to them. Another machine of yours joins with a
single-use code and shows in every swarm as one more machine under the same person, not as a
second person. Swarms you're in are offered to your other machines. Removing one of your machines
shuts it out by replacing your personal key.

**Architecture:**
- **Roster:** the roster gains three signed changes:
  - `AddMachine`;
  - `RemoveMachine`;
  - `Rekey`, which moves a person to a new key and drops machines.

  Existing changes and their JSON are untouched, so every log written under plan A still
  verifies and folds the same.
- **Your machines:** each machine gets one stable *device key* and its own iroh endpoint with the
  ALPN `hydra/mine/1`. It answers only your linked machines, plus anyone holding a valid link
  code.
  - It carries: linking, the shared state of your machines (your name, their names, removed
    machines), swarm offers, and a new personal key.
  - That state lives in `swarms/mine.json` and merges, so any two of your machines that meet
    converge.
- **The service:** `SwarmsService` runs the device endpoint beside the swarms. A new machine
  joins an offered swarm in two steps:
  1. It signs `AddMachine` with the personal key and hands it to one of your machines already
     in that swarm. That machine writes it itself, or passes it to an owner through plan A's
     relay.
  2. It then syncs exactly as an invited joiner does.
- **Removing a machine:** this makes a new personal key and writes a `Rekey` in every swarm.
  The new key reaches your other machines over `hydra/mine/1`. A machine that was offline picks
  it up the next time it meets one that has it.
- **hydraui:** the "You and this machine" page gets your machines, "Add another machine of
  mine…", "Link this machine to one of yours…", the swarms offered to this machine, and
  Remove….

**Tech Stack:** as plan A (iroh 1.4.1, iroh-docs 0.101, iroh-blobs 0.103, iroh-gossip 0.101,
tokio, keyring, serde_json). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-27-swarms-design.md`: §2 (identity bullets), §3
(Machine key, Changes spread, Settings) and §6 ("Add another machine of mine", "Removing one of
your machines"). Plan A is `docs/superpowers/plans/2026-10-27-swarms.md`. Its rulings are in the
code's doc comments.

## Global Constraints

- **Signed JSON compatibility.** `SignedChange` signatures cover `serde_json::to_vec` of the
  `Change`.
  - Never add a field to an existing `Change` variant, or to `Member`, `Machine` or `Kept`.
  - Any new optional field on a *new* type needs `#[serde(default, skip_serializing_if = …)]`.
  - Existing plan A tests must pass unchanged.
- **Codes:**
  - A link code is single-use and expires after 7 days (reuse `INVITE_LIFETIME`). It can be
    cancelled, and it's redeemed only while the machine that made it is running.
  - Its text starts with `hydra-link:`. Swarm invites keep `hydra-swarm:`.
- **Who may link:** a machine that is in no swarm and has no linked machines. A machine already
  in swarms as someone else is refused *before* the code is spent, with the copy: "This machine
  is already in swarms as someone. Leave them before linking it to another of your machines."
- **Device key:**
  - Kept in the keyring through the existing `SwarmKeyFn` under the key id `device`, which gives
    user `swarm-key-device`.
  - The personal key stays under `swarm-person-key`. A new personal key replaces it there.
- **Roles:**
  - A person's role is per swarm, and every one of their machines has it.
  - A new machine gets the write capability exactly when the person is a writer or owner.
- **Readers:** a reader's `AddMachine`, `RemoveMachine` and `Rekey` reach the log through an
  owner (plan A's relay and outbox, `RELAY_RETRY` backoff).
- **Leaving on one machine:**
  - When your person has other machines in the swarm, Leave removes only this machine
    (`RemoveMachine`), and your other machines stay in it.
  - When it is your last machine there, Leave is plan A's person-leaves. The last-owner rule
    applies only then.
- **A removed machine:**
  - It keeps its files and is refused as a peer everywhere.
  - Its past file entries count only as the change that dropped it lists them (plan A's
    `Kept`/`retire`).
  - It can never be added back under the same swarm endpoint.
- **The UI thread never blocks** on the network, the disk or the keyring.
- **Dev builds** are stopped by PID or `scripts\stop-dev-hydraui.ps1`, never by name.
- Commits end with the session's attribution lines. Nothing is pushed or merged without the
  user asking.

## Review Focus

1. **Linking a machine that is already in swarms as another person.** Expect a refusal with the
   copy in Global Constraints, and the code isn't spent: it still works from a clean machine. *Test:* Task 4
   `a_machine_in_swarms_cannot_link_and_the_code_survives`.
2. **Accepting an offer while the machine that offered it is offline**, but another of your
   machines in that swarm is online. Expect the join to go through that one. With none online,
   expect "None of your machines in <swarm> is running. Try again when one is." and the offer
   stays. *Test:* Task 5 `an_offer_is_taken_through_any_machine_of_yours_in_the_swarm`.
3. **Removing a machine while a third machine of yours is offline.** When it comes back, it
   picks up the new key, and its swarms show one person. Two machines writing the same `Rekey`
   is harmless, since the second is a no-op. *Test:* Task 7
   `a_machine_that_was_away_picks_up_the_new_key`.
4. **Changing your name on two machines while they're apart.** Expect both to settle on the
   newer name when they meet, and the roster stops renaming back and forth. *Test:* Task 6
   `names_set_apart_settle_on_the_newer`.
5. **A plan A log or roster after upgrade.** It folds exactly as before, including a person with
   one machine and an empty `mine.json`. *Test:* Task 1
   `plan_a_logs_fold_the_same_with_the_new_changes_known`.

---

### Task 1: Roster changes for machines and new keys

**Files:**
- Modify: `crates/hydra-swarms/src/roster.rs` (types, `Change`, `apply`, helpers, tests)
- Modify: `crates/hydra-swarms/src/ids.rs` (`RekeyCert`)

**Interfaces:**
- Produces in `ids.rs`:
  - `pub struct RekeyCert { pub from: PersonId, pub to: PersonId, pub sig: String }`, where
    `sig` is hex.
  - `RekeyCert::new(from: &PersonKey, to: PersonId) -> RekeyCert` signs the bytes
    `b"hydra-rekey/1\0"` followed by `to`'s 32 public-key bytes.
  - `RekeyCert::verify(&self) -> bool`.
  - `pub fn chain_ends(certs: &[RekeyCert], from: &PersonId) -> Option<PersonId>` follows
    `certs` in order from `from`. Each cert must verify and start where the last one ended. It
    returns the last `to`, or `None` if the chain is empty or broken.
- Produces in `roster.rs`, as new `Change` variants appended after `SetTypes`:
  - `AddMachine { person: PersonId, machine: Machine }`
  - `RemoveMachine { person: PersonId, endpoint: String, #[serde(default)] kept: Kept }`
  - `Rekey { person: PersonId, to: PersonId, certs: Vec<RekeyCert>, #[serde(default)] drop: Vec<String>, #[serde(default)] kept: Kept }`
  - `Roster` gets `pub dropped: BTreeSet<String>`, the endpoints of machines taken out by
    `RemoveMachine` or `Rekey`.
  - `pub fn person_machines(&self, p: &PersonId) -> &[Machine]`.

- [ ] **Step 1: Write the failing tests** in `roster.rs`'s test module, using its existing
  helpers (`genesis`, `add`, `sign`, `fold`, `fold_in_any_order`):
  - `a_person_adds_a_second_machine_and_stays_one_member`: add a writer `w`, then have `w` sign
    `AddMachine` for a machine with fresh endpoint and author ids. Assert:
    - one member for `w`, with 2 machines;
    - `r.writes(new_author)` is true;
    - `r.member_by_endpoint(new_ep)` is `w`.
  - `only_the_person_adds_their_machine`: an owner signing `AddMachine` for `w` changes nothing,
    and nor does a stranger. Only `by == person` counts.
  - `a_machine_whose_ids_are_someone_elses_is_not_added`: use an author id already in `r.ids`
    for another person. Expect no change.
  - `a_dropped_endpoint_never_comes_back`: `RemoveMachine`, then `AddMachine` with the same
    endpoint. Expect the machine absent, and `r.dropped` to contain it.
  - `the_last_machine_is_not_removed`: `RemoveMachine` of a person's only machine. Expect no
    change.
  - `a_removed_writers_machine_keeps_only_what_kept_lists`: mirror the existing
    `kept_files`/`entry` tests for one machine. Assert that `r.retired[author].files` holds
    exactly the listed entries dated at or before the change.
  - `a_rekey_moves_the_person_with_their_role_rank_and_machines`: an owner promoted by the
    anchor rekeys to a new key with one cert and `drop: [ep2]`. Assert:
    - `role_of(old)` is `None`;
    - `role_of(new)` is `Owner`;
    - `ranks[new] == old rank`;
    - `promoters` values that named `old` now name `new`;
    - `r.ids` values are remapped;
    - `ep2` is gone and in `dropped`.
  - `a_rekey_needs_the_chain_and_the_new_keys_signature`: a broken chain, a chain ending
    elsewhere, `by != to`, `to` already a member, or `person` not a member all leave the roster
    unchanged.
  - `the_anchor_rekeys_and_is_still_most_senior`: the anchor rekeys, and then a later junior
    owner's attempt to demote the new key fails (`outranks(new, junior)`).
  - `changes_by_the_old_key_after_a_rekey_do_not_count`: a `Rename` signed by the old key and
    dated after the rekey is ignored.
  - `plan_a_logs_fold_the_same_with_the_new_changes_known`: for each existing test log that
    `fold_in_any_order` is used with, assert `r.dropped.is_empty()`. Also assert JSON
    `serde_json::to_value(&Change::Leave{..})` is unchanged: a byte-for-byte literal of a
    plan A Leave.
  - `RekeyCert` tests in `ids.rs`:
    - `a_cert_verifies_only_for_its_pair`;
    - `a_chain_of_two_ends_at_the_last_key`;
    - `a_broken_chain_ends_nowhere`.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo test -p hydra-swarms --lib roster:: ids::`
  Expected: compile errors for the missing variants and types.

- [ ] **Step 3: Implement.** In `apply`:
  - **`AddMachine`** counts when all of these hold:
    - `c.by == person`;
    - the person is a member;
    - `name_fits(&machine.name)`;
    - the endpoint isn't in `dropped`;
    - neither of the machine's ids is in `r.ids` for another person (generalise `taken` to a
      `fn machine_taken(&self, person, &Machine) -> bool` used by both).

    Then it is pushed to the member's machines and its ids are claimed.
  - **`RemoveMachine`** counts when `c.by == person`, the member has that endpoint, and it isn't
    their only machine. Then the machine is removed and the endpoint is inserted into `dropped`.
    If the person's role is `>= Writer`, call `retire(&[that machine], kept, c.at)`.
  - **`Rekey`** counts when all of these hold:
    - `c.by == *to`;
    - `role_of(person).is_some()`;
    - `role_of(to).is_none()`;
    - `!removed.contains(to)`;
    - `chain_ends(certs, person) == Some(*to)`.

    Then replace `person` by `to` in `members`, `ranks` (key), `promoters` (keys and the `by` of
    values), `outs` (key) and `ids` (values). Then drop the listed endpoints of that member as
    `RemoveMachine` does. The last-machine rule doesn't apply here, because the remover may not
    be in this swarm. Retire them when the role is `>= Writer`.

  None of the three is `retiring()`. The old key can't sign anything that counts afterwards,
  since `role_of(old)` is `None`.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p hydra-swarms --lib`
  Expected: all pass, including every plan A roster test unchanged.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): roster changes for a person's machines and a new personal key"`

### Task 2: Your machines' shared state and link codes

**Files:**
- Create: `crates/hydra-swarms/src/mine.rs`, with `pub mod mine;` in `lib.rs`
- Modify: `crates/hydra-swarms/src/invites.rs`, making the invite store generic over its payload

**Interfaces:**
- Produces in `invites.rs`:
  - `pub struct Invites<R = Role>` and `pub struct Invite<R = Role> { pub id, pub role: R, pub created, pub expires, pub used }`,
    with `R: Serialize + DeserializeOwned + Clone`.
  - `Invites::<R>::at(path: PathBuf) -> Invites<R>`. `Invites::new(dir)` stays as
    `at(dir.join("invites.json"))`.
  - `create(&self, role: R, now)`.
  - `spend`/`redeem` return `R`.
  - `pub(crate)` on `token_hash` and on `random32`.

  Existing swarm invite behaviour and file format are unchanged. Plan A's invite tests pass
  untouched.
- Produces in `mine.rs`:
  - `pub const DEVICE_KEY_ID: &str = "device";`
  - `pub const LINK_PREFIX: &str = "hydra-link:";`
  - `pub struct LinkCode { pub host: String, pub relay: Option<String>, pub token: String }`,
    with `encode`/`decode` like `InviteCode` (URL-safe base64 of JSON, after the prefix).
  - `pub type Links = Invites<()>;`, stored at `<root>/links.json`.
  - `#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)] pub struct Device { pub id: String, pub name: String, pub name_at: u64 }`,
    where `id` is the device endpoint id and `name_at` is Unix microseconds.
  - `#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)] pub struct Mine { pub name: String, pub name_at: u64, pub devices: Vec<Device>, pub removed: Vec<String>, pub chain: Vec<RekeyCert> }`
    is everything your machines agree on. `devices` includes this one.
  - `impl Mine`:
    - `fn load(root: &Path) -> Mine`. A missing file gives the default. A damaged one is moved
      aside to `mine.json.damaged`, as `Registry::load` does, and gives the default.
    - `fn save(&self, root: &Path) -> anyhow::Result<()>`.
    - `fn merge(&mut self, other: &Mine) -> bool`. True if anything changed.
    - `fn linked(&self, me: &str) -> Vec<&Device>`: the devices other than `me` and not
      removed.
    - `fn knows(&self, id: &str) -> bool`: in `devices` and not removed.
    - `fn set_name(&mut self, name: &str, at: u64)`.
    - `fn set_device_name(&mut self, id: &str, name: &str, at: u64)`.
    - `fn remove(&mut self, id: &str)`.

- [ ] **Step 1: Write the failing tests** in `mine.rs`:
  - `merging_takes_the_newer_name_and_device_names`: two `Mine`s whose `name_at` and a device's
    `name_at` differ. Merge both ways. Expect the same result either way, holding the newer of
    each.
  - `a_removal_wins_over_a_device_still_listed_elsewhere`: A has `removed: [d]` and B lists `d`
    as a device. After merging, `d` isn't `knows`, and `removed` contains it on both sides.
  - `the_longer_valid_chain_wins`: the chains `[k1→k2]` and `[k1→k2, k2→k3]` merge to the
    longer. A chain that doesn't extend ours is ignored. `merge` never accepts certs that fail
    `chain_ends`.
  - `a_link_code_round_trips_and_others_do_not_decode`: an `encode`/`decode` round trip. A
    `hydra-swarm:` code doesn't decode, and nor does junk.
  - `links_are_single_use_and_only_hashed`: `Links::at(..)`, `create((), now)`,
    `redeem(token, now)` gives `Some(())` once. The file never contains the token.
  - `a_damaged_mine_file_is_set_aside`.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo test -p hydra-swarms --lib mine:: invites::`
  Expected: compile errors.

- [ ] **Step 3: Implement.** For `merge`:
  - Union `removed`.
  - Union `devices` by `id`, keeping the higher `name_at`'s name. Ties go to the
    lexicographically higher name, so both sides agree.
  - Take the higher `name_at` for `name`, with the same tie rule.
  - Take `other.chain` only when it is longer and its first `chain.len()` certs equal ours, and
    every cert verifies.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p hydra-swarms --lib`
  Expected: all pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): your machines' shared state and single-use link codes"`

### Task 3: The device endpoint and the `hydra/mine/1` protocol

**Files:**
- Create: `crates/hydra-swarms/src/device.rs`, with `pub mod device;` in `lib.rs`
- Modify: `crates/hydra-swarms/src/wire.rs` (the mine messages, sharing `write_msg`/`read_msg`)
- Test: `crates/hydra-swarms/tests/devices.rs`

**Interfaces:**
- Produces in `wire.rs`:
  - `pub const MINE_ALPN: &[u8] = b"hydra/mine/1";`
  - `#[serde(tag = "type", rename_all = "snake_case")] pub enum MineRequest` with these
    variants:
    - `Link { token: String, name: String }`
    - `Sync { mine: Mine }`
    - `Offer { offer: Offer }`
    - `Joining { ns: String, change: SignedChange }`
    - `NewKey { secret: String, chain: Vec<RekeyCert> }`
  - `pub enum MineReply` with these variants:
    - `Linked { person: String, mine: Mine, offers: Vec<Offer> }`, where `person` is the
      personal secret key as hex;
    - `Synced { mine: Mine }`;
    - `Joined { write_secret: Option<String>, host: String }`, where `host` is the answering
      machine's endpoint in that swarm, for the joiner's gate;
    - `Ok`;
    - `Refused { reason: String }`.
  - `#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)] pub struct Offer { pub ns: String, pub anchor: String, pub name: String, pub role: Role, pub from: String }`,
    where `from` is the offering device's id.
- Produces in `device.rs`:
  - `pub struct DeviceNode`, owning an iroh `Endpoint` from `node::endpoint_builder(&device_key, relays)`
    and a `Router` accepting `MINE_ALPN` only.
  - `pub trait DeviceHandler: Send + Sync + 'static { fn answer(&self, from: String, req: MineRequest) -> BoxFuture<'static, MineReply>; }`.
    `from` is the caller's device id.
  - `DeviceNode::start(device_key: [u8; 32], relays: bool, handler: Arc<dyn DeviceHandler>) -> anyhow::Result<DeviceNode>`.
  - `DeviceNode::id(&self) -> String`.
  - `DeviceNode::addr_for_tests(&self) -> EndpointAddr`.
  - `DeviceNode::remember_addr(&self, addr: EndpointAddr)`.
  - `DeviceNode::ask(&self, to: &str, req: &MineRequest) -> anyhow::Result<MineReply>`. A
    30-second timeout, as `invites::ask`.
  - `DeviceNode::shutdown(self)`.

  The handler, not the node, decides who may ask what. The node passes every request through
  with the caller's verified endpoint id.

- [ ] **Step 1: Write the failing tests** in `tests/devices.rs`, with relays off and direct
  addresses:
  - `a_request_reaches_the_handler_with_the_callers_id`: a handler recording `(from, req)`.
    Assert that `from == caller.id()` and the reply comes back.
  - `a_slow_handler_times_out_the_caller`: a handler that never replies. `ask` errors within
    the timeout. Use a test-only shorter timeout via `DeviceNode::ask_within(to, req, Duration)`,
    which `ask` calls with 30 s.
  - `round trips` for every `MineRequest`/`MineReply` variant in `wire.rs`'s test module, as
    plan A's `round_trips_every_message` does.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo test -p hydra-swarms --test devices && cargo test -p hydra-swarms --lib wire::`
  Expected: compile errors.

- [ ] **Step 3: Implement**, modelled on `invites.rs`'s `Hydra` handler and `ask`: one bi-stream
  per request and one frame each way.

- [ ] **Step 4: Run the tests.** Expected: pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): a device endpoint for your own machines (hydra/mine/1)"`

### Task 4: Linking a machine

**Files:**
- Modify: `crates/hydra-swarms/src/service.rs` (`ServiceConfig`, `Command`, `ServiceEvent`, the
  device handler, `run`)
- Modify: `crates/hydra-swarms/src/lib.rs` (re-exports)
- Modify: `crates/hydra-swarms/src/ids.rs` (`Keyring::set`)
- Modify: `crates/hydraui/src/swarms/store.rs` (`SwarmKeys::set_person`) and
  `crates/hydraui/src/workspace/swarms.rs` (pass it to `ServiceConfig::new`), so the workspace
  keeps building
- Test: `crates/hydra-swarms/tests/service.rs` (extend the harness)

**Interfaces:**
- Consumes: Task 2's `Mine`, `Links`, `LinkCode` and `DEVICE_KEY_ID`, and Task 3's
  `DeviceNode`, `MineRequest` and `MineReply`.
- Produces:
  - `pub type PersonKeyFn = Box<dyn Fn([u8; 32]) -> Result<(), String> + Send>`, which stores a
    new personal key.
  - `ServiceConfig::new(root, identity, swarm_key, set_person: PersonKeyFn, relays)`. The new
    parameter goes before `relays`, and every caller is updated.
  - `Keyring::set(&self, user: &str, key: [u8; 32]) -> Result<(), String>` writes lowercase
    hex. `SwarmKeys::set_person(&self, key: [u8; 32]) -> Result<(), String>` uses it, and
    hydraui's `start` passes `Box::new(move |k| keys.set_person(k))`.
  - `Command::LinkCode { reply: oneshot::Sender<Result<String, String>> }`
  - `Command::CancelLink { id: String }`
  - `Command::Link { code: String }`
  - `Command::RemoveMachine { device: String }`, implemented in Task 7. Until then it replies
    `Failed("Not yet.")`.
  - `ServiceEvent::Machines(MachinesView)`, sent whenever `Mine` or link codes change.
  - `ServiceEvent::Named { name: String }`: another machine of yours set your name, so hydraui
    saves it to config.
  - `pub struct MachinesView { pub this: String, pub devices: Vec<DeviceView>, pub links: Vec<Invite<()>>, pub offers: Vec<Offer> }`,
    where `this` is this device's id.
  - `pub struct DeviceView { pub id: String, pub name: String, pub online: bool, pub this_machine: bool }`.
  - The service now starts the device node, from `swarm_key(DEVICE_KEY_ID)`, even with no
    swarms. A device id whose `Mine` lists only itself is "not linked".
  - The service owns `Mine`, and its `name` is the source of your name. At start:
    - If `mine.name` is empty, it is set from `identity.name` with `name_at = now`.
    - If it differs from `identity.name`, the service adopts `mine.name` and emits `Named`.

- [ ] **Step 1: Write the failing tests** in `tests/service.rs`. Extend `Svc` so its `set_person`
  writes into `Svc.person`, an `Arc<Mutex<[u8; 32]>>`, and add `Svc::link_code()` and
  `Svc::machines()` helpers in the style of `invite`/`view`.
  - `a_linked_machine_takes_the_person_and_both_list_each_other`: create `olive` and `olive2`,
    then:
    1. `olive` makes a link code.
    2. `olive2` runs `Command::Link { code }`.

    Assert:
    - `olive2`'s stored person equals `olive`'s;
    - `olive2` emits `Named { name: "olive" }`;
    - both `Machines` views list 2 devices, each `online`, with `this_machine` set on its own
      one;
    - `olive`'s `links` is empty, because the code was spent.
  - `a_link_code_is_single_use`: a second machine using the same code gets a `Failed`
    containing "already been used".
  - `a_machine_in_swarms_cannot_link_and_the_code_survives`: `wren` creates a swarm, then tries
    `olive`'s code. Expect a `Failed` with the exact Global Constraints copy. A fresh machine
    then links with the same code successfully.
  - `a_set_key_is_what_get_returns` in `ids.rs`, skipping when there's no keyring, like the
    existing keyring test.
  - `strangers_get_nothing_from_a_device`: a raw `DeviceNode` that isn't linked asks `olive` for
    `Sync` and `NewKey`. Both are `Refused`, and `olive`'s `Mine` is unchanged.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo test -p hydra-swarms --test service link`
  Expected: compile errors.

- [ ] **Step 3: Implement.**
  - `LinkCode` creates through `Links::at(root/"links.json")` and gives `LinkCode { host: device id, relay, token }.encode()`.
  - `Link` decodes the code, refuses at once if the registry has any entry or `Mine` has other
    devices, then asks the host `Link { token, name: identity.machine_name }`. On `Linked`:
    1. Call `set_person`, and use the new key as the service's identity from then on. No swarm
       is open, because linking needs a machine in none.
    2. Merge `mine`, adding this device.
    3. Save the offers to `<root>/offers.json`.
    4. Emit `Named` and `Machines`.
  - The handler answers:
    - **`Link`:** spend the token. Refuse "That link code …" with the invite refusal texts on
      failure. Add the caller as a `Device { id: from, name, name_at: now }`. Reply with this
      person's key, `Mine`, and an `Offer` for every registry swarm not removed or leaving, with
      the role from its roster.
    - **`Sync`:** only from `knows(from)`. Merge, save, reply with ours, and emit `Machines` and
      `Named` when the name changed.
    - Everything else from a stranger: `Refused { reason: "This isn't one of your machines." }`.
  - Sync with every linked device at start, on every `Mine` change, on `PERIODIC`, and whenever
    a request from one arrives. Online in `DeviceView` means the last ask to that device in the
    last `PERIODIC` succeeded.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p hydra-swarms`
  Expected: all pass, including plan A's.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): link another machine of yours with a single-use code"`

### Task 5: Offers, and joining a swarm as another machine of yours

**Files:**
- Modify: `crates/hydra-swarms/src/service.rs` (offers, `AcceptOffer`/`DeclineOffer`, the
  `Joining` handler, sending offers)
- Modify: `crates/hydra-swarms/src/invites.rs` (`on_relay` accepts a person's machine changes)
- Modify: `crates/hydra-swarms/src/node.rs` (`Core::kept_machine`)
- Test: `crates/hydra-swarms/tests/service.rs`

**Interfaces:**
- Consumes: Task 1's `Change::AddMachine`, and Task 4's handler, `Mine` and `MachinesView.offers`.
- Produces:
  - `Command::AcceptOffer { swarm: String, folder: PathBuf }`
  - `Command::DeclineOffer { swarm: String }`
  - `<root>/offers.json`: `{ offers: Vec<Offer>, declined: Vec<String> }`. A declined `ns` is
    never offered again.
  - `SwarmView.my_other_machines: usize` counts your person's machines in that swarm, other than
    this one.
  - `pub(crate) async fn Core::kept_machine(&self, author: &str) -> anyhow::Result<Kept>` is
    `kept` for one author's file entries and records, with `changes` empty. Task 6's
    `RemoveMachine` and Task 7's `Rekey` use it.

- [ ] **Step 1: Write the failing tests:**
  - `an_offered_swarm_joins_as_the_same_person`:
    1. `olive` creates "Notes" and invites `wren` as a writer.
    2. `olive2` links to `olive`, sees an offer for "Notes" with role `Owner`, and accepts it
       into an empty folder.

    Assert:
    - `wren`'s view of "Notes" shows one member "olive" with 2 machines;
    - a file `olive2` writes reaches `wren`'s folder;
    - `olive2` can write. A file it adds is accepted by `wren`, which proves it got the write
      capability.
  - `a_later_swarm_is_offered_to_your_other_machines`: link first, then `olive` creates
    "Plans". `olive2` gets a toast "olive-pc offered you Plans." and an offer.
  - `a_reader_joins_its_other_machine_through_an_owner`:
    1. `wren` is a reader in "Notes".
    2. `wren2` links to `wren` and accepts.

    `olive` (the owner) writes the relayed `AddMachine`. `olive`'s view shows `wren` with 2
    machines, and `wren2` receives files.
  - `an_offer_is_taken_through_any_machine_of_yours_in_the_swarm`: `olive`, `olive2` and
    `olive3` are linked. `olive` and `olive2` are in "Notes", and `olive` stops. `olive3`
    accepts, and it succeeds through `olive2`. With `olive2` stopped too, accepting fails with
    "None of your machines in Notes is running. Try again when one is." and the offer remains.
  - `a_declined_offer_does_not_come_back`: decline, then re-sync from `olive`. No offer, and no
    toast.

- [ ] **Step 2: Run them to see them fail.** Expected: compile errors, then failures.

- [ ] **Step 3: Implement.**
  - **Offers:** after a swarm is created, joined or accepted, send `Offer` to every linked
    device, retrying on `PERIODIC` until the reply is `Ok`. The pending sends are saved in
    `offers.json` under `owed: Vec<(device, ns)>`. A device receiving an `Offer` for a swarm
    already in its registry, or declined, answers `Ok` and ignores it.
  - **`AcceptOffer`:**
    1. Check that the folder is empty or new.
    2. Make a `join-<16 hex>` key and an author, and build `Machine { endpoint, name: machine_name, author }`.
    3. Sign `AddMachine` with the person key.
    4. Ask each linked device, the offer's `from` first, with `Joining { ns, change }` until one
       answers `Joined`.
    5. Then open as `try_join`'s second half does: `SwarmNode::join` with `Capability::Write`
       when `write_secret` is given, else `Read(ns)`. Use the anchor from the offer, the gate
       open to that device's swarm endpoint, and sync with it.

    Factor `try_join`'s open-and-sync half into
    `async fn open_joined(shared, identity, ns, anchor, name, folder, key_id, cap, author, host: EndpointId) -> anyhow::Result<Opened>`,
    used by both.
  - **The `Joining` handler** accepts only from `knows(from)`, and refuses when the swarm isn't in
    the registry ("Not in that swarm here."). It verifies that `change.by` is this person, that
    the change is `AddMachine` for them, and that its signature is valid. Then it passes the
    change to that swarm's worker as a new `Msg::Joining(change, oneshot)`. The worker writes it
    if it can, else queues it in the outbox and calls `deliver(true)`. It replies `Joined` with
    the write secret when the person's role is `>= Writer`, and with `host` set to this
    machine's endpoint in that swarm.
  - **`on_relay`:** extend the `own` match to accept `AddMachine { person, .. }` and
    `RemoveMachine { person, .. }` where `person == change.by`. The sender must still be a
    machine of `change.by`.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p hydra-swarms`
  Expected: all pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): swarms are offered to your other machines and join as the same person"`

### Task 6: Names across your machines, and leaving on one machine

**Files:**
- Modify: `crates/hydra-swarms/src/service.rs` (`SetIdentity`, `Named`, `leave`)
- Test: `crates/hydra-swarms/tests/service.rs`

**Interfaces:**
- Consumes: Task 4's `Mine.set_name`, `set_device_name` and `Named`, and Task 1's
  `RemoveMachine`.
- Produces:
  - `SetIdentity` sets `Mine` name and this device's name with `at = now_micros()`, then syncs
    to your machines. A `Sync` that changes the name updates the service's identity, sends
    `Msg::Identity` to every swarm (plan A's rename path) and emits `Named`.
  - The rename path compares against `Mine`'s name, so two machines never rename back and
    forth.
  - `Command::Leave` on a swarm where `my_other_machines > 0` writes `RemoveMachine` for this
    machine (with `kept_machine` when the role is `>= Writer`), relayed through an owner for a
    reader. It then forgets the swarm locally as plan A's leave-finished does, keeping the files.

- [ ] **Step 1: Write the failing tests:**
  - `your_name_changes_on_every_machine`: `olive` and `olive2` are linked and both in "Notes".
    `olive` runs `SetIdentity { name: "Olive O", .. }`. `olive2` emits `Named { name: "Olive O" }`.
    `wren`'s view shows "Olive O" once, and stays that way for `QUIET`.
  - `names_set_apart_settle_on_the_newer`: stop `olive2`, rename on `olive` ("A"), then rename on
    `olive2` ("B", later) while `olive` is stopped. Start both. Both end with "B", and `wren`'s
    roster ends with "B" and stays there for `QUIET`.
  - `leaving_on_one_machine_keeps_the_person_in`: `olive2` leaves "Notes".
    - `wren` shows "olive" with 1 machine.
    - `olive2`'s folder files remain, and it no longer gets changes.
    - `olive` is unaffected.
  - `leaving_on_the_last_machine_is_the_person_leaving`: unchanged plan A behaviour when the
    person has one machine in the swarm. The existing test still passes.

- [ ] **Step 2: Run them to see them fail.**

- [ ] **Step 3: Implement**, as in Interfaces. A machine's own name stays per machine: a
  `Device` name change from a `Sync` updates only `DeviceView`, never this machine's
  `machine_name`.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p hydra-swarms`
  Expected: all pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): your name follows you across your machines; leave on one machine"`

### Task 7: Removing one of your machines

**Files:**
- Modify: `crates/hydra-swarms/src/service.rs` (`RemoveMachine`, `NewKey`, `restart_swarms`,
  removal detection)
- Modify: `crates/hydra-swarms/src/invites.rs` (`on_relay` accepts `Rekey`, and `on_removed`
  accepts a dropped endpoint)
- Test: `crates/hydra-swarms/tests/service.rs`

**Interfaces:**
- Consumes: Task 1's `Rekey`, `RekeyCert` and `chain_ends`; Task 2's `Mine.chain` and `remove`;
  Task 4's `set_person`; Task 5's `kept_machine`.
- Produces:
  - **`Command::RemoveMachine { device }`** refuses this machine ("You can't remove the machine
    you're on.") and unknown ids. Otherwise it:
    1. Generates `new = PersonKey::generate()` and pushes `RekeyCert::new(&old, new.id())` onto
       `Mine.chain`.
    2. Runs `Mine.remove(device)`, then saves.
    3. Calls `set_person(new)`.
    4. Restarts every swarm with the new identity (`restart_swarms`: stop every swarm task, then
       reopen every registry entry as at start).
    5. Queues `NewKey { secret: new hex, chain }` to every other linked device, retried on
       `PERIODIC` and on contact, and saved under `mine_owed.json`.
    6. Toasts "<device name> was removed from your machines."
  - **Rekeying a swarm:** after each roster read, a swarm worker looks for the first cert in
    `Mine.chain` whose `from` the roster still has as a member. If it finds one, it writes
    `Rekey { person: that from, to: current id, certs: the chain from that cert to the end, drop, kept }`,
    signed by the current key. The roster shows a new key only once the `Rekey` is written, so
    finding none means there is nothing to do. `drop` is the endpoints in that roster of machines on no device in `Mine`
    any more. `kept` merges `kept_machine` for each dropped author. A reader relays it through
    the outbox. This one rule covers the remover and every machine that learns the key later.
  - **`NewKey` handler:** only from `knows(from)`, and only when `chain_ends(chain, current id)`
    gives the id of `secret`. Then it runs `set_person`, merges the chain and calls
    `restart_swarms`.
  - **`on_relay` accepts `Rekey`** when the sender is a machine of `change.person` (the old key),
    `change.by == to`, and the signature verifies.
  - **Removal detection:** a worker whose roster has its own endpoint in `dropped` treats it as
    removed (`mark_removed`), with the toast "This machine was removed from <swarm>." The device
    handler answers every request from a removed device with
    `Refused { reason: "This machine was removed from your machines." }`. A device that gets
    that refusal emits it as a toast once and stops syncing `Mine`.
  - **Telling the removed machine:** for each swarm, send plan A's `Request::Removed` with the
    `Rekey` change to the removed machine's endpoints, best effort. `on_removed` also accepts a
    change whose fold puts this machine's endpoint in `dropped`.

- [ ] **Step 1: Write the failing tests:**
  - `removing_a_machine_shuts_it_out_and_keeps_you_in`: `olive`, `olive2` and `olive3` are
    linked, all in "Notes" with `wren` (writer). `olive` removes `olive3`. Assert:
    - `wren`'s view shows one "olive" with 2 machines, under a new person id;
    - `olive2`'s stored person equals `olive`'s new one;
    - `olive3` toasts "This machine was removed from Notes.";
    - a file `olive3` writes afterwards never reaches `wren`;
    - files `olive3` wrote before are still in `wren`'s folder.
  - `a_machine_that_was_away_picks_up_the_new_key`: stop `olive2`, then `olive` removes
    `olive3`, then start `olive2`. `olive2` gets the new key, its "Notes" view shows
    `my_role == Writer`/`Owner` as before, and `wren` still sees one "olive". `olive2` and
    `olive` both write the `Rekey` for any swarm only they share (a second swarm "Plans" that
    only `olive2` is in, with `wren` as owner). `wren`'s "Plans" shows the new id.
  - `the_old_key_is_refused_after_a_removal`: a `Rename` signed with the old key and passed by
    hand to `wren`'s node as a relay is refused, and the roster is unchanged.
  - `you_cannot_remove_the_machine_you_are_on`.

- [ ] **Step 2: Run them to see them fail.**

- [ ] **Step 3: Implement**, as in Interfaces. `restart_swarms` lives in `run`: on
  `Msg::Restart` it sends `Msg::Stop` to each handle, awaits the tasks, then spawns
  `open_existing` for each registry entry with the new identity, as at start.

- [ ] **Step 4: Run every swarm test.**
  Run: `cargo test -p hydra-swarms`
  Expected: all pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(swarms): removing one of your machines replaces your key and shuts it out"`

### Task 8: hydraui: keys, names and starting with linked machines

**Files:**
- Modify: `crates/hydraui/src/swarms/mod.rs` (`has_swarms` becomes `has_swarms_or_machines`)
- Modify: `crates/hydraui/src/workspace/swarms.rs` (`ServiceEvent::Named`/`Machines`,
  `Command::Link` starts the service)

**Interfaces:**
- Consumes: Task 4's `ServiceEvent::Named` and `ServiceEvent::Machines`.
- Produces:
  - `swarms::has_swarms_or_machines(paths) -> bool` is true when the registry is non-empty
    (plan A's rule), or when `swarms/mine.json` has more than one device.
  - `Workspace.swarm_machines: Option<MachinesView>`.
  - On `Named`, save `peer_name` through `config::set`, off the UI thread, as
    `swarms_save_you` does, and refresh the You fields.
  - `Command::Link` goes through `swarms_create_or_join`, so it starts the service on a machine
    with no swarms.

- [ ] **Step 1: Write the failing tests:**
  - `has_swarms_or_machines` with an empty registry and a `mine.json` of 1 device (false) and of
    2 devices (true).
  - `a_named_event_saves_the_name` against the config file in a temp `HydraPaths`, in the style
    of `swarms_do_not_start_before_a_name_is_set`.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo test -p hydraui swarms`

- [ ] **Step 3: Implement**, as in Interfaces.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p hydraui && cargo test -p hydra-swarms`
  Expected: pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(hydraui): swarms keep your new key and name, and run for linked machines"`

### Task 9: hydraui: the You page, linking, offers and Remove…

**Files:**
- Modify: `crates/hydraui/src/workspace/swarms_page.rs` (the You page sections and dialogs)
- Modify: `crates/hydraui/src/workspace/settings_tab.rs` (a sidebar entry under SWARMS, "Link
  this machine…", shown when not linked and in no swarm)
- Modify: `README.md` (the Swarms section), `docs/superpowers/specs/2026-10-27-swarms-design.md`
  (record this plan's calls in §6)

**Interfaces:**
- Consumes: `Workspace.swarm_machines`, and `Command::LinkCode`, `CancelLink`, `Link`,
  `AcceptOffer`, `DeclineOffer` and `RemoveMachine`.
- Produces, on the **You and this machine** page in the HydraUI look:
  - **"Your machines":** one row per `DeviceView`, with:
    - a dot (online green, offline muted);
    - its name, with "(this machine)" on its own;
    - **Remove…** on the others.

    Under the rows go any pending link codes ("Link code made <day>, unused, expires <day>", with
    **Cancel**), and the button **Add another machine of mine…**.
  - **Add another machine of mine…** is a dialog like the invite one:
    - It shows the code from `LinkCode` with **Copy code**.
    - The note reads: "Works once · expires in 7 days · this hydraui must be running when your
      other machine uses it".
    - The body reads: "On your other machine, open Settings › Swarms › Link this machine… and
      paste this code. It becomes yours: your name, and the swarms you're in are offered to it."
  - **Link this machine to one of yours…** is a dialog with a code input. It runs
    `checked_link` (it must start with `hydra-link:`; otherwise "That isn't a link code. It
    starts with hydra-link:"). It asks for the machine name only when unset, using the identity
    fields as join does. Then it sends `Command::Link`.
  - **"Offered to this machine"** (shown when `offers` isn't empty): one row per offer, with the
    swarm name, role badge and "from <device name>", plus **Choose folder…** (the folder picker
    as in join, then `AcceptOffer`) and **Not on this machine** (`DeclineOffer`).
  - **Remove…** asks for confirmation through the existing confirm dialog:
    - Title: "Remove <name>?"
    - Body: "It's shut out of every swarm you're in, and your other machines get a new key for
      you. It keeps the files it already has. This can't be undone; to use it again, link it
      again from scratch."
    - Confirm button: "Remove".
  - **Leave on a swarm page:** when `my_other_machines > 0`, the button reads "Leave on this
    machine…" and the note reads "Your other machines stay in <swarm>. This machine keeps its
    files but stops syncing them."
- In the README's Swarms section, add a paragraph on linking your machines and on removing
  one, and add the known limit: "A removed machine still holds your old key. hydra no longer
  accepts it, but a modified client could still sign backdated changes with it, as with the
  other members-log limits."
- In the spec's §6, add bullets marked *My call (plan B)*:
  - linking needs a machine in no swarm;
  - leaving on one machine drops only that machine;
  - offers carry the write capability only to your own machines, over `hydra/mine/1`.

- [ ] **Step 1: Write the failing tests** in `swarms_page.rs`'s test module, for the pure
  helpers:
  - `checked_link`: accepts a `hydra-link:` code, and refuses a `hydra-swarm:` code and blanks
    with the copy above.
  - `device_detail(&DeviceView) -> String`: "this machine", "online" or "offline".
  - `leave_label(&SwarmView)`: "Leave on this machine…" when `my_other_machines > 0`, else
    "Leave swarm…".
  - `link_text(&Invite<()>, &TimeZone)`: as `invite_text`, for link codes.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo test -p hydraui swarms_page`

- [ ] **Step 3: Implement** the helpers and the page sections.

- [ ] **Step 4: Run the full verification.**
  Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
  Expected: clean, all pass.

- [ ] **Step 5: Commit**
  `git commit -m "feat(hydraui): link your machines, take offered swarms, and remove a machine"`

- [ ] **Step 6: Check by hand** (record the result for the user; don't claim it unless done).
  Run two dev copies, each with its own `HYDRA_HOME` and stopped by PID:
  1. Make a swarm on copy 1.
  2. Copy 1: Add another machine of mine…
  3. Copy 2: Link this machine…
  4. Take the offer on copy 2 and see one person with two machines.
  5. Remove copy 2 from copy 1.
