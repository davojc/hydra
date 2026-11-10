use std::path::Path;

/// Writes a file that only the current user can read. If the file can't be
/// restricted, it is removed so a secret never stays on disk with open access.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    write_restricted(path, contents).inspect_err(|_| {
        let _ = std::fs::remove_file(path);
    })
}

/// Lets everyone who can read `path` run it, as git needs for a hook. Windows has no such
/// bit: there it does nothing.
#[cfg(unix)]
pub fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    // Execute wherever read is allowed: 0o644 -> 0o755, 0o600 -> 0o700.
    let wanted = mode | ((mode & 0o444) >> 2);
    if wanted != mode {
        perms.set_mode(wanted);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Lets everyone who can read `path` run it, as git needs for a hook. Windows has no such
/// bit: there it does nothing.
#[cfg(windows)]
pub fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn write_restricted(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    windows_acl::only_this_user(path)
}

/// Gives a file one access entry, full control for the user this process runs as,
/// and stops it inheriting any. Removing inherited entries isn't enough: a file in a
/// folder with nothing to inherit gets Windows' default grants (SYSTEM,
/// Administrators, the logon session) as entries of its own.
#[cfg(windows)]
mod windows_acl {
    use std::io::{Error, Result};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION,
        GetLengthSid, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
        PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_DESCRIPTOR, SetFileSecurityW,
        SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// `FILE_ALL_ACCESS`, which lives behind a feature this crate doesn't otherwise need.
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
    /// `SECURITY_DESCRIPTOR_REVISION`.
    const SD_REVISION: u32 = 1;

    fn check(ok: i32) -> Result<()> {
        if ok == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn only_this_user(path: &Path) -> Result<()> {
        // The token's user, in a u64 buffer so the TOKEN_USER inside is aligned.
        let mut user = vec![0u64; 16];
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: plain Win32 calls on buffers sized as passed; the handle is closed below.
        unsafe {
            check(OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token,
            ))?;
            let mut needed = 0u32;
            let mut got = GetTokenInformation(
                token,
                TokenUser,
                user.as_mut_ptr().cast(),
                (user.len() * 8) as u32,
                &mut needed,
            );
            if got == 0 && needed as usize > user.len() * 8 {
                user = vec![0u64; (needed as usize).div_ceil(8)];
                got = GetTokenInformation(
                    token,
                    TokenUser,
                    user.as_mut_ptr().cast(),
                    (user.len() * 8) as u32,
                    &mut needed,
                );
            }
            let got = check(got);
            CloseHandle(token);
            got?;
        }
        // SAFETY: GetTokenInformation filled `user` with a TOKEN_USER.
        let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };

        // SAFETY: the ACL buffer is sized for one ACE carrying `sid`, u32-aligned; the
        // descriptor and ACL outlive SetFileSecurityW, which copies them.
        unsafe {
            let acl_len = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                - std::mem::size_of::<u32>()
                + GetLengthSid(sid) as usize;
            let mut acl = vec![0u32; acl_len.div_ceil(4)];
            let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
            check(InitializeAcl(acl_ptr, acl_len as u32, ACL_REVISION))?;
            check(AddAccessAllowedAce(
                acl_ptr,
                ACL_REVISION,
                FILE_ALL_ACCESS,
                sid,
            ))?;

            let mut sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
            let sd_ptr = (&mut sd as *mut SECURITY_DESCRIPTOR).cast();
            check(InitializeSecurityDescriptor(sd_ptr, SD_REVISION))?;
            check(SetSecurityDescriptorDacl(sd_ptr, 1, acl_ptr, 0))?;

            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            check(SetFileSecurityW(
                wide.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                sd_ptr,
            ))
        }
    }
}

#[cfg(not(windows))]
fn write_restricted(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies when the file is created; tighten a pre-existing one too.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_restricts() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("creds.json");
        write_private(&f, "{}").unwrap();
        write_private(&f, "{\"a\":1}").unwrap(); // rewriting a restricted file works
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"a\":1}");
        #[cfg(windows)]
        {
            let out = std::process::Command::new("icacls")
                .arg(&f)
                .output()
                .unwrap();
            let acl = String::from_utf8_lossy(&out.stdout);
            assert!(!acl.contains("BUILTIN\\Users"), "{acl}");
            assert!(acl.contains(&std::env::var("USERNAME").unwrap()), "{acl}");
        }
    }

    /// A folder with nothing to inherit, as CI's temp folder is, gives a new file
    /// Windows' default grants (SYSTEM, Administrators, the logon session) as its
    /// own entries rather than inherited ones; they must go too.
    #[cfg(windows)]
    #[test]
    fn only_this_user_is_left_when_the_folder_gives_nothing_to_inherit() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("bare");
        std::fs::create_dir(&bare).unwrap();
        let user = std::env::var("USERNAME").unwrap();
        let status = std::process::Command::new("icacls")
            .arg(&bare)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{user}:F"))
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let f = bare.join("bridge.json");
        write_private(&f, "{}").unwrap();
        let out = std::process::Command::new("icacls")
            .arg(&f)
            .output()
            .unwrap();
        let acl = String::from_utf8_lossy(&out.stdout);
        assert!(!acl.contains("(I)"), "{acl}");
        assert_eq!(acl.matches(":(").count(), 1, "{acl}");
        assert!(acl.contains(&user), "{acl}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{}");
    }
}
