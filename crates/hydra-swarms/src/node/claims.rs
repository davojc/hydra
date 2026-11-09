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
use crate::claims::{
    ClaimRecord, ContextSettings, DoneRecord, Records, ReleaseRecord, ReopenRecord,
};
use crate::paths::{claim_key, claims_context_prefix, done_key, release_key, reopen_key};
use crate::roster::{Role, Roster};
use crate::wire::{CLAIMS_ALPN, ClaimAnswer, ClaimAsk, OwnRecords, read_msg, write_msg};

/// A heard claim that hasn't been renewed for this long is dropped, in microseconds, so it can
/// never block a key for long even if its context's settings are unknown.
const HEARD_FOR: u64 = 60 * 60 * 1_000_000;

/// The most an answer gives to a machine's own records. Past it the oldest done marks stay behind,
/// since an answer too long to send would count as no answer at all.
const OWN_RECORDS_BYTES: usize = 1 << 20;

/// How long the handler waits for a member to send its question.
const READ_ASK_FOR: Duration = Duration::from_secs(10);

/// When each tombstone was written, by its author and the document key it ended.
type Tombstones = HashMap<(String, Vec<u8>), u64>;

/// What a member said of its own claims in a context. It stands in for tombstones that haven't
/// synced: whether a machine's claim counts depends on its other claims, so one that ended and
/// still looks live here would push its newer claims over the limit.
pub(super) struct ToldOwn {
    through: u64,
    /// Each claim it had then: the key, and when it was made.
    claims: HashMap<String, u64>,
    /// When this machine was told, by its own clock.
    heard_at: u64,
}

impl ToldOwn {
    /// Whether what was said ends `claim`. A claim made later says nothing either way, since it
    /// hadn't been made.
    fn ended(&self, claim: &ClaimRecord) -> bool {
        claim.claimed_at <= self.through && self.claims.get(&claim.key) != Some(&claim.claimed_at)
    }
}

/// What the machine `me` can say of its own records, given when it last ended a record there.
///
/// `through` is the newest of its claims and tombstones, not the time now: a claim is stamped
/// before it is written, so at any time now one could be stamped and still on its way into the
/// document, and it mustn't be taken for ended. The swarm's one task makes these one at a time, so
/// everything stamped before the newest of them is already written.
fn own_records(
    context: &str,
    me: &str,
    records: &Records,
    last_ended: u64,
    budget: usize,
) -> Option<OwnRecords> {
    let claims: Vec<ClaimRecord> = records
        .claims
        .iter()
        .filter(|(author, _)| author == me)
        .map(|(_, c)| c.clone())
        .collect();
    // A list that left a claim out would say that claim had ended.
    let mut room = budget.checked_sub(claims.iter().map(sent_size).sum())?;

    let mut mine: Vec<&DoneRecord> = records
        .dones
        .iter()
        .filter(|(author, _)| author == me)
        .map(|(_, d)| d)
        .collect();
    mine.sort_by_key(|d| std::cmp::Reverse(d.at));
    let mut dones = Vec::new();
    for done in mine {
        let Some(left) = room.checked_sub(sent_size(done)) else {
            break;
        };
        room = left;
        dones.push(done.clone());
    }

    let newest_claim = claims.iter().map(|c| c.claimed_at).max().unwrap_or(0);
    Some(OwnRecords {
        context: context.to_string(),
        claims,
        dones,
        through: newest_claim.max(last_ended),
    })
}

/// How much of an answer `record` takes up.
fn sent_size<T: Serialize>(record: &T) -> usize {
    serde_json::to_vec(record).map_or(0, |b| b.len())
}

