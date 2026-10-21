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
        self.command(env!("CARGO_BIN_EXE_hydra"))
    }

    /// `hydra` for a test that starts an interactive shell. zsh and bash read their startup
    /// files from HOME, so on Unix HOME is the fake user folder: the developer's own
    /// `.zshrc` stays out. (Only here: macOS finds the login keychain through HOME.)
    pub fn hydra_shell(&self) -> assert_cmd::Command {
        with_home(self.hydra(), &self.dir.path().join("user"))
    }

    /// `program` with the same hermetic environment.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::new(program);
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

    /// Writes a program `name` into `dir` that runs `windows` (a `.cmd` batch file) on
    /// Windows and `unix` (an executable `sh` script) elsewhere.
    pub fn write_launcher(dir: &std::path::Path, name: &str, windows: &str, unix: &str) {
        std::fs::create_dir_all(dir).unwrap();
        if cfg!(windows) {
            std::fs::write(dir.join(format!("{name}.cmd")), windows).unwrap();
        } else {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{unix}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    pub fn env_toml(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join("envs").join(name).join("env.toml")).unwrap()
    }
}

/// The shell `hydra shell` tests start: pwsh on Windows, zsh (macOS's own) elsewhere.
pub const SHELL: &str = if cfg!(windows) { "pwsh" } else { "zsh" };

/// What a `hydra shell` test types to print the folder as `[<folder>]` and leave.
pub const PRINT_PWD_AND_EXIT: &str = if cfg!(windows) {
    "Write-Output \"[$PWD]\"; exit 0\n"
} else {
    "echo \"[$PWD]\"; exit 0\n"
};

/// A command that prints `[<value of var>]` (`[]` when unset).
pub fn show(var: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "pwsh".into(),
            "-NoProfile".into(),
            "-Command".into(),
            format!("Write-Output \"[$env:{var}]\""),
        ]
    } else {
        vec!["sh".into(), "-c".into(), format!("echo \"[${{{var}}}]\"")]
    }
}

/// A command that exits with `code`.
pub fn exit_with(code: i32) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".into(), "/c".into(), format!("exit {code}")]
    } else {
        vec!["sh".into(), "-c".into(), format!("exit {code}")]
    }
}

/// `c` with HOME at `home` on Unix, for a command that starts an interactive shell (see
/// `Home::hydra_shell`).
pub fn with_home(mut c: assert_cmd::Command, home: &std::path::Path) -> assert_cmd::Command {
    if cfg!(unix) {
        c.env("HOME", home);
    }
    c
}

/// A path written with `\` separators, in the platform's own.
pub fn native(path: &str) -> String {
    path.replace('\\', std::path::MAIN_SEPARATOR_STR)
}
