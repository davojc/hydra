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
    /// Registers a terminal started in `cwd`; `None` when the server isn't running.
    pub fn register(cwd: PathBuf) -> Option<McpTerminal> {
        Some(McpTerminal::register_on(server::get()?.clone(), cwd))
    }

    fn register_on(server: Arc<server::Server>, cwd: PathBuf) -> McpTerminal {
        let token = server::lock(server.registry()).register(&label_for(None, &cwd), cwd.clone());
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
        let term = McpTerminal::register(PathBuf::from("C:/w/work")).unwrap();
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
        let term = McpTerminal::register_on(server.clone(), PathBuf::from("C:/w/web"));
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
        let term = McpTerminal::register_on(server.clone(), PathBuf::from("C:/w/web"));
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
