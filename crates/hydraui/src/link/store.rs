//! The phone link's saved state: its key in the OS credential store, and the paired phone and
//! on/off setting in config.toml.
use hydra_core::paths::HydraPaths;
#[cfg(target_os = "macos")]
use hydra_platform::macos_keychain::Keychain;

use crate::config;

/// Credential Manager user the key is stored under.
const USER: &str = "phone-link-key";

/// The phone link's secret key, kept in the OS credential store as lowercase hex.
pub struct LinkKey {
    service: String,
}

#[cfg(not(target_os = "macos"))]
fn failed(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Default for LinkKey {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkKey {
    /// The key hydraui itself uses.
    pub fn new() -> Self {
        Self::with_service("hydraui")
    }

    /// A key under another service name, so tests never touch the real one.
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    /// The stored key, made and stored first when there is none. An entry that isn't 32 bytes
    /// of hex is an error, never silently replaced: that would change the link's identity.
    pub fn get_or_create(&self) -> Result<[u8; 32], String> {
        match self.read()? {
            Some(hex) => {
                from_hex(hex.trim()).ok_or_else(|| "the stored link key is damaged".into())
            }
            None => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(|e| format!("random key: {e}"))?;
                self.write(&to_hex(&key))?;
                Ok(key)
            }
        }
    }

    /// The stored hex, or `None` when there is none. macOS goes through /usr/bin/security, so
    /// Always Allow lasts across builds (hydra-platform's macos_keychain).
    #[cfg(target_os = "macos")]
    fn read(&self) -> Result<Option<String>, String> {
        Keychain::user().get(&self.service, USER)
    }

    #[cfg(not(target_os = "macos"))]
    fn read(&self) -> Result<Option<String>, String> {
        match keyring::Entry::new(&self.service, USER)
            .map_err(failed)?
            .get_password()
        {
            Ok(hex) => Ok(Some(hex)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(failed(e)),
        }
    }

    #[cfg(target_os = "macos")]
    fn write(&self, hex: &str) -> Result<(), String> {
        Keychain::user().set(&self.service, USER, hex)
    }

    #[cfg(not(target_os = "macos"))]
    fn write(&self, hex: &str) -> Result<(), String> {
        keyring::Entry::new(&self.service, USER)
            .map_err(failed)?
            .set_password(hex)
            .map_err(failed)
    }

    /// Forgets the stored key, if any.
    #[cfg(test)]
    fn forget(&self) {
        #[cfg(target_os = "macos")]
        let _ = Keychain::user().delete(&self.service, USER);
        #[cfg(not(target_os = "macos"))]
        let _ = keyring::Entry::new(&self.service, USER).and_then(|e| e.delete_credential());
    }
}

fn to_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Remembers the paired phone's endpoint id in config.toml; `None` forgets it.
pub fn set_paired(paths: &HydraPaths, phone: Option<&str>) -> Result<(), String> {
    config::set(paths, "phone", phone.map(Into::into))
}

/// Turns the phone link on or off in config.toml.
pub fn set_enabled(paths: &HydraPaths, on: bool) -> Result<(), String> {
    config::set(paths, "phone_access", Some(on.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_created_once() {
        let k = LinkKey::with_service(&format!("hydraui-test-link-{}", std::process::id()));
        let first = match k.get_or_create() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipped: no keyring here ({e})");
                return;
            }
        };
        assert_eq!(k.get_or_create().unwrap(), first);
        assert_ne!(first, [0u8; 32]);
        k.forget();
    }

    #[test]
    fn hex_round_trips() {
        let mut k = [0u8; 32];
        k[0] = 0xab;
        k[31] = 0x01;
        assert_eq!(from_hex(&to_hex(&k)), Some(k));
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex(&"g".repeat(64)), None);
    }

    #[test]
    fn pairing_and_enabling_are_saved_in_config() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        set_enabled(&paths, true).unwrap();
        set_paired(&paths, Some("abc")).unwrap();
        let (cfg, problems) = crate::config::load(&paths);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(cfg.phone_access);
        assert_eq!(cfg.phone.as_deref(), Some("abc"));
        set_paired(&paths, None).unwrap();
        set_enabled(&paths, false).unwrap();
        let (cfg, _) = crate::config::load(&paths);
        assert!(!cfg.phone_access);
        assert_eq!(cfg.phone, None);
    }
}
