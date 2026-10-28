//! Your machines, as the service runs them: this machine's device endpoint, what your machines
//! share (`mine.json`), the link codes it has made, and the swarms offered to it. Requests from
//! machines that aren't yours are refused here, whatever they ask.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_docs::{Author, Capability, NamespaceId};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{
    DeviceView, Identity, Joining, KeyState, MachinesView, Opened, PERIODIC, PersonKeyFn,
    ServiceEvent, Shared, load_json, lock, now_secs, open_joined, random32, save_json,
};
use crate::device::{DeviceHandler, DeviceNode};
use crate::ids::{PersonId, PersonKey, RekeyCert, chain_ends, from_hex, to_hex};
use crate::invites::{Unusable, namespace_secret};
use crate::mine::{LINKS_FILE, LinkCode, Links, Mine};
use crate::node::now_micros;
use crate::roster::{Change, Machine, SignedChange};
use crate::wire::{MineReply, MineRequest, Offer};

const OFFERS_FILE: &str = "offers.json";
/// Your machines still owed your new personal key.
const KEY_OWED_FILE: &str = "key_owed.json";
/// How long a machine taking an offer waits on each of your other machines.
const ACCEPT_WAIT: Duration = Duration::from_secs(25);

pub(super) const NOT_YOURS: &str = "This isn't one of your machines.";
pub(super) const CUT_OFF: &str = "This machine was removed from your machines.";
pub(super) const IN_SWARMS: &str = "This machine is already in swarms as someone. Leave them before \
                                    linking it to another of your machines.";
pub(super) const NOT_A_LINK: &str = "That isn't a link code. It starts with hydra-link:";

fn link_text(u: Unusable) -> &'static str {
    match u {
        Unusable::Used => "This link code has already been used.",
        Unusable::Expired => "This link code has expired.",
        Unusable::Cancelled => "This link code was cancelled.",
    }
}

/// The swarms offered to this machine, and the ones it said no to, in `offers.json`.
#[derive(Serialize, Deserialize, Default)]
struct Offers {
    #[serde(default)]
    offers: Vec<Offer>,
    #[serde(default)]
    declined: Vec<String>,
}

/// What the service's main task must do for this module.
pub(super) enum Up {
    /// Another machine of yours renamed you.
    Named(String),
    /// This machine was linked and now has this personal key.
    Person([u8; 32]),
    /// Another machine of yours is joining swarm `ns` through this one.
    Joining {
        ns: String,
        change: Box<SignedChange>,
        reply: oneshot::Sender<MineReply>,
    },
}

/// The answer to a machine of yours asking about a swarm that isn't open here.
pub(super) fn not_here() -> MineReply {
    refused("Not in that swarm here.")
}

pub(super) struct Machines {
    shared: Arc<Shared>,
    /// This machine's device id.
    me: String,
    node: OnceLock<DeviceNode>,
    mine: Mutex<Mine>,
    links: Links,
    offers: Mutex<Offers>,
    /// When each of your machines was last heard from.
    heard: Mutex<HashMap<String, Instant>>,
    /// The personal key, as the service holds it now.
    person: Mutex<[u8; 32]>,
    set_person: Mutex<PersonKeyFn>,
    up: mpsc::UnboundedSender<Up>,
    /// Your machines still owed your new personal key, by device id.
    key_owed: Mutex<Vec<String>>,
    /// Another of your machines said this one was removed: it stops asking them anything.
    cut_off: std::sync::atomic::AtomicBool,
}

struct Handler(Weak<Machines>);

impl DeviceHandler for Handler {
    fn answer(&self, from: String, req: MineRequest) -> BoxFuture<'static, MineReply> {
        let machines = self.0.upgrade();
        async move {
            match machines {
                Some(m) => {
                    let reply = m.answer(from.clone(), req).await;
                    // A machine of yours that was away gets your new key as soon as it asks.
                    if lock(&m.key_owed).contains(&from) {
                        tokio::spawn(async move { m.hand_key_to(&from).await });
                    }
                    reply
                }
                None => MineReply::Refused {
                    reason: "This machine is closing.".into(),
                },
            }
        }
        .boxed()
    }
}

