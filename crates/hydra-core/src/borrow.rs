//! Borrowing a tool's login from another environment: `[claude]` `from = "personal"`.
use std::collections::BTreeMap;

use crate::envedit::TOOLS;
use crate::name::EnvName;
use crate::paths::HydraPaths;

/// Tools that can't be borrowed: commit author and keys stay per environment.
pub const NOT_BORROWABLE: &[&str] = &["git"];

/// Replaces each `[tool] from = "<owner>"` section in `table` with the owner's
/// section and returns which tools are borrowed from whom. Errors are
/// user-facing, with `  -> ` fix lines.
pub fn resolve(
    paths: &HydraPaths,
    name: &EnvName,
    table: &mut toml::Table,
) -> Result<BTreeMap<String, EnvName>, String> {
    let mut borrowed = BTreeMap::new();
    for tool in TOOLS {
        let Some(toml::Value::Table(section)) = table.get(*tool) else {
            continue;
        };
        let Some(from) = section.get("from") else {
            continue;
        };
        if NOT_BORROWABLE.contains(tool) {
            return Err(format!(
                "[{tool}] can't be borrowed: commit author and SSH key stay per environment\n  -> set [{tool}] in {name} itself"
            ));
        }
        let owner = owner_name(name, tool, from)?;
        if let Some(extra) = section.keys().find(|k| *k != "from") {
            return Err(format!(
                "[{tool}] borrows from {owner}, so it can't also set {extra}\n  -> remove {extra}; {owner}'s [{tool}] settings are used"
            ));
        }
        let owned = owner_section(paths, name, tool, &owner)?;
        table.insert(tool.to_string(), toml::Value::Table(owned));
        borrowed.insert(tool.to_string(), owner);
    }
    Ok(borrowed)
}

fn owner_name(name: &EnvName, tool: &str, from: &toml::Value) -> Result<EnvName, String> {
    let s = from.as_str().ok_or_else(|| {
        format!("[{tool}] from must be an environment name, like from = \"personal\"")
    })?;
    let owner = EnvName::parse(s).map_err(|e| format!("[{tool}] from: {e}"))?;
    if &owner == name {
        return Err(format!(
            "{name} can't borrow [{tool}] from itself\n  -> remove from = \"{name}\""
        ));
    }
    Ok(owner)
}

/// The owner's own `[tool]` table, read straight from its env.toml.
fn owner_section(
    paths: &HydraPaths,
    name: &EnvName,
    tool: &str,
    owner: &EnvName,
) -> Result<toml::Table, String> {
    let path = paths.env_file(owner);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{name} borrows [{tool}] from {owner}, but there's no environment {owner}\n  -> fix from in [{tool}] in {name}'s env.toml"
            ));
        }
        Err(e) => return Err(format!("can't read {}: {e}", path.display())),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut doc: toml::Table =
        toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;
    match doc.remove(tool) {
        Some(toml::Value::Table(t)) => match t.get("from") {
            Some(next) => {
                let next = next.as_str().unwrap_or("?");
                Err(format!(
                    "{name} borrows [{tool}] from {owner}, which borrows it from {next}\n  -> borrow from the owner: from = \"{next}\""
                ))
            }
            None => Ok(t),
        },
        _ => Err(format!(
            "{name} borrows [{tool}] from {owner}, but {owner} has no [{tool}]\n  -> add it there: hydra add {tool} {owner}"
        )),
    }
}
