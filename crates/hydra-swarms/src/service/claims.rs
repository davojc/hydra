//! Claims on a swarm's task: taking a key, renewing it for as long as its handle lives, and telling
//! the holder when it stops being this machine's. The rules are in `crate::claims`, and the
//! records in `crate::node::claims`.
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{Msg, SETTLE, Sent, SwarmView, Worker};
use crate::claims::{
    ClaimRecord, ContextSettings, DoneRecord, Holder, KeyState, People, Records, ReleaseRecord,
    ReopenRecord, Take, decide_take, settle, valid_context_name,
};
use crate::ids::PersonId;
use crate::node::{SwarmNode, now_micros};
use crate::roster::{Role, Roster};
use crate::wire::ClaimAnswer;

/// What a lapse allows for clocks and sync running late: well above any likely skew.
pub(super) const CLOCK_SLACK: Duration = Duration::from_secs(30);
/// How long the members get to answer. A member that doesn't is taken to be out of reach.
const ASK_FOR: Duration = Duration::from_secs(3);
/// How often the view is worked out again with nothing to prompt it, so lapses show.
const VIEW_EVERY: Duration = Duration::from_secs(15);
const MAX_KEY_BYTES: usize = 1024;
/// Other machines silently ignore a record over 64 KiB, so what a caller says about itself can't
/// be allowed to take one there.
const MAX_LABEL_CHARS: usize = 200;
const RECENT_DONE: usize = 20;

pub const NO_LONGER_YOURS: &str = "This claim is no longer yours.";

pub(super) type TakeReply = oneshot::Sender<Result<ClaimOutcome, String>>;

/// What came of asking for a key. Times are Unix microseconds.
#[derive(Debug)]
pub enum ClaimOutcome {
    Yours(ClaimHandle),
    Held {
        person: String,
        machine: String,
        this_machine: bool,
        since: u64,
    },
    Done {
        person: String,
        at: u64,
    },
    Paused,
    AtLimit(i32),
    /// Only for a retake: the claim it came for has ended, and this is why.
    Gone(LostReason),
}

/// Why a claim stopped being this machine's.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum LostReason {
    Beaten { person: String, machine: String },
    Done { person: String },
    Released { person: String },
    Lapsed,
}

/// A live claim. The service renews it for as long as this is held; dropping it stops the
/// renewals, and the claim then lapses by itself.
#[derive(Debug)]
pub struct ClaimHandle {
    id: u64,
    swarm: String,
    context: String,
    key: String,
    lapse_micros: u64,
    lost: mpsc::UnboundedReceiver<LostReason>,
    worker: mpsc::UnboundedSender<Msg>,
}

impl ClaimHandle {
    pub fn swarm(&self) -> &str {
        &self.swarm
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// How long, in microseconds, this claim can go unrenewed before it lapses.
    pub fn lapse_micros(&self) -> u64 {
        self.lapse_micros
    }

    /// Next loss, once; None after the swarm has gone.
    pub async fn lost(&mut self) -> Option<LostReason> {
        self.lost.recv().await
    }

    /// Gives the key up unfinished, so anyone can claim it straight away.
    pub async fn release(self) -> Result<(), String> {
        self.finish(false).await
    }

    /// Marks the key finished and frees this machine's slot.
    pub async fn done(self) -> Result<(), String> {
        self.finish(true).await
    }

    async fn finish(&self, done: bool) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        let id = self.id;
        if self
            .worker
            .send(Msg::ClaimFinish { id, done, reply })
            .is_err()
        {
            return Err(NO_LONGER_YOURS.to_string());
        }
        answer
            .await
            .unwrap_or_else(|_| Err(NO_LONGER_YOURS.to_string()))
    }
}

impl Drop for ClaimHandle {
    fn drop(&mut self) {
        // A swarm that has gone has nothing to stop.
        let _ = self.worker.send(Msg::ClaimDrop { id: self.id });
    }
}

