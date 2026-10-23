//! One hydraui owns the phone link at a time, across processes: a lock on `phone-link.lock`
//! in the running folder, with the owner's pid in `phone-link.pid` for the others to read.
use hydra_core::paths::HydraPaths;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use crate::claim::running_dir;

fn lock_path(paths: &HydraPaths) -> PathBuf {
    running_dir(paths).join("phone-link.lock")
}

fn pid_path(paths: &HydraPaths) -> PathBuf {
    running_dir(paths).join("phone-link.pid")
}

/// This hydraui owns the phone link; dropping it (or the process ending) lets go.
pub struct OwnerLock {
    /// Locked while this lives; closing it unlocks. The lock file stays in place, so a racing
    /// owner can't lock a file that is about to be deleted.
    _lock: File,
    pid: PathBuf,
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid);
    }
}

/// Takes the phone link for this process, or `None` when another hydraui (or an earlier call
/// here) has it.
pub fn try_own(paths: &HydraPaths) -> Option<OwnerLock> {
    std::fs::create_dir_all(running_dir(paths)).ok()?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path(paths))
        .ok()?;
    lock.try_lock().ok()?;
    let pid = pid_path(paths);
    let _ = std::fs::write(&pid, std::process::id().to_string());
    Some(OwnerLock { _lock: lock, pid })
}

/// Whether some hydraui has the phone link. Probes with a lock it lets go of at once.
pub fn held_elsewhere(paths: &HydraPaths) -> bool {
    let Ok(file) = File::open(lock_path(paths)) else {
        return false;
    };
    match file.try_lock() {
        Err(TryLockError::WouldBlock) => true,
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::Error(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_owner_at_a_time() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        assert!(!held_elsewhere(&paths), "nobody owns it yet");
        let first = try_own(&paths).expect("first owner");
        assert!(try_own(&paths).is_none());
        assert!(held_elsewhere(&paths));
        drop(first);
        assert!(!held_elsewhere(&paths));
        let again = try_own(&paths);
        assert!(again.is_some(), "free again once dropped");
    }

    #[test]
    fn the_owner_writes_its_pid() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let _o = try_own(&paths).unwrap();
        let pid = crate::claim::running_dir(&paths).join("phone-link.pid");
        assert_eq!(
            std::fs::read_to_string(pid).unwrap().trim(),
            std::process::id().to_string()
        );
    }
}
