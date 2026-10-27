//! People: a signing key per person, and the OS credential store that keeps it.
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A person, known by the public half of their signing key. Shown and saved in iroh's z-base-32
/// form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PersonId(pub iroh::PublicKey);

impl fmt::Display for PersonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_z32())
    }
}

impl FromStr for PersonId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        iroh::PublicKey::from_z32(s)
            .map(PersonId)
            .map_err(|e| format!("not a person id: {e}"))
    }
}

impl Serialize for PersonId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PersonId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A person's secret signing key.
pub struct PersonKey(iroh::SecretKey);

impl PersonKey {
    pub fn generate() -> PersonKey {
        PersonKey(iroh::SecretKey::generate())
    }

    pub fn from_bytes(b: [u8; 32]) -> PersonKey {
        PersonKey(iroh::SecretKey::from_bytes(&b))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn id(&self) -> PersonId {
        PersonId(self.0.public())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.0.sign(msg).to_bytes().to_vec()
    }
}

/// Whether `sig` is `id`'s signature of `msg`.
pub fn verify(id: &PersonId, msg: &[u8], sig: &[u8]) -> bool {
    iroh::Signature::try_from(sig)
        .map(|s| id.0.verify(msg, &s).is_ok())
        .unwrap_or(false)
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn failed(e: impl fmt::Display) -> String {
    e.to_string()
}

/// 32-byte secrets in the OS credential store as lowercase hex (the phone link's key works the
/// same way).
pub struct Keyring {
    service: String,
}

impl Keyring {
    /// A keyring under `service`; tests use a service of their own so they never touch real keys.
    pub fn new(service: &str) -> Keyring {
        Keyring {
            service: service.to_string(),
        }
    }

    /// The secret stored for `user`, made and stored first when there is none. An entry that
    /// isn't 32 bytes of hex is an error, never silently replaced: that would change who the
    /// person is.
    pub fn get_or_create(&self, user: &str) -> Result<[u8; 32], String> {
        let entry = keyring::Entry::new(&self.service, user).map_err(failed)?;
        match entry.get_password() {
            Ok(hex) => from_hex(hex.trim())
                .and_then(|v| <[u8; 32]>::try_from(v).ok())
                .ok_or_else(|| format!("the stored key for {user} is damaged")),
            Err(keyring::Error::NoEntry) => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(|e| format!("random key: {e}"))?;
                entry.set_password(&to_hex(&key)).map_err(failed)?;
                Ok(key)
            }
            Err(e) => Err(failed(e)),
        }
    }

    /// Forgets the secret stored for `user`; there being none is fine.
    pub fn delete(&self, user: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(&self.service, user).map_err(failed)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(failed(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_only_for_its_signer_and_message() {
        let a = PersonKey::generate();
        let b = PersonKey::generate();
        let sig = a.sign(b"hello");
        assert!(verify(&a.id(), b"hello", &sig));
        assert!(!verify(&b.id(), b"hello", &sig));
        assert!(!verify(&a.id(), b"hullo", &sig));
        assert!(!verify(&a.id(), b"hello", &sig[..10]));
    }

    #[test]
    fn keys_and_ids_round_trip() {
        let k = PersonKey::generate();
        let again = PersonKey::from_bytes(k.to_bytes());
        assert_eq!(k.id(), again.id());
        let text = k.id().to_string();
        assert_eq!(text.parse::<PersonId>(), Ok(k.id()));
        let json = serde_json::to_string(&k.id()).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<PersonId>(&json).unwrap(), k.id());
    }

    #[test]
    fn the_keyring_never_replaces_a_damaged_key() {
        let service = format!("hydraui-test-swarms-{}", std::process::id());
        let ring = Keyring::new(&service);
        let first = match ring.get_or_create("alice") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipped: no keyring here ({e})");
                return;
            }
        };
        assert_eq!(ring.get_or_create("alice").unwrap(), first);
        let entry = keyring::Entry::new(&service, "alice").unwrap();
        entry.set_password("not hex").unwrap();
        assert!(ring.get_or_create("alice").is_err());
        assert_eq!(entry.get_password().unwrap(), "not hex");
        ring.delete("alice").unwrap();
        ring.delete("alice").unwrap();
    }
}