impl Machines {
    /// Starts this machine's device endpoint and takes in `mine.json`, adding this machine to
    /// it. Your name in it, once set, wins over `identity`'s: the main task adopts it.
    pub(super) async fn start(
        shared: Arc<Shared>,
        device_key: [u8; 32],
        identity: &Identity,
        set_person: PersonKeyFn,
        up: mpsc::UnboundedSender<Up>,
    ) -> anyhow::Result<Arc<Machines>> {
        let me = SecretKey::from_bytes(&device_key).public().to_string();
        let mut mine = Mine::load(&shared.root);
        let now = now_micros();
        if mine.name.is_empty() {
            mine.set_name(&identity.name, now);
        }
        let named_here = mine
            .devices
            .iter()
            .any(|d| d.id == me && d.name == identity.machine_name);
        if !named_here {
            mine.set_device_name(&me, &identity.machine_name, now);
        }
        mine.save(&shared.root)?;
        let offers: Offers = load_json(&shared.root.join(OFFERS_FILE));
        let machines = Arc::new(Machines {
            links: Links::at(shared.root.join(LINKS_FILE)),
            shared,
            me,
            node: OnceLock::new(),
            mine: Mutex::new(mine),
            offers: Mutex::new(offers),
            heard: Mutex::default(),
            person: Mutex::new(identity.person),
            set_person: Mutex::new(set_person),
            up,
            key_owed: Mutex::new(Vec::new()),
            cut_off: std::sync::atomic::AtomicBool::new(false),
        });
        *lock(&machines.key_owed) = load_json(&machines.shared.root.join(KEY_OWED_FILE));
        machines.share_keys();
        let handler = Arc::new(Handler(Arc::downgrade(&machines)));
        let node = DeviceNode::start(device_key, machines.shared.relays, handler).await?;
        if let Some(book) = &machines.shared.direct {
            book.add(node.addr_for_tests());
        }
        let _ = machines.node.set(node);
        Ok(machines)
    }

    pub(super) fn name(&self) -> String {
        lock(&self.mine).name.clone()
    }

    fn node(&self) -> &DeviceNode {
        self.node.get().expect("set at start")
    }

    /// Lets the device endpoint reach every machine in the test book; nothing without one.
    fn remember_peers(&self) {
        if let Some(book) = &self.shared.direct {
            for addr in book.all() {
                self.node().remember_addr(addr);
            }
        }
    }

    fn heard_from(&self, id: &str) {
        lock(&self.heard).insert(id.to_string(), Instant::now());
    }

    pub(super) fn view(&self) -> MachinesView {
        let mine = lock(&self.mine).clone();
        let heard = lock(&self.heard).clone();
        let devices = mine
            .devices
            .iter()
            .filter(|d| mine.knows(&d.id))
            .map(|d| {
                let this_machine = d.id == self.me;
                let recent = heard.get(&d.id).is_some_and(|t| t.elapsed() < PERIODIC);
                DeviceView {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    online: this_machine || recent,
                    this_machine,
                }
            })
            .collect();
        MachinesView {
            this: self.me.clone(),
            devices,
            links: self.links.pending(now_secs()),
            offers: self.pending_offers(),
        }
    }

    pub(super) fn publish(&self) {
        self.shared.emit(ServiceEvent::Machines(self.view()));
    }

    fn save_mine(&self, mine: &Mine) {
        if let Err(e) = mine.save(&self.shared.root) {
            self.shared
                .failed(format!("Your machines couldn't be saved: {e:#}"));
        }
    }

