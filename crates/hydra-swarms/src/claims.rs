//! Claims: one machine in a swarm takes a job so no other machine does it too. A context is a
//! named kind of job, and an owner sets its rules in the roster.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::roster::{Role, Roster};

/// The longest a context's name may be.
const MAX_CONTEXT_NAME: usize = 64;

/// Whether `name` can name a context: 1 to 64 of a-z, 0-9, `-` and `_`.
pub fn valid_context_name(name: &str) -> bool {
    (1..=MAX_CONTEXT_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// How claims in one context behave. An owner sets them in the roster.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ContextSettings {
    /// How often a holder renews a claim, in seconds.
    pub renew_secs: u32,
    /// How many renewals a holder may miss before the claim lapses.
    pub lapse_after: u32,
    /// How many claims one machine may hold at once: -1 is no limit, and 0 pauses the context
    /// (claims already held run on).
    pub max_per_machine: i32,
    /// How long a finished claim is kept, in seconds.
    pub keep_done_secs: u64,
}

impl Default for ContextSettings {
    fn default() -> Self {
        ContextSettings {
            renew_secs: 60,
            lapse_after: 3,
            max_per_machine: 1,
            keep_done_secs: 30 * 24 * 60 * 60,
        }
    }
}

impl ContextSettings {
    /// How long, in microseconds, a claim may go without a renewal before it lapses: the missed
    /// renewals allowed, plus `slack` for clocks and sync that run late.
    pub fn lapse_micros(&self, slack: Duration) -> u64 {
        let secs = u64::from(self.renew_secs) * u64::from(self.lapse_after);
        secs.saturating_mul(1_000_000)
            .saturating_add(slack.as_micros().min(u128::from(u64::MAX)) as u64)
    }

    /// The settings with anything out of range brought to the nearest value that works, so a
    /// bad change can't stop claims from ever lapsing or being kept.
    pub fn normalised(self) -> Self {
        ContextSettings {
            renew_secs: self.renew_secs.max(1),
            lapse_after: self.lapse_after.max(1),
            max_per_machine: self.max_per_machine.max(-1),
            keep_done_secs: self.keep_done_secs.max(60),
        }
    }
}

/// What is holding a claim.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum HolderKind {
    Session,
    /// Nothing uses this yet; it is here so records written later need no new format.
    PipelineRun,
}

/// Who holds a claim, for people to read.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Holder {
    pub kind: HolderKind,
    pub label: String,
}

/// A machine saying it has taken a key. Times are Unix microseconds.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ClaimRecord {
    pub context: String,
    pub key: String,
    pub claimed_at: u64,
    pub renewed_at: u64,
    /// The context's `max_per_machine` when the claim was made, so lowering the limit later
    /// takes nothing away.
    pub limit: i32,
    pub holder: Holder,
}

/// A machine saying a key is finished.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct DoneRecord {
    pub context: String,
    pub key: String,
    pub at: u64,
}

/// A writer undoing a finished key: every done and claim on it made at or before `at` stops
/// counting.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ReopenRecord {
    pub context: String,
    pub key: String,
    pub at: u64,
}

/// A machine ending one claim, its own or another's: the one `target` made at `claimed_at`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ReleaseRecord {
    pub context: String,
    pub key: String,
    /// The author id of the machine whose claim this ends.
    pub target: String,
    pub claimed_at: u64,
    pub at: u64,
}

/// Who may write what, by author id, as the roster stands.
#[derive(Clone, Default, Debug)]
pub struct People {
    /// Each current member machine's person id and role.
    pub members: BTreeMap<String, (String, Role)>,
    /// Machines of people who were writers and no longer are: their finished keys still count.
    pub retired: BTreeSet<String>,
}

impl People {
    pub fn from_roster(roster: &Roster) -> People {
        let mut members = BTreeMap::new();
        for m in &roster.members {
            for machine in &m.machines {
                members.insert(machine.author.clone(), (m.person.to_string(), m.role));
            }
        }
        People {
            members,
            retired: roster.retired.keys().cloned().collect(),
        }
    }

    pub(crate) fn writes(&self, author: &str) -> bool {
        self.members
            .get(author)
            .is_some_and(|(_, role)| *role >= Role::Writer)
    }

    /// Whether `by` may end the claim that `target` made: an owner may end anyone's, and a
    /// person may end the claims of their own machines.
    pub(crate) fn may_release(&self, by: &str, target: &str) -> bool {
        let Some((person, role)) = self.members.get(by).filter(|_| self.writes(by)) else {
            return false;
        };
        *role == Role::Owner
            || self
                .members
                .get(target)
                .is_some_and(|(theirs, _)| theirs == person)
    }
}

