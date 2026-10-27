//! Single-use invites and the hydra protocol around them: the invite store, joining with a code,
//! readers' own changes passed to an owner to write, and an owner's notices to a machine.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_docs::{Author, AuthorId, Capability, NamespaceId, NamespaceSecret};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::ids::{PersonId, PersonKey, from_hex, to_hex, verify};
use crate::node::{Core, CoreSlot, NodeConfig, NodeEvent, SwarmNode, endpoint_builder, now_micros};
use crate::roster::{Change, Machine, Member, Role, SignedChange};
use crate::wire::{ALPN, Reply, Request, read_msg, write_msg};

/// How long an invite can be used for, in seconds.
pub const INVITE_LIFETIME: u64 = 7 * 24 * 3600;

const CODE_PREFIX: &str = "hydra-swarm:";

/// How long either side waits on the other for one request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const USED: &str = "This invite has already been used.";
const EXPIRED: &str = "This invite has expired.";
const CANCELLED: &str = "This invite was cancelled.";
const NOT_AN_OWNER: &str = "Whoever invited you is no longer an owner of this swarm.";
/// An owner's refusal to pass on a change from a machine it doesn't count as the signer's,
/// which, for a change the signer sends from their own machine, means they are out already.
pub(crate) const NOT_THEIR_MACHINE: &str = "Only a member's own machine can pass their changes on.";

/// What someone pastes to join: who to ask, and the secret that proves they were invited.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct InviteCode {
    /// The inviting machine's endpoint id in this swarm.
    pub host: String,
    pub relay: Option<String>,
    /// 64 hex.
    pub token: String,
}

impl InviteCode {
    /// `hydra-swarm:` and the code as base64url JSON.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("an invite code always serialises");
        format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// A pasted code; whitespace anywhere in it, as from a wrapped message, is ignored.
    pub fn decode(s: &str) -> Option<InviteCode> {
        let s: String = s.split_whitespace().collect();
        let json = URL_SAFE_NO_PAD.decode(s.strip_prefix(CODE_PREFIX)?).ok()?;
        let code: InviteCode = serde_json::from_slice(&json).ok()?;
        code.host.parse::<EndpointId>().ok()?;
        is_token(&code.token).then_some(code)
    }

    fn addr(&self) -> anyhow::Result<EndpointAddr> {
        let id: EndpointId = self.host.parse().context("the invite's host isn't valid")?;
        let mut addr = EndpointAddr::new(id);
        if let Some(relay) = &self.relay {
            let url: RelayUrl = relay.parse().context("the invite's relay isn't valid")?;
            addr = addr.with_relay_url(url);
        }
        Ok(addr)
    }
}

/// The longest person or machine name taken in, in characters, so a change naming one stays far
/// under the size the roster reads.
const MAX_NAME_CHARS: usize = 200;

fn name_fits(name: &str) -> bool {
    name.chars().count() <= MAX_NAME_CHARS
}

fn is_token(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn random32() -> io::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(format!("random bytes: {e}")))?;
    Ok(b)
}

fn token_hash(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Invite {
    pub id: String,
    pub role: Role,
    /// Unix seconds.
    pub created: u64,
    /// Unix seconds.
    pub expires: u64,
    pub used: bool,
}

/// An invite as kept on disk: with a hash of its token, never the token.
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(flatten)]
    invite: Invite,
    token_sha256: String,
    #[serde(default)]
    cancelled: bool,
}

/// The invites this machine has made for one swarm, in `invites.json` in the swarm's folder.
pub struct Invites {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Invites {
    pub fn new(dir: &Path) -> Invites {
        Invites {
            path: dir.join("invites.json"),
            lock: Mutex::new(()),
        }
    }

