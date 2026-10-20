//! The Jev API key (TypeSafe's Jev decision model), kept in the OS credential store.

/// Credential Manager user the key is stored under.
const USER: &str = "jev";

/// The stored Jev API key, under one credential-store service name.
pub struct JevKey {
    service: String,
}

fn failed(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl JevKey {
    /// The key hydraui itself uses.
    pub fn app() -> Self {
        Self::with_service("hydraui")
    }

    /// A key under another service name, so tests never touch the real one.
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, USER).map_err(failed)
    }

    /// The stored key, or `None` when none is set.
    pub fn get(&self) -> Result<Option<String>, String> {
        match self.entry()?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(failed(e)),
        }
    }

    /// Stores the key without its surrounding spaces; a blank one is refused.
    pub fn set(&self, key: &str) -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("the Jev API key is empty".to_string());
        }
        self.entry()?.set_password(key).map_err(failed)
    }

    /// Forgets the key; false when there was none.
    pub fn remove(&self) -> Result<bool, String> {
        match self.entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(failed(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_set_read_and_removed() {
        let k = JevKey::with_service(&format!("hydraui-test-{}", std::process::id()));
        assert_eq!(k.get().unwrap(), None);
        k.set("  tsk-123  ").unwrap();
        assert_eq!(k.get().unwrap().as_deref(), Some("tsk-123"));
        assert!(k.remove().unwrap());
        assert_eq!(k.get().unwrap(), None);
        assert!(!k.remove().unwrap());
    }

    #[test]
    fn a_blank_key_is_refused() {
        let k = JevKey::with_service("hydraui-test-blank");
        assert!(k.set("   ").is_err());
    }
}
