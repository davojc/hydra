use std::path::{Path, PathBuf};

use crate::name::EnvName;

/// Every location under the hydra home folder (`~/.hydra`, or `HYDRA_HOME`).
#[derive(Debug, Clone)]
pub struct HydraPaths {
    root: PathBuf,
}

impl HydraPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `HYDRA_HOME` if set, otherwise `~/.hydra`.
    pub fn discover() -> std::io::Result<Self> {
        if let Some(h) = std::env::var_os("HYDRA_HOME") {
            return Ok(Self::new(h));
        }
        let home = dirs::home_dir().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "can't find your home folder")
        })?;
        Ok(Self::new(home.join(".hydra")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn secret_index(&self) -> PathBuf {
        self.root.join("secrets.toml")
    }
    pub fn envs_dir(&self) -> PathBuf {
        self.root.join("envs")
    }
    pub fn env_dir(&self, n: &EnvName) -> PathBuf {
        self.envs_dir().join(n.as_str())
    }
    pub fn env_file(&self, n: &EnvName) -> PathBuf {
        self.env_dir(n).join("env.toml")
    }
    pub fn base_dir(&self) -> PathBuf {
        self.root.join("base")
    }
    pub fn state_root(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn state_dir(&self, n: &EnvName) -> PathBuf {
        self.state_root().join(n.as_str())
    }
    pub fn shims_dir(&self) -> PathBuf {
        self.root.join("shims")
    }
}

/// Expands a leading `~` (followed by `/` or `\`) to the user's home folder.
pub fn expand_tilde(p: &str, user_home: &Path) -> PathBuf {
    if p == "~" {
        return user_home.to_path_buf();
    }
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => user_home.join(rest),
        None => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_hydra_home() {
        let p = HydraPaths::new("/h");
        let work = EnvName::parse("work").unwrap();
        assert_eq!(p.config_file(), Path::new("/h/config.toml"));
        assert_eq!(p.secret_index(), Path::new("/h/secrets.toml"));
        assert_eq!(p.env_file(&work), Path::new("/h/envs/work/env.toml"));
        assert_eq!(p.state_dir(&work), Path::new("/h/state/work"));
        assert_eq!(p.base_dir(), Path::new("/h/base"));
        assert_eq!(p.shims_dir(), Path::new("/h/shims"));
    }

    #[test]
    fn expands_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/me"));
        assert_eq!(expand_tilde("~/.ssh/id", home), home.join(".ssh/id"));
        assert_eq!(expand_tilde("~\\.ssh\\id", home), home.join(".ssh\\id"));
        assert_eq!(
            expand_tilde("C:/keys/id", home),
            PathBuf::from("C:/keys/id")
        );
    }
}