    /// A new invite for `role` made at `now` (Unix seconds), and its token.
    pub fn create(&self, role: Role, now: u64) -> io::Result<(Invite, String)> {
        let token = to_hex(&random32()?);
        let invite = Invite {
            id: to_hex(&random32()?[..8]),
            role,
            created: now,
            expires: now.saturating_add(INVITE_LIFETIME),
            used: false,
        };
        self.change(|all| {
            all.push(Stored {
                invite: invite.clone(),
                token_sha256: token_hash(&token),
                cancelled: false,
            });
            Ok(())
        })?;
        Ok((invite, token))
    }

    pub fn cancel(&self, id: &str) -> io::Result<()> {
        self.change(|all| {
            let stored = all
                .iter_mut()
                .find(|s| s.invite.id == id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such invite"))?;
            stored.cancelled = true;
            Ok(())
        })
    }

    /// The invites that can still be used, oldest first.
    pub fn pending(&self, now: u64) -> Vec<Invite> {
        let _held = self.hold();
        self.load()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.invite.used && !s.cancelled && now < s.invite.expires)
            .map(|s| s.invite)
            .collect()
    }

    /// Marks it used and returns its role if `token` is unused, uncancelled and unexpired.
    pub fn redeem(&self, token: &str, now: u64) -> Option<Role> {
        self.spend(token, now).ok()?.ok()
    }

    /// `redeem`, with what to tell the joiner when it can't be used.
    fn spend(&self, token: &str, now: u64) -> io::Result<Result<Role, &'static str>> {
        let hash = token_hash(token);
        let mut outcome = Err(USED);
        self.change(|all| {
            let Some(s) = all.iter_mut().find(|s| s.token_sha256 == hash) else {
                return Ok(());
            };
            outcome = if s.invite.used {
                Err(USED)
            } else if s.cancelled {
                Err(CANCELLED)
            } else if now >= s.invite.expires {
                Err(EXPIRED)
            } else {
                s.invite.used = true;
                Ok(s.invite.role)
            };
            Ok(())
        })?;
        Ok(outcome)
    }

