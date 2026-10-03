use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use crate::name::EnvName;
use crate::paths::HydraPaths;

/// Held (shared) by each running `hydra shell` for an environment. Dropping it releases the lock.
#[derive(Debug)]
pub struct ShellLock {
    _file: File,
}

fn lock_path(paths: &HydraPaths, name: &EnvName) -> PathBuf {
    paths.state_dir(name).join("running.lock")
}

pub fn hold_shared(paths: &HydraPaths, name: &EnvName) -> std::io::Result<ShellLock> {
    let path = lock_path(paths, name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    file.lock_shared()?;
    Ok(ShellLock { _file: file })
}

/// True while any hydra shell for this environment is open.
pub fn is_running(paths: &HydraPaths, name: &EnvName) -> std::io::Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(paths, name))
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_lock_marks_environment_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        let work = EnvName::parse("work").unwrap();
        assert!(!is_running(&paths, &work).unwrap());
        let a = hold_shared(&paths, &work).unwrap();
        let b = hold_shared(&paths, &work).unwrap(); // two terminals at once
        assert!(is_running(&paths, &work).unwrap());
        drop(a);
        assert!(is_running(&paths, &work).unwrap());
        drop(b);
        assert!(!is_running(&paths, &work).unwrap());
    }
}