    fn save_offers(&self, offers: &Offers) {
        if let Err(e) = save_json(&self.shared.root.join(OFFERS_FILE), offers) {
            self.shared
                .failed(format!("The swarms offered here couldn't be saved: {e:#}"));
        }
    }

    pub(super) fn link_code(&self) -> Result<String, String> {
        let (_, token) = self
            .links
            .create((), now_secs())
            .map_err(|e| format!("Couldn't make a link code: {e}"))?;
        let code = LinkCode {
            host: self.me.clone(),
            relay: self.node().relay(),
            token,
        };
        self.publish();
        Ok(code.encode())
    }

    /// You renamed yourself or this machine here: your other machines take it at the next sync.
    pub(super) fn set_identity(&self, name: &str, machine_name: &str) {
        {
            let mut mine = lock(&self.mine);
            let now = now_micros();
            if mine.name != name {
                mine.set_name(name, now);
            }
            mine.set_device_name(&self.me, machine_name, now);
            self.save_mine(&mine);
        }
        self.publish();
    }

    /// Notes that this machine is in a swarm as `endpoint`, so a removal of it from another of
    /// yours knows what to take out.
    pub(super) fn note_endpoint(&self, endpoint: &str) {
        let mut mine = lock(&self.mine);
        let known = mine
            .devices
            .iter()
            .any(|d| d.id == self.me && d.endpoints.iter().any(|e| e == endpoint));
        if !known {
            mine.add_endpoint(&self.me, endpoint);
            self.save_mine(&mine);
        }
    }

    /// Gives the swarms what they need to move this person to their current key.
    fn share_keys(&self) {
        let mine = lock(&self.mine);
        let gone = mine
            .removed
            .iter()
            .flat_map(|d| mine.endpoints_of(d))
            .collect();
        *lock(&self.shared.keys) = KeyState {
            chain: mine.chain.clone(),
            gone,
        };
    }