    fn hold(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load(&self) -> io::Result<Vec<Stored>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Loads the invites, lets `f` change them and saves them, all under the lock, so a token
    /// can't be spent twice at once.
    fn change(&self, f: impl FnOnce(&mut Vec<Stored>) -> io::Result<()>) -> io::Result<()> {
        let _held = self.hold();
        let mut all = self.load()?;
        f(&mut all)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn now_secs() -> u64 {
    u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0)
}

/// Joins with `code`: asks its host to spend it, as `me` (whose role is ignored) signed for by
/// `person`, from the endpoint `machine_key` makes. Returns the capability handed over, the
/// swarm's anchor and its name. The endpoint is closed before this returns, so the swarm's node
/// can bind the same key.
pub async fn redeem(
    code: &InviteCode,
    me: Member,
    person: &PersonKey,
    machine_key: [u8; 32],
    relays: bool,
) -> anyhow::Result<(Capability, PersonId, String)> {
    redeem_via(code, me, person, machine_key, code.addr()?, relays).await
}

/// `redeem` dialling `host` directly, relays off.
pub async fn redeem_at(
    code: &InviteCode,
    me: Member,
    person: &PersonKey,
    machine_key: [u8; 32],
    host: EndpointAddr,
) -> anyhow::Result<(Capability, PersonId, String)> {
    redeem_via(code, me, person, machine_key, host, false).await
}

async fn redeem_via(
    code: &InviteCode,
    me: Member,
    person: &PersonKey,
    machine_key: [u8; 32],
    host: EndpointAddr,
    relays: bool,
) -> anyhow::Result<(Capability, PersonId, String)> {
    if me.person != person.id() {
        bail!("the person joining must be the one signing for it");
    }
    let join = Request::Join {
        token: code.token.clone(),
        member: me,
        proof: to_hex(&person.sign(code.token.as_bytes())),
    };
    let endpoint = endpoint_builder(&machine_key, relays).bind().await?;
    let replied = ask(&endpoint, host, &join).await;
    endpoint.close().await;
    match replied? {
        Reply::Joined {
            ns,
            anchor,
            name,
            write_secret,
        } => {
            let ns: NamespaceId = ns
                .parse()
                .map_err(|_| anyhow!("the swarm's id isn't valid"))?;
            let anchor: PersonId = anchor.parse().map_err(anyhow::Error::msg)?;
            let cap = match write_secret {
                None => Capability::Read(ns),
                Some(hex) => Capability::Write(
                    namespace_secret(&hex, ns)
                        .context("the write access handed over is damaged")?,
                ),
            };
            Ok((cap, anchor, name))
        }
        Reply::Refused { reason } => Err(anyhow!(reason)),
        Reply::Ok => bail!("the inviting machine didn't say how to join"),
    }
}

/// `hex` as the secret of the namespace `ns`.
fn namespace_secret(hex: &str, ns: NamespaceId) -> Option<NamespaceSecret> {
    let bytes = <[u8; 32]>::try_from(from_hex(hex)?).ok()?;
    let secret = NamespaceSecret::from_bytes(&bytes);
    (secret.id() == ns).then_some(secret)
}

/// Sends one request on its own stream and reads the reply.
async fn ask(endpoint: &Endpoint, to: EndpointAddr, req: &Request) -> anyhow::Result<Reply> {
    let asked = async {
        let conn = endpoint.connect(to, ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_msg(&mut send, req).await?;
        send.finish()?;
        let reply = read_msg(&mut recv)
            .await?
            .context("the other machine hung up without replying")?;
        conn.close(0u32.into(), b"done");
        anyhow::Ok(reply)
    };
    tokio::time::timeout(REQUEST_TIMEOUT, asked)
        .await
        .context("the other machine didn't answer in time")?
}

fn refused(reason: &str) -> Reply {
    Reply::Refused {
        reason: reason.to_string(),
    }
}

impl SwarmNode {
    /// Joins the swarm `code` invites to, as `me`, whose one machine entry is filled in with this
    /// machine's endpoint and a new author, keeping the machine name given.
    ///
    /// A fresh joiner's gate is empty, so this opens it to the inviter alone, syncs with it, and
    /// once the roster shows this machine as a member, sets the gate from the roster.
    pub async fn join_with_invite(
        cfg: NodeConfig,
        code: &InviteCode,
        me: Member,
    ) -> anyhow::Result<SwarmNode> {
        let host = code.addr()?;
        SwarmNode::join_via(cfg, code, me, host).await
    }

    /// `join_with_invite` dialling `host` directly, for tests with relays off.
    pub async fn join_with_invite_at(
        cfg: NodeConfig,
        code: &InviteCode,
        me: Member,
        host: EndpointAddr,
    ) -> anyhow::Result<SwarmNode> {
        SwarmNode::join_via(cfg, code, me, host).await
    }

    async fn join_via(
        mut cfg: NodeConfig,
        code: &InviteCode,
        me: Member,
        host: EndpointAddr,
    ) -> anyhow::Result<SwarmNode> {
        let author = Author::from_bytes(&random32()?);
        let machine = Machine {
            endpoint: SecretKey::from_bytes(&cfg.machine_key).public().to_string(),
            name: me
                .machines
                .first()
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            author: author.id().to_string(),
        };
        let me = Member {
            machines: vec![machine],
            ..me
        };
        let (cap, anchor, _) = redeem_via(
            code,
            me,
            &cfg.person,
            cfg.machine_key,
            host.clone(),
            cfg.relays,
        )
        .await?;
        cfg.anchor = Some(anchor);
        let node = SwarmNode::join(cfg, cap, author).await?;
        let inviter = host.id.to_string();
        node.remember_addr(host);
        node.set_gate(std::slice::from_ref(&inviter));
        let events = node.events();
        if let Err(e) = node.sync_with(&[inviter]).await {
            node.shutdown().await;
            return Err(e);
        }
        tokio::spawn(gate_from_roster_once_in(
            std::sync::Arc::downgrade(&node.core),
            events,
        ));
        Ok(node)
    }

    /// A new invite to this swarm for `role`, and its code. Only an owner can make one, and
    /// `person` must be this machine's.
    pub async fn invite(
        &self,
        role: Role,
        person: &PersonKey,
    ) -> anyhow::Result<(Invite, InviteCode)> {
        if person.id() != self.core.person.id() {
            bail!("invites are made by this machine's person");
        }
        let roster = self
            .roster()
            .await?
            .context("this swarm's members haven't arrived yet")?;
        if roster.role_of(&person.id()) != Some(Role::Owner) {
            bail!("Only owners can invite people.");
        }
        let (invite, token) = self.core.invites.create(role, now_secs())?;
        let relay = self
            .core
            .endpoint
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());
        let code = InviteCode {
            host: self.endpoint_id(),
            relay,
            token,
        };
        Ok((invite, code))
    }

    pub fn cancel_invite(&self, id: &str) -> anyhow::Result<()> {
        Ok(self.core.invites.cancel(id)?)
    }

    pub fn pending_invites(&self) -> Vec<Invite> {
        self.core.invites.pending(now_secs())
    }

    /// Asks the owner's machine `owner` to write `change`, one of this person's own that a
    /// reader can't write itself.
    pub async fn relay(&self, owner: &str, change: SignedChange) -> anyhow::Result<()> {
        self.tell(owner, &Request::Relay { change }).await
    }

    /// Tells the machine `endpoint` that its person was removed from the swarm.
    pub async fn send_removed(&self, endpoint: &str) -> anyhow::Result<()> {
        self.tell(endpoint, &Request::Removed).await
    }

    /// Hands the write capability to the machine `endpoint`, whose person the roster now shows
    /// as a writer or owner.
    pub async fn send_promoted(&self, endpoint: &str) -> anyhow::Result<()> {
        let roster = self
            .roster()
            .await?
            .context("this swarm's members haven't arrived yet")?;
        let promoted = roster
            .member_by_endpoint(endpoint)
            .is_some_and(|m| m.role >= Role::Writer);
        if !promoted {
            bail!("that machine's person isn't a writer or owner");
        }
        let write_secret = self.core.write_secret().await?;
        self.tell(endpoint, &Request::Promoted { write_secret })
            .await
    }

    async fn tell(&self, to: &str, req: &Request) -> anyhow::Result<()> {
        let id: EndpointId = to.parse().context("not an endpoint id")?;
        match ask(&self.core.endpoint, EndpointAddr::new(id), req).await? {
            Reply::Ok => Ok(()),
            Reply::Refused { reason } => Err(anyhow!(reason)),
            Reply::Joined { .. } => bail!("the other machine gave a reply that doesn't fit"),
        }
    }
}

/// Waits until the roster shows this machine as a member, then sets the gate from it.
async fn gate_from_roster_once_in(core: Weak<Core>, mut events: broadcast::Receiver<NodeEvent>) {
    loop {
        match events.recv().await {
            Ok(NodeEvent::Changed | NodeEvent::ContentReady(_) | NodeEvent::SyncFinished)
            | Err(RecvError::Lagged(_)) => {}
            Ok(_) => continue,
            Err(RecvError::Closed) => return,
        }
        let Some(core) = core.upgrade() else {
            return;
        };
        let me = core.endpoint.id().to_string();
        if let Ok(Some(r)) = core.roster().await
            && r.member_by_endpoint(&me).is_some()
        {
            core.set_gate(&r.endpoints());
            return;
        }
    }
}

/// The hydra protocol's handler. It isn't gated: each request checks its own sender.
#[derive(Clone)]
pub(crate) struct Hydra(pub(crate) CoreSlot);

impl std::fmt::Debug for Hydra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hydra")
    }
}

