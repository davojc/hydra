#![allow(dead_code, clippy::new_without_default)]
use std::path::PathBuf;

/// A throwaway HYDRA_HOME plus its own Credential Manager namespace.
pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().join(".hydra")
    }

    pub fn hydra(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::new(env!("CARGO_BIN_EXE_hydra"));
        let service = format!(
            "hydra-test-{}",
            self.dir.path().file_name().unwrap().to_string_lossy()
        );
        // Hermetic by default: a fake user folder (so the real ~/.claude and ~/.claude.json
        // are never read or written) and no inherited Claude sign-in variables.
        c.env("HYDRA_HOME", self.root())
            .env("HYDRA_KEYRING_SERVICE", service)
            .env("HYDRA_USER_HOME", self.dir.path().join("user"))
            // git searches (e.g. for `.git/info/exclude`) stop at the test folder, so no test
            // can touch a repo that happens to contain the machine's temp folder.
            .env("GIT_CEILING_DIRECTORIES", self.dir.path())
            // The developer's own global git config (often set by GIT_CONFIG_GLOBAL) must
            // never leak in: the fake user's file stands in for it.
            .env(
                "GIT_CONFIG_GLOBAL",
                self.dir.path().join("user").join(".gitconfig"),
            )
            .env_remove("HYDRA_USER_GIT_CONFIG")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("HYDRA_ALLOW")
            .env_remove("HYDRA_ENV")
            .env_remove("HYDRA_ENV_VARS");
        for var in [
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
        ] {
            c.env_remove(var);
        }
        c
    }

    pub fn write_env(&self, name: &str, toml: &str) {
        let d = self.root().join("envs").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("env.toml"), toml).unwrap();
    }

    pub fn env_toml(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join("envs").join(name).join("env.toml")).unwrap()
    }
}
