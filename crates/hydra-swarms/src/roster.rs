//! The membership roster: a swarm's people and their roles, folded from signed changes.
//!
//! Every change is signed by the person making it, and the roster is whatever the changes add up
//! to when taken in (time, signature) order, so every machine that has the same changes agrees on
//! it whatever order they arrived in.
//!
//! The time is the signer's to choose, so a change that takes someone out of a role (a removal,
//! a leaving, a demotion) carries what its signer held of that person's work: their changes, and
//! their machines' file entries and parent records. Their changes dated before it that it doesn't
//! list are ignored, so nobody can backdate a change to before they lost the standing to make it;
//! and a former writer's files count only as it lists them, so their past edits and deletes stay
//! as they were (up to its own time) and nothing they write afterwards gets in.
//!
//! Owners rank by seniority down the tree of who promoted whom (see [`Rank`]), and an owner can
//! demote or remove only owners junior to them, whatever the date. So two owners can never undo
//! each other, and a change taking someone out never drops a senior's changes.
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};

use crate::ids::{PersonId, PersonKey, from_hex, to_hex, verify};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Reader,
    Writer,
    Owner,
}

/// One of a person's machines in the swarm.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Machine {
    pub endpoint: String,
    pub name: String,
    /// The machine's iroh-docs author id.
    pub author: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub person: PersonId,
    pub name: String,
    pub role: Role,
    pub machines: Vec<Machine>,
}

/// What a change taking someone out of a role keeps of what they did before it, as its signer
/// held it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Kept {
    /// The signatures of their changes.
    #[serde(default)]
    pub changes: Vec<String>,
    /// Their machines' document entries.
    #[serde(default)]
    pub authors: Vec<KeptAuthor>,
}

/// One machine's file entries and parent records.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct KeptAuthor {
    pub author: String,
    #[serde(default)]
    pub files: Vec<KeptEntry>,
    #[serde(default)]
    pub records: Vec<KeptEntry>,
}

/// A document entry, by the path it is for.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct KeptEntry {
    pub path: String,
    #[serde(flatten)]
    pub version: Version,
}

/// An entry's time, in Unix microseconds, and content (None for a delete).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    pub at: u64,
    pub hash: Option<Hash>,
}

/// What still counts of a machine whose person is no longer a writer: its file entries and
/// parent records, by path, as the changes that took them off writing listed them.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Retired {
    pub machine: String,
    pub files: BTreeMap<String, Version>,
    pub records: BTreeMap<String, Version>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    Genesis {
        name: String,
        owner: Member,
    },
    AddMember {
        member: Member,
    },
    /// A demotion keeps the person's past work in `kept`; a promotion leaves it empty.
    SetRole {
        person: PersonId,
        role: Role,
        #[serde(default)]
        kept: Kept,
    },
    Remove {
        person: PersonId,
        #[serde(default)]
        kept: Kept,
    },
    Leave {
        person: PersonId,
        #[serde(default)]
        kept: Kept,
    },
    Rename {
        person: PersonId,
        name: String,
    },
    RenameMachine {
        person: PersonId,
        endpoint: String,
        name: String,
    },
    RenameSwarm {
        name: String,
    },
    SetTypes {
        types: Vec<String>,
    },
}

