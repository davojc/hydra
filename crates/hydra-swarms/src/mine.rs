//! Your machines: what they share (your name, their names, the ones removed, and the chain of
//! your personal keys), kept in `mine.json` and merged whenever two of them meet, and the
//! single-use codes that link another machine to you.
use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use crate::ids::{RekeyCert, chain_ends};
use crate::invites::{Invites, is_token};

/// The key id (for the service's `SwarmKeyFn`) of this machine's device key, its identity
/// between your own machines.
pub const DEVICE_KEY_ID: &str = "device";
pub const LINK_PREFIX: &str = "hydra-link:";
pub const MINE_FILE: &str = "mine.json";
pub const LINKS_FILE: &str = "links.json";

/// The codes this machine has made to link another of yours.
pub type Links = Invites<()>;

/// What another machine pastes to link to you: which machine to ask, and the secret.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct LinkCode {
    /// The linking machine's device id.
    pub host: String,
    pub relay: Option<String>,
    /// 64 hex.
    pub token: String,
}

impl LinkCode {
    /// `hydra-link:` and the code as base64url JSON.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("a link code always serialises");
        format!("{LINK_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// A pasted code; whitespace anywhere in it is ignored.
    pub fn decode(s: &str) -> Option<LinkCode> {
        let s: String = s.split_whitespace().collect();
        let json = URL_SAFE_NO_PAD.decode(s.strip_prefix(LINK_PREFIX)?).ok()?;
        let code: LinkCode = serde_json::from_slice(&json).ok()?;
        code.host.parse::<EndpointId>().ok()?;
        is_token(&code.token).then_some(code)
    }
}

/// One of your machines.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Device {
    /// Its device endpoint id.
    pub id: String,
    pub name: String,
    /// When `name` was set, in Unix microseconds.
    pub name_at: u64,
}

/// Everything your machines agree on. `devices` includes this one.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Mine {
    /// Your name, and when it was set (Unix microseconds).
    pub name: String,
    pub name_at: u64,
    pub devices: Vec<Device>,
    /// The ids of machines removed from yours, which never count again.
    pub removed: Vec<String>,
    /// Each personal key vouching for the next, from your first to your current one.
    pub chain: Vec<RekeyCert>,
}

/// Whether (`at`, `name`) is newer than (`old_at`, `old`): ties go to the higher name, so every
/// machine settles the same way.
fn newer(at: u64, name: &str, old_at: u64, old: &str) -> bool {
    (at, name) > (old_at, old)
}

impl Mine {
    /// What `root/mine.json` holds; nothing when it is missing. A damaged file is set aside as
    /// `mine.json.damaged`, so nothing overwrites it.
    pub fn load(root: &Path) -> Mine {
        let path = root.join(MINE_FILE);
        let Ok(bytes) = fs::read(&path) else {
            return Mine::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            let _ = fs::rename(&path, path.with_extension("json.damaged"));
            Mine::default()
        })
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join(MINE_FILE);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Takes in what another of your machines holds: the newer name of each, every device and
    /// removal, and a longer chain that carries ours on. True if anything changed.
    pub fn merge(&mut self, other: &Mine) -> bool {
        let before = self.clone();
        if newer(other.name_at, &other.name, self.name_at, &self.name) {
            self.name = other.name.clone();
            self.name_at = other.name_at;
        }
        for d in &other.devices {
            self.set_device_name(&d.id, &d.name, d.name_at);
        }
        for r in &other.removed {
            self.remove(r);
        }
        let extends = other.chain.len() > self.chain.len()
            && other.chain.starts_with(&self.chain)
            && other
                .chain
                .first()
                .is_some_and(|first| chain_ends(&other.chain, &first.from).is_some());
        if extends {
            self.chain = other.chain.clone();
        }
        *self != before
    }

    /// The machines other than `me` that are still yours.
    pub fn linked(&self, me: &str) -> Vec<&Device> {
        self.devices
            .iter()
            .filter(|d| d.id != me && self.knows(&d.id))
            .collect()
    }

    /// Whether `id` is one of your machines and not removed.
    pub fn knows(&self, id: &str) -> bool {
        self.devices.iter().any(|d| d.id == id) && !self.removed.iter().any(|r| r == id)
    }

    pub fn set_name(&mut self, name: &str, at: u64) {
        self.name = name.to_string();
        self.name_at = at;
    }