/// One context as it stands.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ContextView {
    pub name: String,
    pub settings: ContextSettings,
    pub live: Vec<ClaimView>,
    /// Newest first, at most 20.
    pub done: Vec<DoneView>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ClaimView {
    pub key: String,
    /// The author id of the machine holding it, which is what a release names.
    pub author: String,
    pub person: String,
    pub machine: String,
    /// Unix microseconds.
    pub renewed_at: u64,
    pub holder: Holder,
    /// Held by any of this person's machines.
    pub mine: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DoneView {
    pub key: String,
    pub person: String,
    /// Unix microseconds.
    pub at: u64,
}

/// A claim still being decided: who to tell, and what they will hear of it afterwards.
struct Deciding {
    reply: TakeReply,
    lost: mpsc::UnboundedReceiver<LostReason>,
}

struct BookEntry {
    context: String,
    key: String,
    claim: ClaimRecord,
    settings: ContextSettings,
    lost: mpsc::UnboundedSender<LostReason>,
    /// Some until the asking is over. It counts towards the limit and is renewed meanwhile, but
    /// can't be lost: it isn't anyone's yet.
    deciding: Option<Deciding>,
    renew_at: Instant,
}

/// The claims this machine is holding for something alive.
pub(super) struct ClaimBook {
    live: HashMap<u64, BookEntry>,
    next_id: u64,
    /// When to re-read after a claim changed; many changes close together are read once.
    settle_at: Option<Instant>,
    view_at: Instant,
}

impl ClaimBook {
    pub(super) fn new() -> ClaimBook {
        ClaimBook {
            live: HashMap::new(),
            next_id: 0,
            settle_at: None,
            view_at: Instant::now() + VIEW_EVERY,
        }
    }

    fn add(&mut self, entry: BookEntry) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.live.insert(id, entry);
        id
    }

    fn holding(&self, context: &str, key: &str) -> Option<(u64, &BookEntry)> {
        self.live
            .iter()
            .find(|(_, e)| e.context == context && e.key == key)
            .map(|(id, e)| (*id, e))
    }

    fn next_at(&self) -> Instant {
        let renewals = self.live.values().map(|e| e.renew_at);
        renewals
            .chain(self.settle_at)
            .fold(self.view_at, Instant::min)
    }

    /// Ends every claim because the swarm is going: holders' `lost` gives None from here on.
    pub(super) fn close(&mut self, name: &str) {
        for (_, entry) in self.live.drain() {
            if let Some(deciding) = entry.deciding {
                let _ = deciding.reply.send(Err(format!("{name} isn't open.")));
            }
        }
    }
}

/// The swarm a claim command means: the one named, by id and then by name, or without one, the
/// only one with that context.
pub(super) fn resolve_swarm(
    views: &[SwarmView],
    swarm: Option<&str>,
    context: &str,
) -> Result<String, String> {
    let open = || views.iter().filter(|v| !v.removed);
    if let Some(swarm) = swarm {
        if let Some(v) = views.iter().find(|v| v.id == swarm) {
            return Ok(v.id.clone());
        }
        let mut named = open().filter(|v| v.name == swarm);
        return match (named.next(), named.next()) {
            (Some(v), None) => Ok(v.id.clone()),
            (Some(_), Some(_)) => Err(format!(
                "More than one swarm here is called {swarm}: use its id."
            )),
            (None, _) => Err("That swarm isn't on this machine.".to_string()),
        };
    }
    let mut having = open().filter(|v| v.claims.iter().any(|c| c.name == context));
    match (having.next(), having.next()) {
        (Some(v), None) => Ok(v.id.clone()),
        (Some(_), Some(_)) => Err(format!(
            "{context} is in more than one swarm: name which one."
        )),
        (None, _) => Err(format!("No swarm here has a context called {context}.")),
    }
}

fn check_context(context: &str) -> Result<(), String> {
    if valid_context_name(context) {
        Ok(())
    } else {
        Err("A context's name uses a-z, 0-9, - and _.".to_string())
    }
}

fn check_claim(context: &str, key: &str) -> Result<(), String> {
    check_context(context)?;
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(format!("A claim's key is 1 to {MAX_KEY_BYTES} bytes."));
    }
    Ok(())
}

fn renew_every(settings: &ContextSettings) -> Duration {
    Duration::from_secs(u64::from(settings.renew_secs))
}

