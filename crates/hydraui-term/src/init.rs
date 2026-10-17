//! Per-pane init scripts: `state/<env>/shell/hydraui/<pane-id>.ps1|.bash`, so panes never
//! overwrite each other or the CLI's `init.ps1` (spec §4.5).
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use hydra_platform::shell::{PromptStyle, ShellKind, render_init};

/// A pane id unique across hydraui processes: `<pid>-<n>`.
pub fn next_pane_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Writes the pane's init script (shell integration on, `startup` last) and returns its path.
pub fn write_pane_init(
    paths: &HydraPaths,
    env: &EnvName,
    rgb: Option<(u8, u8, u8)>,
    kind: ShellKind,
    startup: Option<&str>,
    pane_id: &str,
) -> std::io::Result<PathBuf> {
    let dir = paths.state_dir(env).join("shell").join("hydraui");
    std::fs::create_dir_all(&dir)?;
    let ext = match kind {
        ShellKind::Pwsh => "ps1",
        ShellKind::Bash => "bash",
    };
    let path = dir.join(format!("{pane_id}.{ext}"));
    let style = PromptStyle {
        name: env.as_str(),
        rgb,
        integration: true,
    };
    std::fs::write(&path, render_init(kind, &style, startup))?;
    Ok(path)
}

/// Deletes every environment's pane init scripts; hydraui calls it at start. A script is
/// only read when its shell starts, so deleting one a running pane used is harmless.
pub fn clear_pane_inits(paths: &HydraPaths) {
    let Ok(envs) = std::fs::read_dir(paths.state_root()) else {
        return;
    };
    for env in envs.flatten() {
        // Best effort: a folder that can't be removed now is tried again next start.
        let _ = std::fs::remove_dir_all(env.path().join("shell").join("hydraui"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> EnvName {
        EnvName::parse("work").unwrap()
    }

    #[test]
    fn pane_ids_differ_and_name_the_process() {
        assert_ne!(next_pane_id(), next_pane_id());
        assert!(next_pane_id().starts_with(&format!("{}-", std::process::id())));
    }

    #[test]
    fn each_pane_gets_its_own_script_beside_the_clis() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let a = write_pane_init(
            &paths,
            &work(),
            None,
            ShellKind::Pwsh,
            Some("npm run dev"),
            "1-1",
        )
        .unwrap();
        let b = write_pane_init(&paths, &work(), None, ShellKind::Bash, None, "1-2").unwrap();
        let dir = paths.state_dir(&work()).join("shell");
        assert_eq!(a, dir.join("hydraui").join("1-1.ps1"));
        assert_eq!(b, dir.join("hydraui").join("1-2.bash"));
        let text = std::fs::read_to_string(&a).unwrap();
        assert!(text.contains("133;A"), "{text}");
        assert!(text.ends_with("npm run dev\n"), "{text}");
        assert!(!dir.join("init.ps1").exists());
    }

    #[test]
    fn clearing_removes_every_environments_pane_scripts_only() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let personal = EnvName::parse("personal").unwrap();
        let a = write_pane_init(&paths, &work(), None, ShellKind::Pwsh, None, "1-1").unwrap();
        let b = write_pane_init(&paths, &personal, None, ShellKind::Pwsh, None, "1-2").unwrap();
        let cli = paths.state_dir(&work()).join("shell").join("init.ps1");
        std::fs::write(&cli, "cli").unwrap();
        clear_pane_inits(&paths);
        assert!(!a.exists() && !b.exists());
        assert!(cli.exists());
        // No state folder yet: nothing to do, no panic.
        clear_pane_inits(&HydraPaths::new(d.path().join("missing")));
    }
}
