use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::secret::{SecretRef, SecretRefError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarValue {
    Literal(String),
    Secret(SecretRef),
}

/// What one provider adds to a child process's environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contribution {
    pub vars: BTreeMap<String, VarValue>,
    /// Variables to remove even though this provider is configured (they would override it).
    pub unset: BTreeSet<String>,
}

impl Contribution {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn literal(mut self, k: &str, v: impl Into<String>) -> Self {
        self.vars.insert(k.to_string(), VarValue::Literal(v.into()));
        self
    }

    pub fn path(self, k: &str, p: &Path) -> Self {
        self.literal(k, p.to_string_lossy().into_owned())
    }

    pub fn secret(mut self, k: &str, r: SecretRef) -> Self {
        self.vars.insert(k.to_string(), VarValue::Secret(r));
        self
    }

    pub fn unset(mut self, k: &str) -> Self {
        self.unset.insert(k.to_string());
        self
    }

    /// A config value: `secret:<env>/<key>` becomes a secret, anything else a literal.
    pub fn value(self, k: &str, raw: &str) -> Result<Self, SecretRefError> {
        Ok(match SecretRef::parse_value(raw)? {
            Some(r) => self.secret(k, r),
            None => self.literal(k, raw),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Merged {
    /// Variable -> (provider id, value).
    pub vars: BTreeMap<String, (String, VarValue)>,
    pub unset: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{var} is set by both {first} and {second}")]
pub struct Conflict {
    pub var: String,
    pub first: String,
    pub second: String,
}

/// Combines contributions. Variable names compare case-insensitively (Windows semantics).
pub fn merge(parts: Vec<(String, Contribution)>) -> Result<Merged, Vec<Conflict>> {
    let mut m = Merged::default();
    let mut conflicts = Vec::new();
    for (provider, c) in parts {
        for (k, v) in c.vars {
            if let Some((existing, (first, _))) =
                m.vars.iter().find(|(ek, _)| ek.eq_ignore_ascii_case(&k))
            {
                conflicts.push(Conflict {
                    var: existing.clone(),
                    first: first.clone(),
                    second: provider.clone(),
                });
                continue;
            }
            m.vars.insert(k, (provider.clone(), v));
        }
        m.unset.extend(c.unset);
    }
    let set: Vec<String> = m.vars.keys().cloned().collect();
    m.unset
        .retain(|u| !set.iter().any(|k| k.eq_ignore_ascii_case(u)));
    if conflicts.is_empty() {
        Ok(m)
    } else {
        Err(conflicts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_disjoint_contributions_in_order() {
        let a = Contribution::new().literal("A", "1");
        let b = Contribution::new().literal("B", "2");
        let m = merge(vec![("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(
            m.vars["A"],
            ("a".to_string(), VarValue::Literal("1".into()))
        );
        assert_eq!(m.vars["B"].0, "b");
    }

    #[test]
    fn reports_conflicts_naming_both_providers() {
        let a = Contribution::new().literal("GH_TOKEN", "1");
        let b = Contribution::new().literal("gh_token", "2");
        let errs = merge(vec![("github".into(), a), ("env".into(), b)]).unwrap_err();
        assert_eq!(
            errs,
            vec![Conflict {
                var: "GH_TOKEN".into(),
                first: "github".into(),
                second: "env".into()
            }]
        );
        assert_eq!(
            errs[0].to_string(),
            "GH_TOKEN is set by both github and env"
        );
    }

    #[test]
    fn a_set_variable_is_never_unset() {
        let a = Contribution::new().unset("AWS_PROFILE").unset("GIT_SSH");
        let b = Contribution::new().literal("aws_profile", "x");
        let m = merge(vec![("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(m.unset, BTreeSet::from(["GIT_SSH".to_string()]));
    }

    #[test]
    fn value_detects_secret_references() {
        let c = Contribution::new()
            .value("A", "plain")
            .unwrap()
            .value("B", "secret:work/b")
            .unwrap();
        assert_eq!(c.vars["A"], VarValue::Literal("plain".into()));
        assert_eq!(
            c.vars["B"],
            VarValue::Secret(SecretRef::parse_path("work/b").unwrap())
        );
        assert!(Contribution::new().value("C", "secret:bad").is_err());
    }
}
