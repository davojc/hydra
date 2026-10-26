//! macOS: secrets through Apple's `/usr/bin/security` rather than the Security framework.
//!
//! The login keychain gives each item a partition list, and a binary without an Apple-issued
//! Team ID is on it only by the hash of that exact build (`cdhash:`). "Always Allow" therefore
//! lasts until the next build, and every hydra upgrade or rebuild asks again. `security` is in
//! the `apple-tool:` partition, which never changes, so one Always Allow per item lasts. Items
//! it creates trust it from the start (`-T`), and values reach it hex-encoded on its stdin,
//! never in its arguments.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SECURITY: &str = "/usr/bin/security";
/// `errSecItemNotFound`, as `security`'s exit status.
const NOT_FOUND: i32 = 44;
/// `errSecDuplicateItem`.
const DUPLICATE: i32 = 45;

/// Generic passwords in the user's keychains (or, for tests, one keychain file).
pub struct Keychain {
    file: Option<PathBuf>,
}

impl Keychain {
    /// The user's keychain search list: the login keychain.
    pub fn user() -> Self {
        Self { file: None }
    }

    pub fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
        let mut cmd = Command::new(SECURITY);
        cmd.args(["find-generic-password", "-s", service, "-a", account, "-g"]);
        cmd.args(&self.file);
        let out = cmd.output().map_err(|e| format!("{SECURITY}: {e}"))?;
        match out.status.code() {
            Some(0) => password_line(&String::from_utf8_lossy(&out.stderr)).map(Some),
            Some(NOT_FOUND) => Ok(None),
            _ => Err(failure(&out.stderr)),
        }
    }

    pub fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
        // `-X` wants at least one byte; `security -i` reads `""` as an empty argument.
        let data = if value.is_empty() {
            "-w \"\"".to_string()
        } else {
            format!("-X {}", to_hex(value.as_bytes()))
        };
        let add = format!("add-generic-password -s {service} -a {account} -T {SECURITY} {data}");
        match self.interactive(service, account, &add)? {
            (0, _) => Ok(()),
            // Already there: change the value only. Its access list is left alone, as changing
            // it would ask for the keychain password.
            (DUPLICATE, _) => {
                let update = format!("add-generic-password -U -s {service} -a {account} {data}");
                match self.interactive(service, account, &update)? {
                    (0, _) => Ok(()),
                    (_, stderr) => Err(failure(&stderr)),
                }
            }
            (_, stderr) => Err(failure(&stderr)),
        }
    }

    /// `Ok(true)` if a value was deleted.
    pub fn delete(&self, service: &str, account: &str) -> Result<bool, String> {
        let mut cmd = Command::new(SECURITY);
        cmd.args(["delete-generic-password", "-s", service, "-a", account]);
        cmd.args(&self.file);
        let out = cmd.output().map_err(|e| format!("{SECURITY}: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(NOT_FOUND) => Ok(false),
            _ => Err(failure(&out.stderr)),
        }
    }

    /// Runs one `security -i` command line, fed on stdin; its exit status and what it said.
    fn interactive(
        &self,
        service: &str,
        account: &str,
        line: &str,
    ) -> Result<(i32, Vec<u8>), String> {
        // `security -i` splits its line on spaces, so a name with one in it would be misread.
        for name in [service, account] {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic() && b != b'"') {
                return Err(format!("{name:?} can't be a keychain name"));
            }
        }
        let mut line = line.to_string();
        if let Some(file) = &self.file {
            line.push(' ');
            line.push_str(&file.to_string_lossy());
        }
        line.push('\n');
        let mut child = Command::new(SECURITY)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{SECURITY}: {e}"))?;
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(line.as_bytes())
            .map_err(|e| format!("{SECURITY}: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{SECURITY}: {e}"))?;
        match out.status.code() {
            Some(code) => Ok((code, out.stderr)),
            None => Err(failure(&out.stderr)),
        }
    }
}

