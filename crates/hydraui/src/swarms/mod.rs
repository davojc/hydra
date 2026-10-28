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

/// Whether the swarms have anything to run here: a swarm, or another machine of yours linked
/// to this one (which offers it swarms). Reads files: never on the UI thread.
pub fn has_swarms_or_machines(paths: &HydraPaths) -> bool {
    has_swarms(paths) || has_linked_machines(paths)
}

/// Whether `mine.json` lists another machine of yours. Only read: a damaged file is the
/// service's to set aside.
fn has_linked_machines(paths: &HydraPaths) -> bool {
    let Ok(bytes) = std::fs::read(root(paths).join(hydra_swarms::mine::MINE_FILE)) else {
        return false;
    };
    serde_json::from_slice::<hydra_swarms::mine::Mine>(&bytes).is_ok_and(|m| {
        m.devices
            .iter()
            .filter(|d| !m.removed.contains(&d.id))
            .count()
            > 1
    })
}

/// Keeps your name as another machine of yours set it. Writes config.toml: never on the UI
/// thread.
pub fn save_named(paths: &HydraPaths, name: &str) -> Result<(), String> {
    crate::config::set(paths, "peer_name", Some(name.into()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_mine(paths: &HydraPaths, devices: usize) {
        let root = root(paths);
        std::fs::create_dir_all(&root).unwrap();
        let devices: Vec<serde_json::Value> = (0..devices)
            .map(|i| serde_json::json!({"id": format!("d{i}"), "name": "pc", "name_at": 1}))
            .collect();
        let mine = serde_json::json!({"name": "Davo", "name_at": 1, "devices": devices,
            "removed": [], "chain": []});
        std::fs::write(root.join("mine.json"), mine.to_string()).unwrap();
    }

    #[test]
    fn linked_machines_count_as_something_to_run() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert!(!has_swarms_or_machines(&paths));
        write_mine(&paths, 1);
        assert!(!has_swarms_or_machines(&paths), "only this machine");
        write_mine(&paths, 2);
        assert!(has_swarms_or_machines(&paths));
        // Read, never set aside: that is the service's to do.
        std::fs::write(root(&paths).join("mine.json"), "not json").unwrap();
        assert!(!has_swarms_or_machines(&paths));
        assert!(root(&paths).join("mine.json").exists());
    }

    #[test]
    fn a_name_from_another_machine_is_saved() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        save_named(&paths, "Olive O").unwrap();
        assert_eq!(
            crate::config::load(&paths).0.peer_name.as_deref(),
            Some("Olive O")
        );
    }
}
