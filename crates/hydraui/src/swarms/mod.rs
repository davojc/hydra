//! hydraui's side of swarms: where they are kept, what this machine is called in them, and
//! their keys.
use std::path::PathBuf;

use hydra_core::paths::HydraPaths;

pub mod store;

/// Where every swarm is kept: `<state root>/swarms`, never an environment's state folder.
pub fn root(paths: &HydraPaths) -> PathBuf {
    paths.state_root().join("swarms")
}

/// Whether this machine is in any swarm. A registry that can't be read as a list still counts:
/// the service sets a damaged one aside and says so. Reads a file: never on the UI thread.
pub fn has_swarms(paths: &HydraPaths) -> bool {
    let Ok(bytes) = std::fs::read(root(paths).join("registry.json")) else {
        return false;
    };
    serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).map_or(true, |all| !all.is_empty())
}

/// This machine's name when config.toml gives none: the host name.
pub fn default_machine_name() -> String {
    machine_name_from(
        std::env::var("COMPUTERNAME").ok(),
        std::env::var("HOSTNAME").ok(),
    )
}

/// `computer_name` (Windows), else `host_name`, else "This machine".
pub fn machine_name_from(computer_name: Option<String>, host_name: Option<String>) -> String {
    [computer_name, host_name]
        .into_iter()
        .flatten()
        .map(|n| n.trim().to_string())
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| "This machine".into())
}
