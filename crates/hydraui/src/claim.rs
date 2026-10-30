//! One window per layout file, across hydraui processes (spec §7.12): a named mutex per
//! layout path says it is taken, and `running/<pid>-<id>.toml` says which window has it, so
//! a second open can bring that window forward. Windows closes the mutex if hydraui crashes.
//!
//! On Unix the mutex is an advisory lock on `hydraui-layout-<sha>.lock` in the user's temp
//! folder, which the OS releases if hydraui crashes. A window there is named by its process
//! id (the "window" in a record): macOS brings an app forward, not another process's window.
use std::path::{Path, PathBuf};

use hydra_core::paths::HydraPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetExitCodeProcess, OpenMutexW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetWindowThreadProcessId, IsIconic,
    IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

#[cfg(windows)]
const STILL_ACTIVE: u32 = 259;
#[cfg(windows)]
const SYNCHRONIZE: u32 = 0x0010_0000;

pub fn running_dir(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("running")
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `Local\hydraui-layout-<hex sha256 of the path, lowercase, \ separators>`.
pub fn mutex_name(path: &Path) -> String {
    let key = path.to_string_lossy().replace('/', "\\").to_lowercase();
    format!(r"Local\hydraui-layout-{:x}", Sha256::digest(key.as_bytes()))
}

/// `<pid>-<first 16 hex of the mutex hash>.toml`: one per claim, so a window switching
/// files (Save layout as…) never deletes its new record when the old claim drops.
pub fn record_path(running: &Path, layout: &Path) -> PathBuf {
    let name = mutex_name(layout);
    let id = &name[name.len() - 64..][..16];
    running.join(format!("{}-{id}.toml", std::process::id()))
}

#[derive(Serialize, Deserialize)]
struct Record {
    layout: PathBuf,
    window: isize,
}

/// The lock file standing in for the mutex: `<temp>/hydraui-layout-<sha>.lock`.
#[cfg(unix)]
fn lock_path(layout: &Path) -> PathBuf {
    let name = mutex_name(layout);
    std::env::temp_dir().join(format!("hydraui-layout-{}.lock", &name[name.len() - 64..]))
}

/// The lock on `layout`'s lock file, if no other claim (in any process) has it.
#[cfg(unix)]
fn try_lock(layout: &Path) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path(layout))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// This window holds a layout file; dropping it lets go.
pub struct Claim {
    #[cfg(windows)]
    handle: isize,
    /// Locked while the claim lives; dropping the claim unlocks it. The file itself is left
    /// in place, so a claim racing this one can't lock a file that is about to be deleted.
    #[cfg(unix)]
    lock: std::fs::File,
    path: PathBuf,
    record: PathBuf,
}

impl Claim {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // SAFETY: a handle CreateMutexW returned, closed once.
        #[cfg(windows)]
        unsafe {
            CloseHandle(self.handle as _)
        };
        // Unlock rather than only close: a child forked in this moment holds a copy of the
        // file until it execs, and the lock lives as long as any copy does.
        #[cfg(unix)]
        let _ = self.lock.unlock();
        let _ = std::fs::remove_file(&self.record);
    }
}

pub enum Taken {
    Mine(Claim),
    /// Another window has it; its window handle when its record says.
    Elsewhere(Option<isize>),
}

/// Claims `layout` for this window (`hwnd`), or says another window has it.
#[cfg(windows)]
pub fn take(running: &Path, layout: &Path, hwnd: isize) -> Taken {
    let name = wide(&mutex_name(layout));
    // SAFETY: a NUL-terminated name; no security attributes.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: read straight after the call it describes.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if handle.is_null() || existed {
        if !handle.is_null() {
            // SAFETY: our handle to the existing mutex; we don't keep it.
            unsafe { CloseHandle(handle) };
        }
        return Taken::Elsewhere(holder_window(running, layout));
    }
    let record = write_record(running, layout, hwnd);
    Taken::Mine(Claim {
        handle: handle as isize,
        path: layout.to_path_buf(),
        record,
    })
}

/// Claims `layout` for this window (`hwnd`, see `hwnd_of`), or says another window has it.
#[cfg(unix)]
pub fn take(running: &Path, layout: &Path, hwnd: isize) -> Taken {
    let Some(lock) = try_lock(layout) else {
        return Taken::Elsewhere(holder_window(running, layout));
    };
    let record = write_record(running, layout, hwnd);
    Taken::Mine(Claim {
        lock,
        path: layout.to_path_buf(),
        record,
    })
}

