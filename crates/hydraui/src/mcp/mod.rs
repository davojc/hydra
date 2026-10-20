//! hydraui's MCP server (spec: docs/superpowers/specs/2026-10-20-hydraui-mcp-server-design.md):
//! the token registry and the task-board tools.
pub mod registry;
pub mod server;
pub mod tools;

use std::path::PathBuf;

/// A terminal's entry in the server's registry (§2): its token lives as long as this does.
pub struct McpTerminal {
    token: String,
}

impl McpTerminal {
    /// Registers a terminal; `None` when the server isn't running.
    pub fn register(label: &str, cwd: PathBuf) -> Option<McpTerminal> {
        let server = server::get()?;
        let token = server.registry().lock().ok()?.register(label, cwd);
        Some(McpTerminal { token })
    }

    /// The variables the terminal's shell gets.
    pub fn env(&self) -> Vec<(String, String)> {
        let url = server::get()
            .map(|s| s.url().to_string())
            .unwrap_or_default();
        vec![
            ("HYDRA_MCP_URL".into(), url),
            ("HYDRA_MCP_TOKEN".into(), self.token.clone()),
        ]
    }

    pub fn set_cwd(&self, cwd: PathBuf) {
        if let Some(server) = server::get()
            && let Ok(mut r) = server.registry().lock()
        {
            r.set_cwd(&self.token, cwd);
        }
    }

    pub fn rename(&self, label: &str) {
        if let Some(server) = server::get()
            && let Ok(mut r) = server.registry().lock()
        {
            r.rename(&self.token, label);
        }
    }
}

impl Drop for McpTerminal {
    fn drop(&mut self) {
        if let Some(server) = server::get()
            && let Ok(mut r) = server.registry().lock()
        {
            r.revoke(&self.token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_gets_the_servers_address_and_a_token_that_dies_with_it() {
        let server = server::init().expect("server starts");
        let term = McpTerminal::register("work", PathBuf::from("C:/w")).unwrap();
        let env = term.env();
        assert_eq!(
            env[0],
            ("HYDRA_MCP_URL".to_string(), server.url().to_string())
        );
        assert_eq!(env[1].0, "HYDRA_MCP_TOKEN");
        let token = env[1].1.clone();
        assert!(server.registry().lock().unwrap().get(&token).is_some());
        term.rename("api");
        term.set_cwd(PathBuf::from("C:/x"));
        let t = server.registry().lock().unwrap().get(&token).unwrap();
        assert_eq!(t.cwd, PathBuf::from("C:/x"));
        // A folder change moves the terminal but keeps its name.
        assert_eq!(t.name, "api");
        drop(term);
        assert!(server.registry().lock().unwrap().get(&token).is_none());
    }
}
