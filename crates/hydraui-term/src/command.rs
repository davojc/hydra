//! The shell's command line and environment, built as `hydra shell` builds them, for
//! portable-pty.
use std::ffi::OsString;
use std::path::Path;

use hydra_core::resolve::{EnvChange, LaunchEnv};
use hydra_platform::shell::{ShellKind, shell_launch};
use portable_pty::CommandBuilder;

/// What hydraui's grid emulates. Set before the environment's own `[env]`, which can
/// override them.
const TERMINAL_VARS: [(&str, &str); 2] = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// `exe` running `init` in `cwd`, starting from `inherited` (normally `std::env::vars_os()`)
/// changed exactly as `LaunchEnv::apply` changes a `hydra shell` child. portable-pty
/// compares names case-insensitively, so an inherited `Path` and hydra's `PATH` are one
/// variable.
pub fn pane_command(
    kind: ShellKind,
    exe: &Path,
    init: &Path,
    cwd: &Path,
    launch: &LaunchEnv,
    inherited: &[(OsString, OsString)],
) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(exe);
    let (args, vars) = shell_launch(kind, init);
    cmd.args(args);
    cmd.env_clear();
    let mut path = None;
    for (k, v) in inherited {
        if k.eq_ignore_ascii_case("PATH") {
            path = Some(v.clone());
        }
        cmd.env(k, v);
    }
    for (k, v) in TERMINAL_VARS.into_iter().chain(vars) {
        cmd.env(k, v);
    }
    for change in launch.env_changes(path) {
        match change {
            EnvChange::Remove(k) => cmd.env_remove(k),
            EnvChange::Set(k, v) => cmd.env(k, v),
        }
    }
    cmd.cwd(cwd);
    cmd
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use hydra_core::config::EnvConfig;
    use hydra_core::name::EnvName;

    use super::*;

    /// Folders in the platform's own form: `C:\\old` can't go in a Unix PATH, whose
    /// separator is `:`.
    const OLD: &str = if cfg!(windows) { "C:\\old" } else { "/old" };
    const KEEP: &str = if cfg!(windows) { "C:\\keep" } else { "/keep" };

    fn launch(set: &[(&str, &str)]) -> LaunchEnv {
        LaunchEnv {
            name: EnvName::parse("work").unwrap(),
            config: EnvConfig::default(),
            set: set
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            unset: BTreeSet::from(["GH_TOKEN".to_string()]),
            path_drop: vec![PathBuf::from(OLD)],
            warnings: Vec::new(),
        }
    }

    /// Windows spells it `Path`; hydra writes `PATH`. Unix names are case-sensitive, and
    /// always `PATH`.
    fn inherited() -> Vec<(OsString, OsString)> {
        vec![
            (
                if cfg!(windows) { "Path" } else { "PATH" }.into(),
                std::env::join_paths([OLD, KEEP]).unwrap(),
            ),
            ("GH_TOKEN".into(), "leaked".into()),
            ("SystemRoot".into(), "C:\\Windows".into()),
        ]
    }

    fn pwsh(launch: &LaunchEnv) -> CommandBuilder {
        pane_command(
            ShellKind::Pwsh,
            Path::new("C:\\pwsh\\pwsh.exe"),
            Path::new("C:\\s\\1-1.ps1"),
            Path::new("C:\\work"),
            launch,
            &inherited(),
        )
    }

    #[test]
    fn environment_is_hydras_on_top_of_the_inherited_one() {
        let cmd = pwsh(&launch(&[("FOO", "bar")]));
        assert_eq!(cmd.get_env("FOO"), Some(OsStr::new("bar")));
        assert_eq!(cmd.get_env("GH_TOKEN"), None);
        assert_eq!(cmd.get_env("SystemRoot"), Some(OsStr::new("C:\\Windows")));
        assert_eq!(cmd.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(cmd.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(
            cmd.get_cwd().map(PathBuf::from),
            Some(PathBuf::from("C:\\work"))
        );
    }

    #[test]
    fn inherited_path_in_any_case_becomes_one_path_without_the_dropped_folder() {
        let cmd = pwsh(&launch(&[]));
        let paths: Vec<(&str, &str)> = cmd
            .iter_full_env_as_str()
            .filter(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert_eq!(paths[0].1, KEEP);
    }

    #[test]
    fn the_environments_own_term_wins() {
        let cmd = pwsh(&launch(&[("TERM", "dumb")]));
        assert_eq!(cmd.get_env("TERM"), Some(OsStr::new("dumb")));
    }

    #[test]
    fn arguments_match_hydra_shell() {
        let init = Path::new("C:\\s\\1-2.bash");
        let cmd = pane_command(
            ShellKind::Bash,
            Path::new("C:\\Git\\bin\\bash.exe"),
            init,
            Path::new("C:\\work"),
            &launch(&[]),
            &inherited(),
        );
        let mut want = vec![OsString::from("C:\\Git\\bin\\bash.exe")];
        want.extend(shell_launch(ShellKind::Bash, init).0);
        assert_eq!(cmd.get_argv(), &want);
        assert_eq!(cmd.get_env("CHERE_INVOKING"), Some(OsStr::new("1")));
    }
}
