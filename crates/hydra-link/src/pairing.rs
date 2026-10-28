//! Pairing: the QR offer, the secret check and the word pair both screens show.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a pairing offer stays valid.
pub const OFFER_LIFETIME: Duration = Duration::from_secs(120);

const PREFIX: &str = "hydra-pair:";

/// What the desktop puts in the QR code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    pub endpoint: String,
    pub secret: String,
    pub relay: Option<String>,
}

impl PairingOffer {
    /// A fresh offer with a new random 32-byte secret.
    pub fn new(endpoint: String, relay: Option<String>) -> PairingOffer {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        let secret = bytes.iter().map(|b| format!("{b:02x}")).collect();
        PairingOffer {
            endpoint,
            secret,
            relay,
        }
    }

    /// `hydra-pair:` followed by the unpadded base64url of the offer's JSON.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("an offer serializes");
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(s: &str) -> Option<PairingOffer> {
        let body = s.strip_prefix(PREFIX)?;
        let json = URL_SAFE_NO_PAD.decode(body).ok()?;
        serde_json::from_slice(&json).ok()
    }
}

/// Compares two secrets without stopping at the first differing byte.
pub fn secret_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The "adjective-animal" both screens show, derived from the secret and both ids.
pub fn word_pair(secret: &str, desktop: &str, phone: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.update(desktop.as_bytes());
    h.update(phone.as_bytes());
    let d = h.finalize();
    format!(
        "{}-{}",
        ADJECTIVES[d[0] as usize % 64],
        ANIMALS[d[1] as usize % 64]
    )
}

const ADJECTIVES: [&str; 64] = [
    "amber", "azure", "bold", "brave", "bright", "brisk", "calm", "clever", "cosmic", "crisp",
    "curious", "daring", "deep", "eager", "fancy", "fierce", "gentle", "golden", "happy", "hardy",
    "jolly", "keen", "kind", "lively", "lucky", "merry", "mighty", "misty", "nimble", "noble",
    "odd", "plucky", "polite", "proud", "quick", "quiet", "rapid", "royal", "rusty", "shiny",
    "silent", "silver", "sleek", "smooth", "snowy", "solid", "spry", "steady", "stormy", "sunny",
    "swift", "tidy", "tiny", "vivid", "warm", "wild", "windy", "wise", "witty", "young", "zesty",
    "zippy", "breezy", "mellow",
];

const ANIMALS: [&str; 64] = [
    "badger", "beaver", "bison", "camel", "cobra", "condor", "coyote", "crane", "crow", "deer",
    "dingo", "dolphin", "eagle", "falcon", "ferret", "finch", "fox", "gecko", "gibbon", "goat",
    "hare", "hawk", "heron", "horse", "ibis", "jackal", "jaguar", "koala", "lemur", "lion",
    "llama", "lynx", "magpie", "marmot", "mole", "moose", "newt", "otter", "owl", "panda",
    "parrot", "pelican", "penguin", "pony", "puffin", "quail", "rabbit", "raven", "robin",
    "salmon", "seal", "shark", "sloth", "stork", "swan", "tiger", "turtle", "walrus", "weasel",
    "wolf", "wombat", "yak", "zebra", "osprey",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> PairingOffer {
        PairingOffer::new("endpointid".into(), Some("https://relay.example".into()))
    }

    #[test]
    fn an_offer_round_trips() {
        let o = offer();
        let s = o.encode();
        assert!(s.starts_with("hydra-pair:"));
        assert_eq!(PairingOffer::decode(&s), Some(o));
        let n = PairingOffer::new("e".into(), None);
        assert_eq!(PairingOffer::decode(&n.encode()), Some(n));
    }

    #[test]
    fn garbage_is_not_an_offer() {
        assert_eq!(PairingOffer::decode("hello"), None);
        assert_eq!(PairingOffer::decode("hydra-pair:!!"), None);
        assert_eq!(PairingOffer::decode("hydra-pair:aGVsbG8"), None);
    }

    #[test]
    fn secrets_are_64_hex_chars_and_differ() {
        let a = offer().secret;
        let b = offer().secret;
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_ne!(a, b);
    }

    #[test]
    fn secret_matches_only_the_same_secret() {
        let s = offer().secret;
        assert!(secret_matches(&s, &s));
        let mut off = s.clone();
        off.pop();
        off.push(if s.ends_with('0') { '1' } else { '0' });
        assert!(!secret_matches(&s, &off));
        assert!(!secret_matches(&s, ""));
        assert!(!secret_matches(&s, &s[..63]));
    }

    fn is_word(w: &str) -> bool {
        !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase())
    }

    #[test]
    fn both_ends_get_the_same_word() {
        let w = word_pair("secret", "desk", "phone-a");
        assert_eq!(w, word_pair("secret", "desk", "phone-a"));
        let (a, b) = w.split_once('-').unwrap();
        assert!(is_word(a) && is_word(b));
        let differs = (0..20).any(|i| word_pair("secret", "desk", &format!("p{i}")) != w);
        assert!(differs);
    }

    #[test]
    fn word_lists_have_64_unique_lowercase_words() {
        for list in [&ADJECTIVES[..], &ANIMALS[..]] {
            assert_eq!(list.len(), 64);
            assert!(list.iter().all(|w| is_word(w)));
            let mut v: Vec<_> = list.to_vec();
            v.sort();
            v.dedup();
            assert_eq!(v.len(), 64);
        }
    }
}