    /// Names the device `id`, adding it if it is new, unless its name was set later already.
    pub fn set_device_name(&mut self, id: &str, name: &str, at: u64) {
        match self.devices.iter_mut().find(|d| d.id == id) {
            Some(d) => {
                if newer(at, name, d.name_at, &d.name) {
                    d.name = name.to_string();
                    d.name_at = at;
                }
            }
            None => {
                self.devices.push(Device {
                    id: id.to_string(),
                    name: name.to_string(),
                    name_at: at,
                });
                self.devices.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
    }

    pub fn remove(&mut self, id: &str) {
        if !self.removed.iter().any(|r| r == id) {
            self.removed.push(id.to_string());
            self.removed.sort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{PersonKey, RekeyCert};

    fn device(id: &str, name: &str, at: u64) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            name_at: at,
        }
    }

    fn merged(a: &Mine, b: &Mine) -> Mine {
        let mut out = a.clone();
        out.merge(b);
        out
    }

    #[test]
    fn merging_takes_the_newer_name_and_device_names() {
        let a = Mine {
            name: "Olive".into(),
            name_at: 10,
            devices: vec![device("d1", "desk", 5), device("d2", "laptop", 1)],
            ..Mine::default()
        };
        let b = Mine {
            name: "Olive O".into(),
            name_at: 20,
            devices: vec![device("d2", "lappy", 7), device("d3", "phone", 2)],
            ..Mine::default()
        };
        let ab = merged(&a, &b);
        assert_eq!(ab, merged(&b, &a));
        assert_eq!(ab.name, "Olive O");
        let names: Vec<(&str, &str)> = ab
            .devices
            .iter()
            .map(|d| (d.id.as_str(), d.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("d1", "desk"), ("d2", "lappy"), ("d3", "phone")]
        );
        let mut same = ab.clone();
        assert!(!same.merge(&ab));
    }

    #[test]
    fn a_tie_goes_the_same_way_on_both_sides() {
        let a = Mine {
            name: "A".into(),
            name_at: 5,
            ..Mine::default()
        };
        let b = Mine {
            name: "B".into(),
            name_at: 5,
            ..Mine::default()
        };
        assert_eq!(merged(&a, &b).name, "B");
        assert_eq!(merged(&b, &a).name, "B");
    }

    #[test]
    fn a_removal_wins_over_a_device_still_listed_elsewhere() {
        let a = Mine {
            devices: vec![device("me", "desk", 1), device("d", "old", 1)],
            removed: vec!["d".into()],
            ..Mine::default()
        };
        let b = Mine {
            devices: vec![device("me", "desk", 1), device("d", "old", 1)],
            ..Mine::default()
        };
        for m in [merged(&a, &b), merged(&b, &a)] {
            assert!(!m.knows("d"));
            assert!(m.knows("me"));
            assert_eq!(m.removed, vec!["d".to_string()]);
            assert!(m.linked("me").is_empty());
        }
    }

    #[test]
    fn the_longer_valid_chain_wins() {
        let [k1, k2, k3, k4] = [(); 4].map(|()| PersonKey::generate());
        let one = vec![RekeyCert::new(&k1, k2.id())];
        let two = vec![RekeyCert::new(&k1, k2.id()), RekeyCert::new(&k2, k3.id())];
        let ours = Mine {
            chain: one.clone(),
            ..Mine::default()
        };
        let theirs = Mine {
            chain: two.clone(),
            ..Mine::default()
        };
        assert_eq!(merged(&ours, &theirs).chain, two);
        assert_eq!(merged(&theirs, &ours).chain, two);
        // A chain that doesn't extend ours is ignored.
        let elsewhere = Mine {
            chain: vec![RekeyCert::new(&k1, k4.id()), RekeyCert::new(&k4, k3.id())],
            ..Mine::default()
        };
        assert_eq!(merged(&ours, &elsewhere).chain, one);
        // So is one that doesn't verify.
        let mut forged = two.clone();
        forged[1].to = k4.id();
        let forged = Mine {
            chain: forged,
            ..Mine::default()
        };
        assert_eq!(merged(&ours, &forged).chain, one);
        // Anyone's chain is taken when we have none.
        assert_eq!(merged(&Mine::default(), &theirs).chain, two);
    }

    #[test]
    fn names_are_set_with_their_time() {
        let mut m = Mine {
            devices: vec![device("me", "desk", 1)],
            ..Mine::default()
        };
        m.set_name("Olive", 3);
        m.set_device_name("me", "big desk", 4);
        m.set_device_name("new", "laptop", 5);
        assert_eq!((m.name.as_str(), m.name_at), ("Olive", 3));
        assert_eq!(m.devices[0], device("me", "big desk", 4));
        assert_eq!(m.devices[1], device("new", "laptop", 5));
        m.remove("new");
        assert!(!m.knows("new"));
    }

    #[test]
    fn a_link_code_round_trips_and_others_do_not_decode() {
        let host = iroh::SecretKey::generate().public().to_string();
        let code = LinkCode {
            host,
            relay: Some("https://relay.example/".into()),
            token: "ab".repeat(32),
        };
        let text = code.encode();
        assert!(text.starts_with(LINK_PREFIX));
        assert_eq!(LinkCode::decode(&text), Some(code.clone()));
        let wrapped = format!("  {}\n{} ", &text[..20], &text[20..]);
        assert_eq!(LinkCode::decode(&wrapped), Some(code));
        let swarm = text.replacen(LINK_PREFIX, "hydra-swarm:", 1);
        assert_eq!(LinkCode::decode(&swarm), None);
        assert_eq!(LinkCode::decode("hydra-link:junk"), None);
    }

    #[test]
    fn links_are_single_use_and_only_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LINKS_FILE);
        let links = Links::at(path.clone());
        let (link, token) = links.create((), 100).unwrap();
        assert_eq!(links.pending(101), vec![link]);
        assert!(!std::fs::read_to_string(&path).unwrap().contains(&token));
        assert_eq!(links.redeem(&token, 101), Some(()));
        assert_eq!(links.redeem(&token, 102), None);
        assert!(links.pending(103).is_empty());
    }

    #[test]
    fn mine_saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Mine::load(dir.path()), Mine::default());
        let m = Mine {
            name: "Olive".into(),
            name_at: 2,
            devices: vec![device("me", "desk", 1)],
            ..Mine::default()
        };
        m.save(dir.path()).unwrap();
        assert_eq!(Mine::load(dir.path()), m);
    }

    #[test]
    fn a_damaged_mine_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MINE_FILE), "not json").unwrap();
        assert_eq!(Mine::load(dir.path()), Mine::default());
        let aside = dir.path().join("mine.json.damaged");
        assert_eq!(std::fs::read_to_string(aside).unwrap(), "not json");
    }
}
