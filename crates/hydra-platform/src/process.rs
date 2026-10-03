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
        cmd.args(args).stdin(Stdio::null());
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
}
