use std::path::PathBuf;

use crate::config::{ConfigError, load_global};
use crate::lock;
use crate::name::EnvName;
use crate::paths::HydraPaths;
use crate::rewrite;
use crate::secret::{SecretRef, SecretStore, StoreError, delete_env_secrets};

/// Default tab colours, assigned in order as environments are created.
pub const PALETTE: [&str; 6] = [
    "#1f9a8a", "#c98a1e", "#7a5cc4", "#3a7bd5", "#c2413b", "#4f8a2f",
];

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("environment {0} already exists")]
    Exists(EnvName),
    #[error("environment {0} doesn't exist")]
    NotFound(EnvName),
    #[error("a hydra terminal for {0} is still open; close it first")]
    Running(EnvName),
    #[error("{0}")]
    Lends(String),
    #[error(
        "a hydra terminal for {borrower} (which borrows from {owner}) is still open; close it first"
    )]
    BorrowerRunning { borrower: EnvName, owner: EnvName },
    #[error(
        "saved logins from an earlier {name} still exist in {}; delete that folder to start fresh",
        path.display()
    )]
    LeftoverState { name: EnvName, path: PathBuf },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Rewrite(String),
}

#[derive(Debug, Default)]
pub struct EnvList {
    pub valid: Vec<EnvName>,
    /// Folder names under envs/ that aren't valid environment names.
    pub invalid: Vec<String>,
}

pub fn list(paths: &HydraPaths) -> std::io::Result<EnvList> {
    let mut out = EnvList::default();
    let entries = match std::fs::read_dir(paths.envs_dir()) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().into_owned();
        match EnvName::parse(&folder) {
            Ok(n) if paths.env_file(&n).is_file() => out.valid.push(n),
            Ok(_) => {}
            Err(_) => out.invalid.push(folder),
        }
    }
    out.valid.sort();
    out.invalid.sort();
    Ok(out)
}

pub fn template(name: &EnvName, color: &str) -> String {
    format!(
        r##"# Settings for the "{name}" environment. The folder name is the environment's name.
label = "{name}"
color = "{color}"
# home = "E:/projects"              # where new terminals for this environment open

# Turn tools on with: hydra add <tool> {name}
#   e.g. hydra add claude {name}
#        hydra add git {name} --name "Your Name" --email you@example.com --ssh-key ~/.ssh/id_ed25519
# See every tool with: hydra add

# [env]
# MY_API_KEY = "secret:{name}/my-api-key"   # store it with: hydra secret set {name}/my-api-key
"##
    )
}

pub fn create(paths: &HydraPaths, name: &EnvName) -> Result<PathBuf, EnvError> {
    if paths.env_dir(name).exists() {
        return Err(EnvError::Exists(name.clone()));
    }
    let state = paths.state_dir(name);
    if state.exists() {
        return Err(EnvError::LeftoverState {
            name: name.clone(),
            path: state,
        });
    }
    let count = list(paths)?.valid.len();
    std::fs::create_dir_all(paths.env_dir(name))?;
    let file = paths.env_file(name);
    std::fs::write(&file, template(name, PALETTE[count % PALETTE.len()]))?;
    Ok(file)
}

#[derive(Debug)]
pub struct RemoveReport {
    pub secrets: Vec<String>,
    pub state_deleted: bool,
    /// Binding patterns in config.toml that still point at the removed environment.
    pub bindings: Vec<String>,
}

/// Refuses while a terminal of an environment that borrows from `owner` is open.
fn refuse_open_borrowers(paths: &HydraPaths, owner: &EnvName) -> Result<(), EnvError> {
    for (b, _) in crate::borrow::borrowers(paths, owner) {
        if lock::is_running(paths, &b)? {
            return Err(EnvError::BorrowerRunning {
                borrower: b,
                owner: owner.clone(),
            });
        }
    }
    Ok(())
}

