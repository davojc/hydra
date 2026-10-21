//! `<HYDRA_HOME>/hydraui/hydraui.log`: what hydraui did and why, one timestamped line per
//! step, for finding out why something didn't happen (a card not sent, a session skipped).
//! Every hydraui window appends to the same file, each line tagged with its process id. A log
//! over 1 MB at startup is moved to `hydraui.log.old`, replacing the one before.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A log bigger than this at startup is set aside.
const MAX_BYTES: u64 = 1024 * 1024;

static FILE: OnceLock<PathBuf> = OnceLock::new();
static WRITING: Mutex<()> = Mutex::new(());

/// Where the log goes for `hydra_root` (`HydraPaths::root`).
pub fn file_for(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("hydraui.log")
}

/// Logs to `path` from now on, first setting aside a log over 1 MB. Called once, at startup.
pub fn init(path: PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    set_aside_if_big(&path, MAX_BYTES);
    let _ = FILE.set(path);
}

/// The log file, once `init` has run.
pub fn file() -> Option<&'static Path> {
    FILE.get().map(PathBuf::as_path)
}

/// Moves `path` to `<path>.old` when it is bigger than `max` bytes.
fn set_aside_if_big(path: &Path, max: u64) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let _ = std::fs::rename(path, old);
    }
}

/// Appends `text` to the log, stamped with the local time and this process's id. Does nothing
/// before `init`; a log that can't be written is skipped silently.
pub fn write(text: impl AsRef<str>) {
    let Some(path) = file() else { return };
    let entry = entry(&now(), std::process::id(), text.as_ref());
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(entry.as_bytes());
    }
}

/// One log entry: `<stamp> [<pid>] <text>`, each further line of `text` indented under it.
fn entry(stamp: &str, pid: u32, text: &str) -> String {
    let mut lines = text.trim_end().lines();
    let mut out = format!("{stamp} [{pid}] {}\n", lines.next().unwrap_or(""));
    for line in lines {
        out.push_str("    ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The local time as `2026-10-21 08:45:12.345`.
fn now() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime only fills in the struct it is given.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_one_stamped_line() {
        assert_eq!(
            entry("2026-10-21 08:45:12.345", 42, "pickup: offer"),
            "2026-10-21 08:45:12.345 [42] pickup: offer\n"
        );
    }

    #[test]
    fn further_lines_are_indented_under_the_first() {
        assert_eq!(
            entry("T", 1, "Jev was sent:\n  first row  \nsecond row\n\n"),
            "T [1] Jev was sent:\n      first row\n    second row\n"
        );
    }

    #[test]
    fn the_time_has_its_fixed_shape() {
        let t = now();
        assert_eq!(t.len(), "2026-10-21 08:45:12.345".len(), "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], " ");
    }

    #[test]
    fn a_big_log_is_set_aside_and_a_small_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hydraui.log");
        std::fs::write(&path, "0123456789").unwrap();
        set_aside_if_big(&path, 20);
        assert!(path.exists());
        set_aside_if_big(&path, 5);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hydraui.log.old")).unwrap(),
            "0123456789"
        );
    }
}
