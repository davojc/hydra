use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hydra_core::provider::CommandRunner;
use hydra_core::resolve::LaunchEnv;

pub fn find_on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

/// Finds `program` the way a shell does, including `.cmd`/`.bat` launchers such as gcloud.cmd.
pub fn resolve_program(program: &str, path: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() || p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        pathext
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    let try_bare = !cfg!(windows) || p.extension().is_some();
    for dir in std::env::split_paths(path) {
        if try_bare {
            let c = dir.join(program);
            if c.is_file() {
                return Some(c);
            }
        }
        for ext in &exts {
            let c = dir.join(format!("{program}{ext}"));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// Marks a command that runs in the background (its output is captured): on Windows it
/// gets no console window, which a GUI parent such as hydraui would otherwise pop up for it.
pub fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs programs with a prepared environment applied.
pub struct EnvRunner<'a> {
    pub launch: &'a LaunchEnv,
}

impl EnvRunner<'_> {
    pub fn command(&self, program: &str) -> Result<Command, String> {
        let path = self.launch.path_value(std::env::var_os("PATH"));
        let exe = resolve_program(program, &path, std::env::var_os("PATHEXT").as_deref())
            .ok_or_else(|| format!("{program} isn't installed or isn't on PATH"))?;
        let mut cmd = Command::new(exe);
        self.launch.apply(&mut cmd);
        Ok(cmd)
    }
}

impl CommandRunner for EnvRunner<'_> {
    fn output(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let mut cmd = self.command(program)?;
        hide_window(cmd.args(args).stdin(Stdio::null()));
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("command failed")
                .to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn finds_cmd_launchers_via_pathext() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gcloud.cmd"), "@echo off").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let found =
            resolve_program("gcloud", &path, Some(OsStr::new(".COM;.EXE;.BAT;.CMD"))).unwrap();
        assert_eq!(found, dir.path().join("gcloud.cmd"));
    }

    #[test]
    fn explicit_paths_and_missing_programs() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool.exe");
        std::fs::write(&tool, "").unwrap();
        let empty = std::env::join_paths(Vec::<PathBuf>::new()).unwrap();
        assert_eq!(
            resolve_program(tool.to_str().unwrap(), &empty, None),
            Some(tool)
        );
        assert_eq!(
            resolve_program("definitely-not-installed", &empty, None),
            None
        );
    }

    /// Runs `console_window_probe` in a copy of this test binary that has no console, as a
    /// GUI app such as hydraui has none: a background child must not get a console window.
    #[test]
    #[cfg(windows)]
    fn background_commands_get_no_console_window_under_a_gui_parent() {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::console_window_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("HYDRA_CONSOLE_PROBE", "1")
            .creation_flags(DETACHED_PROCESS)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("console window: 0"), "{text}");
    }

    #[test]
    #[ignore = "run by background_commands_get_no_console_window_under_a_gui_parent"]
    #[cfg(windows)]
    fn console_window_probe() {
        if std::env::var_os("HYDRA_CONSOLE_PROBE").is_none() {
            return;
        }
        let mut cmd = Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -Name W -Namespace N -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow();'; [N.W]::GetConsoleWindow()",
        ]);
        let out = hide_window(&mut cmd).output().unwrap();
        println!(
            "console window: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
}
