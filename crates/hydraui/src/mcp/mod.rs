//! hydraui's MCP server (spec: docs/superpowers/specs/2026-10-20-hydraui-mcp-server-design.md):
//! the token registry and the task-board tools.
pub mod registry;
pub mod server;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use crate::pane_view::label_for;

/// What a Claude session in a terminal is doing, as its hooks report it (`hydra hook <state>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Working on a prompt.
    Working,
    /// Asking you something: a permission, a question, a plan to approve.
    Waiting,
    /// Finished its turn, waiting for your next prompt.
    Idle,
    /// The session ended.
    Ended,
}

impl SessionState {
    pub fn parse(s: &str) -> Option<SessionState> {
        match s {
            "working" => Some(SessionState::Working),
            "waiting" => Some(SessionState::Waiting),
            "idle" => Some(SessionState::Idle),
            "ended" => Some(SessionState::Ended),
            _ => None,
        }
    }

    /// Reads a hook's report (`hydra hook <state>`): the state, and whether the session just
    /// started by resuming a conversation. `resumed` (SessionStart for `claude -c`, `claude -r`
    /// or `/resume`) is idle in every way except that the session isn't a new one.
    pub fn parse_report(s: &str) -> Option<(SessionState, bool)> {
        match s {
            "resumed" => Some((SessionState::Idle, true)),
            _ => SessionState::parse(s).map(|state| (state, false)),
        }
    }
}

/// One of Claude's rate-limit windows, as its status line reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    /// Percent of the window used.
    pub used: f64,
    /// When the window resets, in Unix seconds.
    pub resets_at: Option<i64>,
}

/// What a Claude session's status line said about its account's usage (`hydra statusline`):
/// the five-hour and seven-day windows (Pro and Max accounts only) and how full its context is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub five_hour: Option<Limit>,
    pub seven_day: Option<Limit>,
    pub context: Option<f64>,
}

impl Usage {
    /// Reads Claude Code's status JSON; missing or null fields are `None`.
    pub fn parse(status: &serde_json::Value) -> Usage {
        let limit = |key: &str| {
            let w = &status["rate_limits"][key];
            w["used_percentage"].as_f64().map(|used| Limit {
                used,
                resets_at: w["resets_at"].as_i64(),
            })
        };
        Usage {
            five_hour: limit("five_hour"),
            seven_day: limit("seven_day"),
            context: status["context_window"]["used_percentage"].as_f64(),
        }
    }

    /// Whether it says anything about the account's limits.
    pub fn has_limits(&self) -> bool {
        self.five_hour.is_some() || self.seven_day.is_some()
    }
}

/// Why a session's change of state calls for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// It started asking you something.
    NeedsYou,
    /// It finished a turn it was working on.
    Finished,
}

/// The alert, if any, for a session going from `before` to `now`: starting to wait, or going
/// idle after working (a fresh session starting idle isn't news).
pub fn alert_for(before: Option<SessionState>, now: SessionState) -> Option<Alert> {
    match (before, now) {
        (Some(SessionState::Waiting), SessionState::Waiting) => None,
        (_, SessionState::Waiting) => Some(Alert::NeedsYou),
        (Some(SessionState::Working), SessionState::Idle) => Some(Alert::Finished),
        _ => None,
    }
}

/// A terminal's entry in the server's registry (§2): its token lives as long as this does.
pub struct McpTerminal {
    server: Arc<server::Server>,
    token: String,
    /// The folder the terminal started in, which names it when the user hasn't (§2).
    start: PathBuf,
}

impl McpTerminal {
    /// Registers a terminal started in `cwd` in environment `env`; `None` when the server
    /// isn't running.
    pub fn register(cwd: PathBuf, env: &str) -> Option<McpTerminal> {
        Some(McpTerminal::register_on(
            server::get()?.clone(),
            cwd,
            Some(env.to_string()),
        ))
    }

    fn register_on(server: Arc<server::Server>, cwd: PathBuf, env: Option<String>) -> McpTerminal {
        let token =
            server::lock(server.registry()).register_in(&label_for(None, &cwd), cwd.clone(), env);
        McpTerminal {
            server,
            token,
            start: cwd,
        }
    }