impl ProtocolHandler for Hydra {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let from = conn.remote_id();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let Some(core) = self.0.get().and_then(Weak::upgrade) else {
                break;
            };
            let read = tokio::time::timeout(REQUEST_TIMEOUT, read_msg::<_, Request>(&mut recv));
            let Ok(Ok(Some(req))) = read.await else {
                break;
            };
            let reply = core.answer(from, req).await;
            drop(core);
            if write_msg(&mut send, &reply).await.is_err() || send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}

impl Core {
    async fn answer(&self, from: EndpointId, req: Request) -> Reply {
        let answered = match req {
            Request::Join {
                token,
                member,
                proof,
            } => self.on_join(from, &token, member, &proof).await,
            Request::Relay { change } => self.on_relay(from, change).await,
            Request::Removed => self.on_removed(from).await,
            Request::Promoted { write_secret } => self.on_promoted(from, &write_secret).await,
        };
        answered.unwrap_or_else(|e| refused(&format!("Something went wrong here: {e:#}")))
    }

    async fn on_join(
        &self,
        from: EndpointId,
        token: &str,
        member: Member,
        proof: &str,
    ) -> anyhow::Result<Reply> {
        let signed =
            from_hex(proof).is_some_and(|sig| verify(&member.person, token.as_bytes(), &sig));
        if !signed {
            return Ok(refused(
                "This request to join isn't signed by whoever is joining.",
            ));
        }
        let Some(roster) = self.roster().await? else {
            return Ok(refused(NOT_AN_OWNER));
        };
        if roster.role_of(&self.person.id()) != Some(Role::Owner) {
            return Ok(refused(NOT_AN_OWNER));
        }
        if roster.role_of(&member.person).is_some() {
            return Ok(refused("You're already a member of this swarm."));
        }
        let Some(machine) = member.machines.first() else {
            return Ok(refused("The joining machine didn't say what it is called."));
        };
        // Checked before the token is spent: an entry the roster would skip mustn't use it up.
        if !name_fits(&member.name) || !name_fits(&machine.name) {
            return Ok(refused("Your name or your machine's name is too long."));
        }
        if machine.author.parse::<AuthorId>().is_err() {
            return Ok(refused("The joining machine's author isn't valid."));
        }
        let role = match self.invites.spend(token, now_secs())? {
            Ok(role) => role,
            Err(reason) => return Ok(refused(reason)),
        };
        let machine = Machine {
            endpoint: from.to_string(),
            ..machine.clone()
        };
        let member = Member {
            role,
            machines: vec![machine],
            ..member
        };
        let add = SignedChange::sign(
            &self.id(),
            Change::AddMember { member },
            &self.person,
            now_micros(),
        );
        self.write_change(&add).await?;
        self.refresh_gate_from_roster().await?;
        let write_secret = if role >= Role::Writer {
            Some(self.write_secret().await?)
        } else {
            None
        };
        Ok(Reply::Joined {
            ns: self.id(),
            anchor: self.anchor.to_string(),
            name: roster.name,
            write_secret,
        })
    }

