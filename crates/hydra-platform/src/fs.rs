use std::path::Path;

/// Writes a file that only the current user can read. If the file can't be
/// restricted, it is removed so a secret never stays on disk with open access.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    write_restricted(path, contents).inspect_err(|_| {
        let _ = std::fs::remove_file(path);
    })
}

#[cfg(windows)]
fn write_restricted(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    let user =
        std::env::var("USERNAME").map_err(|_| std::io::Error::other("USERNAME isn't set"))?;
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:F"))
        .stdout(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "icacls couldn't restrict {}",
            path.display()
        )))
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
}