impl Change {
    /// The person this change may take out of a role, and what it keeps of their work.
    pub fn retiring(&self) -> Option<(&PersonId, &Kept)> {
        match self {
            Change::SetRole { person, role, kept } if *role < Role::Owner => Some((person, kept)),
            Change::Remove { person, kept } | Change::Leave { person, kept } => {
                Some((person, kept))
            }
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SignedChange {
    pub change: Change,
    pub by: PersonId,
    /// Unix microseconds.
    pub at: u64,
    /// Hex.
    pub sig: String,
}

/// The bytes a change is signed over. The swarm is part of them, so a change can't be replayed
/// into another swarm.
fn message(swarm: &str, change: &Change, by: &PersonId, at: u64) -> Vec<u8> {
    serde_json::to_vec(&(swarm, change, by, at)).expect("a change always serialises")
}

impl SignedChange {
    /// `change` signed by `key`, made at `at`, for the swarm whose namespace id is `swarm`.
    pub fn sign(swarm: &str, change: Change, key: &PersonKey, at: u64) -> SignedChange {
        let by = key.id();
        let sig = to_hex(&key.sign(&message(swarm, &change, &by, at)));
        SignedChange {
            change,
            by,
            at,
            sig,
        }
    }

    pub fn verify(&self, swarm: &str) -> bool {
        from_hex(&self.sig).is_some_and(|sig| {
            verify(
                &self.by,
                &message(swarm, &self.change, &self.by, self.at),
                &sig,
            )
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Roster {
    pub name: String,
    pub members: Vec<Member>,
    pub types: Vec<String>,
    /// People taken out of the swarm (removed or left), until an owner adds them back.
    pub removed: Vec<PersonId>,
    /// By author, the machines of people who were writers and no longer are.
    pub retired: BTreeMap<String, Retired>,
    /// Every machine id (author and endpoint) ever in the swarm, and whose it is.
    pub ids: BTreeMap<String, PersonId>,
    /// Each owner's seniority: lower is more senior (see [`Rank`]).
    pub ranks: BTreeMap<PersonId, Rank>,
    /// Who promoted each owner but the anchor, with that promotion's (at, sig). Kept for an
    /// owner's promotees while that owner is out, so their ranks follow if they come back.
    pub promoters: BTreeMap<PersonId, (PersonId, u64, String)>,
    /// For each person, every change that took them out of a role, oldest first.
    pub outs: BTreeMap<PersonId, Vec<Out>>,
}

/// A change that took someone out of a role.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Out {
    pub at: u64,
    pub sig: String,
    /// They took themselves out (left or stepped down), so it is only their word.
    pub by_self: bool,
}

/// An owner's place in the promotion tree: the (at, sig) of each promotion from the anchor down
/// to theirs. The anchor's is empty, so it ranks first; an owner ranks below whoever promoted
/// them, and owners promoted by the same owner rank by those promotions, which the promoter
/// signed. Compared as a list, so a prefix ranks first.
pub type Rank = Vec<(u64, String)>;

/// Most signed changes are a few hundred bytes, but one taking a writer off writing lists their
/// files, a few hundred bytes each; anything far bigger in the log is not a change.
pub const MAX_CHANGE_BYTES: u64 = 8 << 20;

/// File types synced until an owner chooses others.
const DEFAULT_TYPES: [&str; 1] = [".md"];

/// The longest name (of a person, a machine or a swarm) a change may set.
pub const MAX_NAME_CHARS: usize = 200;

/// Whether `name` may be a person's, a machine's or a swarm's: not blank, and not too long.
pub fn name_fits(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= MAX_NAME_CHARS
}

/// Lowercase, leading dot, no duplicates, no empties, sorted.
fn normalise_types(types: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in types {
        let t = t.trim().trim_start_matches('.').to_lowercase();
        if t.is_empty() {
            continue;
        }
        let t = format!(".{t}");
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out.sort();
    out
}

/// Keeps `v` for `path` unless a newer version is there already.
fn keep_newest(map: &mut BTreeMap<String, Version>, path: &str, v: Version) {
    if map.get(path).is_none_or(|old| v > *old) {
        map.insert(path.to_string(), v);
    }
}

impl Roster {
    /// Folds every change, in (at, sig) order, from the genesis signed by `anchor`. Changes that
    /// break the rules (a bad signature, someone without the standing to make them) are skipped.
    ///
    /// A change that takes someone out of a role voids their changes since someone else last
    /// took them out of one, dated before it and not listed in it, as long as it is effective:
    /// folded without those changes, its signer still has the standing to take them out. So
    /// nobody can backdate a change to before they lost the standing to make it, and taking
    /// themselves out first doesn't help: that is one more of their changes to void.
    ///
    /// Which changes are void is never remembered, only derived: each round folds without the
    /// changes the last round found void and finds them again from the retirements effective in
    /// that fold, until a round finds the same set. Owners are only taken out by owners senior
    /// to them, and seniority comes from promotions signed by seniors, so whether a retirement
    /// is effective depends only on what more senior people's retirements void. Each round
    /// settles at least one more level of the promotion tree, so the rounds end.
    pub fn fold(swarm: &str, anchor: &PersonId, changes: &[SignedChange]) -> Option<Roster> {
        let mut ordered: Vec<&SignedChange> = changes.iter().filter(|c| c.verify(swarm)).collect();
        ordered.sort_by(|a, b| (a.at, &a.sig).cmp(&(b.at, &b.sig)));
        ordered.dedup_by(|a, b| a.sig == b.sig);
        let mut seen: Vec<BTreeSet<usize>> = Vec::new();
        let mut void: BTreeSet<usize> = BTreeSet::new();
        loop {
            let (roster, found) = Roster::round(anchor, &ordered, &void);
            if found == void {
                return roster;
            }
            if let Some(k) = seen.iter().position(|v| *v == found) {
                // Not expected (see above), but should rounds ever cycle, a change is void
                // only if every set in the cycle voids it.
                let mut cycle = seen.split_off(k);
                cycle.push(void);
                let first = cycle[0].clone();
                let void = cycle.iter().fold(first, |a, v| &a & v);
                return Roster::round(anchor, &ordered, &void).0;
            }
            seen.push(std::mem::replace(&mut void, found));
        }
    }

    /// Folds `ordered` without `void`, and gives the changes the retirements effective in that
    /// fold void.
    fn round(
        anchor: &PersonId,
        ordered: &[&SignedChange],
        void: &BTreeSet<usize>,
    ) -> (Option<Roster>, BTreeSet<usize>) {
        let mut found = BTreeSet::new();
        let mut roster: Option<Roster> = None;
        for (i, c) in ordered.iter().enumerate() {
            if void.contains(&i) {
                continue;
            }
            let Some(r) = &mut roster else {
                roster = Roster::genesis(anchor, c);
                continue;
            };
            if let Some(unlisted) = r.unlisted(&ordered[..i], c) {
                let mut without = void.clone();
                without.extend(unlisted.iter().copied());
                let before = Roster::fold_plain(anchor, &ordered[..i], &without);
                if before.is_some_and(|b| b.retires(c)) {
                    found.extend(unlisted);
                }
            }
            r.apply(c);
        }
        (roster, found)
    }

    /// When `c` would take someone out of a role, the positions of their changes in `earlier`
    /// since they were last taken out of one that `c` doesn't list, if any.
    fn unlisted(&self, earlier: &[&SignedChange], c: &SignedChange) -> Option<Vec<usize>> {
        let (person, kept) = c.change.retiring()?;
        let listed: HashSet<&str> = kept.changes.iter().map(String::as_str).collect();
        // Their changes before the latest time someone else took them out were settled then.
        // Taking themselves out is only their word, so it shields nothing unless `c` lists it,
        // and is otherwise one more change to drop.
        let since = self.outs.get(person).and_then(|outs| {
            outs.iter()
                .rev()
                .find(|o| !o.by_self || listed.contains(o.sig.as_str()))
                .map(|o| (o.at, &o.sig))
        });
        let unlisted: Vec<usize> = earlier
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.by == *person
                    && !matches!(d.change, Change::Genesis { .. })
                    && since.is_none_or(|(at, sig)| (d.at, &d.sig) > (at, sig))
                    && !listed.contains(d.sig.as_str())
            })
            .map(|(j, _)| j)
            .collect();
        (!unlisted.is_empty()).then_some(unlisted)
    }

    /// `changes` folded in the order given, leaving out the positions in `void`.
    fn fold_plain(
        anchor: &PersonId,
        changes: &[&SignedChange],
        void: &BTreeSet<usize>,
    ) -> Option<Roster> {
        let mut roster: Option<Roster> = None;
        let kept = changes
            .iter()
            .enumerate()
            .filter(|(i, _)| !void.contains(i));
        for (_, c) in kept {
            match &mut roster {
                None => roster = Roster::genesis(anchor, c),
                Some(r) => r.apply(c),
            }
        }
        roster
    }

    /// The roster a genesis starts, if `c` is the anchor's.
    fn genesis(anchor: &PersonId, c: &SignedChange) -> Option<Roster> {
        let Change::Genesis { name, owner } = &c.change else {
            return None;
        };
        if c.by != *anchor || owner.person != *anchor {
            return None;
        }
        let mut owner = owner.clone();
        owner.role = Role::Owner;
        let mut r = Roster {
            name: name.clone(),
            members: Vec::new(),
            types: DEFAULT_TYPES.iter().map(|t| t.to_string()).collect(),
            removed: Vec::new(),
            retired: BTreeMap::new(),
            ids: BTreeMap::new(),
            ranks: BTreeMap::from([(owner.person, Rank::new())]),
            promoters: BTreeMap::new(),
            outs: BTreeMap::new(),
        };
        r.admit(owner);
        Some(r)
    }

    /// Whether `c` would take its person out of the role they have now.
    fn retires(&self, c: &SignedChange) -> bool {
        let Some((person, _)) = c.change.retiring() else {
            return false;
        };
        let mut after = self.clone();
        after.apply(c);
        after.role_of(person) < self.role_of(person)
    }

    /// Whether `a` is an owner senior to `b`, an owner too.
    pub fn outranks(&self, a: &PersonId, b: &PersonId) -> bool {
        match (self.ranks.get(a), self.ranks.get(b)) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        }
    }

    /// Whether `by`, an owner, may lower `person`'s role: anyone below owner, an owner junior
    /// to them, or themselves.
    fn may_lower(&self, by: &PersonId, person: &PersonId) -> bool {
        self.role_of(by) == Some(Role::Owner)
            && (self.role_of(person) != Some(Role::Owner)
                || by == person
                || self.outranks(by, person))
    }

    /// Makes `person`'s rank the one `c`'s signer promotes them to, and moves the owners they
    /// promoted (and theirs) down with them.
    fn promoted(&mut self, person: PersonId, c: &SignedChange) {
        // The promoter and those above it stay where they are, even if `person` once promoted
        // them: the newest promotion wins, so that link no longer moves them.
        let mut above = HashSet::new();
        let mut at = Some(c.by);
        while let Some(p) = at.filter(|p| above.insert(*p)) {
            at = self.promoters.get(&p).map(|(by, ..)| *by);
        }
        self.promoters
            .retain(|p, (by, ..)| !(*by == person && above.contains(p)));
        let mut rank = self.ranks.get(&c.by).cloned().unwrap_or_default();
        rank.push((c.at, c.sig.clone()));
        self.ranks.insert(person, rank);
        self.promoters.insert(person, (c.by, c.at, c.sig.clone()));
        let mut moved = HashSet::from([person]);
        let mut queue = VecDeque::from([person]);
        while let Some(p) = queue.pop_front() {
            let base = self.ranks[&p].clone();
            let below: Vec<(PersonId, u64, String)> = self
                .promoters
                .iter()
                .filter(|(x, (by, ..))| *by == p && self.ranks.contains_key(x))
                .map(|(x, (_, at, sig))| (*x, *at, sig.clone()))
                .collect();
            for (x, at, sig) in below {
                if moved.insert(x) {
                    let mut rank = base.clone();
                    rank.push((at, sig));
                    self.ranks.insert(x, rank);
                    queue.push_back(x);
                }
            }
        }
    }

    fn apply(&mut self, c: &SignedChange) {
        let by_owner = self.role_of(&c.by) == Some(Role::Owner);
        // Whom this change may take out of a role, with their role and machines as they are now.
        let retiring = c.change.retiring().and_then(|(person, kept)| {
            let m = self.members.iter().find(|m| m.person == *person)?;
            Some((*person, m.role, m.machines.clone(), kept))
        });
        match &c.change {
            Change::Genesis { .. } => {}
            Change::AddMember { member } => {
                if by_owner && self.role_of(&member.person).is_none() && !self.taken(member) {
                    self.removed.retain(|p| *p != member.person);
                    self.admit(member.clone());
                    if member.role == Role::Owner {
                        self.promoted(member.person, c);
                    }
                }
            }
            Change::SetRole { person, role, .. } => match self.role_of(person) {
                // Making an owner an owner again would only move them down the ranks.
                Some(Role::Owner) if *role == Role::Owner => {}
                Some(_) if *role == Role::Owner && by_owner => {
                    self.member_mut(person).role = Role::Owner;
                    self.promoted(*person, c);
                }
                Some(_) if self.may_lower(&c.by, person) && !self.is_last_owner(person) => {
                    self.member_mut(person).role = *role;
                }
                _ => {}
            },
            Change::Remove { person, .. } => {
                if self.may_lower(&c.by, person) {
                    self.take_out(person);
                }
            }
            Change::Leave { person, .. } => {
                if c.by == *person {
                    self.take_out(person);
                }
            }
            Change::Rename { person, name } => {
                let allowed = c.by == *person || by_owner;
                if allowed && self.role_of(person).is_some() && name_fits(name) {
                    self.member_mut(person).name = name.clone();
                }
            }
            Change::RenameMachine {
                person,
                endpoint,
                name,
            } => {
                if (c.by == *person || by_owner) && name_fits(name) {
                    let machine = self
                        .members
                        .iter_mut()
                        .find(|m| m.person == *person)
                        .and_then(|m| m.machines.iter_mut().find(|x| x.endpoint == *endpoint));
                    if let Some(machine) = machine {
                        machine.name = name.clone();
                    }
                }
            }
            Change::RenameSwarm { name } => {
                if by_owner && name_fits(name) {
                    self.name = name.clone();
                }
            }
            Change::SetTypes { types } => {
                if by_owner {
                    self.types = normalise_types(types);
                }
            }
        }
        let Some((person, before, machines, kept)) = retiring else {
            return;
        };
        let now = self.role_of(&person);
        if now >= Some(before) {
            return;
        }
        self.outs.entry(person).or_default().push(Out {
            at: c.at,
            sig: c.sig.clone(),
            by_self: c.by == person,
        });
        if now != Some(Role::Owner) {
            self.ranks.remove(&person);
            self.promoters.remove(&person);
        }
        if before >= Role::Writer && now.is_none_or(|r| r < Role::Writer) {
            self.retire(&machines, kept, c.at);
        }
    }

    /// Adds a member and claims their machines' ids for them.
    fn admit(&mut self, member: Member) {
        for m in &member.machines {
            for id in [&m.author, &m.endpoint] {
                self.ids.insert(id.clone(), member.person);
            }
        }
        self.members.push(member);
    }

    /// Whether one of `member`'s machine ids is, or was, someone else's.
    pub fn taken(&self, member: &Member) -> bool {
        member.machines.iter().any(|m| {
            [&m.author, &m.endpoint]
                .into_iter()
                .any(|id| self.ids.get(id).is_some_and(|p| *p != member.person))
        })
    }

    /// Keeps what `kept` lists for `machines`, whose person stopped being a writer at `at`;
    /// entries dated after that can't have been made while they were one.
    fn retire(&mut self, machines: &[Machine], kept: &Kept, at: u64) {
        for m in machines {
            let retired = self.retired.entry(m.author.clone()).or_default();
            retired.machine = m.name.clone();
            for k in kept.authors.iter().filter(|k| k.author == m.author) {
                for e in k.files.iter().filter(|e| e.version.at <= at) {
                    keep_newest(&mut retired.files, &e.path, e.version);
                }
                for e in k.records.iter().filter(|e| e.version.at <= at) {
                    keep_newest(&mut retired.records, &e.path, e.version);
                }
            }
        }
    }

    fn member_mut(&mut self, p: &PersonId) -> &mut Member {
        self.members
            .iter_mut()
            .find(|m| m.person == *p)
            .expect("checked to be a member")
    }

    fn is_last_owner(&self, p: &PersonId) -> bool {
        self.role_of(p) == Some(Role::Owner)
            && self
                .members
                .iter()
                .filter(|m| m.role == Role::Owner)
                .count()
                == 1
    }

    /// Takes a member out and remembers it, unless that would leave the swarm without an owner.
    fn take_out(&mut self, p: &PersonId) {
        if self.role_of(p).is_some() && !self.is_last_owner(p) {
            self.members.retain(|m| m.person != *p);
            self.removed.push(*p);
        }
    }

    pub fn role_of(&self, p: &PersonId) -> Option<Role> {
        self.members.iter().find(|m| m.person == *p).map(|m| m.role)
    }

    /// Whether `author` is a machine of a current writer or owner.
    pub fn writes(&self, author: &str) -> bool {
        self.member_by_author(author)
            .is_some_and(|m| m.role >= Role::Writer)
    }

    pub fn member_by_author(&self, author: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|m| m.machines.iter().any(|x| x.author == author))
    }

    pub fn member_by_endpoint(&self, endpoint: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|m| m.machines.iter().any(|x| x.endpoint == endpoint))
    }

    /// Every current member machine's endpoint.
    pub fn endpoints(&self) -> Vec<String> {
        self.members
            .iter()
            .flat_map(|m| m.machines.iter().map(|x| x.endpoint.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWARM: &str = "swarm-1";

    fn member(key: &PersonKey, name: &str, role: Role) -> Member {
        let id = key.id().to_string();
        Member {
            person: key.id(),
            name: name.into(),
            role,
            machines: vec![Machine {
                endpoint: format!("ep-{name}"),
                name: format!("{name}-pc"),
                author: format!("au-{}", &id[..6]),
            }],
        }
    }

    fn genesis(owner: &PersonKey) -> SignedChange {
        let change = Change::Genesis {
            name: "Notes".into(),
            owner: member(owner, "olive", Role::Owner),
        };
        SignedChange::sign(SWARM, change, owner, 1)
    }

    fn sign(key: &PersonKey, at: u64, change: Change) -> SignedChange {
        SignedChange::sign(SWARM, change, key, at)
    }

    fn add(by: &PersonKey, at: u64, who: &PersonKey, name: &str, role: Role) -> SignedChange {
        sign(
            by,
            at,
            Change::AddMember {
                member: member(who, name, role),
            },
        )
    }

    fn set_role(by: &PersonKey, at: u64, who: &PersonKey, role: Role) -> SignedChange {
        sign(
            by,
            at,
            Change::SetRole {
                person: who.id(),
                role,
                kept: Kept::default(),
            },
        )
    }

    /// `who`'s changes in `log`, as a change taking them out of a role lists them.
    fn changes_of(log: &[SignedChange], who: &PersonKey) -> Kept {
        Kept {
            changes: log
                .iter()
                .filter(|c| c.by == who.id())
                .map(|c| c.sig.clone())
                .collect(),
            authors: Vec::new(),
        }
    }

    fn rename(by: &PersonKey, at: u64, who: &PersonKey, name: &str) -> SignedChange {
        sign(
            by,
            at,
            Change::Rename {
                person: who.id(),
                name: name.into(),
            },
        )
    }

    fn remove(by: &PersonKey, at: u64, who: &PersonKey) -> SignedChange {
        let kept = Kept::default();
        sign(
            by,
            at,
            Change::Remove {
                person: who.id(),
                kept,
            },
        )
    }

    fn leave(who: &PersonKey, at: u64) -> SignedChange {
        let kept = Kept::default();
        sign(
            who,
            at,
            Change::Leave {
                person: who.id(),
                kept,
            },
        )
    }

    fn rename_machine(by: &PersonKey, at: u64, who: &PersonKey, name: &str) -> SignedChange {
        let endpoint = "ep-wren".to_string();
        sign(
            by,
            at,
            Change::RenameMachine {
                person: who.id(),
                endpoint,
                name: name.into(),
            },
        )
    }

    fn rename_swarm(by: &PersonKey, at: u64, name: &str) -> SignedChange {
        sign(by, at, Change::RenameSwarm { name: name.into() })
    }

    fn fold(anchor: &PersonKey, log: &[SignedChange]) -> Option<Roster> {
        Roster::fold(SWARM, &anchor.id(), log)
    }

    #[test]
    fn a_genesis_by_the_anchor_makes_its_signer_the_owner() {
        let o = PersonKey::generate();
        let r = fold(&o, &[genesis(&o)]).unwrap();
        assert_eq!(r.name, "Notes");
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert_eq!(r.types, vec![".md"]);
        assert_eq!(r.endpoints(), vec!["ep-olive"]);
    }

    #[test]
    fn a_genesis_by_anyone_else_is_ignored() {
        let o = PersonKey::generate();
        let mallory = PersonKey::generate();
        let fake = sign(
            &mallory,
            0,
            Change::Genesis {
                name: "Mine".into(),
                owner: member(&mallory, "mallory", Role::Owner),
            },
        );
        assert!(fold(&o, std::slice::from_ref(&fake)).is_none());
        // The anchor's own genesis still wins, though it is later than the fake one.
        let r = fold(&o, &[fake, genesis(&o)]).unwrap();
        assert_eq!(r.name, "Notes");
        assert_eq!(r.role_of(&mallory.id()), None);
        // A genesis by the anchor that names someone else as the owner doesn't count either.
        let odd = sign(
            &o,
            1,
            Change::Genesis {
                name: "Odd".into(),
                owner: member(&mallory, "mallory", Role::Owner),
            },
        );
        assert!(fold(&o, &[odd]).is_none());
    }

    #[test]
    fn only_owners_add_members() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let x = PersonKey::generate();
        let log = vec![
            genesis(&o),
            add(&o, 2, &w, "wren", Role::Writer),
            add(&w, 3, &x, "xan", Role::Reader),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&w.id()), Some(Role::Writer));
        assert_eq!(r.role_of(&x.id()), None);
    }

    #[test]
    fn an_owner_can_add_another_owner() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let x = PersonKey::generate();
        let log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&p, 3, &x, "xan", Role::Writer),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&p.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&x.id()), Some(Role::Writer));
    }

    #[test]
    fn a_writer_cannot_remove_anyone() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let x = PersonKey::generate();
        let log = vec![
            genesis(&o),
            add(&o, 2, &w, "wren", Role::Writer),
            add(&o, 3, &x, "xan", Role::Reader),
            remove(&w, 4, &x),
            remove(&w, 5, &o),
            set_role(&w, 6, &w, Role::Owner),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.members.len(), 3);
        assert_eq!(r.role_of(&w.id()), Some(Role::Writer));
        assert!(r.removed.is_empty());
    }

    #[test]
    fn the_last_owner_cannot_be_demoted_removed_or_leave() {
        let o = PersonKey::generate();
        let log = vec![
            genesis(&o),
            set_role(&o, 2, &o, Role::Reader),
            remove(&o, 3, &o),
            leave(&o, 4),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert!(r.removed.is_empty());

        // With a second owner the first may step down.
        let p = PersonKey::generate();
        let log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            leave(&o, 3),
            leave(&p, 4),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&o.id()), None);
        assert_eq!(r.role_of(&p.id()), Some(Role::Owner));
        assert_eq!(r.removed, vec![o.id()]);
    }

    #[test]
    fn a_removed_writer_readded_as_reader_is_a_reader() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &w, "wren", Role::Writer),
            remove(&o, 3, &w),
            // Ignored while removed.
            rename(&w, 4, &w, "ghost"),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&w.id()), None);
        assert_eq!(r.removed, vec![w.id()]);

        log.push(add(&o, 5, &w, "wren", Role::Reader));
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&w.id()), Some(Role::Reader));
        assert!(r.removed.is_empty());
    }

    #[test]
    fn members_rename_themselves_and_readers_cannot_rename_others() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let x = PersonKey::generate();
        let log = vec![
            genesis(&o),
            add(&o, 2, &w, "wren", Role::Writer),
            add(&o, 3, &x, "xan", Role::Reader),
            rename(&x, 4, &x, "Xander"),
            rename(&x, 5, &w, "nope"),
            rename_machine(&w, 6, &w, "laptop"),
            rename_machine(&x, 7, &w, "nope"),
            rename(&o, 8, &w, "Wren W"),
            rename_swarm(&x, 9, "nope"),
        ];
        let r = fold(&o, &log).unwrap();
        let name_of = |p: &PersonKey| {
            let m = r.members.iter().find(|m| m.person == p.id());
            m.unwrap().name.clone()
        };
        assert_eq!(name_of(&x), "Xander");
        assert_eq!(name_of(&w), "Wren W");
        let wren = r.member_by_endpoint("ep-wren").unwrap();
        assert_eq!(wren.machines[0].name, "laptop");
        assert_eq!(r.name, "Notes");
    }

    #[test]
    fn a_change_with_a_bad_signature_is_skipped() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let mut altered = add(&o, 2, &w, "wren", Role::Writer);
        altered.at = 3;
        assert!(!altered.verify(SWARM));
        let mut borrowed = add(&o, 4, &w, "wren", Role::Writer);
        borrowed.sig = rename_swarm(&w, 4, "x").sig;
        assert!(!borrowed.verify(SWARM));
        let elsewhere =
            SignedChange::sign("swarm-2", Change::RenameSwarm { name: "x".into() }, &o, 5);
        assert!(!elsewhere.verify(SWARM));
        let r = fold(&o, &[genesis(&o), altered, borrowed, elsewhere]).unwrap();
        assert_eq!(r.members.len(), 1);
        assert_eq!(r.name, "Notes");
        // A bad genesis leaves nothing to fold.
        let mut g = genesis(&o);
        g.sig = "zz".into();
        assert!(fold(&o, &[g]).is_none());
    }

    #[test]
    fn types_are_normalised() {
        let o = PersonKey::generate();
        let types = ["MD", ".Py", "", ".md"].map(String::from).to_vec();
        let r = fold(&o, &[genesis(&o), sign(&o, 2, Change::SetTypes { types })]).unwrap();
        assert_eq!(r.types, vec![".md", ".py"]);
    }

    /// Folds `log` shuffled many ways and checks every fold agrees; gives the roster.
    fn fold_in_any_order(anchor: &PersonKey, log: &[SignedChange]) -> Roster {
        let expected = fold(anchor, log).unwrap();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut shuffled = log.to_vec();
        for _ in 0..50 {
            for i in (1..shuffled.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                shuffled.swap(i, (state % (i as u64 + 1)) as usize);
            }
            assert_eq!(fold(anchor, &shuffled).as_ref(), Some(&expected));
        }
        expected
    }

    fn demote(by: &PersonKey, at: u64, who: &PersonKey, role: Role, kept: Kept) -> SignedChange {
        let person = who.id();
        sign(by, at, Change::SetRole { person, role, kept })
    }

    #[test]
    fn a_demoted_owner_cannot_backdate_a_change_to_take_the_swarm_back() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            rename(&p, 3, &p, "Pat"),
        ];
        log.push(demote(&o, 10, &p, Role::Writer, changes_of(&log, &p)));
        // Signed by pat after the demotion, dated before it.
        log.push(demote(&p, 9, &o, Role::Reader, changes_of(&log, &o)));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&p.id()), Some(Role::Writer));
        // What the demotion listed stays.
        assert_eq!(r.members[1].name, "Pat");
    }

    #[test]
    fn a_removed_owners_backdated_change_relayed_by_a_writer_is_ignored() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let w = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &w, "wren", Role::Writer),
        ];
        let kept = changes_of(&log, &p);
        log.push(sign(
            &o,
            10,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        // Pat, removed, signs these dated before the removal, listing everything of olive's;
        // wren writes them to the log.
        let kept = changes_of(&log, &o);
        log.push(sign(
            &p,
            8,
            Change::Remove {
                person: o.id(),
                kept,
            },
        ));
        log.push(set_role(&p, 9, &w, Role::Owner));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&w.id()), Some(Role::Writer));
        assert_eq!(r.role_of(&p.id()), None);
        assert_eq!(r.removed, vec![p.id()]);
    }

    #[test]
    fn a_leaver_cannot_backdate_a_change_either() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let mut log = vec![genesis(&o), add(&o, 2, &p, "pat", Role::Owner)];
        let kept = changes_of(&log, &p);
        log.push(sign(
            &p,
            10,
            Change::Leave {
                person: p.id(),
                kept,
            },
        ));
        log.push(set_role(&p, 9, &o, Role::Reader));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&p.id()), None);
    }

    #[test]
    fn changes_the_signer_had_not_seen_are_dropped_with_the_demotion() {
        // Two owners demote each other at about the same time, each unaware of the other's.
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let mut log = vec![genesis(&o), add(&o, 2, &p, "pat", Role::Owner)];
        let (by_o, by_p) = (changes_of(&log, &o), changes_of(&log, &p));
        log.push(demote(&p, 5, &o, Role::Writer, by_o));
        log.push(demote(&o, 6, &p, Role::Writer, by_p));
        let r = fold_in_any_order(&o, &log);
        // Pat's demotion, unseen by olive, is dropped by olive's later one.
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&p.id()), Some(Role::Writer));
    }

    fn entry(path: &str, at: u64, text: Option<&str>) -> KeptEntry {
        KeptEntry {
            path: path.into(),
            version: Version {
                at,
                hash: text.map(|t| Hash::new(t.as_bytes())),
            },
        }
    }

    fn kept_files(author: &str, files: Vec<KeptEntry>, records: Vec<KeptEntry>) -> Kept {
        Kept {
            changes: Vec::new(),
            authors: vec![KeptAuthor {
                author: author.into(),
                files,
                records,
            }],
        }
    }

    // The next four came from the re-review's probes.

    #[test]
    fn a_demoted_owner_cannot_retake_the_swarm_by_post_dating() {
        let o = PersonKey::generate();
        let q = PersonKey::generate();
        let p = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &q, "quin", Role::Owner),
            add(&o, 3, &p, "pat", Role::Owner),
        ];
        log.push(demote(&q, 10, &p, Role::Writer, changes_of(&log, &p)));
        // Pat, now a writer, signs after his demotion, listing nothing of quin's.
        log.push(demote(&p, 20, &q, Role::Reader, Kept::default()));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&p.id()), Some(Role::Writer));
        assert_eq!(r.role_of(&q.id()), Some(Role::Owner));
    }

    #[test]
    fn a_removed_owner_cannot_retake_the_swarm_by_post_dating() {
        let o = PersonKey::generate();
        let q = PersonKey::generate();
        let p = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &q, "quin", Role::Owner),
            add(&o, 3, &p, "pat", Role::Owner),
        ];
        let kept = changes_of(&log, &p);
        log.push(sign(
            &q,
            10,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        let kept = Kept::default();
        log.push(sign(
            &p,
            20,
            Change::Remove {
                person: q.id(),
                kept,
            },
        ));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&p.id()), None);
        assert_eq!(r.role_of(&q.id()), Some(Role::Owner));
    }

    #[test]
    fn nobody_can_demote_the_anchor() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let mut log = vec![genesis(&o), add(&o, 2, &p, "pat", Role::Owner)];
        log.push(demote(&p, 10, &o, Role::Writer, changes_of(&log, &o)));
        log.push(demote(&o, 20, &p, Role::Reader, Kept::default()));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&p.id()), Some(Role::Reader));
    }

    #[test]
    fn a_junior_cannot_duel_a_senior_owner() {
        let x = PersonKey::generate();
        let o = PersonKey::generate();
        let a = PersonKey::generate();
        let mut log = vec![
            genesis(&x),
            add(&x, 2, &o, "oscar", Role::Owner),
            add(&o, 5, &a, "alice", Role::Writer),
        ];
        // Oscar, junior to x, can't demote x at any date.
        log.push(demote(&o, 10, &x, Role::Writer, changes_of(&log, &x)));
        let r = fold(&x, &log).unwrap();
        assert_eq!(r.role_of(&x.id()), Some(Role::Owner));
        // x demotes oscar, dated before oscar's attempt, listing what x held: alice stays.
        log.push(demote(&x, 6, &o, Role::Reader, changes_of(&log, &o)));
        let r = fold_in_any_order(&x, &log);
        assert_eq!(r.role_of(&x.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&o.id()), Some(Role::Reader));
        assert_eq!(r.role_of(&a.id()), Some(Role::Writer));
    }

    #[test]
    fn a_member_the_remover_had_not_seen_added_is_dropped_with_their_adder() {
        // Indistinguishable from a backdated add, so it goes; the folder keeps any edit of
        // theirs a machine had applied as a flagged copy (tests/folders.rs).
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let a = PersonKey::generate();
        let mut log = vec![genesis(&o), add(&o, 2, &p, "pat", Role::Owner)];
        let kept = changes_of(&log, &p);
        log.push(add(&p, 5, &a, "alice", Role::Writer));
        log.push(sign(
            &o,
            10,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&a.id()), None);
        assert_eq!(r.role_of(&p.id()), None);
    }

    #[test]
    fn kept_entries_dated_after_the_retirement_are_dropped() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let wren = member(&w, "wren", Role::Writer);
        let au = wren.machines[0].author.clone();
        let kept = kept_files(
            &au,
            vec![
                entry("everyone.md", u64::MAX, None),
                entry("mine.md", 9, Some("mine")),
                entry("late.md", 11, Some("late")),
            ],
            vec![entry("everyone.md", u64::MAX, Some("r"))],
        );
        let log = vec![
            genesis(&o),
            sign(&o, 2, Change::AddMember { member: wren }),
            sign(
                &w,
                10,
                Change::Leave {
                    person: w.id(),
                    kept,
                },
            ),
        ];
        let r = fold_in_any_order(&o, &log);
        let kept = &r.retired[&au];
        assert_eq!(kept.files.keys().collect::<Vec<_>>(), vec!["mine.md"]);
        assert!(kept.records.is_empty());
    }

    #[test]
    fn owners_rank_down_the_promotion_tree() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let q = PersonKey::generate();
        let w = PersonKey::generate();
        let base = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &q, "quin", Role::Owner),
            add(&o, 4, &w, "wren", Role::Writer),
            // Promoted by pat, so wren ranks below pat and above quin, whom olive promoted
            // after pat.
            set_role(&p, 5, &w, Role::Owner),
        ];
        let demoted = |by: &PersonKey, who: &PersonKey| {
            let mut log = base.clone();
            let kept = changes_of(&log, who);
            log.push(demote(by, 9, who, Role::Writer, kept));
            fold(&o, &log).unwrap().role_of(&who.id())
        };
        assert_eq!(demoted(&q, &w), Some(Role::Owner), "quin is junior to wren");
        assert_eq!(demoted(&w, &q), Some(Role::Writer));
        assert_eq!(demoted(&w, &p), Some(Role::Owner), "wren is junior to pat");
        assert_eq!(demoted(&p, &w), Some(Role::Writer));
        assert_eq!(demoted(&q, &o), Some(Role::Owner));
        // Removal follows the same order; anyone may leave, and step down themselves.
        let mut log = base.clone();
        log.push(remove(&q, 9, &p));
        log.push(remove(&p, 10, &q));
        log.push(leave(&w, 11));
        log.push(set_role(&o, 12, &o, Role::Writer));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&p.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&q.id()), None);
        assert_eq!(r.role_of(&w.id()), None);
        assert_eq!(r.role_of(&o.id()), Some(Role::Writer));
    }

    // The next six came from the second re-review's probes.

    /// The changes a change lists, as their signatures.
    fn listing(changes: &[&SignedChange]) -> Kept {
        Kept {
            changes: changes.iter().map(|c| c.sig.clone()).collect(),
            authors: Vec::new(),
        }
    }

    fn step_down(who: &PersonKey, at: u64, kept: Kept) -> SignedChange {
        demote(who, at, who, Role::Writer, kept)
    }

    #[test]
    fn a_backdated_step_down_does_not_shield_a_removed_owners_backdated_add() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let a = PersonKey::generate();
        let mut log = vec![genesis(&o), add(&o, 3, &p, "pat", Role::Owner)];
        let kept = changes_of(&log, &p);
        log.push(sign(
            &o,
            10,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        // After his removal at 10, pat signs these, dated 7 and 8.
        let crony = add(&p, 7, &a, "alice", Role::Owner);
        let down = step_down(&p, 8, listing(&[&crony]));
        log.extend([crony, down]);
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&a.id()), None);
        assert_eq!(r.role_of(&p.id()), None);
    }

    #[test]
    fn a_backdated_leave_does_not_shield_a_removed_owners_backdated_add() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let a = PersonKey::generate();
        let mut log = vec![genesis(&o), add(&o, 3, &p, "pat", Role::Owner)];
        let kept = changes_of(&log, &p);
        log.push(sign(
            &o,
            10,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        let crony = add(&p, 7, &a, "alice", Role::Writer);
        let kept = listing(&[&crony]);
        let leave = sign(
            &p,
            8,
            Change::Leave {
                person: p.id(),
                kept,
            },
        );
        log.extend([crony, leave]);
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&a.id()), None);
    }

    #[test]
    fn a_backdated_step_down_does_not_shield_a_demoted_owners_backdated_removal() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let q = PersonKey::generate();
        let a = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &q, "quin", Role::Owner),
            add(&q, 5, &a, "alice", Role::Writer),
        ];
        log.push(demote(&o, 10, &p, Role::Writer, changes_of(&log, &p)));
        let kept = Kept::default();
        let hit = sign(
            &p,
            7,
            Change::Remove {
                person: q.id(),
                kept,
            },
        );
        let down = step_down(&p, 8, listing(&[&hit]));
        log.extend([hit, down]);
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&q.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&a.id()), Some(Role::Writer));
        assert_eq!(r.role_of(&p.id()), Some(Role::Writer));
    }

    #[test]
    fn an_honest_step_down_still_shields_against_a_later_removal() {
        // Pat steps down, listing everything; a junior who has seen that removes pat later,
        // listing only the step-down. Pat's owner-era add of alice stays.
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let j = PersonKey::generate();
        let a = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &j, "jo", Role::Owner),
            add(&p, 4, &a, "alice", Role::Writer),
        ];
        let down = step_down(&p, 5, changes_of(&log, &p));
        log.push(down.clone());
        let kept = listing(&[&down]);
        log.push(sign(
            &j,
            9,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&p.id()), None);
        assert_eq!(r.role_of(&a.id()), Some(Role::Writer));
    }

    #[test]
    fn a_re_promoted_owners_promotees_rank_below_them() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let q = PersonKey::generate();
        let ray = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &q, "quin", Role::Owner),
            add(&p, 4, &ray, "ray", Role::Owner),
        ];
        log.push(step_down(&p, 5, changes_of(&log, &p)));
        log.push(set_role(&q, 6, &p, Role::Owner));
        let r = fold_in_any_order(&o, &log);
        assert!(r.outranks(&q.id(), &p.id()));
        assert!(r.outranks(&p.id(), &ray.id()));
        assert!(r.outranks(&q.id(), &ray.id()));
        // Ray, below pat again, can't demote pat; pat can demote ray.
        let mut tried = log.clone();
        tried.push(demote(&ray, 9, &p, Role::Writer, changes_of(&log, &p)));
        assert_eq!(
            fold(&o, &tried).unwrap().role_of(&p.id()),
            Some(Role::Owner)
        );
    }

    #[test]
    fn a_re_promoted_owner_cannot_backdate_a_promotion_to_when_he_was_senior() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let q = PersonKey::generate();
        let c = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &q, "quin", Role::Owner),
        ];
        log.push(step_down(&p, 5, changes_of(&log, &p)));
        log.push(set_role(&q, 6, &p, Role::Owner));
        log.push(add(&p, 4, &c, "cro", Role::Owner));
        let r = fold_in_any_order(&o, &log);
        assert!(!r.outranks(&c.id(), &q.id()));
    }

    // The next three came from the third re-review's probes.

    #[test]
    fn a_drop_by_someone_who_lost_their_standing_does_not_stand() {
        // Quin, removed at 100, backdates oz's promotion to 30, so oz outranks pat; oz backdates
        // a removal of pat to 50 that doesn't list pat's add of wren. Olive's removal of quin
        // drops oz's promotion, so oz's removal never happened and wren stays.
        let o = PersonKey::generate();
        let q = PersonKey::generate();
        let p = PersonKey::generate();
        let oz = PersonKey::generate();
        let w = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 10, &q, "quin", Role::Owner),
            add(&o, 20, &p, "pat", Role::Owner),
            add(&p, 40, &w, "wren", Role::Writer),
        ];
        let kept = changes_of(&log, &q);
        log.push(sign(
            &o,
            100,
            Change::Remove {
                person: q.id(),
                kept,
            },
        ));
        log.push(add(&q, 30, &oz, "oz", Role::Owner));
        let kept = Kept::default();
        log.push(sign(
            &oz,
            50,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&oz.id()), None);
        assert_eq!(r.role_of(&p.id()), Some(Role::Owner));
        assert_eq!(r.role_of(&w.id()), Some(Role::Writer));
    }

    #[test]
    fn a_drop_by_someone_who_lost_their_standing_does_not_undo_a_leave() {
        let o = PersonKey::generate();
        let q = PersonKey::generate();
        let p = PersonKey::generate();
        let oz = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 10, &q, "quin", Role::Owner),
            add(&o, 20, &p, "pat", Role::Writer),
            leave(&p, 40),
        ];
        let kept = changes_of(&log, &q);
        log.push(sign(
            &o,
            100,
            Change::Remove {
                person: q.id(),
                kept,
            },
        ));
        log.push(add(&q, 30, &oz, "oz", Role::Owner));
        let kept = Kept::default();
        log.push(sign(
            &oz,
            50,
            Change::Remove {
                person: p.id(),
                kept,
            },
        ));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&p.id()), None);
        assert_eq!(r.removed, vec![p.id(), q.id()]);
    }

    #[test]
    fn a_tangled_log_folds_to_one_roster_in_any_order() {
        // Two removed owners backdate removals of each other and steps down, and a sock of one
        // backdates more. It must end, and agree whatever the order.
        let o = PersonKey::generate();
        let q = PersonKey::generate();
        let p = PersonKey::generate();
        let oz = PersonKey::generate();
        let w = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 10, &q, "quin", Role::Owner),
            add(&o, 20, &p, "pat", Role::Owner),
            add(&p, 40, &w, "wren", Role::Writer),
        ];
        let (by_q, by_p) = (changes_of(&log, &q), changes_of(&log, &p));
        log.push(sign(
            &o,
            100,
            Change::Remove {
                person: q.id(),
                kept: by_q,
            },
        ));
        log.push(sign(
            &o,
            101,
            Change::Remove {
                person: p.id(),
                kept: by_p,
            },
        ));
        log.push(remove(&q, 60, &p));
        log.push(remove(&p, 61, &q));
        log.push(step_down(&q, 62, Kept::default()));
        log.push(leave(&p, 63));
        log.push(add(&q, 30, &oz, "oz", Role::Owner));
        log.push(remove(&oz, 35, &q));
        log.push(set_role(&oz, 36, &p, Role::Owner));
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.role_of(&oz.id()), None);
        assert_eq!(r.role_of(&q.id()), None);
        assert_eq!(r.role_of(&p.id()), None);
        assert_eq!(r.role_of(&w.id()), Some(Role::Writer));
        assert_eq!(r.role_of(&o.id()), Some(Role::Owner));
    }

    #[test]
    fn a_promotion_cycle_keeps_the_newest_promotion() {
        // Pat promotes ray, steps down, and ray makes pat an owner again: pat now ranks below
        // ray, and every machine agrees.
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let ray = PersonKey::generate();
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&p, 3, &ray, "ray", Role::Owner),
        ];
        log.push(step_down(&p, 5, changes_of(&log, &p)));
        log.push(set_role(&ray, 6, &p, Role::Owner));
        let r = fold_in_any_order(&o, &log);
        assert!(r.outranks(&o.id(), &ray.id()));
        assert!(r.outranks(&ray.id(), &p.id()));
    }

    #[test]
    fn rank_comes_from_the_latest_promotion_and_only_from_a_promotion() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let q = PersonKey::generate();
        let quin_demotes_pat = |mut log: Vec<SignedChange>| {
            let kept = changes_of(&log, &p);
            log.push(demote(&q, 9, &p, Role::Writer, kept));
            fold_in_any_order(&o, &log).role_of(&p.id())
        };
        let mut log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &q, "quin", Role::Owner),
            // Quin making pat an owner again would put pat below quin: it does nothing.
            set_role(&q, 4, &p, Role::Owner),
        ];
        assert_eq!(quin_demotes_pat(log.clone()), Some(Role::Owner));
        // Demoted, then made an owner again by quin, pat now ranks below quin.
        log.push(demote(&o, 5, &p, Role::Writer, changes_of(&log, &p)));
        log.push(set_role(&q, 6, &p, Role::Owner));
        assert_eq!(quin_demotes_pat(log), Some(Role::Writer));
    }

    #[test]
    fn a_writer_taken_off_writing_leaves_what_the_change_kept() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let x = PersonKey::generate();
        let wren = member(&w, "wren", Role::Writer);
        let au = wren.machines[0].author.clone();
        let first = kept_files(
            &au,
            vec![entry("a.md", 5, Some("a")), entry("b.md", 6, None)],
            vec![entry("a.md", 5, Some("ra"))],
        );
        // The second demotion's signer held an older a.md and a newer c.md.
        let mut second = kept_files(
            &au,
            vec![entry("a.md", 4, Some("old")), entry("c.md", 9, Some("c"))],
            Vec::new(),
        );
        // Entries listed for someone else's machine count for nothing.
        second.authors.push(KeptAuthor {
            author: "au-other".into(),
            files: vec![entry("z.md", 9, Some("z"))],
            records: Vec::new(),
        });
        let log = vec![
            genesis(&o),
            sign(&o, 2, Change::AddMember { member: wren }),
            add(&o, 3, &x, "xan", Role::Reader),
            demote(&o, 7, &w, Role::Reader, first),
            set_role(&o, 8, &w, Role::Writer),
            sign(
                &o,
                10,
                Change::Remove {
                    person: w.id(),
                    kept: second,
                },
            ),
            // A reader has nothing to keep.
            demote(
                &o,
                11,
                &x,
                Role::Reader,
                kept_files("au-x", vec![entry("x.md", 1, None)], Vec::new()),
            ),
            remove(&o, 12, &x),
        ];
        let r = fold_in_any_order(&o, &log);
        assert_eq!(r.retired.len(), 1);
        let kept = &r.retired[&au];
        assert_eq!(kept.machine, "wren-pc");
        let files: Vec<(&str, u64)> = kept.files.iter().map(|(p, v)| (p.as_str(), v.at)).collect();
        assert_eq!(files, vec![("a.md", 5), ("b.md", 6), ("c.md", 9)]);
        assert_eq!(kept.files["b.md"].hash, None);
        assert_eq!(kept.records["a.md"].hash, Some(Hash::new(b"ra")));
        assert!(!r.writes(&au));
    }

    #[test]
    fn a_member_whose_machine_is_already_someone_elses_is_not_added() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let x = PersonKey::generate();
        let mut same_author = member(&x, "xan", Role::Writer);
        same_author.machines[0].author =
            member(&w, "wren", Role::Writer).machines[0].author.clone();
        let mut same_endpoint = member(&x, "xan", Role::Writer);
        same_endpoint.machines[0].endpoint = "ep-olive".into();
        let mut removed_ones = member(&x, "xan", Role::Writer);
        removed_ones.machines[0].endpoint = "ep-wren".into();
        let log = vec![
            genesis(&o),
            add(&o, 2, &w, "wren", Role::Writer),
            sign(
                &o,
                3,
                Change::AddMember {
                    member: same_author,
                },
            ),
            sign(
                &o,
                4,
                Change::AddMember {
                    member: same_endpoint,
                },
            ),
            remove(&o, 5, &w),
            sign(
                &o,
                6,
                Change::AddMember {
                    member: removed_ones,
                },
            ),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&x.id()), None);
        // The same person may come back on the machine they had.
        let mut log = log;
        log.push(add(&o, 7, &w, "wren", Role::Reader));
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.role_of(&w.id()), Some(Role::Reader));
    }

    #[test]
    fn empty_or_overlong_names_are_skipped() {
        let o = PersonKey::generate();
        let w = PersonKey::generate();
        let long = "n".repeat(201);
        let log = vec![
            genesis(&o),
            add(&o, 2, &w, "wren", Role::Writer),
            rename(&w, 3, &w, "  "),
            rename(&w, 4, &w, &long),
            rename_machine(&w, 5, &w, ""),
            rename_machine(&w, 6, &w, &long),
            rename_swarm(&o, 7, " \t"),
            rename_swarm(&o, 8, &long),
        ];
        let r = fold(&o, &log).unwrap();
        assert_eq!(r.members[1].name, "wren");
        assert_eq!(r.members[1].machines[0].name, "wren-pc");
        assert_eq!(r.name, "Notes");
        let fits = "n".repeat(200);
        let log = vec![genesis(&o), rename_swarm(&o, 2, &fits)];
        assert_eq!(fold(&o, &log).unwrap().name, fits);
    }

    #[test]
    fn the_roster_is_the_same_whatever_order_entries_arrive_in() {
        let o = PersonKey::generate();
        let p = PersonKey::generate();
        let w = PersonKey::generate();
        let x = PersonKey::generate();
        let types = vec!["TXT".to_string(), "md".to_string()];
        let log = vec![
            genesis(&o),
            add(&o, 2, &p, "pat", Role::Owner),
            add(&o, 3, &w, "wren", Role::Writer),
            add(&p, 4, &x, "xan", Role::Reader),
            set_role(&o, 5, &x, Role::Writer),
            rename(&x, 6, &x, "Xander"),
            remove(&p, 7, &w),
            add(&o, 8, &w, "wren", Role::Reader),
            rename_swarm(&p, 9, "Field notes"),
            sign(&o, 10, Change::SetTypes { types }),
            leave(&w, 11),
            leave(&o, 12),
        ];
        let expected = fold(&o, &log).unwrap();
        assert_eq!(expected.name, "Field notes");
        assert_eq!(expected.role_of(&x.id()), Some(Role::Writer));
        assert_eq!(expected.role_of(&w.id()), None);
        assert_eq!(expected.role_of(&o.id()), None);

        // A tiny fixed-seed generator (xorshift64) keeps the shuffles repeatable.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut shuffled = log.clone();
        for _ in 0..50 {
            for i in (1..shuffled.len()).rev() {
                shuffled.swap(i, (next() % (i as u64 + 1)) as usize);
            }
            assert_eq!(fold(&o, &shuffled).as_ref(), Some(&expected));
        }
    }
}