    async fn on_relay(&self, from: EndpointId, change: SignedChange) -> anyhow::Result<Reply> {
        let Some(roster) = self
            .roster()
            .await?
            .filter(|r| r.role_of(&self.person.id()) == Some(Role::Owner))
        else {
            return Ok(refused("Only an owner's machine can pass your changes on."));
        };
        // Only a current member's own machine may ask, so strangers can't fill the log.
        let sender = roster.member_by_endpoint(&from.to_string());
        if sender.is_none_or(|m| m.person != change.by) {
            return Ok(refused(NOT_THEIR_MACHINE));
        }
        if !change.verify(&self.id()) {
            return Ok(refused("That change isn't signed properly."));
        }
        let own = match &change.change {
            Change::Rename { person, name } => *person == change.by && name_fits(name),
            Change::RenameMachine { person, name, .. } => *person == change.by && name_fits(name),
            Change::Leave { person } => *person == change.by,
            _ => false,
        };
        if !own {
            return Ok(refused(
                "Only your own name, your machines' names or your leaving can be passed on.",
            ));
        }
        self.write_change(&change).await?;
        Ok(Reply::Ok)
    }

    async fn on_removed(&self, from: EndpointId) -> anyhow::Result<Reply> {
        if !self.sent_by_owner(from).await? {
            return Ok(refused("Only an owner can say who was removed."));
        }
        // Nobody listening is fine.
        let _ = self.events.send(NodeEvent::Removed);
        Ok(Reply::Ok)
    }