    /// The variables the terminal's shell gets.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("HYDRA_MCP_URL".into(), self.server.url().to_string()),
            ("HYDRA_MCP_TOKEN".into(), self.token.clone()),
        ]
    }

    /// The terminal's name in the server: the taker its sessions write on cards (§4).
    pub fn name(&self) -> Option<String> {
        server::lock(self.server.registry())
            .get(&self.token)
            .map(|t| t.name)
    }

    /// The terminal's token, which names it in the server's events.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn set_cwd(&self, cwd: PathBuf) {
        server::lock(self.server.registry()).set_cwd(&self.token, cwd);
    }

    /// Names the terminal after the user's name for it, else its start folder: a `cd` never
    /// renames it (§2).
    pub fn rename(&self, name: Option<&str>) {
        server::lock(self.server.registry()).rename(&self.token, &label_for(name, &self.start));
    }
}

impl Drop for McpTerminal {
    fn drop(&mut self) {
        server::lock(self.server.registry()).revoke(&self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_reads_the_limits_claude_gave() {
        let u = Usage::parse(&serde_json::json!({
            "rate_limits": {"five_hour": {"used_percentage": 42.5, "resets_at": 1700000000}},
            "context_window": {"used_percentage": null},
        }));
        assert_eq!(
            u.five_hour,
            Some(Limit {
                used: 42.5,
                resets_at: Some(1700000000)
            })
        );
        assert_eq!((u.seven_day, u.context), (None, None));
        assert!(u.has_limits());
        assert!(!Usage::parse(&serde_json::json!({})).has_limits());
    }

    #[test]
    fn a_resumed_report_is_idle_but_resumed() {
        use SessionState::*;
        assert_eq!(SessionState::parse_report("resumed"), Some((Idle, true)));
        assert_eq!(SessionState::parse_report("idle"), Some((Idle, false)));
        assert_eq!(
            SessionState::parse_report("working"),
            Some((Working, false))
        );
        assert_eq!(SessionState::parse_report("asleep"), None);
        assert_eq!(SessionState::parse("resumed"), None);
    }

    #[test]
    fn only_waiting_and_finishing_alert() {
        use SessionState::*;
        assert_eq!(alert_for(Some(Working), Waiting), Some(Alert::NeedsYou));
        assert_eq!(alert_for(None, Waiting), Some(Alert::NeedsYou));
        assert_eq!(alert_for(Some(Waiting), Waiting), None, "already said");
        assert_eq!(alert_for(Some(Working), Idle), Some(Alert::Finished));
        assert_eq!(alert_for(Some(Waiting), Idle), None);
        assert_eq!(alert_for(None, Idle), None, "a session starting");
        assert_eq!(alert_for(Some(Idle), Working), None);
        assert_eq!(alert_for(Some(Working), Ended), None);
    }

    #[test]
    fn a_terminal_gets_the_servers_address_and_a_token_that_dies_with_it() {
        let server = server::init().expect("server starts");
        let term = McpTerminal::register(PathBuf::from("C:/w/work"), "work").unwrap();
        let env = term.env();
        assert_eq!(
            env[0],
            ("HYDRA_MCP_URL".to_string(), server.url().to_string())
        );
        assert_eq!(env[1].0, "HYDRA_MCP_TOKEN");
        let token = env[1].1.clone();
        let t = server.registry().lock().unwrap().get(&token).unwrap();
        assert_eq!(t.name, "work");
        term.rename(Some("api"));
        assert_eq!(term.name().as_deref(), Some("api"));
        term.set_cwd(PathBuf::from("C:/x"));
        let t = server.registry().lock().unwrap().get(&token).unwrap();
        assert_eq!(t.cwd, PathBuf::from("C:/x"));
        // A folder change moves the terminal but keeps its name.
        assert_eq!(t.name, "api");
        drop(term);
        assert!(server.registry().lock().unwrap().get(&token).is_none());
    }

    #[test]
    fn clearing_the_name_goes_back_to_the_start_folders() {
        let server = server::Server::start().unwrap();
        let term = McpTerminal::register_on(server.clone(), PathBuf::from("C:/w/web"), None);
        term.rename(Some("api"));
        term.set_cwd(PathBuf::from("C:/w/other"));
        term.rename(None);
        let token = term.env()[1].1.clone();
        let t = server.registry().lock().unwrap().get(&token).unwrap();
        assert_eq!(t.name, "web");
    }

    #[test]
    fn a_closed_terminals_token_dies_even_after_a_panic() {
        let server = server::Server::start().unwrap();
        let term = McpTerminal::register_on(server.clone(), PathBuf::from("C:/w/web"), None);
        let token = term.env()[1].1.clone();
        let poisoner = server.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.registry().lock();
            panic!("poison the registry");
        })
        .join();
        assert!(server.registry().is_poisoned());
        drop(term);
        let registry = server.registry().lock().unwrap_or_else(|e| e.into_inner());
        assert!(registry.get(&token).is_none());
    }
}
