//! Claims in the swarm document, and asking other members what they know about a key. A claim
//! someone has told this machine about but the document hasn't brought yet is held in memory, so
//! two claimers that can reach each other see each other whatever sync is doing.
use std::collections::HashMap;
use std::sync::Weak;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{EndpointAddr, EndpointId};
use iroh_blobs::Hash;
use iroh_docs::store::Query;
use serde::Serialize;

use super::{Core, CoreSlot, MAX_RECORD_BYTES, NodeEvent, SwarmNode, lock, now_micros};
use crate::claims::{ClaimRecord, ContextSettings, DoneRecord, Records, ReleaseRecord};
use crate::paths::{claim_key, claims_context_prefix, done_key, release_key};
use crate::roster::{Role, Roster};
use crate::wire::{CLAIMS_ALPN, ClaimAnswer, ClaimAsk, read_msg, write_msg};

/// A heard claim that hasn't been renewed for this long is dropped, in microseconds, so it can
/// never block a key for long even if its context's settings are unknown.
const HEARD_FOR: u64 = 60 * 60 * 1_000_000;

/// How long the handler waits for a member to send its question.
const READ_ASK_FOR: Duration = Duration::from_secs(10);

/// When each tombstone was written, by its author and the document key it ended.
type Tombstones = HashMap<(String, Vec<u8>), u64>;

/// One entry under a context.
enum Record {
    Claim(ClaimRecord),
    Done(DoneRecord),
    Release(ReleaseRecord),
}

/// What `bytes` say, as long as they say it about the document key they are stored under: an
/// entry only counts for the key it is filed under, whatever its content claims.
fn parse_record(doc_key: &[u8], bytes: &[u8]) -> Option<Record> {
    if let Ok(c) = serde_json::from_slice::<ClaimRecord>(bytes)
        && doc_key == claim_key(&c.context, &c.key)
    {
        return Some(Record::Claim(c));
    }
    if let Ok(d) = serde_json::from_slice::<DoneRecord>(bytes)
        && doc_key == done_key(&d.context, &d.key)
    {
        return Some(Record::Done(d));
    }
    if let Ok(r) = serde_json::from_slice::<ReleaseRecord>(bytes)
        && doc_key == release_key(&r.context, &r.key, &r.target)
    {
        return Some(Record::Release(r));
    }
    None
}

/// Whether a record told to this machine is small enough to keep, by the limit entries in the
/// document are held to.
fn fits<T: Serialize>(record: &T) -> bool {
    serde_json::to_vec(record).is_ok_and(|b| b.len() as u64 <= MAX_RECORD_BYTES)
}

impl SwarmNode {
    /// Writes or renews this machine's claim.
    pub async fn write_claim(&self, claim: &ClaimRecord) -> anyhow::Result<()> {
        let key = claim_key(&claim.context, &claim.key);
        self.core.write_record(key, claim).await
    }

    /// Writes a tombstone for this machine's claim on `key`.
    pub async fn delete_claim(&self, context: &str, key: &str) -> anyhow::Result<()> {
        let core = &self.core;
        core.ensure_writable()?;
        core.doc.del(core.author, claim_key(context, key)).await?;
        Ok(())
    }

    pub async fn write_done(&self, done: &DoneRecord) -> anyhow::Result<()> {
        let key = done_key(&done.context, &done.key);
        self.core.write_record(key, done).await
    }

    pub async fn write_release(&self, release: &ReleaseRecord) -> anyhow::Result<()> {
        let key = release_key(&release.context, &release.key, &release.target);
        self.core.write_record(key, release).await
    }

    /// What this machine knows about a context: its document and what members have told it, with
    /// at most one claim per machine per key.
    pub async fn claim_records(&self, context: &str) -> anyhow::Result<Records> {
        self.core.claim_records(context).await
    }

