//! The membership roster: a swarm's people and their roles, folded from signed changes.
//!
//! Every change is signed by the person making it, and the roster is whatever the changes add up
//! to when taken in (time, signature) order, so every machine that has the same changes agrees on
//! it whatever order they arrived in.
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
    SetRole {
        person: PersonId,
        role: Role,
    },
    Remove {
        person: PersonId,
    },
    Leave {
        person: PersonId,
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
}

/// File types synced until an owner chooses others.
const DEFAULT_TYPES: [&str; 1] = [".md"];

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

impl Roster {
    /// Folds every change, in (at, sig) order, from the genesis signed by `anchor`. Changes that
    /// break the rules (a bad signature, someone without the standing to make them) are skipped.
    pub fn fold(swarm: &str, anchor: &PersonId, changes: &[SignedChange]) -> Option<Roster> {
        let mut ordered: Vec<&SignedChange> = changes.iter().filter(|c| c.verify(swarm)).collect();
        ordered.sort_by(|a, b| (a.at, &a.sig).cmp(&(b.at, &b.sig)));
        let mut roster: Option<Roster> = None;
        for c in ordered {
            match &mut roster {
                None => {
                    if let Change::Genesis { name, owner } = &c.change
                        && c.by == *anchor
                        && owner.person == *anchor
                    {
                        let mut owner = owner.clone();
                        owner.role = Role::Owner;
                        roster = Some(Roster {
                            name: name.clone(),
                            members: vec![owner],
                            types: DEFAULT_TYPES.iter().map(|t| t.to_string()).collect(),
                            removed: Vec::new(),
                        });
                    }
                }
                Some(r) => r.apply(c),
            }
        }
        roster
    }

    fn apply(&mut self, c: &SignedChange) {
        let by_owner = self.role_of(&c.by) == Some(Role::Owner);
        match &c.change {
            Change::Genesis { .. } => {}
            Change::AddMember { member } => {
                if by_owner && self.role_of(&member.person).is_none() {
                    self.removed.retain(|p| *p != member.person);
                    self.members.push(member.clone());
                }
            }
            Change::SetRole { person, role } => {
                if by_owner
                    && self.role_of(person).is_some()
                    && (*role == Role::Owner || !self.is_last_owner(person))
                {
                    self.member_mut(person).role = *role;
                }
            }
            Change::Remove { person } => {
                if by_owner {
                    self.take_out(person);
                }
            }
            Change::Leave { person } => {
                if c.by == *person {
                    self.take_out(person);
                }
            }
            Change::Rename { person, name } => {
                if (c.by == *person || by_owner) && self.role_of(person).is_some() {
                    self.member_mut(person).name = name.clone();
                }
            }
            Change::RenameMachine {
                person,
                endpoint,
                name,
            } => {
                if c.by == *person || by_owner {
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
                if by_owner {
                    self.name = name.clone();
                }
            }
            Change::SetTypes { types } => {
                if by_owner {
                    self.types = normalise_types(types);
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
            },
        )
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
        sign(by, at, Change::Remove { person: who.id() })
    }

    fn leave(who: &PersonKey, at: u64) -> SignedChange {
        sign(who, at, Change::Leave { person: who.id() })
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
