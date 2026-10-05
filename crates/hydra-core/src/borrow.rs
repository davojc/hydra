//! Borrowing a tool's login from another environment: `[claude]` `from = "personal"`.
use std::collections::BTreeMap;

use crate::envedit::TOOLS;
use crate::name::EnvName;
use crate::paths::HydraPaths;

/// Tools that can't be borrowed: commit author and keys stay per environment.
pub const NOT_BORROWABLE: &[&str] = &["git"];

/// Keys a borrowed section still sets itself: `[github]` owners and strict are guard policy,
/// which stays per environment.
pub fn own_keys(tool: &str) -> &'static [&'static str] {
    if tool == "github" {
        &["owners", "strict"]
    } else {
        &[]
    }
}

/// Replaces each `[tool] from = "<owner>"` section in `table` with the owner's
/// section and returns which tools are borrowed from whom. Errors are
/// user-facing, with `  -> ` fix lines.
pub fn resolve(
    paths: &HydraPaths,
    name: &EnvName,
    table: &mut toml::Table,
) -> Result<BTreeMap<String, EnvName>, String> {
    let mut borrowed = BTreeMap::new();
    for (tool, section) in borrowing(table) {
        let (owned, owner) = resolve_one(paths, name, tool, &section)?;
        table.insert(tool.to_string(), toml::Value::Table(owned));
        borrowed.insert(tool.to_string(), owner);
    }
    Ok(borrowed)
}

/// Like [`resolve`], but a section that can't be borrowed doesn't fail the whole table: it
/// is removed, and its tool and error are returned (in TOOLS order) next to what was borrowed.
pub fn resolve_lenient(
    paths: &HydraPaths,
    name: &EnvName,
    table: &mut toml::Table,
) -> (BTreeMap<String, EnvName>, Vec<(String, String)>) {
    let mut borrowed = BTreeMap::new();
    let mut failed = Vec::new();
    for (tool, section) in borrowing(table) {
        match resolve_one(paths, name, tool, &section) {
            Ok((owned, owner)) => {
                table.insert(tool.to_string(), toml::Value::Table(owned));
                borrowed.insert(tool.to_string(), owner);
            }
            Err(e) => {
                table.remove(tool);
                failed.push((tool.to_string(), e));
            }
        }
    }
    (borrowed, failed)
}

/// Keeps only the environment's own settings, without reading any other environment: a
/// borrowed section is removed, except for the keys it still sets itself ([`own_keys`]).
pub fn keep_own(table: &mut toml::Table) {
    for (tool, mut section) in borrowing(table) {
        let keep = own_keys(tool);
        if keep.is_empty() {
            table.remove(tool);
        } else {
            section.retain(|k, _| keep.contains(&k));
            table.insert(tool.to_string(), toml::Value::Table(section));
        }
    }
}

/// The tool sections in `table` that have a `from` key, in TOOLS order.
fn borrowing(table: &toml::Table) -> Vec<(&'static str, toml::Table)> {
    TOOLS
        .iter()
        .filter_map(|tool| match table.get(*tool) {
            Some(toml::Value::Table(s)) if s.contains_key("from") => Some((*tool, s.clone())),
            _ => None,
        })
        .collect()
}

/// The owner's section for one borrowing `section`, with the borrower's own keys in place of
/// the owner's, and the owner's name.
fn resolve_one(
    paths: &HydraPaths,
    name: &EnvName,
    tool: &str,
    section: &toml::Table,
) -> Result<(toml::Table, EnvName), String> {
    if NOT_BORROWABLE.contains(&tool) {
        return Err(format!(
            "[{tool}] can't be borrowed: commit author and SSH key stay per environment\n  -> set [{tool}] in {name} itself"
        ));
    }
    let from = section.get("from").expect("a borrowing section has from");
    let owner = owner_name(name, tool, from)?;
    let keep = own_keys(tool);
    if let Some(extra) = section
        .keys()
        .find(|k| *k != "from" && !keep.contains(&k.as_str()))
    {
        return Err(format!(
            "[{tool}] borrows from {owner}, so it can't also set {extra}\n  -> remove {extra}; {owner}'s [{tool}] settings are used"
        ));
    }
    let mut owned = owner_section(paths, name, tool, &owner)?;
    for key in keep {
        owned.remove(*key);
        if let Some(v) = section.get(*key) {
            owned.insert(key.to_string(), v.clone());
        }
    }
    Ok((owned, owner))
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

/// The `from` that env.toml `text` itself names for `tool`, without reading the owner (so it
/// works when the owner is broken). None if the text doesn't parse or the tool isn't borrowed.
pub fn declared_owner(text: &str, tool: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let doc = toml::from_str::<toml::Table>(text).ok()?;
    doc.get(tool)?.get("from")?.as_str().map(str::to_string)
}

/// Every environment that borrows something from `owner`, with the tools, sorted by name.
/// Unreadable or broken env files are skipped (they fail on their own launch).
pub fn borrowers(paths: &HydraPaths, owner: &EnvName) -> Vec<(EnvName, Vec<String>)> {
    let Ok(list) = crate::envs::list(paths) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in list.valid {
        let Ok(text) = std::fs::read_to_string(paths.env_file(&n)) else {
            continue;
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let Ok(doc) = toml::from_str::<toml::Table>(text) else {
            continue;
        };
        let tools: Vec<String> = TOOLS
            .iter()
            .filter(|t| {
                doc.get(**t)
                    .and_then(|s| s.get("from"))
                    .and_then(|f| f.as_str())
                    == Some(owner.as_str())
            })
            .map(|t| t.to_string())
            .collect();
        if !tools.is_empty() {
            out.push((n, tools));
        }
    }
    out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_owner_reads_only_the_text() {
        let text = "[claude]\nfrom = \"personal\"\n[codex]\n";
        assert_eq!(declared_owner(text, "claude").as_deref(), Some("personal"));
        assert_eq!(declared_owner(text, "codex"), None);
        assert_eq!(declared_owner(text, "aws"), None);
        assert_eq!(
            declared_owner("\u{feff}codex = { from = \"p\" }\r\n", "codex").as_deref(),
            Some("p")
        );
        assert_eq!(declared_owner("[[[ nope", "claude"), None);
    }

    #[test]
    fn borrowers_lists_who_borrows_what() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        for (n, t) in [
            ("personal", "[claude]\n[github]\n"),
            (
                "work",
                "[claude]\nfrom = \"personal\"\n[github]\nfrom = \"personal\"\n",
            ),
            ("client", "codex = { from = \"personal\" }\n"),
            ("other", "[claude]\nfrom = \"work\"\n"),
            ("broken", "[[[ nope"),
        ] {
            let n = EnvName::parse(n).unwrap();
            std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
            std::fs::write(paths.env_file(&n), t).unwrap();
        }
        let got = borrowers(&paths, &EnvName::parse("personal").unwrap());
        let got: Vec<(String, Vec<String>)> =
            got.into_iter().map(|(e, t)| (e.to_string(), t)).collect();
        assert_eq!(
            got,
            vec![
                ("client".to_string(), vec!["codex".to_string()]),
                (
                    "work".to_string(),
                    vec!["claude".to_string(), "github".to_string()]
                ),
            ]
        );
    }
}
