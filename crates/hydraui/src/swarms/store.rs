//! The swarms' keys in the OS credential store: your person key, and this machine's key for
//! each swarm.
use std::ffi::OsStr;

use hydra_swarms::ids::Keyring;

/// Credential Manager user your person key is stored under.
pub const PERSON_USER: &str = "swarm-person-key";

/// The credential service: `hydraui`, or `hydraui:<HYDRA_HOME>` when `HYDRA_HOME` is set, so two
/// copies with separate hydra roots are two people with separate keys.
pub fn service_for(hydra_home: Option<&OsStr>) -> String {
    match hydra_home {
        Some(home) => format!("hydraui:{}", home.to_string_lossy()),
        None => "hydraui".into(),
    }
}

/// Credential Manager user a swarm's machine key is stored under: `id` is the swarm's namespace
/// id for one made here, `join-<16 hex>` for one joined here.
pub fn swarm_user(id: &str) -> String {
    format!("swarm-key-{id}")
}

/// The keys this hydraui keeps for its swarms.
pub struct SwarmKeys {
    keyring: Keyring,
}

impl Default for SwarmKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmKeys {
    /// The keys of this hydra root.
    pub fn new() -> Self {
        Self::with_service(&service_for(std::env::var_os("HYDRA_HOME").as_deref()))
    }

    /// Keys under another service name, so tests never touch the real ones.
    pub fn with_service(service: &str) -> Self {
        Self {
            keyring: Keyring::new(service),
        }
    }

    /// Your person key, made the first time.
    pub fn person(&self) -> Result<[u8; 32], String> {
        self.keyring.get_or_create(PERSON_USER)
    }

    /// Keeps `key` as your person key from now on.
    pub fn set_person(&self, key: [u8; 32]) -> Result<(), String> {
        self.keyring.set(PERSON_USER, key)
    }

    /// This machine's key for the swarm whose key id is `id`, made the first time.
    pub fn swarm(&self, id: &str) -> Result<[u8; 32], String> {
        self.keyring.get_or_create(&swarm_user(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_swarm_has_its_own_key_and_the_person_one_more() {
        let service = format!("hydraui-test-swarms-{}", std::process::id());
        let keys = SwarmKeys::with_service(&service);
        let person = match keys.person() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("skipped: no keyring here ({e})");
                return;
            }
        };
        assert_eq!(keys.person().unwrap(), person);
        let a = keys.swarm("join-0123456789abcdef").unwrap();
        assert_eq!(keys.swarm("join-0123456789abcdef").unwrap(), a);
        assert_ne!(keys.swarm("other").unwrap(), a);
        assert_ne!(a, person);
        let ring = Keyring::new(&service);
        for user in [
            PERSON_USER.to_string(),
            swarm_user("join-0123456789abcdef"),
            swarm_user("other"),
        ] {
            let _ = ring.delete(&user);
        }
    }
}