    /// Asks every one of `endpoints` at once what it knows about `claim`'s key, telling it about
    /// the claim. Any that fail or don't answer within `timeout` are left out.
    pub async fn ask_claim(
        &self,
        endpoints: &[String],
        claim: &ClaimRecord,
        timeout: Duration,
    ) -> Vec<ClaimAnswer> {
        let ask = ClaimAsk {
            claim: claim.clone(),
        };
        let asks = endpoints.iter().map(|endpoint| {
            let ask = &ask;
            async move {
                let asked = tokio::time::timeout(timeout, self.core.ask_one(endpoint, ask));
                asked.await.ok()?.ok()?
            }
        });
        futures::future::join_all(asks)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    /// Keeps what members answered, from those whose records count by `roster`. Nothing they say
    /// about this machine's own records is kept: its document already knows.
    pub fn take_answers(&self, answers: Vec<ClaimAnswer>, roster: &Roster) {
        let core = &self.core;
        let me = core.author.to_string();
        for answer in answers {
            for (author, claim) in answer.claims {
                if author != me && roster.writes(&author) {
                    core.hold_claim(author, claim);
                }
            }
            for (author, done) in answer.dones {
                // A former writer's finished keys still count, as `settle` has it.
                let counts = roster.writes(&author) || roster.retired.contains_key(&author);
                if author != me && counts {
                    core.hold_done(author, done);
                }
            }
        }
    }

    /// Deletes this machine's claims that have lapsed, its done marks past `keep_done`, and its
    /// releases that old too: a release has to outlast a split, since the holder it ends may
    /// still be renewing out of sight, so it isn't enough for the claim to have lapsed here.
    pub async fn prune_claims(
        &self,
        context: &str,
        settings: &ContextSettings,
        now: u64,
        slack: Duration,
    ) -> anyhow::Result<()> {
        let core = &self.core;
        let lapse = settings.lapse_micros(slack);
        let keep = settings.keep_done_secs.saturating_mul(1_000_000);
        let query = Query::author(core.author)
            .key_prefix(claims_context_prefix(context))
            .build();
        for e in core.entries(query).await? {
            let Some(bytes) = core.read_blob(e.content_hash()).await? else {
                continue;
            };
            let old = match parse_record(e.key(), &bytes) {
                Some(Record::Claim(c)) => now.saturating_sub(c.renewed_at) > lapse,
                Some(Record::Done(d)) => now.saturating_sub(d.at) > keep,
                Some(Record::Release(r)) => now.saturating_sub(r.at) > keep,
                None => false,
            };
            if old {
                core.doc.del(core.author, e.key().to_vec()).await?;
            }
        }
        Ok(())
    }

    /// From now on this node takes questions about claims and never answers them.
    pub fn silent_claims_for_tests(&self) {
        self.core.silent_claims.store(true, Ordering::Release);
    }

    /// Stops live sync of the document; `sync_with` starts it again.
    pub async fn leave_sync_for_tests(&self) -> anyhow::Result<()> {
        self.core.doc.leave().await
    }
}

impl Core {
    async fn write_record<T: Serialize>(&self, key: Vec<u8>, record: &T) -> anyhow::Result<()> {
        self.ensure_writable()?;
        let bytes = serde_json::to_vec(record)?;
        self.doc.set_bytes(self.author, key, bytes).await?;
        Ok(())
    }

    /// Holds `claim` as told to this machine, unless what it holds from that author is newer.
    fn hold_claim(&self, author: String, claim: ClaimRecord) {
        if !fits(&claim) {
            return;
        }
        let at = (claim.context.clone(), claim.key.clone(), author);
        let mut heard = lock(&self.heard);
        if heard
            .get(&at)
            .is_none_or(|c| claim.claimed_at >= c.claimed_at)
        {
            heard.insert(at, claim);
        }
    }

    fn hold_done(&self, author: String, done: DoneRecord) {
        if !fits(&done) {
            return;
        }
        let at = (done.context.clone(), done.key.clone(), author);
        let mut heard = lock(&self.heard_dones);
        if heard.get(&at).is_none_or(|d| done.at >= d.at) {
            heard.insert(at, done);
        }
    }

    pub(crate) async fn claim_records(&self, context: &str) -> anyhow::Result<Records> {
        let query = Query::key_prefix(claims_context_prefix(context))
            .include_empty()
            .build();
        let mut records = Records::default();
        let mut tombstones = Tombstones::new();
        for e in self.entries(query).await? {
            let author = e.author().to_string();
            if e.content_hash() == Hash::EMPTY {
                tombstones.insert((author, e.key().to_vec()), e.timestamp());
                continue;
            }
            if e.content_len() > MAX_RECORD_BYTES {
                continue;
            }
            // Not here yet: `ClaimsChanged` brings a re-read when it comes.
            let Some(bytes) = self.read_blob(e.content_hash()).await? else {
                continue;
            };
            match parse_record(e.key(), &bytes) {
                Some(Record::Claim(c)) => records.claims.push((author, c)),
                Some(Record::Done(d)) => records.dones.push((author, d)),
                Some(Record::Release(r)) => records.releases.push((author, r)),
                None => {}
            }
        }
        self.merge_heard(context, &mut records, &tombstones, now_micros());
        Ok(records)
    }

