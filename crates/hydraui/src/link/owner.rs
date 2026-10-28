//! One hydraui owns something at a time, across processes: a lock on `<name>.lock` in the
//! running folder, with the owner's pid in `<name>.pid` for the others to read. The phone link
//! is `phone-link`; the swarms are `swarms`.
use hydra_core::paths::HydraPaths;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use crate::claim::running_dir;

/// The phone link's lock.
const PHONE_LINK: &str = "phone-link";

fn lock_path(paths: &HydraPaths, name: &str) -> PathBuf {
    running_dir(paths).join(format!("{name}.lock"))
}

fn pid_path(paths: &HydraPaths, name: &str) -> PathBuf {
    running_dir(paths).join(format!("{name}.pid"))
}

/// This hydraui owns a lock; dropping it (or the process ending) lets go.
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
    try_own_named(paths, PHONE_LINK)
}

/// Whether some hydraui has the phone link.
pub fn held_elsewhere(paths: &HydraPaths) -> bool {
    held_elsewhere_named(paths, PHONE_LINK)
}

/// Takes the lock `name` for this process, or `None` when another hydraui (or an earlier call
/// here) has it.
pub fn try_own_named(paths: &HydraPaths, name: &str) -> Option<OwnerLock> {
    std::fs::create_dir_all(running_dir(paths)).ok()?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path(paths, name))
        .ok()?;
    lock.try_lock().ok()?;
    let pid = pid_path(paths, name);
    let _ = std::fs::write(&pid, std::process::id().to_string());
    Some(OwnerLock { _lock: lock, pid })
}

/// Whether some hydraui has the lock `name`. Probes with a lock it lets go of at once.
pub fn held_elsewhere_named(paths: &HydraPaths, name: &str) -> bool {
    let Ok(file) = File::open(lock_path(paths, name)) else {
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
    fn two_named_locks_are_independent() {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        let phone = try_own(&paths).expect("the phone link");
        let swarms = try_own_named(&paths, "swarms").expect("swarms, beside the phone link");
        assert!(try_own_named(&paths, "swarms").is_none());
        assert!(held_elsewhere_named(&paths, "swarms"));
        drop(swarms);
        assert!(!held_elsewhere_named(&paths, "swarms"));
        assert!(held_elsewhere(&paths), "the phone link is still held");
        assert!(running_dir(&paths).join("swarms.lock").exists());
        drop(phone);
        assert!(!held_elsewhere_named(&paths, "phone-link"));
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