/// The person and machine behind an author id, as the roster has them.
fn who(roster: &Roster, author: &str) -> (String, String) {
    match roster.member_by_author(author) {
        Some(m) => {
            let machine = m.machines.iter().find(|x| x.author == author);
            let machine = machine.map_or("another machine", |x| x.name.as_str());
            (m.name.clone(), machine.to_string())
        }
        // Finished keys outlive the people who finished them.
        None => {
            let machine = roster.retired.get(author);
            let machine = machine.map_or("another machine", |r| r.machine.as_str());
            ("a former member".to_string(), machine.to_string())
        }
    }
}

/// What a decision that doesn't give this machine the key tells the caller. `Go` gets here only
/// when a claim already written has since ended, by a release say, so asking again is the answer.
fn told(take: Take, key: &str, roster: &Roster) -> Result<ClaimOutcome, String> {
    match take {
        Take::Go | Take::TakeUp(_) => {
            Err(format!("{key} couldn't be claimed just now. Try again."))
        }
        Take::Held { author, claim } => {
            let (person, machine) = who(roster, &author);
            Ok(ClaimOutcome::Held {
                person,
                machine,
                this_machine: false,
                since: claim.claimed_at,
            })
        }
        Take::Done { author, at } => Ok(ClaimOutcome::Done {
            person: who(roster, &author).0,
            at,
        }),
        Take::Paused => Ok(ClaimOutcome::Paused),
        Take::AtLimit(limit) => Ok(ClaimOutcome::AtLimit(limit)),
    }
}

fn loss_reason(
    records: &Records,
    people: &People,
    roster: &Roster,
    me: &str,
    claim: &ClaimRecord,
    state: Option<&KeyState>,
) -> LostReason {
    let released = records.releases.iter().find(|(by, r)| {
        r.target == me
            && r.key == claim.key
            && r.claimed_at == claim.claimed_at
            && people.may_release(by, me)
    });
    if let Some((by, _)) = released {
        return LostReason::Released {
            person: who(roster, by).0,
        };
    }
    match state {
        Some(KeyState::Done { author, .. }) => LostReason::Done {
            person: who(roster, author).0,
        },
        Some(KeyState::Held { author, .. }) => {
            let (person, machine) = who(roster, author);
            LostReason::Beaten { person, machine }
        }
        _ => LostReason::Lapsed,
    }
}

/// Why a retake finds nothing to take up. With this machine's own record pruned too, the claim
/// is long past its lapse.
fn retake_loss(
    records: &Records,
    roster: &Roster,
    me: &str,
    key: &str,
    state: Option<&KeyState>,
) -> LostReason {
    let mine = records
        .claims
        .iter()
        .find(|(author, claim)| author == me && claim.key == key);
    match mine {
        Some((_, claim)) => {
            let people = People::from_roster(roster);
            loss_reason(records, &people, roster, me, claim, state)
        }
        None => LostReason::Lapsed,
    }
}

fn context_view(
    name: &str,
    settings: ContextSettings,
    states: &BTreeMap<String, KeyState>,
    roster: &Roster,
    me: &PersonId,
) -> ContextView {
    let mut live = Vec::new();
    let mut done = Vec::new();
    for (key, state) in states {
        match state {
            KeyState::Held { author, claim } => {
                let (person, machine) = who(roster, author);
                live.push(ClaimView {
                    key: key.clone(),
                    author: author.clone(),
                    person,
                    machine,
                    renewed_at: claim.renewed_at,
                    holder: claim.holder.clone(),
                    mine: roster
                        .member_by_author(author)
                        .is_some_and(|m| m.person == *me),
                });
            }
            KeyState::Done { author, at } => done.push(DoneView {
                key: key.clone(),
                person: who(roster, author).0,
                at: *at,
            }),
            KeyState::Free => {}
        }
    }
    done.sort_by_key(|d| std::cmp::Reverse(d.at));
    done.truncate(RECENT_DONE);
    ContextView {
        name: name.to_string(),
        settings,
        live,
        done,
    }
}

/// How a take goes on from what the records said.
enum Step {
    Answer(ClaimOutcome),
    /// The claim is written, and the members are to be asked.
    Ask {
        node: Arc<SwarmNode>,
        claim: ClaimRecord,
        settings: ContextSettings,
        endpoints: Vec<String>,
    },
}

