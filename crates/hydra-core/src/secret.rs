use std::collections::BTreeMap;

use crate::name::EnvName;

/// A `secret:<env>/<key>` reference from a config file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef {
    pub env: EnvName,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretRefError {
    #[error("invalid secret reference {0:?}: expected secret:<env>/<key>, like secret:work/linear")]
    Reference(String),
    #[error("invalid secret name {0:?}: expected <env>/<key>, like work/linear")]
    Name(String),
}

/// Secret keys: 1-64 of letters, digits, `_`, `-`, `.`.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

impl SecretRef {
    pub const PREFIX: &'static str = "secret:";

    /// Parses `work/linear` (the form used on the command line).
    pub fn parse_path(s: &str) -> Result<Self, SecretRefError> {
        let err = || SecretRefError::Name(s.to_string());
        let (env, key) = s.split_once('/').ok_or_else(err)?;
        let env = EnvName::parse(env).map_err(|_| err())?;
        if !valid_key(key) {
            return Err(err());
        }
        Ok(Self {
            env,
            key: key.to_string(),
        })
    }

    /// Parses a config value. `Ok(None)` means it is a plain value, not a reference.
    pub fn parse_value(s: &str) -> Result<Option<Self>, SecretRefError> {
        match s.strip_prefix(Self::PREFIX) {
            None => Ok(None),
            Some(rest) => Self::parse_path(rest)
                .map(Some)
                .map_err(|_| SecretRefError::Reference(s.to_string())),
        }
    }

    pub fn path(&self) -> String {
        format!("{}/{}", self.env, self.key)
    }

    pub fn to_value(&self) -> String {
        format!("{}{}", Self::PREFIX, self.path())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("secret store: {0}")]
    Backend(String),
}

/// Where secret values live. Implementations must never log values.
pub trait SecretStore {
    fn get(&self, r: &SecretRef) -> Result<Option<String>, StoreError>;
    fn set(&self, r: &SecretRef, value: &str) -> Result<(), StoreError>;
    /// `Ok(true)` if a value was deleted.
    fn delete(&self, r: &SecretRef) -> Result<bool, StoreError>;
    /// Keys stored for one environment, sorted.
    fn list(&self, env: &EnvName) -> Result<Vec<String>, StoreError>;
}

/// In-memory store for tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: std::sync::Mutex<BTreeMap<SecretRef, String>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, r: &SecretRef) -> Result<Option<String>, StoreError> {
        Ok(self.inner.lock().unwrap().get(r).cloned())
    }
    fn set(&self, r: &SecretRef, value: &str) -> Result<(), StoreError> {
        self.inner
            .lock()
            .unwrap()
            .insert(r.clone(), value.to_string());
        Ok(())
    }
    fn delete(&self, r: &SecretRef) -> Result<bool, StoreError> {
        Ok(self.inner.lock().unwrap().remove(r).is_some())
    }
    fn list(&self, env: &EnvName) -> Result<Vec<String>, StoreError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .keys()
            .filter(|r| &r.env == env)
            .map(|r| r.key.clone())
            .collect())
    }
}

/// Shows at most the last four characters; nothing for values under 8 characters.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 8 {
        return "\u{b7}\u{b7}\u{b7}\u{b7}".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("\u{b7}\u{b7}\u{b7}\u{b7}{tail}")
}

pub fn delete_env_secrets(
    store: &dyn SecretStore,
    env: &EnvName,
) -> Result<Vec<String>, StoreError> {
    let keys = store.list(env)?;
    for key in &keys {
        store.delete(&SecretRef {
            env: env.clone(),
            key: key.clone(),
        })?;
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_references() {
        let r = SecretRef::parse_value("secret:work/linear")
            .unwrap()
            .unwrap();
        assert_eq!(r.env.as_str(), "work");
        assert_eq!(r.key, "linear");
        assert_eq!(r.path(), "work/linear");
        assert_eq!(r.to_value(), "secret:work/linear");
    }

    #[test]
    fn plain_values_are_not_references() {
        assert_eq!(SecretRef::parse_value("hello").unwrap(), None);
    }

    #[test]
    fn rejects_malformed_references() {
        for s in [
            "secret:work",
            "secret:Work/x",
            "secret:work/",
            "secret:work/a b",
        ] {
            assert_eq!(
                SecretRef::parse_value(s),
                Err(SecretRefError::Reference(s.into())),
                "{s}"
            );
        }
    }

    #[test]
    fn parses_cli_paths() {
        assert_eq!(
            SecretRef::parse_path("work/github-token").unwrap().key,
            "github-token"
        );
        let err = SecretRef::parse_path("work").unwrap_err().to_string();
        assert!(
            err.contains("expected <env>/<key>, like work/linear"),
            "{err}"
        );
    }

    fn r(path: &str) -> SecretRef {
        SecretRef::parse_path(path).unwrap()
    }

    #[test]
    fn memory_store_round_trip() {
        let s = MemoryStore::default();
        assert_eq!(s.get(&r("work/a")).unwrap(), None);
        s.set(&r("work/a"), "1").unwrap();
        s.set(&r("work/b"), "2").unwrap();
        s.set(&r("home/a"), "3").unwrap();
        assert_eq!(s.get(&r("work/a")).unwrap().as_deref(), Some("1"));
        assert_eq!(
            s.list(&EnvName::parse("work").unwrap()).unwrap(),
            vec!["a", "b"]
        );
        assert!(s.delete(&r("work/a")).unwrap());
        assert!(!s.delete(&r("work/a")).unwrap());
    }

    #[test]
    fn deletes_an_environments_secrets() {
        let s = MemoryStore::default();
        let work = EnvName::parse("work").unwrap();
        s.set(&r("work/a"), "1").unwrap();
        s.set(&r("work/b"), "2").unwrap();
        s.set(&r("home/a"), "3").unwrap();
        assert_eq!(delete_env_secrets(&s, &work).unwrap(), vec!["a", "b"]);
        assert!(s.list(&work).unwrap().is_empty());
        assert_eq!(s.get(&r("home/a")).unwrap().as_deref(), Some("3"));
    }

    #[test]
    fn masks_values() {
        assert_eq!(mask("lin_api_12345a91f"), "\u{b7}\u{b7}\u{b7}\u{b7}a91f");
        assert_eq!(mask("short"), "\u{b7}\u{b7}\u{b7}\u{b7}");
    }
}
