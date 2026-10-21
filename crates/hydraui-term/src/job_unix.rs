//! A pane's process tree on Unix, the counterpart of the Windows Job Object (spec §6.1).
//! portable-pty starts every shell as a session leader, so the session is the job: whatever
//! the shell starts, even in its own process group or after its parent exits, keeps the
//! session id until it calls `setsid` itself.

/// Dropping it kills every process in the session.
#[derive(Debug)]
pub struct Job {
    leader: libc::pid_t,
}

impl Job {
    /// The job of the session `leader` leads.
    pub fn new(leader: u32) -> std::io::Result<Self> {
        let leader = libc::pid_t::try_from(leader)
            .map_err(|_| std::io::Error::other(format!("{leader} isn't a process id")))?;
        Ok(Job { leader })
    }

    /// The programs running in the job, as lowercase file names (`zsh`, `claude`). A program
    /// is named as it was started (its `argv[0]`), not by the file a symlink led to: Claude
    /// Code's `claude` is a link to `versions/<version>`. Processes that end meanwhile are left
    /// out.
    pub fn programs(&self) -> Vec<String> {
        self.members().into_iter().filter_map(program).collect()
    }

    /// The processes in the session, the leader too while it runs.
    fn members(&self) -> Vec<libc::pid_t> {
        all_pids()
            .into_iter()
            // SAFETY: plain call; a process that has gone makes it return -1.
            .filter(|&pid| unsafe { libc::getsid(pid) } == self.leader)
            .collect()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // A process can start another between the listing and the kill, so go round again
        // until the session is empty (a few rounds at most).
        for _ in 0..4 {
            let members = self.members();
            if members.is_empty() {
                break;
            }
            for pid in members {
                // SAFETY: plain call; a process that has gone makes it fail harmlessly.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

/// Process `pid`'s program, as `Job::programs` names it.
fn program(pid: libc::pid_t) -> Option<String> {
    let argv0 = argv0(pid)?;
    // A login shell's argv[0] is `-zsh`.
    let argv0 = argv0.strip_prefix('-').unwrap_or(&argv0);
    let name = std::path::Path::new(argv0).file_name()?;
    Some(name.to_string_lossy().to_lowercase())
}

/// Every process id on the system.
#[cfg(target_os = "macos")]
fn all_pids() -> Vec<libc::pid_t> {
    // SAFETY: a null buffer asks for the count only.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(n) = usize::try_from(n) else {
        return Vec::new();
    };
    // Room for processes started meanwhile.
    let mut pids: Vec<libc::pid_t> = vec![0; n + 64];
    let bytes = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()).unwrap_or(0);
    // SAFETY: `pids` is writable for `bytes` bytes.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(n).unwrap_or(0));
    pids.retain(|&pid| pid > 0);
    pids
}

/// Every process id on the system.
#[cfg(not(target_os = "macos"))]
fn all_pids() -> Vec<libc::pid_t> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|&pid| pid > 0)
        .collect()
}

/// `pid`'s `argv[0]`, from `KERN_PROCARGS2`: argc, the executable's path, NUL padding, then
/// the arguments.
#[cfg(target_os = "macos")]
fn argv0(pid: libc::pid_t) -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a null buffer asks for the size only; `mib` holds the 3 names given.
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` is writable for `size` bytes, which the call updates to what it wrote.
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 {
        return None;
    }
    buf.truncate(size);
    let rest = buf.get(size_of::<libc::c_int>()..)?;
    let rest = &rest[rest.iter().position(|&b| b == 0)?..];
    let rest = &rest[rest.iter().position(|&b| b != 0)?..];
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// `pid`'s `argv[0]`, the first field of `/proc/<pid>/cmdline`.
#[cfg(not(target_os = "macos"))]
fn argv0(pid: libc::pid_t) -> Option<String> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let first = cmdline.split(|&b| b == 0).next()?;
    (!first.is_empty()).then(|| String::from_utf8_lossy(first).into_owned())
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// Starts `program args` as a session leader, the way portable-pty starts a shell.
    fn leader(program: &std::path::Path, args: &[&str]) -> Child {
        let mut cmd = Command::new(program);
        cmd.args(args).stdout(Stdio::null());
        // SAFETY: setsid is async-signal-safe, as pre_exec requires.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().unwrap()
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn closing_the_job_ends_its_processes() {
        // The shell's own child is in the session too, and must end with it.
        let mut child = leader("/bin/sh".as_ref(), &["-c", "sleep 60 & wait"]);
        let job = Job::new(child.id()).unwrap();
        wait_until("the shell never started sleep", || {
            job.programs().contains(&"sleep".to_string())
        });
        let watcher = Job::new(child.id()).unwrap();
        drop(job);
        wait_until("sh outlived its job", || {
            child.try_wait().unwrap().is_some()
        });
        wait_until("sleep outlived its job", || watcher.programs().is_empty());
    }

    #[test]
    fn lists_the_programs_running_in_the_job() {
        let mut child = leader("/bin/sh".as_ref(), &["-c", "sleep 60 & wait"]);
        let job = Job::new(child.id()).unwrap();
        wait_until("no sleep in the job", || {
            let programs = job.programs();
            programs.contains(&"sh".to_string()) && programs.contains(&"sleep".to_string())
        });
        drop(job);
        let _ = child.wait();
    }

    #[test]
    fn a_program_started_through_a_symlink_has_the_symlinks_name() {
        // Claude Code's installer puts `claude` on PATH as a link to `versions/<version>`.
        let d = tempfile::tempdir().unwrap();
        let link = d.path().join("Claude");
        std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
        let mut child = leader(&link, &["60"]);
        let job = Job::new(child.id()).unwrap();
        wait_until("the link's name isn't listed", || {
            job.programs() == vec!["claude".to_string()]
        });
        drop(job);
        let _ = child.wait();
    }
}