impl Worker {
    pub(super) async fn claim_take(
        &mut self,
        context: String,
        key: String,
        holder: Holder,
        retake: bool,
        reply: TakeReply,
    ) {
        let answer = match self.take_step(&context, &key, holder, retake).await {
            Ok(Step::Answer(outcome)) => Ok(outcome),
            Ok(Step::Ask {
                node,
                claim,
                settings,
                endpoints,
            }) => {
                let (lost_tx, lost) = mpsc::unbounded_channel();
                let id = self.claim_book.add(BookEntry {
                    context,
                    key,
                    claim: claim.clone(),
                    settings,
                    lost: lost_tx,
                    deciding: Some(Deciding { reply, lost }),
                    renew_at: Instant::now() + renew_every(&settings),
                });
                self.shared.remember_peers(&node);
                self.sends.spawn(async move {
                    let answers = node.ask_claim(&endpoints, &claim, ASK_FOR).await;
                    Sent::ClaimAsked { id, answers }
                });
                self.claims_refresh().await;
                return;
            }
            Err(text) => Err(text),
        };
        self.claims_refresh().await;
        let _ = reply.send(answer);
    }

    async fn take_step(
        &mut self,
        context: &str,
        key: &str,
        mut holder: Holder,
        retake: bool,
    ) -> Result<Step, String> {
        check_claim(context, key)?;
        let node = self.live_node()?;
        let roster = self.fresh_roster(&node).await?;
        let role = roster.role_of(&self.person.id());
        if role.is_none_or(|r| r < Role::Writer) {
            return Err(format!("Readers can't claim in {}.", self.name));
        }
        let settings = self.context_settings(&roster, context)?;
        let me = node.author();
        if let Some((_, entry)) = self.claim_book.holding(context, key) {
            let (person, machine) = who(&roster, &me);
            return Ok(Step::Answer(ClaimOutcome::Held {
                person,
                machine,
                this_machine: true,
                since: entry.claim.claimed_at,
            }));
        }
        let (records, states) = self.settled(&node, &roster, context, &settings).await?;
        holder.label = holder.label.chars().take(MAX_LABEL_CHARS).collect();
        let take = decide_take(&states, key, &me, &settings);
        // A key that is free isn't this holder's any more. A new claim would look to it like the
        // one it had, while it could have lost the key and had someone else's work done meanwhile.
        if retake && matches!(take, Take::Go | Take::Paused | Take::AtLimit(_)) {
            let reason = retake_loss(&records, &roster, &me, key, states.get(key));
            return Ok(Step::Answer(ClaimOutcome::Gone(reason)));
        }
        match take {
            Take::Go => {
                let now = now_micros();
                let claim = ClaimRecord {
                    context: context.to_string(),
                    key: key.to_string(),
                    claimed_at: now,
                    renewed_at: now,
                    limit: settings.max_per_machine,
                    holder,
                };
                node.write_claim(&claim)
                    .await
                    .map_err(|e| format!("{}: {e:#}", self.name))?;
                let endpoints = roster
                    .endpoints()
                    .into_iter()
                    .filter(|e| *e != self.my_endpoint)
                    .collect();
                Ok(Step::Ask {
                    node,
                    claim,
                    settings,
                    endpoints,
                })
            }
            Take::TakeUp(mut claim) => {
                // It may have gone a while unrenewed if this machine restarted, so it is renewed
                // straight away, as held by whatever is taking it up now.
                claim.holder = holder;
                let (lost_tx, lost) = mpsc::unbounded_channel();
                let id = self.claim_book.add(BookEntry {
                    context: context.to_string(),
                    key: key.to_string(),
                    claim,
                    settings,
                    lost: lost_tx,
                    deciding: None,
                    renew_at: Instant::now(),
                });
                let handle = self.claim_handle(id, context, key, lost)?;
                Ok(Step::Answer(ClaimOutcome::Yours(handle)))
            }
            other => told(other, key, &roster).map(Step::Answer),
        }
    }

    fn claim_handle(
        &self,
        id: u64,
        context: &str,
        key: &str,
        lost: mpsc::UnboundedReceiver<LostReason>,
    ) -> Result<ClaimHandle, String> {
        let worker = self
            .own_tx
            .upgrade()
            .ok_or_else(|| format!("{} isn't open.", self.name))?;
        let settings = self.claim_book.live.get(&id).map(|e| e.settings);
        let lapse_micros = settings
            .unwrap_or_default()
            .lapse_micros(self.shared.claim_slack);
        Ok(ClaimHandle {
            id,
            swarm: self.ns.clone(),
            context: context.to_string(),
            key: key.to_string(),
            lapse_micros,
            lost,
            worker,
        })
    }

