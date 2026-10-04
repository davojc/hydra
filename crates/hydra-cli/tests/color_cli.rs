mod common;
use common::Home;
use predicates::prelude::*;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";

#[test]
fn piped_output_has_no_escape_codes() {
    let h = Home::new();
    h.write_env("work", "label = \"acme work\"\ncolor = \"#1f9a8a\"\n");
    h.hydra()
        .args(["env", "list"])
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b[").not());
}

#[test]
fn forced_colour_paints_success_green() {
    let h = Home::new();
    h.write_env("work", "label = \"work\"\n");
    h.hydra()
        .args(["add", "claude", "work"])
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .assert()
        .success()
        .stdout(
            predicate::str::contains(GREEN).and(predicate::str::contains("added claude to work")),
        );
}

#[test]
fn forced_colour_paints_sign_in_hint_yellow() {
    let h = Home::new();
    h.write_env("work", "[github]\n");
    let out = h
        .hydra()
        .args(["shell", "work", "--shell", "pwsh"])
        .write_stdin("exit 0\n")
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let err = String::from_utf8_lossy(&out);
    let at = err.find("not signed in").expect(&err);
    assert!(err[..at].contains(YELLOW), "{err:?}");
}

#[test]
fn forced_colour_paints_whoami_ok_green() {
    let h = Home::new();
    let key = h.dir.path().join("id_test");
    std::fs::write(&key, "not a real key").unwrap();
    let key = key.to_string_lossy().replace('\\', "/");
    h.write_env(
        "work",
        &format!("[git]\nemail = \"dev@example.com\"\nssh_key = \"{key}\"\n"),
    );
    h.hydra()
        .args(["whoami", "--env", "work"])
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{GREEN}ok")));
}

#[test]
fn no_colour_turns_colour_off() {
    let h = Home::new();
    h.write_env("work", "label = \"work\"\n");
    h.hydra()
        .args(["add", "claude", "work"])
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b[").not());
}