/// One context's entries, each with the author id of the machine that wrote it.
#[derive(Clone, Default, Debug)]
pub struct Records {
    pub claims: Vec<(String, ClaimRecord)>,
    pub dones: Vec<(String, DoneRecord)>,
    pub releases: Vec<(String, ReleaseRecord)>,
    pub reopens: Vec<(String, ReopenRecord)>,
}

/// Where a key stands once the records are settled.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyState {
    Free,
    Held { author: String, claim: ClaimRecord },
    Done { author: String, at: u64 },
}

/// Settles one context's records into the state of each key that isn't free (a key missing from
/// the result is free). Every machine with the same records and roster gets the same answer.
pub fn settle(
    records: &Records,
    people: &People,
    settings: &ContextSettings,
    now: u64,
    slack: Duration,
) -> BTreeMap<String, KeyState> {
    let keep = settings.keep_done_secs.saturating_mul(1_000_000);
    // The newest reopen by a writer or a retired machine, per key, like dones: a machine that
    // has left doesn't take its reopens back. A reader's changes nothing.
    let mut reopened: BTreeMap<&str, u64> = BTreeMap::new();
    for (author, r) in &records.reopens {
        if people.writes(author) || people.retired.contains(author) {
            let at = reopened.entry(r.key.as_str()).or_insert(r.at);
            *at = (*at).max(r.at);
        }
    }
    let covered = |key: &str, at: u64| reopened.get(key).is_some_and(|&r| at <= r);
    // A retired machine's finished keys still count; its claims never do.
    let dones: Vec<_> = records
        .dones
        .iter()
        .filter(|(author, d)| {
            (people.writes(author) || people.retired.contains(author))
                && !covered(&d.key, d.at)
                && now.saturating_sub(d.at) <= keep
        })
        .collect();
    let finished: BTreeSet<&str> = dones.iter().map(|(_, d)| d.key.as_str()).collect();

    let released = |author: &str, c: &ClaimRecord| {
        records.releases.iter().any(|(by, r)| {
            r.target == author
                && r.claimed_at == c.claimed_at
                && r.key == c.key
                && people.may_release(by, author)
        })
    };
    let lapse = settings.lapse_micros(slack);
    let mut by_author: BTreeMap<&str, Vec<&ClaimRecord>> = BTreeMap::new();
    for (author, c) in &records.claims {
        // A finished key doesn't count against the limit, so marking one done frees its slot.
        if people.writes(author)
            && !finished.contains(c.key.as_str())
            && !released(author, c)
            && !covered(&c.key, c.claimed_at)
            && now.saturating_sub(c.renewed_at) <= lapse
        {
            by_author.entry(author).or_default().push(c);
        }
    }

    // Each machine's oldest claims fit under the limit they were made with; later ones are
    // ignored and don't count against it.
    let mut live: Vec<(&str, &ClaimRecord)> = Vec::new();
    for (author, mut claims) in by_author {
        claims.sort_by(|a, b| (a.claimed_at, &a.key).cmp(&(b.claimed_at, &b.key)));
        let mut count = 0;
        for c in claims {
            if c.limit >= 0 && count >= c.limit {
                continue;
            }
            count += 1;
            live.push((author, c));
        }
    }

    let mut states: BTreeMap<String, KeyState> = BTreeMap::new();
    for (author, c) in live {
        let beats = match states.get(&c.key) {
            Some(KeyState::Held { author: a, claim }) => {
                (c.claimed_at, author) < (claim.claimed_at, a.as_str())
            }
            _ => true,
        };
        if beats {
            states.insert(
                c.key.clone(),
                KeyState::Held {
                    author: author.to_string(),
                    claim: c.clone(),
                },
            );
        }
    }
    // Done beats any live claim, and the newest done is the one shown.
    let mut newest: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
    for (author, d) in dones {
        let e = newest.entry(&d.key).or_insert((d.at, author));
        if (d.at, author.as_str()) > *e {
            *e = (d.at, author);
        }
    }
    for (key, (at, author)) in newest {
        states.insert(
            key.to_string(),
            KeyState::Done {
                author: author.to_string(),
                at,
            },
        );
    }
    states
}

