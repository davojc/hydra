use std::path::PathBuf;

use anyhow::Context;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_core::secret::SecretStore;
use hydra_platform::keyring_store::KeyringStore;

pub struct App {
    pub paths: HydraPaths,
    pub user_home: PathBuf,
    pub store: Box<dyn SecretStore>,
}

/// `HYDRA_USER_HOME`, else the user's home folder.
pub fn user_home() -> anyhow::Result<PathBuf> {
    match std::env::var_os("HYDRA_USER_HOME") {
        Some(h) => Ok(PathBuf::from(h)),
        None => dirs::home_dir().context("can't find your home folder"),
    }
}

impl App {
    pub fn from_env() -> anyhow::Result<Self> {
        let paths = HydraPaths::discover()?;
        let user_home = user_home()?;
        let store = Box::new(KeyringStore::from_env(&paths));
        Ok(Self {
            paths,
            user_home,
            store,
        })
    }

    /// A valid name of an environment that exists.
    pub fn env_name(&self, s: &str) -> anyhow::Result<EnvName> {
        let n = EnvName::parse(s)?;
        if !self.paths.env_file(&n).is_file() {
            anyhow::bail!("environment {n} doesn't exist (create it with `hydra env new {n}`)");
        }
        Ok(n)
    }
}