    async fn on_promoted(&self, from: EndpointId, write_secret: &str) -> anyhow::Result<Reply> {
        if !self.sent_by_owner(from).await? {
            return Ok(refused("Only an owner can hand over write access."));
        }
        let Some(secret) = namespace_secret(write_secret, self.doc.id()) else {
            return Ok(refused("That isn't this swarm's write access."));
        };
        // iroh-docs merges the write capability into the read one this machine holds.
        self.docs
            .import_namespace(Capability::Write(secret))
            .await?;
        self.writable
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(Reply::Ok)
    }

    /// Whether `from` is a machine of someone this node's roster shows as an owner.
    async fn sent_by_owner(&self, from: EndpointId) -> anyhow::Result<bool> {
        Ok(self.roster().await?.is_some_and(|r| {
            r.member_by_endpoint(&from.to_string())
                .is_some_and(|m| m.role == Role::Owner)
        }))
    }

    /// This swarm's write capability as hex.
    async fn write_secret(&self) -> anyhow::Result<String> {
        match self.capability(Role::Writer).await? {
            Capability::Write(secret) => Ok(to_hex(&secret.to_bytes())),
            Capability::Read(_) => {
                bail!("this machine can't hand out write access it doesn't have")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn store() -> (tempfile::TempDir, Invites) {
        let dir = tempfile::tempdir().unwrap();
        let invites = Invites::new(dir.path());
        (dir, invites)
    }

    #[test]
    fn an_invite_is_spent_once_and_lists_until_then() {
        let (_dir, invites) = store();
        let (invite, token) = invites.create(Role::Writer, NOW).unwrap();
        assert!(is_token(&token));
        assert_eq!(invite.expires, NOW + INVITE_LIFETIME);
        assert_eq!(invites.pending(NOW), vec![invite]);
        assert_eq!(invites.redeem(&token, NOW + 1), Some(Role::Writer));
        assert_eq!(invites.redeem(&token, NOW + 2), None);
        assert_eq!(invites.spend(&token, NOW + 2).unwrap(), Err(USED));
        assert!(invites.pending(NOW).is_empty());
    }

    #[test]
    fn refusals_say_why() {
        let (_dir, invites) = store();
        assert_eq!(invites.spend(&"0".repeat(64), NOW).unwrap(), Err(USED));
        let (_, old) = invites.create(Role::Reader, NOW).unwrap();
        assert_eq!(
            invites.spend(&old, NOW + INVITE_LIFETIME).unwrap(),
            Err(EXPIRED)
        );
        let (cancelled, token) = invites.create(Role::Reader, NOW).unwrap();
        invites.cancel(&cancelled.id).unwrap();
        assert_eq!(invites.spend(&token, NOW).unwrap(), Err(CANCELLED));
        assert!(invites.pending(NOW + INVITE_LIFETIME).is_empty());
        assert!(invites.cancel("nope").is_err());
    }

    #[test]
    fn only_a_hash_of_the_token_is_stored() {
        let (dir, invites) = store();
        let (_, token) = invites.create(Role::Owner, NOW).unwrap();
        let saved = std::fs::read_to_string(dir.path().join("invites.json")).unwrap();
        assert!(!saved.contains(&token));
        assert!(saved.contains(&token_hash(&token)));
        let again = Invites::new(dir.path());
        assert_eq!(again.redeem(&token, NOW), Some(Role::Owner));
    }

    #[test]
    fn codes_that_are_not_ours_do_not_decode() {
        let code = InviteCode {
            host: SecretKey::from_bytes(&[7; 32]).public().to_string(),
            relay: None,
            token: "Z".repeat(64),
        };
        assert_eq!(InviteCode::decode(&code.encode()), None);
        assert_eq!(InviteCode::decode("hydra-link:abc"), None);
    }
}