pub fn remove(
    paths: &HydraPaths,
    name: &EnvName,
    store: &dyn SecretStore,
    delete_state: bool,
) -> Result<RemoveReport, EnvError> {
    if !paths.env_file(name).is_file() {
        return Err(EnvError::NotFound(name.clone()));
    }
    if lock::is_running(paths, name)? {
        return Err(EnvError::Running(name.clone()));
    }
    let lent = crate::borrow::borrowers(paths, name);
    if !lent.is_empty() {
        let lines: Vec<String> = lent
            .iter()
            .map(|(b, tools)| format!("{b} borrows {} from {name}", tools.join(", ")))
            .collect();
        let first = &lent[0];
        return Err(EnvError::Lends(format!(
            "{}\n  -> remove those first, e.g. hydra remove {} {}",
            lines.join("\n"),
            first.1[0],
            first.0
        )));
    }
    let bindings = load_global(paths)?
        .bindings
        .into_iter()
        .filter(|(_, env)| env == name.as_str())
        .map(|(pattern, _)| pattern)
        .collect();
    let secrets = delete_env_secrets(store, name)?;
    std::fs::remove_dir_all(paths.env_dir(name))?;
    let state = paths.state_dir(name);
    let state_deleted = delete_state && state.exists();
    if state_deleted {
        std::fs::remove_dir_all(&state)?;
    }
    Ok(RemoveReport {
        secrets,
        state_deleted,
        bindings,
    })
}

#[derive(Debug)]
pub struct RenameReport {
    pub bindings: Vec<String>,
    pub references: usize,
    pub secrets: Vec<String>,
    pub borrowers: Vec<String>,
}