/// The value on `security find-generic-password -g`'s `password:` line: `"text"` when every
/// byte prints plainly, else `0x` and its bytes in hex (then a quoted rendering, ignored).
fn password_line(stderr: &str) -> Result<String, String> {
    let rest = stderr
        .lines()
        .find_map(|l| l.strip_prefix("password:"))
        .ok_or("no password line from security")?
        .trim_start_matches(' ');
    if let Some(hex) = rest.strip_prefix("0x") {
        let hex = hex.split(' ').next().unwrap_or_default();
        let bytes = from_hex(hex).ok_or("unreadable hex from security")?;
        return String::from_utf8(bytes).map_err(|_| "the stored value isn't UTF-8".to_string());
    }
    let rest = rest.trim_end();
    match rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(text) => Ok(text.to_string()),
        None if rest.is_empty() => Ok(String::new()),
        None => Err("unreadable password line from security".to_string()),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `security`'s own reason, its first line (`security: SecKeychain…: The user name or
/// passphrase you entered is not correct.`), for the error a person sees.
fn failure(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    match text.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => line.to_string(),
        None => format!("{SECURITY} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_are_read_from_their_quotes() {
        assert_eq!(password_line("password: \"abcd\"\n").unwrap(), "abcd");
        assert_eq!(password_line("password: \"a b\"\n").unwrap(), "a b");
    }

    #[test]
    fn hex_values_are_decoded_and_their_rendering_ignored() {
        // "abcd" stored as text and the two bytes 0xab 0xcd differ only in this prefix.
        assert!(password_line("password: 0xABCD \n").is_err(), "not UTF-8");
        assert_eq!(
            password_line("password: 0x68C3A96C6C6F  \"h\\303\\251llo\"\n").unwrap(),
            "héllo"
        );
        assert_eq!(
            password_line("password: 0x22615C22  \"\"a\\134\"\"\n").unwrap(),
            "\"a\\\""
        );
        assert_eq!(
            password_line("password: 0x0961  \"\\011a\"\n").unwrap(),
            "\ta"
        );
    }

    #[test]
    fn an_empty_value_is_empty() {
        assert_eq!(password_line("password: \n").unwrap(), "");
    }

    #[test]
    fn a_missing_password_line_is_an_error() {
        assert!(password_line("keychain: \"x\"\n").is_err());
    }

    #[test]
    fn hex_round_trips() {
        let bytes = "héllo ✓".as_bytes();
        assert_eq!(from_hex(&to_hex(bytes)).unwrap(), bytes);
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }

    /// A keychain file of its own, so the test neither touches the login keychain nor
    /// joins the search list; deleted when the test ends.
    struct Scratch {
        _dir: tempfile::TempDir,
        file: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("t.keychain");
            for args in [
                ["create-keychain", "-p", "x"],
                ["unlock-keychain", "-p", "x"],
            ] {
                let ok = Command::new(SECURITY)
                    .args(args)
                    .arg(&file)
                    .status()
                    .unwrap()
                    .success();
                assert!(ok, "{args:?}");
            }
            Self { _dir: dir, file }
        }

        fn keychain(&self) -> Keychain {
            Keychain {
                file: Some(self.file.clone()),
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = Command::new(SECURITY)
                .arg("delete-keychain")
                .arg(&self.file)
                .status();
        }
    }

    #[test]
    fn values_round_trip_through_security() {
        let scratch = Scratch::new();
        let k = scratch.keychain();
        assert_eq!(k.get("svc", "work/linear").unwrap(), None);
        for value in [
            "lin_api_abcd",
            "abcd",
            "héllo ✓",
            "quote \" and \\",
            "\ttab",
            "",
        ] {
            k.set("svc", "work/linear", value).unwrap();
            assert_eq!(k.get("svc", "work/linear").unwrap().as_deref(), Some(value));
        }
        assert!(k.delete("svc", "work/linear").unwrap());
        assert!(!k.delete("svc", "work/linear").unwrap());
        assert_eq!(k.get("svc", "work/linear").unwrap(), None);
    }

    /// A scratch keychain is the old file format, which has no partition lists; in the login
    /// keychain an item `security` creates is in its `apple-tool:` partition.
    #[test]
    fn a_created_item_trusts_security_and_not_this_build() {
        let scratch = Scratch::new();
        scratch.keychain().set("svc", "work/a", "v").unwrap();
        let dump = Command::new(SECURITY)
            .args(["dump-keychain", "-a"])
            .arg(&scratch.file)
            .output()
            .unwrap();
        let dump = String::from_utf8_lossy(&dump.stdout);
        assert!(dump.contains("0: /usr/bin/security (OK)"), "{dump}");
        assert!(!dump.contains("cdhash"), "{dump}");
        assert!(!dump.contains("hydra_platform"), "{dump}");
    }

    #[test]
    fn names_that_security_would_split_are_refused() {
        let scratch = Scratch::new();
        let err = scratch.keychain().set("svc", "a b", "v").unwrap_err();
        assert!(err.contains("can't be a keychain name"), "{err}");
    }
}
