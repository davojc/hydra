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
//! as they were and nothing they write afterwards gets in.
use std::collections::{BTreeMap, HashSet};

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
}

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
    /// When a change would take someone out of a role, their earlier changes it doesn't list are
    /// dropped if, folded again without them, it still does: so a change backdated to before
    /// the one taking its signer's role away can't undo the standing of whoever signed that.
    pub fn fold(swarm: &str, anchor: &PersonId, changes: &[SignedChange]) -> Option<Roster> {
        let mut ordered: Vec<&SignedChange> = changes.iter().filter(|c| c.verify(swarm)).collect();
        ordered.sort_by(|a, b| (a.at, &a.sig).cmp(&(b.at, &b.sig)));
        ordered.dedup_by(|a, b| a.sig == b.sig);
        let mut dropped: HashSet<&str> = HashSet::new();
        let mut roster: Option<Roster> = None;
        for (i, c) in ordered.iter().enumerate() {
            if dropped.contains(c.sig.as_str()) {
                continue;
            }
            let Some(r) = &mut roster else {
                roster = Roster::genesis(anchor, c);
                continue;
            };
            if let Some((person, kept)) = c.change.retiring() {
                let listed: HashSet<&str> = kept.changes.iter().map(String::as_str).collect();
                let unlisted: Vec<&str> = ordered[..i]
                    .iter()
                    .filter(|d| {
                        d.by == *person
                            && !matches!(d.change, Change::Genesis { .. })
                            && !listed.contains(d.sig.as_str())
                            && !dropped.contains(d.sig.as_str())
                    })
                    .map(|d| d.sig.as_str())
                    .collect();
                if !unlisted.is_empty() {
                    let mut without = dropped.clone();
                    without.extend(unlisted);
                    let before = Roster::fold_plain(anchor, &ordered[..i], &without);
                    if let Some(before) = before.filter(|b| b.retires(c, person)) {
                        dropped = without;
                        *r = before;
                    }
                }
            }
            r.apply(c);
        }
        roster
    }

    /// `changes` folded in the order given, leaving out `dropped`.
    fn fold_plain(
        anchor: &PersonId,
        changes: &[&SignedChange],
        dropped: &HashSet<&str>,
    ) -> Option<Roster> {
        let mut roster: Option<Roster> = None;
        for c in changes.iter().filter(|c| !dropped.contains(c.sig.as_str())) {
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
        };
        r.admit(owner);
        Some(r)
    }

    /// Whether `c` would take `person` out of the role they have now.
    fn retires(&self, c: &SignedChange, person: &PersonId) -> bool {
        let mut after = self.clone();
        after.apply(c);
        after.role_of(person) < self.role_of(person)
    }

    fn apply(&mut self, c: &SignedChange) {
        let by_owner = self.role_of(&c.by) == Some(Role::Owner);
        // A writer this change may take off writing, with their machines as they are now.
        let writer = c.change.retiring().and_then(|(person, kept)| {
            let m = self.members.iter().find(|m| m.person == *person)?;
            (m.role >= Role::Writer).then(|| (*person, m.machines.clone(), kept))
        });
        match &c.change {
            Change::Genesis { .. } => {}
            Change::AddMember { member } => {
                if by_owner && self.role_of(&member.person).is_none() && !self.taken(member) {
                    self.removed.retain(|p| *p != member.person);
                    self.admit(member.clone());
                }
            }
            Change::SetRole { person, role, .. } => {
                if by_owner
                    && self.role_of(person).is_some()
                    && (*role == Role::Owner || !self.is_last_owner(person))
                {
                    self.member_mut(person).role = *role;
                }
            }
            Change::Remove { person, .. } => {
                if by_owner {
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
        if let Some((person, machines, kept)) = writer
            && self.role_of(&person).is_none_or(|r| r < Role::Writer)
        {
            self.retire(&machines, kept);
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

    /// Keeps what `kept` lists for `machines`, whose person just stopped being a writer.
    fn retire(&mut self, machines: &[Machine], kept: &Kept) {
        for m in machines {
            let retired = self.retired.entry(m.author.clone()).or_default();
            retired.machine = m.name.clone();
            for k in kept.authors.iter().filter(|k| k.author == m.author) {
                for e in &k.files {
                    keep_newest(&mut retired.files, &e.path, e.version);
                }
                for e in &k.records {
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