pub fn rename(
    paths: &HydraPaths,
    old: &EnvName,
    new: &EnvName,
    store: &dyn SecretStore,
) -> Result<RenameReport, EnvError> {
    if !paths.env_file(old).is_file() {
        return Err(EnvError::NotFound(old.clone()));
    }
    if paths.env_dir(new).exists() {
        return Err(EnvError::Exists(new.clone()));
    }
    if lock::is_running(paths, old)? {
        return Err(EnvError::Running(old.clone()));
    }
    refuse_open_borrowers(paths, old)?;
    let new_state = paths.state_dir(new);
    if new_state.exists() {
        return Err(EnvError::Rewrite(format!(
            "saved logins for {new} still exist in {}; delete that folder or pick another name",
            new_state.display()
        )));
    }
    if !store.list(new)?.is_empty() {
        return Err(EnvError::Rewrite(format!(
            "secrets are already stored under {new}; remove them with hydra secret rm or pick another name"
        )));
    }

    // Prepare every rewrite first, so a parse error stops the rename before anything moves.
    let cfg_path = paths.config_file();
    let (new_config, bindings) = match std::fs::read_to_string(&cfg_path) {
        Ok(t) => {
            let (text, changed) = rewrite::rename_bindings(&t, old.as_str(), new.as_str())
                .map_err(|e| EnvError::Rewrite(format!("{}: {e}", cfg_path.display())))?;
            (Some(text), changed)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut env_files = Vec::new();
    let mut borrowers = Vec::new();
    for n in list(paths)?.valid {
        let path = paths.env_file(&n);
        let text = std::fs::read_to_string(&path)?;
        let rewrite_err =
            |e: toml_edit::TomlError| EnvError::Rewrite(format!("{}: {e}", path.display()));
        let (out, refs) =
            rewrite::rename_secret_refs(&text, old.as_str(), new.as_str()).map_err(rewrite_err)?;
        let (out, lent) =
            rewrite::rename_borrow_owner(&out, old.as_str(), new.as_str()).map_err(rewrite_err)?;
        if lent > 0 {
            borrowers.push(n.to_string());
        }
        if refs > 0 || lent > 0 {
            env_files.push((if &n == old { new.clone() } else { n }, out, refs));
        }
    }

    // Copy secrets first; if that fails nothing has changed except the copies, which we remove.
    let secrets = store.list(old)?;
    let mut copied = Vec::new();
    for key in &secrets {
        let src = SecretRef {
            env: old.clone(),
            key: key.clone(),
        };
        let dst = SecretRef {
            env: new.clone(),
            key: key.clone(),
        };
        let result = store.get(&src).and_then(|v| match v {
            Some(v) => store.set(&dst, &v),
            None => Ok(()),
        });
        if let Err(e) = result {
            for c in &copied {
                let _ = store.delete(c);
            }
            return Err(e.into());
        }
        copied.push(dst);
    }

    let partial = |done: &str, failed: &str, e: &dyn std::fmt::Display, fix: String| {
        EnvError::Rewrite(format!(
            "{done} but couldn't {failed}: {e}; {fix} (secrets were copied to {new}; the old ones under {old} are still there)"
        ))
    };
    std::fs::rename(paths.env_dir(old), paths.env_dir(new)).map_err(|e| {
        for c in &copied {
            let _ = store.delete(c);
        }
        EnvError::Io(e)
    })?;
    let old_state = paths.state_dir(old);
    if old_state.exists() {
        std::fs::rename(&old_state, &new_state).map_err(|e| {
            partial(
                &format!("renamed the env folder to {new}"),
                "move the saved logins",
                &e,
                format!(
                    "move {} to {} by hand",
                    old_state.display(),
                    new_state.display()
                ),
            )
        })?;
    }
    if let Some(text) = new_config
        && !bindings.is_empty()
    {
        std::fs::write(&cfg_path, text).map_err(|e| {
            partial(
                &format!("renamed the env folder to {new}"),
                "update config.toml",
                &e,
                format!("change bindings from \"{old}\" to \"{new}\" by hand"),
            )
        })?;
    }
    let mut references = 0;
    for (n, text, count) in env_files {
        let file = paths.env_file(&n);
        std::fs::write(&file, text).map_err(|e| {
            partial(
                &format!("renamed the env folder to {new}"),
                &format!("update {}", file.display()),
                &e,
                format!("change secret:{old}/ references and from = \"{old}\" to {new} by hand"),
            )
        })?;
        references += count;
    }
    for key in &secrets {
        store
            .delete(&SecretRef {
                env: old.clone(),
                key: key.clone(),
            })
            .map_err(|e| {
                partial(
                    &format!("renamed {old} to {new}"),
                    &format!("remove the old secret {old}/{key}"),
                    &e,
                    format!("remove the old secrets with hydra secret rm {old}/{key}"),
                )
            })?;
    }
    Ok(RenameReport {
        bindings,
        references,
        secrets,
        borrowers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_env;
    use crate::secret::{MemoryStore, SecretRef};

    fn home() -> (tempfile::TempDir, HydraPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        (dir, paths)
    }

    fn n(s: &str) -> EnvName {
        EnvName::parse(s).unwrap()
    }

    #[test]
    fn create_writes_a_loadable_template_with_rotating_colours() {
        let (_d, paths) = home();
        create(&paths, &n("work")).unwrap();
        create(&paths, &n("personal")).unwrap();
        let work = load_env(&paths, &n("work")).unwrap();
        assert_eq!(work.label.as_deref(), Some("work"));
        assert_eq!(work.color.as_deref(), Some(PALETTE[0]));
        assert_eq!(
            load_env(&paths, &n("personal")).unwrap().color.as_deref(),
            Some(PALETTE[1])
        );
        assert!(matches!(
            create(&paths, &n("work")),
            Err(EnvError::Exists(_))
        ));
    }

    #[test]
    fn list_separates_invalid_folders() {
        let (_d, paths) = home();
        create(&paths, &n("work")).unwrap();
        std::fs::create_dir_all(paths.envs_dir().join("Bad Name")).unwrap();
        std::fs::create_dir_all(paths.envs_dir().join("empty")).unwrap(); // no env.toml
        let l = list(&paths).unwrap();
        assert_eq!(l.valid, vec![n("work")]);
        assert_eq!(l.invalid, vec!["Bad Name".to_string()]);
    }

    #[test]
    fn remove_deletes_folder_secrets_and_optionally_state() {
        let (_d, paths) = home();
        let store = MemoryStore::default();
        create(&paths, &n("work")).unwrap();
        std::fs::write(paths.config_file(), "[bindings]\n\"E:/w/**\" = \"work\"\n").unwrap();
        store
            .set(&SecretRef::parse_path("work/a").unwrap(), "1")
            .unwrap();
        std::fs::create_dir_all(paths.state_dir(&n("work")).join("gh")).unwrap();

        let r = remove(&paths, &n("work"), &store, false).unwrap();
        assert_eq!(r.secrets, vec!["a"]);
        assert_eq!(r.bindings, vec!["E:/w/**"]);
        assert!(!r.state_deleted);
        assert!(!paths.env_dir(&n("work")).exists());
        assert!(paths.state_dir(&n("work")).exists());
    }

    #[test]
    fn remove_refuses_while_a_shell_is_open() {
        let (_d, paths) = home();
        create(&paths, &n("work")).unwrap();
        let _lock = lock::hold_shared(&paths, &n("work")).unwrap();
        let err = remove(&paths, &n("work"), &MemoryStore::default(), true).unwrap_err();
        assert!(matches!(err, EnvError::Running(_)));
    }

    #[test]
    fn owner_rename_refuses_while_a_borrower_is_open() {
        let (_d, paths) = home();
        create(&paths, &n("personal")).unwrap();
        create(&paths, &n("work")).unwrap();
        std::fs::write(paths.env_file(&n("personal")), "[codex]\n").unwrap();
        std::fs::write(paths.env_file(&n("work")), "[codex]\nfrom = \"personal\"\n").unwrap();
        let _lock = lock::hold_shared(&paths, &n("work")).unwrap();
        let err = rename(&paths, &n("personal"), &n("home"), &MemoryStore::default()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "a hydra terminal for work (which borrows from personal) is still open; close it first"
        );
    }

    #[test]
    fn rename_moves_everything() {
        let (_d, paths) = home();
        let store = MemoryStore::default();
        create(&paths, &n("work")).unwrap();
        create(&paths, &n("other")).unwrap();
        std::fs::write(
            paths.env_file(&n("work")),
            "[env]
A = \"secret:work/a\"
",
        )
        .unwrap();
        std::fs::write(
            paths.env_file(&n("other")),
            "[env]
B = \"secret:work/b\"
",
        )
        .unwrap();
        std::fs::write(
            paths.config_file(),
            "[bindings]
\"E:/w/**\" = \"work\"
",
        )
        .unwrap();
        std::fs::create_dir_all(paths.state_dir(&n("work")).join("gh")).unwrap();
        store
            .set(&SecretRef::parse_path("work/a").unwrap(), "1")
            .unwrap();

        let r = rename(&paths, &n("work"), &n("acme"), &store).unwrap();
        assert_eq!(r.bindings, vec!["E:/w/**"]);
        assert_eq!(r.references, 2);
        assert_eq!(r.secrets, vec!["a"]);
        assert!(!paths.env_dir(&n("work")).exists());
        assert!(paths.state_dir(&n("acme")).join("gh").is_dir());
        assert_eq!(
            load_env(&paths, &n("acme")).unwrap().env["A"],
            "secret:acme/a"
        );
        assert_eq!(
            load_env(&paths, &n("other")).unwrap().env["B"],
            "secret:acme/b"
        );
        assert!(
            std::fs::read_to_string(paths.config_file())
                .unwrap()
                .contains("= \"acme\"")
        );
        assert_eq!(
            store
                .get(&SecretRef::parse_path("acme/a").unwrap())
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn rename_refuses_existing_target_and_open_shells() {
        let (_d, paths) = home();
        let store = MemoryStore::default();
        create(&paths, &n("work")).unwrap();
        create(&paths, &n("home")).unwrap();
        assert!(matches!(
            rename(&paths, &n("work"), &n("home"), &store),
            Err(EnvError::Exists(_))
        ));
        let _lock = lock::hold_shared(&paths, &n("work")).unwrap();
        assert!(matches!(
            rename(&paths, &n("work"), &n("acme"), &store),
            Err(EnvError::Running(_))
        ));
    }

    #[test]
    fn rename_refuses_leftover_state_or_secrets_and_moves_nothing() {
        let (_d, paths) = home();
        let store = MemoryStore::default();
        create(&paths, &n("work")).unwrap();
        std::fs::create_dir_all(paths.state_dir(&n("acme"))).unwrap();
        let err = rename(&paths, &n("work"), &n("acme"), &store).unwrap_err();
        assert!(err.to_string().contains("saved logins for acme"), "{err}");
        assert!(paths.env_file(&n("work")).is_file());
        assert!(!paths.env_dir(&n("acme")).exists());

        std::fs::remove_dir_all(paths.state_dir(&n("acme"))).unwrap();
        store
            .set(&SecretRef::parse_path("acme/x").unwrap(), "1")
            .unwrap();
        store
            .set(&SecretRef::parse_path("work/a").unwrap(), "2")
            .unwrap();
        let err = rename(&paths, &n("work"), &n("acme"), &store).unwrap_err();
        assert!(
            err.to_string().contains("already stored under acme"),
            "{err}"
        );
        assert!(paths.env_file(&n("work")).is_file());
        assert!(!paths.env_dir(&n("acme")).exists());
        assert_eq!(store.list(&n("work")).unwrap(), vec!["a"]);
        assert_eq!(store.list(&n("acme")).unwrap(), vec!["x"]);
    }

    #[test]
    fn create_refuses_leftover_saved_logins() {
        let (_d, paths) = home();
        let state = paths.state_dir(&n("work"));
        std::fs::create_dir_all(state.join("gh")).unwrap();
        let err = create(&paths, &n("work")).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "saved logins from an earlier work still exist in {}; delete that folder to start fresh",
                state.display()
            )
        );
        assert!(!paths.env_dir(&n("work")).exists());
    }
}
