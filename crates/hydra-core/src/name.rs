use std::fmt;

/// An environment's identifier: its folder name under `envs/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid environment name {0:?}: use 1-32 lowercase letters, digits and '-', starting with a letter or digit"
)]
pub struct NameError(pub String);

impl EnvName {
    pub fn parse(s: &str) -> Result<Self, NameError> {
        let b = s.as_bytes();
        let ok = !b.is_empty()
            && b.len() <= 32
            && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
            && b.iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-');
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(NameError(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for EnvName {
    type Err = NameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        for n in ["work", "client-acme", "0day", "a", &"a".repeat(32)] {
            assert_eq!(EnvName::parse(n).unwrap().as_str(), n);
        }
    }

    #[test]
    fn rejects_invalid_names() {
        for n in ["", "Work", "-work", "a b", "a_b", "wörk", &"a".repeat(33)] {
            assert_eq!(EnvName::parse(n), Err(NameError(n.to_string())), "{n:?}");
        }
    }

    #[test]
    fn error_explains_the_rule() {
        let msg = EnvName::parse("Work").unwrap_err().to_string();
        assert!(msg.contains("lowercase letters, digits and '-'"), "{msg}");
    }
}
