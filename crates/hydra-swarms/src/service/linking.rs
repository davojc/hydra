//! Your machines, as the service runs them: this machine's device endpoint, what your machines
//! share (`mine.json`), the link codes it has made, and the swarms offered to it. Requests from
//! machines that aren't yours are refused here, whatever they ask.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use futures::FutureExt;
use futures::future::BoxFuture;
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{
    DeviceView, Identity, MachinesView, PERIODIC, PersonKeyFn, ServiceEvent, Shared, load_json,
    lock, now_secs, save_json,
};
use crate::device::{DeviceHandler, DeviceNode};
use crate::ids::{from_hex, to_hex};
use crate::invites::Unusable;
use crate::mine::{LINKS_FILE, LinkCode, Links, Mine};
use crate::node::now_micros;
use crate::wire::{MineReply, MineRequest, Offer};

const OFFERS_FILE: &str = "offers.json";

pub(super) const NOT_YOURS: &str = "This isn't one of your machines.";
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
}

struct Handler(Weak<Machines>);

impl DeviceHandler for Handler {
    fn answer(&self, from: String, req: MineRequest) -> BoxFuture<'static, MineReply> {
        let machines = self.0.upgrade();
        async move {
            match machines {
                Some(m) => m.answer(from, req).await,
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
        });
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
            offers: lock(&self.offers).offers.clone(),
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
        if let Err(text) = self.try_link(&code, machine_name).await {
            self.shared.failed(text);
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
    pub(super) async fn sync_all(self: Arc<Self>) {
        let (devices, ours) = {
            let mine = lock(&self.mine);
            let ids: Vec<String> = mine.linked(&self.me).iter().map(|d| d.id.clone()).collect();
            (ids, mine.clone())
        };
        if devices.is_empty() {
            return;
        }
        self.remember_peers();
        for id in devices {
            let req = MineRequest::Sync { mine: ours.clone() };
            if let Ok(MineReply::Synced { mine }) = self.node().ask(&id, &req).await {
                self.heard_from(&id);
                self.take_in(&mine);
            }
        }
        self.publish();
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
        if let Some(name) = renamed {
            let _ = self.up.send(Up::Named(name));
        }
    }

    async fn answer(&self, from: String, req: MineRequest) -> MineReply {
        if let MineRequest::Link { token, name } = req {
            return self.on_link(from, &token, &name);
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
            _ => refused("This machine doesn't do that yet."),
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
