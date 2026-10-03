use std::path::Path;

/// Writes a file that only the current user can read.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    restrict_to_user(path)
}

#[cfg(windows)]
fn restrict_to_user(path: &Path) -> std::io::Result<()> {
    let user = std::env::var("USERNAME").map_err(|_| std::io::Error::other("USERNAME isn't set"))?;
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:F"))
        .stdout(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("icacls couldn't restrict {}", path.display())))
    }
}

#[cfg(not(windows))]
fn restrict_to_user(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
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
            let out = std::process::Command::new("icacls").arg(&f).output().unwrap();
            let acl = String::from_utf8_lossy(&out.stdout);
            assert!(!acl.contains("BUILTIN\\Users"), "{acl}");
            assert!(acl.contains(&std::env::var("USERNAME").unwrap()), "{acl}");
        }
    }
}
