mod common;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use common::Home;
use predicates::prelude::*;

const EXE_ASSET: &str = "hydra-x86_64-pc-windows-msvc.exe";
const NEW_BYTES: &[u8] = b"new hydra build";

/// A tiny HTTP/1.1 server answering GETs for a fixed set of paths (404 otherwise).
struct FakeGitHub {
    base: String,
}

impl FakeGitHub {
    /// Serves a latest release `tag` whose exe is NEW_BYTES and whose .sha256 file is `sha_file`
    /// (None: the real checksum of NEW_BYTES).
    fn release(tag: &str, sha_file: Option<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let sha = sha_file
            .map(str::to_string)
            .unwrap_or_else(|| format!("{}  {EXE_ASSET}\n", sha256_hex(NEW_BYTES)));
        let json = format!(
            r#"{{"tag_name":"{tag}","assets":[
                {{"name":"{EXE_ASSET}","browser_download_url":"{base}/dl/{EXE_ASSET}"}},
                {{"name":"{EXE_ASSET}.sha256","browser_download_url":"{base}/dl/{EXE_ASSET}.sha256"}}]}}"#
        );
        let mut routes: HashMap<String, Vec<u8>> = HashMap::new();
        routes.insert(
            "/repos/davojc/hydra/releases/latest".into(),
            json.into_bytes(),
        );
        routes.insert(format!("/dl/{EXE_ASSET}"), NEW_BYTES.to_vec());
        routes.insert(format!("/dl/{EXE_ASSET}.sha256"), sha.into_bytes());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, &routes);
            }
        });
        Self { base }
    }
}

fn serve(mut stream: TcpStream, routes: &HashMap<String, Vec<u8>>) {
    let mut req = Vec::new();
    let mut buf = [0u8; 1024];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => req.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&req);
    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
    let (status, body) = match routes.get(&path) {
        Some(b) => ("200 OK", b.clone()),
        None => ("404 Not Found", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A fake installed hydra.exe containing `old`.
fn installed(h: &Home) -> PathBuf {
    let dir = h.dir.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("hydra.exe");
    std::fs::write(&exe, "old").unwrap();
    exe
}

fn update(h: &Home, api: &str, exe: &PathBuf) -> assert_cmd::Command {
    let mut c = h.hydra();
    c.arg("update")
        .env("HYDRA_UPDATE_API", api)
        .env("HYDRA_UPDATE_EXE", exe)
        .env("HYDRA_UPDATE_SKIP_PROCESS_CHECK", "1");
    c
}

#[test]
fn newer_release_replaces_the_exe() {
    let h = Home::new();
    let exe = installed(&h);
    let gh = FakeGitHub::release("v0.4.0", None);
    update(&h, &gh.base, &exe)
        .assert()
        .success()
        .stdout(predicate::str::contains("updated to 0.4.0"));
    assert_eq!(std::fs::read(&exe).unwrap(), NEW_BYTES);
    let dir = exe.parent().unwrap();
    assert!(!dir.join("hydra.new.exe").exists());
}

#[test]
fn checksum_mismatch_changes_nothing() {
    let h = Home::new();
    let exe = installed(&h);
    let wrong = format!("{}  {EXE_ASSET}\n", "0".repeat(64));
    let gh = FakeGitHub::release("v0.4.0", Some(&wrong));
    update(&h, &gh.base, &exe)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "checksum mismatch - nothing changed",
        ));
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
    assert!(!exe.parent().unwrap().join("hydra.new.exe").exists());
}

#[test]
fn check_only_reports_the_available_version() {
    let h = Home::new();
    let exe = installed(&h);
    let gh = FakeGitHub::release("v0.4.0", None);
    update(&h, &gh.base, &exe)
        .arg("--check")
        .assert()
        .success()
        .stdout(predicate::str::contains("is installed; 0.4.0 is available"));
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
}

#[test]
fn older_release_is_up_to_date() {
    let h = Home::new();
    let exe = installed(&h);
    let gh = FakeGitHub::release("v0.3.0", None);
    update(&h, &gh.base, &exe)
        .assert()
        .success()
        .stdout(predicate::str::contains("is up to date"));
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
}

#[test]
fn force_installs_even_when_up_to_date() {
    let h = Home::new();
    let exe = installed(&h);
    let gh = FakeGitHub::release("v0.3.0", None);
    update(&h, &gh.base, &exe)
        .arg("--force")
        .assert()
        .success()
        .stdout(predicate::str::contains("updated to 0.3.0"));
    assert_eq!(std::fs::read(&exe).unwrap(), NEW_BYTES);
}

#[test]
fn unreachable_api_gives_a_clear_error() {
    let h = Home::new();
    let exe = installed(&h);
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port(); // the listener is dropped here, so the port is closed
    update(&h, &format!("http://127.0.0.1:{port}"), &exe)
        .assert()
        .failure()
        .stderr(predicate::str::contains("can't reach GitHub"));
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
}

#[test]
fn version_flag_reports_a_dev_build() {
    Home::new()
        .hydra()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(concat!(
            "hydra ",
            env!("CARGO_PKG_VERSION"),
            "-dev"
        )));
}

#[test]
fn start_removes_a_leftover_old_exe() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_hydra"));
    let old = bin.parent().unwrap().join("hydra.old.exe");
    std::fs::write(&old, "left over from an update").unwrap();
    Home::new().hydra().arg("--version").assert().success();
    assert!(!old.exists());
}
