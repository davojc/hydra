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