/// Writes the running record that says `hwnd` has `layout`, and returns its path.
fn write_record(running: &Path, layout: &Path, hwnd: isize) -> PathBuf {
    let record = record_path(running, layout);
    let text = toml::to_string(&Record {
        layout: layout.to_path_buf(),
        window: hwnd,
    })
    .unwrap_or_default();
    let _ = std::fs::create_dir_all(running).and_then(|()| std::fs::write(&record, text));
    record
}

/// Whether some window (in any hydraui) has `layout`.
#[cfg(unix)]
pub fn is_claimed(layout: &Path) -> bool {
    let Ok(file) = std::fs::File::open(lock_path(layout)) else {
        return false;
    };
    // Locking it (and letting go at once, as `file` closes) only works when nobody has it.
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Whether some window (in any hydraui) has `layout`.
#[cfg(windows)]
pub fn is_claimed(layout: &Path) -> bool {
    let name = wide(&mutex_name(layout));
    // SAFETY: a NUL-terminated name; the handle, if any, is closed at once.
    unsafe {
        let h = OpenMutexW(SYNCHRONIZE, 0, name.as_ptr());
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
    }
    true
}

/// Whether process `pid` is running.
#[cfg(windows)]
pub(crate) fn alive(pid: u32) -> bool {
    // SAFETY: query-only access; the handle is closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE
    }
}

/// Whether process `pid` is running.
#[cfg(unix)]
pub(crate) fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    // EPERM: it exists but belongs to someone else.
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether `hwnd` is a window that belongs to `pid`.
#[cfg(windows)]
fn window_of(pid: u32, hwnd: isize) -> bool {
    let hwnd = hwnd as HWND;
    let mut owner = 0u32;
    // SAFETY: IsWindow accepts any value; the owner pid is written to a local.
    let found = unsafe { IsWindow(hwnd) != 0 && GetWindowThreadProcessId(hwnd, &mut owner) != 0 };
    found && owner == pid
}

/// Whether `hwnd` is a window that belongs to `pid`: on Unix a window is named by its
/// process (`hwnd_of`).
#[cfg(unix)]
fn window_of(pid: u32, hwnd: isize) -> bool {
    isize::try_from(pid).is_ok_and(|pid| pid == hwnd)
}

/// The window that has `layout`, from the running records. Records of processes that are
/// gone are deleted; a record whose window isn't its process's (a reused pid) is skipped.
pub fn holder_window(running: &Path, layout: &Path) -> Option<isize> {
    let entries = std::fs::read_dir(running).ok()?;
    let mut found = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let pid = path
            .file_stem()
            .and_then(|s| s.to_str()?.split('-').next()?.parse::<u32>().ok());
        let Some(pid) = pid else {
            continue;
        };
        if !alive(pid) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Some(rec) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<Record>(&t).ok())
        else {
            continue;
        };
        if hydraui_sets::same_path(&rec.layout, layout) && window_of(pid, rec.window) {
            found = Some(rec.window);
        }
    }
    found
}

/// Restores `hwnd` if minimised and brings it to the front.
#[cfg(windows)]
pub fn bring_forward(hwnd: isize) {
    let hwnd = hwnd as HWND;
    // SAFETY: plain window calls; a stale handle makes them fail harmlessly.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

/// Unhides the hydraui process `hwnd` names (`hwnd_of`) and brings its windows forward.
#[cfg(target_os = "macos")]
pub fn bring_forward(hwnd: isize) {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    let Ok(pid) = libc::pid_t::try_from(hwnd) else {
        return;
    };
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        app.unhide();
        app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
}

/// Other Unix desktops have no portable way to raise another process's window.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn bring_forward(_hwnd: isize) {}

/// Flashes `hwnd`'s taskbar button until the window comes to the front.
#[cfg(windows)]
pub fn flash(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd: hwnd as HWND,
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: `info` is a filled FLASHWINFO; a stale handle makes the call fail harmlessly.
    unsafe { FlashWindowEx(&info) };
}

/// Bounces hydraui's Dock icon once, if it isn't the active app (as the taskbar flash only
/// shows while the window is in the background). `hwnd` is always this process here.
#[cfg(target_os = "macos")]
pub fn flash(_hwnd: isize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
    // Only the main thread may use NSApplication; flash is called from the UI.
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm)
            .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
    }
}

