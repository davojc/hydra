//! A Windows Job Object that ends a pane's whole process tree when it closes (spec §6.1).
use std::os::windows::io::{AsRawHandle, BorrowedHandle};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

/// Dropping it kills every process in it. Processes a member starts join it too.
#[derive(Debug)]
pub struct Job(HANDLE);

// SAFETY: a job handle is a kernel handle, usable from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> std::io::Result<Self> {
        // SAFETY: no name and default security; the returned Job owns the handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = Job(handle);
        let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: `info` is the struct this information class expects, with its size.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts a running process, and everything it starts from now on, in the job.
    pub fn assign(&self, process: BorrowedHandle<'_>) -> std::io::Result<()> {
        // SAFETY: `process` is borrowed, so it stays open for this call.
        if unsafe { AssignProcessToJobObject(self.0, process.as_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// The programs running in the job, as lowercase file names without `.exe` (`pwsh`,
    /// `claude`). Processes that end or can't be opened meanwhile are left out.
    pub fn programs(&self) -> Vec<String> {
        // The header's two u32s fill the first usize; up to 255 ids follow.
        let mut buf = vec![0usize; 256];
        // SAFETY: `buf` is writable for its byte length and aligned for the list struct.
        let ok = unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr().cast(),
                (buf.len() * size_of::<usize>()) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Vec::new();
        }
        // SAFETY: the call filled the header, and `n` ids after it, within `buf`.
        let list = unsafe { &*buf.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
        let n = (list.NumberOfProcessIdsInList as usize).min(buf.len() - 1);
        buf[1..=n]
            .iter()
            .filter_map(|&pid| program(pid as u32))
            .collect()
    }
}

/// Process `pid`'s program, as `Job::programs` names it.
fn program(pid: u32) -> Option<String> {
    // SAFETY: plain call; a null handle means it failed.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut name = [0u16; 1024];
    let mut len = name.len() as u32;
    // SAFETY: `name` holds `len` u16s; the handle is ours and closed once below.
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, name.as_mut_ptr(), &mut len)
    };
    // SAFETY: as above.
    unsafe { CloseHandle(process) };
    if ok == 0 {
        return None;
    }
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&name[..len as usize]));
    Some(path.file_stem()?.to_string_lossy().to_lowercase())
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn closing_the_job_ends_its_processes() {
        let mut child = Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let job = Job::new().unwrap();
        job.assign(child.as_handle()).unwrap();
        assert!(child.try_wait().unwrap().is_none(), "ping ended early");
        drop(job);
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "ping outlived its job"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn lists_the_programs_running_in_the_job() {
        let mut child = Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let job = Job::new().unwrap();
        job.assign(child.as_handle()).unwrap();
        assert!(
            job.programs().contains(&"ping".to_string()),
            "{:?}",
            job.programs()
        );
        drop(job);
        let _ = child.wait();
    }
}
