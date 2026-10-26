//! Which terminal a token belongs to (spec §2): a map from token to the terminal's name and
//! folder, alive while the terminal is.

use std::collections::HashMap;
use std::path::PathBuf;

use super::SessionState;
use crate::runs::{Change, MIN_WEEK_LEFT, Runs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// Unique among registered terminals, and fits `[name]` in the board file.
    pub name: String,
    pub cwd: PathBuf,
    /// The hydra environment it runs in; `None` outside one (tests).
    pub env: Option<String>,
}

/// Another terminal with a Claude session running, as `tasks_read` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub cwd: PathBuf,
    pub state: SessionState,
}

pub struct Registry {
    terminals: HashMap<String, Terminal>,
    /// What each terminal's Claude session last said it is doing (its status hooks).
    sessions: HashMap<String, SessionState>,
    /// The orchestrator's runs (orchestrator spec §3), behind the same lock as the tokens.
    pub runs: Runs,
    /// The share of the seven-day Claude limit a stage needs left (orchestrator spec §5.1).
    pub min_week_left: u8,
    /// `<HYDRA_HOME>`, where pipelines and run logs live; the run tools refuse without it.
    pub hydra_root: Option<PathBuf>,
    /// Claude session ids already written to a run's log, as (run, who, id) (§13).
    logged_claude: std::collections::HashSet<(String, String, String)>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            terminals: HashMap::new(),
            sessions: HashMap::new(),
            runs: Runs::default(),
            min_week_left: MIN_WEEK_LEFT,
            hydra_root: None,
            logged_claude: Default::default(),
        }
    }
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
        self.register_in(label, cwd, None)
    }

    /// As `register`, for a terminal in environment `env`.
    pub fn register_in(&mut self, label: &str, cwd: PathBuf, env: Option<String>) -> String {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the OS has a random source");
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let name = self.unique_name(&token, label);
        self.terminals
            .insert(token.clone(), Terminal { name, cwd, env });
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

    /// The terminal holding `token` closed: forgets it, and returns what that changed for the
    /// runs (a stage it was running fails, a run it orchestrated stops).
    pub fn revoke(&mut self, token: &str) -> Vec<Change> {
        self.terminals.remove(token);
        self.sessions.remove(token);
        self.runs.session_gone(token)
    }

    /// What `token`'s Claude session last said it is doing.
    pub fn session(&self, token: &str) -> Option<SessionState> {
        self.sessions.get(token).copied()
    }

    /// The Claude session in `token`'s terminal is in `state`; `Ended` forgets it.
    pub fn set_session(&mut self, token: &str, state: SessionState) {
        if state == SessionState::Ended || !self.terminals.contains_key(token) {
            self.sessions.remove(token);
        } else {
            self.sessions.insert(token.to_string(), state);
        }
    }

    /// Claude session `id` reported from `token`'s terminal: the run and who it is there
    /// (`orchestrator`, or the stage attempt's label) the first time that run hears of it, so
    /// its log can keep it for a restart (orchestrator spec §13); `None` otherwise.
    pub fn claude_session(&mut self, token: &str, id: &str) -> Option<(String, String)> {
        let (run, who) = match self.runs.role(token) {
            crate::runs::Role::Orchestrator(r) => (r.id.clone(), "orchestrator".to_string()),
            crate::runs::Role::Stage(r, i) => (r.id.clone(), r.stages[i].latest()?.label.clone()),
            crate::runs::Role::Other => return None,
        };
        self.logged_claude
            .insert((run.clone(), who.clone(), id.to_string()))
            .then_some((run, who))
    }

    /// The terminals other than `token`'s with a Claude session running, by name.
    pub fn peers(&self, token: &str) -> Vec<Peer> {
        let mut peers: Vec<Peer> = self
            .sessions
            .iter()
            .filter(|(t, _)| t.as_str() != token)
            .filter_map(|(t, state)| {
                let term = self.terminals.get(t)?;
                Some(Peer {
                    name: term.name.clone(),
                    cwd: term.cwd.clone(),
                    state: *state,
                })
            })
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        peers
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
    fn peers_are_the_other_terminals_running_claude() {
        let mut r = Registry::default();
        let a = r.register("a", "C:/w".into());
        let b = r.register("b", "C:/w".into());
        let c = r.register("c", "C:/w".into());
        r.set_session(&b, SessionState::Working);
        r.set_session(&c, SessionState::Idle);
        r.set_session("stranger", SessionState::Working);
        let names: Vec<String> = r.peers(&a).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["b", "c"]);
        assert_eq!(r.peers(&b).len(), 1, "not itself");
        r.set_session(&c, SessionState::Ended);
        r.revoke(&b);
        assert!(r.peers(&a).is_empty());
    }

    #[test]
    fn a_name_is_never_a_checkbox() {
        assert_eq!(clean_name("x"), "x-term");
        assert_eq!(clean_name(" X "), "X-term");
        assert_eq!(clean_name("xy"), "xy");
    }
}