    fn context_settings(&self, roster: &Roster, context: &str) -> Result<ContextSettings, String> {
        roster
            .claim_contexts
            .get(context)
            .map(|s| s.normalised())
            .ok_or_else(|| format!("{} has no context called {context}.", self.name))
    }

    async fn settled(
        &self,
        node: &SwarmNode,
        roster: &Roster,
        context: &str,
        settings: &ContextSettings,
    ) -> Result<(Records, BTreeMap<String, KeyState>), String> {
        let records = node
            .claim_records(context)
            .await
            .map_err(|e| format!("{}: {e:#}", self.name))?;
        let people = People::from_roster(roster);
        let states = settle(
            &records,
            &people,
            settings,
            now_micros(),
            self.shared.claim_slack,
        );
        Ok((records, states))
    }

    /// The members have answered, or had their time: everything known decides it.
    pub(super) async fn claim_asked(&mut self, id: u64, answers: Vec<ClaimAnswer>) {
        let Some(entry) = self.claim_book.live.get(&id) else {
            return;
        };
        let (context, key) = (entry.context.clone(), entry.key.clone());
        let decided = self.decide_asked(answers, &context, &key).await;
        let Some(deciding) = self
            .claim_book
            .live
            .get_mut(&id)
            .and_then(|e| e.deciding.take())
        else {
            return;
        };
        let Deciding { reply, lost } = deciding;
        let answer = match decided {
            Ok((Take::TakeUp(_), _)) => self
                .claim_handle(id, &context, &key, lost)
                .map(ClaimOutcome::Yours),
            Ok((take, roster)) => told(take, &key, &roster),
            Err(text) => Err(text),
        };
        if !matches!(answer, Ok(ClaimOutcome::Yours(_))) {
            self.claim_book.live.remove(&id);
            if let Some(node) = &self.node {
                // If this fails, the claim lapses by itself.
                let _ = node.delete_claim(&context, &key).await;
            }
        }
        self.claims_refresh().await;
        let _ = reply.send(answer);
    }

    async fn decide_asked(
        &self,
        answers: Vec<ClaimAnswer>,
        context: &str,
        key: &str,
    ) -> Result<(Take, Roster), String> {
        let node = self.live_node()?;
        let roster = self.fresh_roster(&node).await?;
        node.take_answers(answers, &roster);
        let settings = self.context_settings(&roster, context)?;
        let (_, states) = self.settled(&node, &roster, context, &settings).await?;
        let take = decide_take(&states, key, &node.author(), &settings);
        Ok((take, roster))
    }

    /// Reads every context again: tells holders about any claim that stopped being theirs, and
    /// works out what the view shows. The roster is read afresh, since judging this machine's
    /// claims by an older one could lose them for nothing.
    pub(super) async fn claims_refresh(&mut self) {
        self.claim_book.settle_at = None;
        self.claim_book.view_at = Instant::now() + VIEW_EVERY;
        let Ok(node) = self.live_node() else {
            return;
        };
        // On a failed read the view stays as it was, and the next refresh tries again.
        let Ok(Some(roster)) = node.roster().await else {
            return;
        };
        let me = self.person.id();
        let mut views = Vec::with_capacity(roster.claim_contexts.len());
        for (name, settings) in &roster.claim_contexts {
            let settings = settings.normalised();
            // One context that can't be read leaves that context as it was, not all the others.
            let Ok((records, states)) = self.settled(&node, &roster, name, &settings).await else {
                views.extend(self.claims_view.iter().find(|c| c.name == *name).cloned());
                continue;
            };
            self.notice_losses(&node, &roster, name, settings, &records, &states)
                .await;
            views.push(context_view(name, settings, &states, &roster, &me));
        }
        self.claims_view = views;
    }

