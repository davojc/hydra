mod common;
use common::Home;
use predicates::prelude::*;

#[test]
fn set_and_remove_secret() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    h.hydra()
        .args(["secret", "set", "work/linear"])
        .write_stdin("lin_api_123\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("stored work/linear"));
    let index = std::fs::read_to_string(h.root().join("secrets.toml")).unwrap();
    assert!(index.contains("linear") && !index.contains("lin_api_123"));
    h.hydra()
        .args(["secret", "rm", "work/linear"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed work/linear"));
    h.hydra()
        .args(["secret", "rm", "work/linear"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no secret named work/linear"));
}

#[test]
fn set_secret_for_unknown_env_fails() {
    let h = Home::new();
    h.hydra()
        .args(["secret", "set", "wrok/linear"])
        .write_stdin("x\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("environment wrok doesn't exist"));
}

#[test]
fn set_secret_rejects_empty_value_and_bad_names() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    h.hydra()
        .args(["secret", "set", "work/linear"])
        .write_stdin("\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing stored"));
    h.hydra()
        .args(["secret", "set", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("like work/linear"));
}

/// Removes a secret from this home's Credential Manager namespace, even if the test fails.
struct SecretGuard<'a>(&'a Home, &'a str);

impl Drop for SecretGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.hydra().args(["secret", "rm", self.1]).output();
    }
}

#[test]
fn set_secret_stores_all_of_piped_stdin() {
    let h = Home::new();
    h.write_env("work", "[env]\nCREDS = \"secret:work/creds\"\n");
    let _guard = SecretGuard(&h, "work/creds");
    let json = "{\n  \"type\": \"authorized_user\",\n  \"client_id\": \"abc\"\n}";
    h.hydra()
        .args(["secret", "set", "work/creds"])
        .write_stdin(format!("{json}\r\n"))
        .assert()
        .success();
    let out = h
        .hydra()
        .args([
            "run",
            "work",
            "--",
            "pwsh",
            "-NoProfile",
            "-Command",
            "Write-Output \"[$env:CREDS]\"",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert!(stdout.contains(&format!("[{json}]")), "{stdout}");
}