/// Other Unix desktops have no portable attention request.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn flash(_hwnd: isize) {}

/// The window's name in the running records: this process's id on Unix (see the module
/// docs). The `gpui::Window` argument keeps the signature the same on every platform.
#[cfg(unix)]
pub fn hwnd_of(_window: &gpui::Window) -> isize {
    isize::try_from(std::process::id()).unwrap_or(0)
}

/// The window's HWND, 0 when it can't be had.
#[cfg(windows)]
pub fn hwnd_of(window: &gpui::Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // gpui's own `window_handle()` is its id; the trait's is the HWND.
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout path no other test (or real hydraui) uses, so mutex names don't collide.
    fn unique(d: &tempfile::TempDir) -> PathBuf {
        d.path().join("t.hydraui")
    }

    /// Removes the layout's lock file when the test ends: on Unix it lives in the temp
    /// folder, outside `d`. Windows' named mutex leaves nothing behind.
    #[cfg(unix)]
    struct LockFileGone(PathBuf);

    #[cfg(unix)]
    impl Drop for LockFileGone {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(lock_path(&self.0));
        }
    }

    #[cfg(windows)]
    struct LockFileGone;

    fn lock_file_gone(layout: &Path) -> LockFileGone {
        #[cfg(unix)]
        return LockFileGone(layout.to_path_buf());
        #[cfg(windows)]
        {
            let _ = layout;
            LockFileGone
        }
    }

    #[test]
    fn the_mutex_name_ignores_case_and_slashes() {
        let a = mutex_name(Path::new("C:/Work/A.hydraui"));
        assert_eq!(a, mutex_name(Path::new(r"c:\work\a.hydraui")));
        assert!(a.starts_with(r"Local\hydraui-layout-"), "{a}");
        assert_eq!(a.len(), r"Local\hydraui-layout-".len() + 64, "{a}");
        assert_ne!(a, mutex_name(Path::new(r"c:\work\b.hydraui")));
    }

    #[test]
    fn a_second_claim_finds_the_first_window() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("running");
        let layout = unique(&d);
        let _gone = lock_file_gone(&layout);
        // This test's process stands in for the holder, so its pid is alive; it has no real
        // window, so hwnd 0 fails the window check and the holder's window is `None`.
        let Taken::Mine(first) = take(&running, &layout, 0) else {
            panic!("first claim")
        };
        assert_eq!(first.path(), layout.as_path());
        assert!(is_claimed(&layout));
        assert!(matches!(take(&running, &layout, 0), Taken::Elsewhere(None)));
        drop(first);
    }

    #[test]
    fn a_dropped_claim_frees_the_file_at_once() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("running");
        let layout = unique(&d);
        let _gone = lock_file_gone(&layout);
        let Taken::Mine(c) = take(&running, &layout, 0) else {
            panic!("first claim")
        };
        let record = record_path(&running, &layout);
        assert!(record.exists(), "the running record is written");
        drop(c);
        assert!(!is_claimed(&layout));
        assert!(!record.exists(), "and removed");
        assert!(matches!(take(&running, &layout, 0), Taken::Mine(_)));
    }

    #[test]
    fn records_of_dead_processes_are_ignored_and_removed() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("running");
        std::fs::create_dir_all(&running).unwrap();
        let layout = unique(&d);
        let _gone = lock_file_gone(&layout);
        let line = format!(
            "layout = {:?}\nwindow = 1234\n",
            layout.display().to_string()
        );
        // A pid that isn't running (Windows pids are multiples of 4; above i32::MAX on Unix).
        let dead = running.join("4294967291-0123456789abcdef.toml");
        std::fs::write(&dead, &line).unwrap();
        assert_eq!(holder_window(&running, &layout), None);
        assert!(!dead.exists());
        // A live pid whose recorded window isn't its own (pid reuse) is ignored too.
        std::fs::write(record_path(&running, &layout), &line).unwrap();
        assert_eq!(holder_window(&running, &layout), None);
    }
}
