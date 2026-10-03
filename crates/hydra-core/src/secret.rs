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
}