/// One entry under a context.
enum Record {
    Claim(ClaimRecord),
    Done(DoneRecord),
    Release(ReleaseRecord),
    Reopen(ReopenRecord),
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
    if let Ok(r) = serde_json::from_slice::<ReopenRecord>(bytes)
        && doc_key == reopen_key(&r.context, &r.key)
    {
        return Some(Record::Reopen(r));
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

    pub async fn write_reopen(&self, reopen: &ReopenRecord) -> anyhow::Result<()> {
        let key = reopen_key(&reopen.context, &reopen.key);
        self.core.write_record(key, reopen).await
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
            // Who it came from, by who was asked: a member can't speak for another machine here.
            let answerer = roster
                .member_by_endpoint(&answer.from)
                .and_then(|m| m.machines.iter().find(|x| x.endpoint == answer.from))
                .map(|x| x.author.clone())
                .filter(|author| *author != me && roster.writes(author));
            if let (Some(author), Some(own)) = (answerer, answer.own) {
                core.hold_own(author, own);
            }
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
    /// releases that old too, or past the lapse time if that is longer: a release has to outlast
    /// a split, since the holder it ends may still be renewing out of sight, so it isn't enough
    /// for the claim to have lapsed here.
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
                Some(Record::Release(r)) => now.saturating_sub(r.at) > keep.max(lapse),
                // Every done it covers is at or before it, so has aged out by then.
                Some(Record::Reopen(r)) => now.saturating_sub(r.at) > keep,
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

    /// From now on reading `context`'s claims fails, as a damaged store would.
    pub fn unreadable_claims_for_tests(&self, context: &str) {
        *lock(&self.core.unreadable_claims) = Some(context.to_string());
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

    /// Takes in what `author` says of its own records in a context.
    fn hold_own(&self, author: String, own: OwnRecords) {
        let OwnRecords {
            context,
            claims,
            dones,
            through,
        } = own;
        let mut current = HashMap::new();
        for claim in claims.into_iter().filter(|c| c.context == context) {
            current.insert(claim.key.clone(), claim.claimed_at);
            self.hold_claim(author.clone(), claim);
        }
        for done in dones.into_iter().filter(|d| d.context == context) {
            self.hold_done(author.clone(), done);
        }
        let mut told = lock(&self.told_own);
        let at = (context, author);
        if told.get(&at).is_none_or(|t| through >= t.through) {
            let told_own = ToldOwn {
                through,
                claims: current,
                heard_at: now_micros(),
            };
            told.insert(at, told_own);
        }
    }

    pub(crate) async fn claim_records(&self, context: &str) -> anyhow::Result<Records> {
        if lock(&self.unreadable_claims).as_deref() == Some(context) {
            anyhow::bail!("{context} can't be read");
        }
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
                Some(Record::Reopen(r)) => records.reopens.push((author, r)),
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

        let mut told = lock(&self.told_own);
        told.retain(|_, t| now.saturating_sub(t.heard_at) <= HEARD_FOR);
        records.claims.retain(|(author, c)| {
            let at = (context.to_string(), author.clone());
            told.get(&at).is_none_or(|t| !t.ended(c))
        });
        drop(told);

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
        let answer: Option<ClaimAnswer> = read_msg(&mut recv).await?;
        conn.close(0u32.into(), b"done");
        Ok(answer.map(|answer| ClaimAnswer {
            from: endpoint.to_string(),
            ..answer
        }))
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

    /// When this machine last ended one of its records in a context, by the tombstone it left.
    async fn last_ended(&self, context: &str) -> anyhow::Result<u64> {
        let query = Query::author(self.author)
            .key_prefix(claims_context_prefix(context))
            .include_empty()
            .build();
        let entries = self.entries(query).await?;
        let ended = entries.iter().filter(|e| e.content_hash() == Hash::EMPTY);
        Ok(ended.map(|e| e.timestamp()).max().unwrap_or(0))
    }

    /// Everything this machine holds for `claim`'s key, and its own records in the whole context.
    ///
    /// The claim is stored before anything is read, and that order is the point: of two claimers
    /// that can reach each other, whichever asks second is answered with the first one's claim,
    /// and whichever asks first has told the second before it could claim.
    async fn answer_ask(&self, author: String, claim: ClaimRecord) -> anyhow::Result<ClaimAnswer> {
        let (context, key) = (claim.context.clone(), claim.key.clone());
        self.hold_claim(author, claim);
        // Nobody listening is fine.
        let _ = self.events.send(NodeEvent::ClaimHeard);
        // Read before the records, so nothing the tombstones cover can be missing from them for
        // any reason but having ended.
        let last_ended = self.last_ended(&context).await?;
        let records = self.claim_records(&context).await?;
        let me = self.author.to_string();
        let own = own_records(&context, &me, &records, last_ended, OWN_RECORDS_BYTES);
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
            own,
            from: String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::{Holder, HolderKind};

    fn claim(key: &str, claimed_at: u64) -> ClaimRecord {
        ClaimRecord {
            context: "inbox".into(),
            key: key.into(),
            claimed_at,
            renewed_at: claimed_at,
            limit: 1,
            holder: Holder {
                kind: HolderKind::Session,
                label: "session".into(),
            },
        }
    }

    fn done(key: &str, at: u64) -> DoneRecord {
        DoneRecord {
            context: "inbox".into(),
            key: key.into(),
            at,
        }
    }

    #[test]
    fn a_machine_speaks_only_of_its_own_records() {
        let records = Records {
            claims: vec![("me".into(), claim("a", 5)), ("them".into(), claim("b", 9))],
            dones: vec![("them".into(), done("c", 9)), ("me".into(), done("d", 2))],
            releases: vec![],
            reopens: vec![],
        };
        let own = own_records("inbox", "me", &records, 3, 1 << 20).unwrap();
        assert_eq!(own.context, "inbox");
        assert_eq!(own.claims, vec![claim("a", 5)]);
        assert_eq!(own.dones, vec![done("d", 2)]);
    }

    #[test]
    fn what_a_machine_says_runs_up_to_its_newest_claim_or_tombstone_and_no_further() {
        let records = |claims: Vec<(String, ClaimRecord)>| Records {
            claims,
            ..Records::default()
        };
        let mine = vec![("me".to_string(), claim("a", 5))];
        let through = |r: &Records, ended| {
            own_records("inbox", "me", r, ended, 1 << 20)
                .unwrap()
                .through
        };
        assert_eq!(through(&records(mine.clone()), 3), 5);
        assert_eq!(through(&records(mine), 7), 7);
        assert_eq!(through(&records(vec![]), 7), 7);
        assert_eq!(through(&records(vec![]), 0), 0);
    }

    #[test]
    fn only_the_newest_done_marks_go_when_they_dont_all_fit() {
        let records = Records {
            claims: vec![("me".into(), claim("a", 5))],
            dones: vec![
                ("me".into(), done("old", 1)),
                ("me".into(), done("new", 3)),
                ("me".into(), done("mid", 2)),
            ],
            releases: vec![],
            reopens: vec![],
        };
        let room = sent_size(&claim("a", 5)) + 2 * sent_size(&done("new", 3));
        let own = own_records("inbox", "me", &records, 0, room).unwrap();
        assert_eq!(own.claims.len(), 1, "claims come first");
        assert_eq!(own.dones, vec![done("new", 3), done("mid", 2)]);
    }

    #[test]
    fn nothing_is_said_when_the_claims_dont_all_fit() {
        let records = Records {
            claims: vec![("me".into(), claim("a", 5)), ("me".into(), claim("b", 6))],
            ..Records::default()
        };
        let room = sent_size(&claim("a", 5)) + 1;
        // A list that left a claim out would say that claim had ended.
        assert_eq!(own_records("inbox", "me", &records, 0, room), None);
    }

    #[test]
    fn only_a_claim_made_by_then_and_not_listed_has_ended() {
        let told = ToldOwn {
            through: 10,
            claims: HashMap::from([("k2".to_string(), 8)]),
            heard_at: 0,
        };
        assert!(told.ended(&claim("k1", 5)), "not listed");
        assert!(!told.ended(&claim("k2", 8)), "listed");
        assert!(
            told.ended(&claim("k2", 6)),
            "an older claim on a listed key"
        );
        assert!(!told.ended(&claim("k3", 11)), "made after");
    }
}
