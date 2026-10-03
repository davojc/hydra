use std::path::PathBuf;

use crate::config::{ConfigError, load_global};
use crate::lock;
use crate::name::EnvName;
use crate::paths::HydraPaths;
use crate::secret::{SecretStore, StoreError, delete_env_secrets};

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

# [git]
# name    = "Your Name"
# email   = "you@example.com"
# ssh_key = "~/.ssh/id_ed25519"

# [github]                          # then run: hydra auth github {name}
# owners = ["your-org"]

# [aws]
# profile = "my-profile"

# [env]
# MY_API_KEY = "secret:{name}/my-api-key"   # store it with: hydra secret set {name}/my-api-key
"##
    )
}

pub fn create(paths: &HydraPaths, name: &EnvName) -> Result<PathBuf, EnvError> {
    if paths.env_dir(name).exists() {
        return Err(EnvError::Exists(name.clone()));
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
}
