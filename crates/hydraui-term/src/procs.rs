//! The processes a pane's shell has started, by walking the process tree from its id (a job
//! object's process list doesn't include what the shell starts under ConPTY).
use std::collections::HashMap;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

/// The exe names of `root` and every process under it, `root` first; empty when `root` isn't
/// running.
pub fn descendant_names(root: u32) -> Vec<String> {
    // SAFETY: a snapshot of all processes; the handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut procs: Vec<(u32, u32, String)> = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: `entry` is a PROCESSENTRY32W with its size set.
    let mut more = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        procs.push((
            entry.th32ProcessID,
            entry.th32ParentProcessID,
            String::from_utf16_lossy(&entry.szExeFile[..len]),
        ));
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    // SAFETY: opened above, closed once.
    unsafe { CloseHandle(snap) };

    let Some(root_name) = procs.iter().find(|p| p.0 == root).map(|p| p.2.clone()) else {
        return Vec::new();
    };
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if p.0 != p.1 {
            children.entry(p.1).or_default().push(i);
        }
    }
    let mut out = vec![root_name];
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        for &i in children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
            // A parent id can be reused: never walk back into an ancestor.
            if out.len() > 4096 || procs[i].0 == root {
                continue;
            }
            out.push(procs[i].2.clone());
            queue.push(procs[i].0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_shells_children_are_named() {
        let mut shell = Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let names = loop {
            let names = descendant_names(shell.id());
            if names.iter().any(|n| n.eq_ignore_ascii_case("ping.exe"))
                || start.elapsed() > Duration::from_secs(5)
            {
                break names;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &shell.id().to_string()])
            .output();
        let _ = shell.wait();
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("cmd.exe")),
            "{names:?}"
        );
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("ping.exe")),
            "{names:?}"
        );
    }

    #[test]
    fn a_process_that_isnt_running_has_no_names() {
        assert!(descendant_names(u32::MAX - 3).is_empty());
    }
}