/// What a machine should do about taking a key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Take {
    /// Free, and under the limit: make the claim.
    Go,
    /// Already this machine's: keep renewing the one it has.
    TakeUp(ClaimRecord),
    Held {
        author: String,
        claim: ClaimRecord,
    },
    Done {
        author: String,
        at: u64,
    },
    /// The context is paused.
    Paused,
    /// This machine already holds as many as it may.
    AtLimit(i32),
}

pub fn decide_take(
    states: &BTreeMap<String, KeyState>,
    key: &str,
    me: &str,
    settings: &ContextSettings,
) -> Take {
    match states.get(key) {
        Some(KeyState::Done { author, at }) => {
            return Take::Done {
                author: author.clone(),
                at: *at,
            };
        }
        Some(KeyState::Held { author, claim }) if author == me => {
            return Take::TakeUp(claim.clone());
        }
        Some(KeyState::Held { author, claim }) => {
            return Take::Held {
                author: author.clone(),
                claim: claim.clone(),
            };
        }
        Some(KeyState::Free) | None => {}
    }
    let limit = settings.max_per_machine;
    if limit == 0 {
        return Take::Paused;
    }
    let mine = states
        .values()
        .filter(|s| matches!(s, KeyState::Held { author, .. } if author == me))
        .count();
    if limit > 0 && mine >= limit as usize {
        return Take::AtLimit(limit);
    }
    Take::Go
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_specs() {
        let d = ContextSettings::default();
        assert_eq!(
            (
                d.renew_secs,
                d.lapse_after,
                d.max_per_machine,
                d.keep_done_secs
            ),
            (60, 3, 1, 2_592_000)
        );
        assert_eq!(d.lapse_micros(Duration::from_secs(30)), 210_000_000);
    }

    #[test]
    fn settings_out_of_range_are_raised_to_the_nearest_that_works() {
        let n = ContextSettings {
            renew_secs: 0,
            lapse_after: 0,
            max_per_machine: -5,
            keep_done_secs: 1,
        }
        .normalised();
        assert_eq!(
            (
                n.renew_secs,
                n.lapse_after,
                n.max_per_machine,
                n.keep_done_secs
            ),
            (1, 1, -1, 60)
        );
        let paused = ContextSettings {
            max_per_machine: 0,
            ..Default::default()
        };
        assert_eq!(paused.normalised(), paused);
    }

    #[test]
    fn context_names_are_short_and_plain() {
        assert!(valid_context_name("inbox"));
        assert!(valid_context_name("a-b_9"));
        assert!(valid_context_name(&"a".repeat(64)));
        assert!(!valid_context_name(""));
        assert!(!valid_context_name(&"a".repeat(65)));
        assert!(!valid_context_name("Has Space"));
        assert!(!valid_context_name("Upper"));
        assert!(!valid_context_name("dot.ted"));
    }

    const SEC: u64 = 1_000_000;

    fn holder() -> Holder {
        Holder {
            kind: HolderKind::Session,
            label: "session".into(),
        }
    }

    fn claim(key: &str, claimed_at: u64, limit: i32) -> ClaimRecord {
        ClaimRecord {
            context: "inbox".into(),
            key: key.into(),
            claimed_at,
            renewed_at: claimed_at,
            limit,
            holder: holder(),
        }
    }

    fn done(key: &str, at: u64) -> DoneRecord {
        DoneRecord {
            context: "inbox".into(),
            key: key.into(),
            at,
        }
    }

    fn release(target: &str, key: &str, claimed_at: u64) -> ReleaseRecord {
        ReleaseRecord {
            context: "inbox".into(),
            key: key.into(),
            target: target.into(),
            claimed_at,
            at: claimed_at + 1,
        }
    }

    /// Writers "a" and "a2" (one person), "aaa", "b", "bbb" and "c", an owner "boss" and a reader.
    fn people() -> People {
        let mut p = People::default();
        for (author, person, role) in [
            ("a", "pa", Role::Writer),
            ("a2", "pa", Role::Writer),
            ("aaa", "paaa", Role::Writer),
            ("b", "pb", Role::Writer),
            ("bbb", "pbbb", Role::Writer),
            ("c", "pc", Role::Writer),
            ("boss", "pboss", Role::Owner),
            ("reader", "pr", Role::Reader),
        ] {
            p.members.insert(author.into(), (person.into(), role));
        }
        p
    }

    fn settings(max: i32) -> ContextSettings {
        ContextSettings {
            max_per_machine: max,
            ..Default::default()
        }
    }

    fn records(claims: &[(&str, ClaimRecord)]) -> Records {
        Records {
            claims: claims
                .iter()
                .map(|(a, c)| (a.to_string(), c.clone()))
                .collect(),
            ..Default::default()
        }
    }

    fn settle_at(r: &Records, max: i32, now: u64) -> BTreeMap<String, KeyState> {
        settle(r, &people(), &settings(max), now, Duration::from_secs(30))
    }

    fn holder_of(states: &BTreeMap<String, KeyState>, key: &str) -> Option<String> {
        match states.get(key) {
            Some(KeyState::Held { author, .. }) => Some(author.clone()),
            _ => None,
        }
    }

    #[test]
    fn the_earliest_claim_wins() {
        let r = records(&[("a", claim("k", 200, -1)), ("b", claim("k", 100, -1))]);
        assert_eq!(
            holder_of(&settle_at(&r, -1, 300), "k").as_deref(),
            Some("b")
        );
        let r = records(&[("b", claim("k", 100, -1)), ("a", claim("k", 200, -1))]);
        assert_eq!(
            holder_of(&settle_at(&r, -1, 300), "k").as_deref(),
            Some("b")
        );
    }

    #[test]
    fn equal_times_go_to_the_lowest_machine_id() {
        let r = records(&[("bbb", claim("k", 100, -1)), ("aaa", claim("k", 100, -1))]);
        assert_eq!(
            holder_of(&settle_at(&r, -1, 300), "k").as_deref(),
            Some("aaa")
        );
    }

    #[test]
    fn done_beats_any_live_claim() {
        let mut r = records(&[("a", claim("k", 100, -1))]);
        r.dones.push(("b".into(), done("k", 500)));
        assert_eq!(
            settle_at(&r, -1, 600).get("k"),
            Some(&KeyState::Done {
                author: "b".into(),
                at: 500
            })
        );
    }

    #[test]
    fn a_claim_lapses_after_its_time_with_slack() {
        // Renewed every 60 s, 3 missed allowed, 30 s of slack: 210 s.
        let r = records(&[("a", claim("k", 0, -1))]);
        assert!(holder_of(&settle_at(&r, -1, 210 * SEC), "k").is_some());
        assert!(!settle_at(&r, -1, 211 * SEC).contains_key("k"));
    }

    #[test]
    fn done_is_forgotten_after_keep_done() {
        let mut r = Records::default();
        r.dones.push(("a".into(), done("k", 1000)));
        let keep = settings(1).keep_done_secs * SEC;
        assert!(settle_at(&r, 1, 1000 + keep).contains_key("k"));
        assert!(!settle_at(&r, 1, 1000 + keep + 1).contains_key("k"));
    }

    #[test]
    fn a_removed_members_done_still_counts_but_their_claim_does_not() {
        let mut p = people();
        p.members.remove("c");
        p.retired.insert("c".into());
        let mut r = records(&[("c", claim("live", 100, -1))]);
        r.dones.push(("c".into(), done("finished", 100)));
        let s = settle(&r, &p, &settings(-1), 200, Duration::from_secs(30));
        assert!(!s.contains_key("live"));
        assert!(matches!(s.get("finished"), Some(KeyState::Done { .. })));
        // Someone who was never a member gets neither.
        let r = Records {
            dones: vec![("stranger".into(), done("x", 100))],
            claims: vec![("stranger".into(), claim("y", 100, -1))],
            ..Default::default()
        };
        assert!(settle(&r, &p, &settings(-1), 200, Duration::ZERO).is_empty());
    }

    #[test]
    fn a_readers_entries_are_ignored() {
        let mut r = records(&[("reader", claim("k", 100, -1))]);
        r.dones.push(("reader".into(), done("j", 100)));
        assert!(settle_at(&r, -1, 200).is_empty());
    }

    #[test]
    fn newest_claims_over_the_limit_are_ignored() {
        let r = records(&[
            ("a", claim("k3", 300, 2)),
            ("a", claim("k1", 100, 2)),
            ("a", claim("k2", 200, 2)),
        ]);
        let s = settle_at(&r, 2, 400);
        assert_eq!(holder_of(&s, "k1").as_deref(), Some("a"));
        assert_eq!(holder_of(&s, "k2").as_deref(), Some("a"));
        assert!(!s.contains_key("k3"));
    }

    #[test]
    fn lowering_the_limit_takes_nothing_away() {
        let r = records(&[("a", claim("k1", 100, 2)), ("a", claim("k2", 200, 2))]);
        let s = settle_at(&r, 1, 400);
        assert!(holder_of(&s, "k1").is_some() && holder_of(&s, "k2").is_some());
    }

    #[test]
    fn a_claim_made_under_zero_is_ignored() {
        let r = records(&[("a", claim("k", 100, 0))]);
        assert!(settle_at(&r, 0, 200).is_empty());
    }

    #[test]
    fn no_limit_means_no_limit() {
        let claims: Vec<(&str, ClaimRecord)> = (0..50)
            .map(|i| ("a", claim(&format!("k{i}"), 100 + i, -1)))
            .collect();
        let s = settle_at(&records(&claims), -1, 1000);
        assert_eq!(s.len(), 50);
        assert!(s.values().all(|v| matches!(v, KeyState::Held { .. })));
    }

    #[test]
    fn a_release_only_ends_the_claim_it_names() {
        let mut r = records(&[("a", claim("k", 500, -1))]);
        r.releases.push(("a".into(), release("a", "k", 100)));
        assert!(holder_of(&settle_at(&r, -1, 600), "k").is_some());
    }

    #[test]
    fn a_person_can_release_their_other_machine() {
        let mut r = records(&[("a", claim("k", 100, -1))]);
        r.releases.push(("a2".into(), release("a", "k", 100)));
        assert!(!settle_at(&r, -1, 200).contains_key("k"));
    }

    #[test]
    fn an_owner_can_release_anyones() {
        let mut r = records(&[("a", claim("k", 100, -1))]);
        r.releases.push(("boss".into(), release("a", "k", 100)));
        assert!(!settle_at(&r, -1, 200).contains_key("k"));
    }

    #[test]
    fn a_writer_cannot_release_someone_elses() {
        let mut r = records(&[("a", claim("k", 100, -1))]);
        r.releases.push(("b".into(), release("a", "k", 100)));
        assert_eq!(
            holder_of(&settle_at(&r, -1, 200), "k").as_deref(),
            Some("a")
        );
    }

    #[test]
    fn a_done_key_frees_its_machines_slot() {
        let mut r = records(&[("a", claim("k1", 100, 1)), ("a", claim("k2", 200, 1))]);
        r.dones.push(("a".into(), done("k1", 150)));
        let s = settle_at(&r, 1, 300);
        assert!(matches!(s.get("k1"), Some(KeyState::Done { .. })));
        assert_eq!(holder_of(&s, "k2").as_deref(), Some("a"));
    }

    fn reopen(key: &str, at: u64) -> ReopenRecord {
        ReopenRecord {
            context: "inbox".into(),
            key: key.into(),
            at,
        }
    }

    #[test]
    fn a_reopen_frees_a_finished_key() {
        let mut r = Records::default();
        r.dones.push(("a".into(), done("k", 10 * SEC)));
        r.reopens.push(("b".into(), reopen("k", 20 * SEC)));
        assert!(!settle_at(&r, -1, 30 * SEC).contains_key("k"));
    }

    #[test]
    fn a_done_after_a_reopen_counts() {
        let mut r = Records::default();
        r.dones.push(("a".into(), done("k", 10 * SEC)));
        r.reopens.push(("b".into(), reopen("k", 20 * SEC)));
        r.dones.push(("c".into(), done("k", 30 * SEC)));
        assert_eq!(
            settle_at(&r, -1, 40 * SEC).get("k"),
            Some(&KeyState::Done {
                author: "c".into(),
                at: 30 * SEC
            })
        );
    }

    #[test]
    fn a_done_at_the_reopens_time_is_covered() {
        let mut r = Records::default();
        r.dones.push(("a".into(), done("k", 20 * SEC)));
        r.reopens.push(("b".into(), reopen("k", 20 * SEC)));
        assert!(!settle_at(&r, -1, 30 * SEC).contains_key("k"));
    }

    #[test]
    fn a_reopen_ends_claims_made_before_it() {
        let mut c = claim("k", 5 * SEC, -1);
        c.renewed_at = 30 * SEC;
        let mut r = records(&[("a", c)]);
        r.dones.push(("a".into(), done("k", 10 * SEC)));
        r.reopens.push(("b".into(), reopen("k", 20 * SEC)));
        assert!(!settle_at(&r, -1, 30 * SEC).contains_key("k"));
        r.claims.push(("b".into(), claim("k", 25 * SEC, -1)));
        assert_eq!(
            holder_of(&settle_at(&r, -1, 30 * SEC), "k").as_deref(),
            Some("b")
        );
    }

    #[test]
    fn a_readers_or_strangers_reopen_changes_nothing() {
        for who in ["reader", "stranger"] {
            let mut r = Records::default();
            r.dones.push(("a".into(), done("k", 10 * SEC)));
            r.reopens.push((who.into(), reopen("k", 20 * SEC)));
            assert!(matches!(
                settle_at(&r, -1, 30 * SEC).get("k"),
                Some(KeyState::Done { .. })
            ));
        }
    }

    #[test]
    fn a_retired_machines_reopen_still_counts() {
        let mut p = people();
        p.members.remove("c");
        p.retired.insert("c".into());
        let mut r = Records::default();
        r.dones.push(("a".into(), done("k", 10 * SEC)));
        r.reopens.push(("c".into(), reopen("k", 20 * SEC)));
        let s = settle(&r, &p, &settings(-1), 30 * SEC, Duration::from_secs(30));
        assert!(!s.contains_key("k"));
    }

    #[test]
    fn only_the_newest_reopen_matters() {
        let mut r = Records::default();
        r.reopens.push(("b".into(), reopen("k", 20 * SEC)));
        r.dones.push(("a".into(), done("k", 30 * SEC)));
        r.reopens.push(("b".into(), reopen("k", 40 * SEC)));
        assert!(!settle_at(&r, -1, 50 * SEC).contains_key("k"));
    }

    #[test]
    fn a_covered_claim_does_not_count_against_the_limit() {
        let mut r = records(&[
            ("a", claim("k1", 100 * SEC, 1)),
            ("a", claim("k2", 200 * SEC, 1)),
        ]);
        r.reopens.push(("b".into(), reopen("k1", 150 * SEC)));
        let s = settle_at(&r, 1, 200 * SEC);
        assert!(!s.contains_key("k1"));
        assert_eq!(holder_of(&s, "k2").as_deref(), Some("a"));
    }

    #[test]
    fn a_reopen_is_for_its_own_key_only() {
        let mut r = Records::default();
        r.dones.push(("a".into(), done("k", 10 * SEC)));
        r.reopens.push(("b".into(), reopen("other", 20 * SEC)));
        assert!(matches!(
            settle_at(&r, -1, 30 * SEC).get("k"),
            Some(KeyState::Done { .. })
        ));
    }

    #[test]
    fn a_split_heals_to_one_holder() {
        let one = records(&[("b", claim("k", 100, -1)), ("c", claim("k", 150, -1))]);
        let other = records(&[("c", claim("k", 150, -1)), ("b", claim("k", 100, -1))]);
        let (x, y) = (settle_at(&one, -1, 300), settle_at(&other, -1, 300));
        assert_eq!(x, y);
        assert_eq!(holder_of(&x, "k").as_deref(), Some("b"));
    }

    #[test]
    fn take_says_go_paused_at_limit_held_done_and_take_up() {
        let mine = claim("mine", 100, 1);
        let theirs = claim("theirs", 100, 1);
        let mut states = BTreeMap::new();
        states.insert(
            "mine".to_string(),
            KeyState::Held {
                author: "me".into(),
                claim: mine.clone(),
            },
        );
        states.insert(
            "theirs".to_string(),
            KeyState::Held {
                author: "you".into(),
                claim: theirs.clone(),
            },
        );
        states.insert(
            "finished".to_string(),
            KeyState::Done {
                author: "you".into(),
                at: 9,
            },
        );
        let take = |key: &str, me: &str, max: i32| decide_take(&states, key, me, &settings(max));
        assert_eq!(take("fresh", "nobody", 1), Take::Go);
        assert_eq!(take("fresh", "me", 2), Take::Go);
        assert_eq!(take("fresh", "me", -1), Take::Go);
        assert_eq!(take("fresh", "me", 1), Take::AtLimit(1));
        assert_eq!(take("fresh", "me", 0), Take::Paused);
        assert_eq!(
            take("theirs", "me", 1),
            Take::Held {
                author: "you".into(),
                claim: theirs
            }
        );
        assert_eq!(
            take("finished", "me", 0),
            Take::Done {
                author: "you".into(),
                at: 9
            }
        );
        // Paused, but the claim is already mine: taking it up is allowed.
        assert_eq!(take("mine", "me", 0), Take::TakeUp(mine.clone()));
        assert_eq!(take("mine", "me", 1), Take::TakeUp(mine));
    }
}