    async fn notice_losses(
        &mut self,
        node: &SwarmNode,
        roster: &Roster,
        context: &str,
        settings: ContextSettings,
        records: &Records,
        states: &BTreeMap<String, KeyState>,
    ) {
        let me = node.author();
        let people = People::from_roster(roster);
        let mut lost = Vec::new();
        for (id, entry) in &mut self.claim_book.live {
            if entry.context != context {
                continue;
            }
            if entry.settings != settings {
                // A shorter lapse mustn't catch this claim waiting out the longer renewal.
                entry.settings = settings;
                let sooner = Instant::now() + renew_every(&settings);
                entry.renew_at = entry.renew_at.min(sooner);
            }
            if entry.deciding.is_some() {
                continue;
            }
            let state = states.get(&entry.key);
            if !matches!(state, Some(KeyState::Held { author, .. }) if *author == me) {
                let reason = loss_reason(records, &people, roster, &me, &entry.claim, state);
                lost.push((*id, reason));
            }
        }
        for (id, reason) in lost {
            self.claim_lost(node, id, reason).await;
        }
    }

    async fn claim_lost(&mut self, node: &SwarmNode, id: u64, reason: LostReason) {
        if let Some(entry) = self.claim_book.live.remove(&id) {
            // Whoever held it may have gone, which is fine.
            let _ = entry.lost.send(reason);
            // If this fails, the claim lapses by itself.
            let _ = node.delete_claim(&entry.context, &entry.key).await;
        }
    }

    /// A claim changed somewhere: read them again shortly, without waking the folder.
    pub(super) fn claims_changed(&mut self) {
        let settle_at = &mut self.claim_book.settle_at;
        settle_at.get_or_insert_with(|| Instant::now() + SETTLE);
    }

    pub(super) fn next_claims_at(&self) -> Option<Instant> {
        self.live_node().ok()?;
        Some(self.claim_book.next_at())
    }

    /// Does what has come due: renewals, then any reading that is waiting.
    pub(super) async fn claims_due(&mut self) {
        let Ok(node) = self.live_node() else {
            return;
        };
        let now = Instant::now();
        let due: Vec<u64> = self
            .claim_book
            .live
            .iter()
            .filter(|(_, e)| e.renew_at <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            self.renew_claim(&node, id).await;
        }
        let book = &self.claim_book;
        if book.view_at <= now || book.settle_at.is_some_and(|at| at <= now) {
            self.claims_refresh().await;
        }
    }

    /// Wall-clock time decides whether the claim has lapsed, as `Instant` doesn't reliably count
    /// while a machine sleeps.
    async fn renew_claim(&mut self, node: &SwarmNode, id: u64) {
        let slack = self.shared.claim_slack;
        let Some(entry) = self.claim_book.live.get_mut(&id) else {
            return;
        };
        let now = now_micros();
        let lapse = entry.settings.lapse_micros(slack);
        if entry.deciding.is_none() && now.saturating_sub(entry.claim.renewed_at) > lapse {
            self.claim_lost(node, id, LostReason::Lapsed).await;
            return;
        }
        entry.renew_at = Instant::now() + renew_every(&entry.settings);
        let mut claim = entry.claim.clone();
        claim.renewed_at = now;
        match node.write_claim(&claim).await {
            Ok(()) => entry.claim = claim,
            Err(e) => self.shared.failed(format!("{}: {e:#}", self.name)),
        }
    }

    pub(super) fn claim_drop(&mut self, id: u64) {
        self.claim_book.live.remove(&id);
    }

    pub(super) async fn claim_finish(
        &mut self,
        id: u64,
        done: bool,
        reply: oneshot::Sender<Result<(), String>>,
    ) {
        let finished = self.try_finish(id, done).await;
        self.claims_refresh().await;
        let _ = reply.send(finished);
    }

    async fn try_finish(&mut self, id: u64, done: bool) -> Result<(), String> {
        let node = self.live_node().map_err(|_| NO_LONGER_YOURS)?;
        // A loss not noticed yet counts, or this could mark done a key someone else now holds.
        self.claims_refresh().await;
        let entry = self.claim_book.live.get(&id).ok_or(NO_LONGER_YOURS)?;
        let failed = |e: anyhow::Error| format!("{}: {e:#}", self.name);
        if done {
            let record = DoneRecord {
                context: entry.context.clone(),
                key: entry.key.clone(),
                at: now_micros(),
            };
            node.write_done(&record).await.map_err(failed)?;
        }
        node.delete_claim(&entry.context, &entry.key)
            .await
            .map_err(failed)?;
        self.claim_book.live.remove(&id);
        Ok(())
    }