    fn cut_off(&self) {
        if !self.cut_off.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.shared.toast("", CUT_OFF.to_string());
        }
    }

    /// Removes your machine `device`: you get a new personal key, vouched for by the old one,
    /// which goes to your other machines; every swarm moves you to it and takes that machine
    /// out.
    pub(super) async fn remove_machine(&self, device: &str) -> Result<(), String> {
        if device == self.me {
            return Err("You can't remove the machine you're on.".into());
        }
        let name = {
            let mine = lock(&self.mine);
            if !mine.knows(device) {
                return Err("That isn't one of your machines.".into());
            }
            mine.devices
                .iter()
                .find(|d| d.id == device)
                .map(|d| d.name.clone())
                .unwrap_or_default()
        };
        let old = PersonKey::from_bytes(*lock(&self.person));
        let new = PersonKey::generate();
        self.store_person(new.to_bytes()).await?;
        let others: Vec<String> = {
            let mut mine = lock(&self.mine);
            mine.chain.push(RekeyCert::new(&old, new.id()));
            mine.remove(device);
            self.save_mine(&mine);
            mine.linked(&self.me).iter().map(|d| d.id.clone()).collect()
        };
        {
            let mut owed = lock(&self.key_owed);
            *owed = others;
            self.save_key_owed(&owed);
        }
        self.share_keys();
        let _ = self.up.send(Up::Person(new.to_bytes()));
        self.shared
            .toast("", format!("{name} was removed from your machines."));
        self.publish();
        Ok(())
    }

    fn save_key_owed(&self, owed: &[String]) {
        if let Err(e) = save_json(&self.shared.root.join(KEY_OWED_FILE), &owed) {
            self.shared
                .failed(format!("Your machines couldn't be saved: {e:#}"));
        }
    }

    /// Gives your machine `id` your new personal key, if it is still owed it.
    async fn hand_key_to(&self, id: &str) {
        if !lock(&self.key_owed).iter().any(|d| d == id) {
            return;
        }
        let req = MineRequest::NewKey {
            secret: to_hex(&*lock(&self.person)),
            chain: lock(&self.mine).chain.clone(),
        };
        if let Ok(MineReply::Ok) = self.node().ask(id, &req).await {
            let mut owed = lock(&self.key_owed);
            owed.retain(|d| d != id);
            self.save_key_owed(&owed);
        }
    }

    /// Takes your new personal key from another of your machines, when the chain it sends
    /// vouches for it from the key this machine holds.
    async fn on_new_key(&self, secret: &str, chain: Vec<RekeyCert>) -> MineReply {
        let Some(key) = from_hex(secret).and_then(|v| <[u8; 32]>::try_from(v).ok()) else {
            return refused("That key isn't valid.");
        };
        let current = PersonKey::from_bytes(*lock(&self.person)).id();
        let new = PersonKey::from_bytes(key).id();
        if new == current {
            return MineReply::Ok;
        }
        let from_here = chain.iter().position(|c| c.from == current);
        if from_here.and_then(|i| chain_ends(&chain[i..], &current)) != Some(new) {
            return refused("That key doesn't follow from the one this machine holds.");
        }
        if let Err(text) = self.store_person(key).await {
            return refused(&text);
        }
        {
            let mut mine = lock(&self.mine);
            let theirs = Mine {
                chain,
                ..Mine::default()
            };
            mine.merge(&theirs);
            self.save_mine(&mine);
        }
        self.share_keys();
        let _ = self.up.send(Up::Person(key));
        self.publish();
        MineReply::Ok
    }

    pub(super) fn cancel_link(&self, id: &str) {
        if let Err(e) = self.links.cancel(id) {
            self.shared
                .failed(format!("Couldn't cancel that link code: {e}"));
        }
        self.publish();
    }

    /// Whether this machine may link to another: in no swarm, and no other machine of yours.
    fn may_link(&self) -> bool {
        self.shared.registry.entries().is_empty() && lock(&self.mine).linked(&self.me).is_empty()
    }

    /// Links this machine to the one that made `code`, taking its person: the main task hears
    /// the new key through `Up::Person` and the name through `Up::Named`.
    pub(super) async fn link(self: Arc<Self>, code: String, machine_name: String) {
        match self.try_link(&code, machine_name).await {
            // Your other machines hear of this one, and it of them.
            Ok(()) => self.sync_all().await,
            Err(text) => self.shared.failed(text),
        }
    }

    async fn try_link(&self, code: &str, machine_name: String) -> Result<(), String> {
        let code = LinkCode::decode(code).ok_or(NOT_A_LINK)?;
        // Checked before the code is spent.
        if !self.may_link() {
            return Err(IN_SWARMS.into());
        }
        self.remember_peers();
        if self.shared.direct.is_none() {
            let id: EndpointId = code.host.parse().map_err(|_| NOT_A_LINK)?;
            let mut addr = EndpointAddr::new(id);
            if let Some(url) = code
                .relay
                .as_deref()
                .and_then(|r| r.parse::<RelayUrl>().ok())
            {
                addr = addr.with_relay_url(url);
            }
            self.node().remember_addr(addr);
        }
        let req = MineRequest::Link {
            token: code.token.clone(),
            name: machine_name.clone(),
        };
        let reply = self
            .node()
            .ask(&code.host, &req)
            .await
            .map_err(|e| format!("Couldn't reach your other machine: {e:#}"))?;
        let (person, theirs, offers) = match reply {
            MineReply::Linked {
                person,
                mine,
                offers,
            } => (person, mine, offers),
            MineReply::Refused { reason } => return Err(reason),
            _ => return Err("Your other machine gave a reply that doesn't fit.".into()),
        };
        let person: [u8; 32] = from_hex(&person)
            .and_then(|v| v.try_into().ok())
            .ok_or("Your other machine sent a key that isn't valid.")?;
        self.store_person(person).await?;
        self.heard_from(&code.host);
        // This machine had nothing of its own to keep: it takes on yours whole.
        let mut mine = theirs;
        mine.set_device_name(&self.me, &machine_name, now_micros());
        self.save_mine(&mine);
        let name = mine.name.clone();
        *lock(&self.mine) = mine;
        {
            let mut held = lock(&self.offers);
            held.offers = offers;
            self.save_offers(&held);
        }
        let _ = self.up.send(Up::Person(person));
        let _ = self.up.send(Up::Named(name));
        self.publish();
        Ok(())
    }

    /// Keeps `person` as the personal key, in the keyring first.
    async fn store_person(&self, person: [u8; 32]) -> Result<(), String> {
        let stored = {
            let set = lock(&self.set_person);
            (set)(person)
        };
        stored.map_err(|e| format!("Couldn't keep your key on this machine: {e}"))?;
        *lock(&self.person) = person;
        Ok(())
    }

    /// Asks each of your other machines what it holds, and takes it in.
    /// Asks each of your other machines what it holds, and takes it in, and offers each the
    /// swarms on this machine (it ignores the ones it has, or said no to).
    pub(super) async fn sync_all(self: Arc<Self>) {
        let (devices, ours) = {
            let mine = lock(&self.mine);
            let ids: Vec<String> = mine.linked(&self.me).iter().map(|d| d.id.clone()).collect();
            (ids, mine.clone())
        };
        if devices.is_empty() || self.cut_off.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        self.remember_peers();
        let offers = self.offers_here();
        for id in devices {
            let req = MineRequest::Sync { mine: ours.clone() };
            let mine = match self.node().ask(&id, &req).await {
                Ok(MineReply::Synced { mine }) => mine,
                Ok(MineReply::Refused { reason }) if reason == CUT_OFF => {
                    self.cut_off();
                    return;
                }
                _ => continue,
            };
            self.heard_from(&id);
            self.take_in(&mine);
            self.hand_key_to(&id).await;
            for offer in &offers {
                let req = MineRequest::Offer {
                    offer: offer.clone(),
                };
                let _ = self.node().ask(&id, &req).await;
            }
        }
        self.publish();
    }

    /// The swarms offered here that this machine isn't in.
    fn pending_offers(&self) -> Vec<Offer> {
        lock(&self.offers)
            .offers
            .iter()
            .filter(|o| !self.shared.registry.contains(&o.ns))
            .cloned()
            .collect()
    }

    fn on_offer(&self, from: &str, offer: Offer) -> MineReply {
        let fresh = {
            let mut held = lock(&self.offers);
            let known = self.shared.registry.contains(&offer.ns)
                || held.declined.contains(&offer.ns)
                || held.offers.iter().any(|o| o.ns == offer.ns);
            if !known {
                held.offers.push(offer.clone());
                self.save_offers(&held);
            }
            !known
        };
        if fresh {
            let device = lock(&self.mine)
                .devices
                .iter()
                .find(|d| d.id == from)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "Another machine of yours".into());
            self.shared
                .toast(&offer.ns, format!("{device} offered you {}.", offer.name));
            self.publish();
        }
        MineReply::Ok
    }

    pub(super) fn decline(&self, ns: &str) {
        {
            let mut held = lock(&self.offers);
            held.offers.retain(|o| o.ns != ns);
            if !held.declined.iter().any(|d| d == ns) {
                held.declined.push(ns.to_string());
            }
            self.save_offers(&held);
        }
        self.publish();
    }

    /// A machine of yours joining swarm `ns`: checks the change is this person's `AddMachine`,
    /// then has the swarm's task deal with it.
    async fn on_joining(&self, ns: String, change: SignedChange) -> MineReply {
        let me = PersonKey::from_bytes(*lock(&self.person)).id();
        let ours = matches!(&change.change, Change::AddMachine { person, .. } if *person == me);
        if !ours || change.by != me || !change.verify(&ns) {
            return refused("That isn't a machine of yours joining.");
        }
        if !self.shared.registry.contains(&ns) {
            return not_here();
        }
        let (reply, answer) = oneshot::channel();
        let change = Box::new(change);
        if self.up.send(Up::Joining { ns, change, reply }).is_err() {
            return not_here();
        }
        answer.await.unwrap_or_else(|_| not_here())
    }

    /// Joins the offered swarm `ns` into `folder` through any machine of yours in it.
    pub(super) async fn accept(
        self: Arc<Self>,
        identity: Identity,
        ns: String,
        folder: PathBuf,
    ) -> Option<Opened> {
        match self.try_accept(&identity, &ns, folder).await {
            Ok((opened, name)) => {
                {
                    let mut held = lock(&self.offers);
                    held.offers.retain(|o| o.ns != ns);
                    self.save_offers(&held);
                }
                self.shared.toast(&ns, format!("Joined {name}."));
                self.publish();
                Some(opened)
            }
            Err(text) => {
                self.shared.failed(text);
                None
            }
        }
    }

    async fn try_accept(
        &self,
        identity: &Identity,
        ns: &str,
        folder: PathBuf,
    ) -> Result<(Opened, String), String> {
        let offer = lock(&self.offers)
            .offers
            .iter()
            .find(|o| o.ns == ns)
            .cloned()
            .ok_or("That swarm isn't offered to this machine.")?;
        if self.shared.registry.contains(ns) {
            return Err(format!("You're already in {}.", offer.name));
        }
        if std::fs::read_dir(&folder).is_ok_and(|mut d| d.next().is_some()) {
            return Err("A swarm can only be joined into an empty or new folder.".into());
        }
        let failed = |e: anyhow::Error| format!("{}: {e:#}", offer.name);
        let key_id = format!("join-{}", to_hex(&random32().map_err(failed)?[..8]));
        let machine_key = self.shared.machine_key(&key_id).await.map_err(failed)?;
        let author = Author::from_bytes(&random32().map_err(failed)?);
        let person = PersonKey::from_bytes(*lock(&self.person));
        let machine = Machine {
            endpoint: SecretKey::from_bytes(&machine_key).public().to_string(),
            name: identity.machine_name.clone(),
            author: author.id().to_string(),
        };
        let change = Change::AddMachine {
            person: person.id(),
            machine,
        };
        let change = SignedChange::sign(ns, change, &person, now_micros());
        let (write_secret, host) = self.join_through_any(ns, &change).await.ok_or_else(|| {
            format!(
                "None of your machines in {} is running. Try again when one is.",
                offer.name
            )
        })?;
        let ns_id: NamespaceId = ns.parse().map_err(|_| "That swarm's id isn't valid.")?;
        let cap = match write_secret.as_deref() {
            Some(hex) => Capability::Write(
                namespace_secret(hex, ns_id)
                    .ok_or("Your machine sent write access that isn't valid.")?,
            ),
            None => Capability::Read(ns_id),
        };
        let anchor: PersonId = offer
            .anchor
            .parse()
            .map_err(|_| "That swarm's anchor isn't valid.")?;
        let host_id: EndpointId = host.parse().map_err(|_| "Your machine's id isn't valid.")?;
        let host = match &self.shared.direct {
            Some(book) => book
                .get(&host)
                .unwrap_or_else(|| EndpointAddr::new(host_id)),
            None => EndpointAddr::new(host_id),
        };
        let joining = Joining {
            anchor,
            name: offer.name.clone(),
            folder,
            key_id,
            machine_key,
            cap,
            author,
            host,
        };
        let opened = open_joined(&self.shared, identity, joining)
            .await
            .map_err(failed)?;
        Ok((opened, offer.name))
    }

    /// Asks every machine of yours at once to let this one into `ns`; the first to manage it
    /// answers with write access (for a writer) and its endpoint in the swarm.
    async fn join_through_any(
        &self,
        ns: &str,
        change: &SignedChange,
    ) -> Option<(Option<String>, String)> {
        let devices: Vec<String> = lock(&self.mine)
            .linked(&self.me)
            .iter()
            .map(|d| d.id.clone())
            .collect();
        self.remember_peers();
        let req = MineRequest::Joining {
            ns: ns.to_string(),
            change: change.clone(),
        };
        let node = self.node();
        let mut asks: FuturesUnordered<_> = devices
            .iter()
            .map(|id| node.ask_within(id, &req, ACCEPT_WAIT))
            .collect();
        while let Some(answer) = asks.next().await {
            if let Ok(MineReply::Joined { write_secret, host }) = answer {
                return Some((write_secret, host));
            }
        }
        None
    }

    /// Merges what another of your machines holds; a new name goes to the main task.
    fn take_in(&self, theirs: &Mine) {
        let renamed = {
            let mut mine = lock(&self.mine);
            let before = mine.name.clone();
            if !mine.merge(theirs) {
                return;
            }
            self.save_mine(&mine);
            (mine.name != before).then(|| mine.name.clone())
        };
        self.share_keys();
        if let Some(name) = renamed {
            let _ = self.up.send(Up::Named(name));
        }
    }

    async fn answer(&self, from: String, req: MineRequest) -> MineReply {
        if let MineRequest::Link { token, name } = req {
            return self.on_link(from, &token, &name);
        }
        let removed = lock(&self.mine).removed.contains(&from);
        if removed {
            return refused(CUT_OFF);
        }
        if !lock(&self.mine).knows(&from) {
            return refused(NOT_YOURS);
        }
        self.heard_from(&from);
        match req {
            MineRequest::Sync { mine } => {
                self.take_in(&mine);
                self.publish();
                MineReply::Synced {
                    mine: lock(&self.mine).clone(),
                }
            }
            MineRequest::Offer { offer } => self.on_offer(&from, offer),
            MineRequest::Joining { ns, change } => self.on_joining(ns, change).await,
            MineRequest::NewKey { secret, chain } => self.on_new_key(&secret, chain).await,
            MineRequest::Link { .. } => refused("This machine is yours already."),
        }
    }

    fn on_link(&self, from: String, token: &str, name: &str) -> MineReply {
        if !crate::roster::name_fits(name) {
            return refused("Your machine's name can't be blank or too long.");
        }
        match self.links.spend(token, now_secs()) {
            Ok(Ok(())) => {}
            Ok(Err(u)) => return refused(link_text(u)),
            Err(e) => return refused(&format!("Something went wrong here: {e}")),
        }
        let mine = {
            let mut mine = lock(&self.mine);
            mine.set_device_name(&from, name, now_micros());
            self.save_mine(&mine);
            mine.clone()
        };
        self.heard_from(&from);
        self.shared
            .toast("", format!("{name} is now one of your machines."));
        self.publish();
        MineReply::Linked {
            person: to_hex(&*lock(&self.person)),
            mine,
            offers: self.offers_here(),
        }
    }

    /// Every swarm on this machine, as offered to another of yours.
    fn offers_here(&self) -> Vec<Offer> {
        let entries = self.shared.registry.entries();
        self.shared
            .views()
            .into_iter()
            .filter(|v| !v.removed)
            .filter_map(|v| {
                let entry = entries.iter().find(|e| e.ns == v.id && !e.leaving)?;
                Some(Offer {
                    ns: v.id.clone(),
                    anchor: entry.anchor.clone(),
                    name: v.name.clone(),
                    role: v.my_role,
                    from: self.me.clone(),
                })
            })
            .collect()
    }

    pub(super) async fn shutdown(&self) {
        if let Some(node) = self.node.get() {
            node.shutdown().await;
        }
    }
}

fn refused(reason: &str) -> MineReply {
    MineReply::Refused {
        reason: reason.to_string(),
    }
}
