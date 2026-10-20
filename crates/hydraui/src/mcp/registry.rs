//! Which terminal a token belongs to (spec §2): a map from token to the terminal's name and
//! folder, alive while the terminal is.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// Unique among registered terminals, and fits `[name]` in the board file.
    pub name: String,
    pub cwd: PathBuf,
}

#[derive(Default)]
pub struct Registry {
    terminals: HashMap<String, Terminal>,
}

/// A label made safe as a taker: whitespace runs become one space, `]` becomes `)`, an empty
/// result is `terminal`, and `x` or `X` (a checkbox in the board file) gets `-term` added.
pub fn clean_name(label: &str) -> String {
    let name = label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(']', ")");
    match name.as_str() {
        "" => "terminal".into(),
        "x" | "X" => format!("{name}-term"),
        _ => name,
    }
}

impl Registry {
    /// `label` cleaned, then numbered (`name-2`, `name-3`, ...) if another token holds it.
    fn unique_name(&self, token: &str, label: &str) -> String {
        let base = clean_name(label);
        let taken = |n: &str| {
            self.terminals
                .iter()
                .any(|(t, x)| t != token && x.name == n)
        };
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|n| !taken(n))
            .expect("an unbounded range has a free name")
    }

    /// Adds a terminal and returns its token: 128 random bits as 32 hex characters.
    pub fn register(&mut self, label: &str, cwd: PathBuf) -> String {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the OS has a random source");
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let name = self.unique_name(&token, label);
        self.terminals.insert(token.clone(), Terminal { name, cwd });
        token
    }

    pub fn rename(&mut self, token: &str, label: &str) {
        let name = self.unique_name(token, label);
        if let Some(t) = self.terminals.get_mut(token) {
            t.name = name;
        }
    }

    pub fn set_cwd(&mut self, token: &str, cwd: PathBuf) {
        if let Some(t) = self.terminals.get_mut(token) {
            t.cwd = cwd;
        }
    }

    pub fn revoke(&mut self, token: &str) {
        self.terminals.remove(token);
    }

    pub fn get(&self, token: &str) -> Option<Terminal> {
        self.terminals.get(token).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_clean_and_follow_renames() {
        let mut r = Registry::default();
        let a = r.register("hydra", "C:\\w".into());
        let b = r.register("hydra", "C:\\w".into());
        assert_eq!(r.get(&b).unwrap().name, "hydra-2");
        let c = r.register(" odd ] \n name ", "C:\\w".into());
        assert_eq!(r.get(&c).unwrap().name, "odd ) name");
        r.revoke(&a);
        assert!(r.get(&a).is_none());
        let d = r.register("hydra", "C:\\w".into());
        assert_eq!(r.get(&d).unwrap().name, "hydra");
        r.rename(&b, "api");
        assert_eq!(r.get(&b).unwrap().name, "api");
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    #[test]
    fn blank_labels_and_self_renames_are_handled() {
        let mut r = Registry::default();
        let a = r.register("  ", "C:\\w".into());
        assert_eq!(r.get(&a).unwrap().name, "terminal");
        r.rename(&a, "terminal");
        assert_eq!(r.get(&a).unwrap().name, "terminal");
        r.set_cwd(&a, "D:\\x".into());
        assert_eq!(r.get(&a).unwrap().cwd, PathBuf::from("D:\\x"));
    }

    #[test]
    fn a_name_is_never_a_checkbox() {
        assert_eq!(clean_name("x"), "x-term");
        assert_eq!(clean_name(" X "), "X-term");
        assert_eq!(clean_name("xy"), "xy");
    }
}