    pub(super) async fn claims_list(&mut self, context: &str) -> Result<ContextView, String> {
        check_context(context)?;
        self.live_node()?;
        self.claims_refresh().await;
        let found = self.claims_view.iter().find(|c| c.name == context);
        found
            .cloned()
            .ok_or_else(|| format!("{} has no context called {context}.", self.name))
    }

    pub(super) async fn claim_release(
        &mut self,
        context: &str,
        key: &str,
        author: &str,
    ) -> Result<(), String> {
        check_claim(context, key)?;
        let node = self.live_node()?;
        let roster = self.fresh_roster(&node).await?;
        let me = node.author();
        let held_here = self
            .claim_book
            .holding(context, key)
            .filter(|(_, e)| author == me && e.deciding.is_none())
            .map(|(id, _)| id);
        if let Some(id) = held_here {
            let person = who(&roster, &me).0;
            self.claim_lost(&node, id, LostReason::Released { person })
                .await;
        } else {
            if !People::from_roster(&roster).may_release(&me, author) {
                return Err("Only the machine's person or an owner can release this claim.".into());
            }
            let settings = self.context_settings(&roster, context)?;
            let (_, states) = self.settled(&node, &roster, context, &settings).await?;
            let claimed_at = match states.get(key) {
                Some(KeyState::Held { author: a, claim }) if a == author => claim.claimed_at,
                _ => return Err("That machine no longer holds this claim.".to_string()),
            };
            let release = ReleaseRecord {
                context: context.to_string(),
                key: key.to_string(),
                target: author.to_string(),
                claimed_at,
                at: now_micros(),
            };
            node.write_release(&release)
                .await
                .map_err(|e| format!("{}: {e:#}", self.name))?;
        }
        self.claims_refresh().await;
        Ok(())
    }

    pub(super) async fn claim_reopen(&mut self, context: &str, key: &str) -> Result<(), String> {
        check_claim(context, key)?;
        let node = self.live_node()?;
        let roster = self.fresh_roster(&node).await?;
        if !People::from_roster(&roster).writes(&node.author()) {
            return Err(format!(
                "Only writers can reopen a finished key in {}.",
                self.name
            ));
        }
        let reopen = ReopenRecord {
            context: context.to_string(),
            key: key.to_string(),
            at: now_micros(),
        };
        node.write_reopen(&reopen)
            .await
            .map_err(|e| format!("{}: {e:#}", self.name))?;
        self.claims_refresh().await;
        Ok(())
    }

    pub(super) async fn claim_context_set(
        &mut self,
        name: String,
        settings: ContextSettings,
    ) -> Result<(), String> {
        check_context(&name)?;
        let only = format!("Only owners can change {}'s claim contexts.", self.name);
        let change = crate::roster::Change::SetClaimContext { name, settings };
        self.owner_change(change, only).await?;
        // A take that doesn't name its swarm finds the context through the view.
        self.claims_refresh().await;
        Ok(())
    }

    /// Makes a claim here look as a machine waking from sleep would find it.
    pub(super) fn claim_age_for_tests(&mut self, context: &str, key: &str, by_secs: u64) {
        let entry = self
            .claim_book
            .live
            .values_mut()
            .find(|e| e.context == context && e.key == key);
        if let Some(entry) = entry {
            let by = by_secs.saturating_mul(1_000_000);
            entry.claim.renewed_at = entry.claim.renewed_at.saturating_sub(by);
            entry.renew_at = Instant::now();
        }
    }

    /// Deletes what this machine no longer needs in each context.
    pub(super) async fn prune_claims(&self, node: &SwarmNode) {
        let Some(roster) = self.roster.as_ref().filter(|_| node.can_write()) else {
            return;
        };
        for (name, settings) in &roster.claim_contexts {
            let settings = settings.normalised();
            // What can't be deleted now is tried again next time.
            let _ = node
                .prune_claims(name, &settings, now_micros(), self.shared.claim_slack)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_get_the_three_seconds_the_spec_gives_them() {
        assert_eq!(ASK_FOR, Duration::from_secs(3));
    }
}