    /// Brings what members have told this machine into what its document says, keeping one
    /// record per machine per key, the newest. Heard records the document has caught up with are
    /// forgotten, as are those a newer tombstone ended. A tombstone older than a heard record
    /// says nothing about it, since a machine can release a key and then claim it again.
    fn merge_heard(&self, context: &str, records: &mut Records, gone: &Tombstones, now: u64) {
        let ended = |author: &str, doc_key: Vec<u8>, at: u64| {
            gone.get(&(author.to_string(), doc_key))
                .is_some_and(|tombstone| at < *tombstone)
        };

        let mut heard = lock(&self.heard);
        heard.retain(|(ctx, key, author), c| {
            now.saturating_sub(c.renewed_at) <= HEARD_FOR
                && (ctx != context || !ended(author, claim_key(ctx, key), c.claimed_at))
        });
        records.claims.retain(|(author, c)| {
            let at = (context.to_string(), c.key.clone(), author.clone());
            if heard.get(&at).is_some_and(|h| h.claimed_at > c.claimed_at) {
                return false;
            }
            heard.remove(&at);
            true
        });
        let told = heard.iter().filter(|((ctx, ..), _)| ctx == context);
        records
            .claims
            .extend(told.map(|((_, _, author), c)| (author.clone(), c.clone())));
        drop(heard);

        let mut heard = lock(&self.heard_dones);
        heard.retain(|(ctx, key, author), d| {
            ctx != context || !ended(author, done_key(ctx, key), d.at)
        });
        records.dones.retain(|(author, d)| {
            let at = (context.to_string(), d.key.clone(), author.clone());
            if heard.get(&at).is_some_and(|h| h.at > d.at) {
                return false;
            }
            heard.remove(&at);
            true
        });
        let told = heard.iter().filter(|((ctx, ..), _)| ctx == context);
        records
            .dones
            .extend(told.map(|((_, _, author), d)| (author.clone(), d.clone())));
    }

    async fn ask_one(&self, endpoint: &str, ask: &ClaimAsk) -> anyhow::Result<Option<ClaimAnswer>> {
        let id: EndpointId = endpoint.parse().context("not an endpoint id")?;
        let conn = self
            .endpoint
            .connect(EndpointAddr::new(id), CLAIMS_ALPN)
            .await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_msg(&mut send, ask).await?;
        send.finish()?;
        let answer = read_msg(&mut recv).await?;
        conn.close(0u32.into(), b"done");
        Ok(answer)
    }

    /// The author id of the machine at `from`, if its person may claim. It is what the roster
    /// says, never what the sender says.
    async fn asking_author(&self, from: EndpointId) -> Option<String> {
        let roster = self.roster().await.ok()??;
        let endpoint = from.to_string();
        let member = roster
            .member_by_endpoint(&endpoint)
            .filter(|m| m.role >= Role::Writer)?;
        let machine = member.machines.iter().find(|m| m.endpoint == endpoint)?;
        Some(machine.author.clone())
    }

    /// Everything this machine holds for `claim`'s key.
    ///
    /// The claim is stored before anything is read, and that order is the point: of two claimers
    /// that can reach each other, whichever asks second is answered with the first one's claim,
    /// and whichever asks first has told the second before it could claim.
    async fn answer_ask(&self, author: String, claim: ClaimRecord) -> anyhow::Result<ClaimAnswer> {
        let (context, key) = (claim.context.clone(), claim.key.clone());
        self.hold_claim(author, claim);
        // Nobody listening is fine.
        let _ = self.events.send(NodeEvent::ClaimHeard);
        let records = self.claim_records(&context).await?;
        Ok(ClaimAnswer {
            claims: records
                .claims
                .into_iter()
                .filter(|(_, c)| c.key == key)
                .collect(),
            dones: records
                .dones
                .into_iter()
                .filter(|(_, d)| d.key == key)
                .collect(),
        })
    }
}

/// The claims protocol's handler. The gate keeps non-members off this ALPN, and each question
/// still checks its sender may claim.
#[derive(Clone)]
pub(super) struct Claims(pub(super) CoreSlot);

impl std::fmt::Debug for Claims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Claims")
    }
}

impl ProtocolHandler for Claims {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let from = conn.remote_id();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let Some(core) = self.0.get().and_then(Weak::upgrade) else {
                break;
            };
            let Some(author) = core.asking_author(from).await else {
                // Closing the stream is the refusal: the asker counts it as no answer.
                let _ = send.finish();
                continue;
            };
            let read = tokio::time::timeout(READ_ASK_FOR, read_msg::<_, ClaimAsk>(&mut recv));
            let Ok(Ok(Some(ClaimAsk { claim }))) = read.await else {
                break;
            };
            let silent = core.silent_claims.load(Ordering::Acquire);
            let answer = core.answer_ask(author, claim).await;
            drop(core);
            if silent {
                conn.closed().await;
                break;
            }
            let Ok(answer) = answer else {
                let _ = send.finish();
                continue;
            };
            if write_msg(&mut send, &answer).await.is_err() || send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}
