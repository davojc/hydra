mod common;
use common::Home;
use predicates::prelude::*;

fn show(var: &str) -> Vec<String> {
    vec![
        "pwsh".into(),
        "-NoProfile".into(),
        "-Command".into(),
        format!("Write-Output \"[$env:{var}]\""),
    ]
}

fn run_args(env: &str, cmd: Vec<String>) -> Vec<String> {
    let mut a = vec!["run".to_string(), env.to_string(), "--".to_string()];
    a.extend(cmd);
    a
}

#[test]
fn run_sets_provider_vars_and_hydra_env() {
    let h = Home::new();
    h.write_env("work", "[aws]\nprofile = \"acme-dev\"\n");
    h.hydra()
        .args(run_args("work", show("AWS_PROFILE")))
        .assert()
        .success()
        .stdout(predicate::str::contains("[acme-dev]"));
    h.hydra()
        .args(run_args("work", show("HYDRA_ENV")))
        .assert()
        .success()
        .stdout(predicate::str::contains("[work]"));
}

#[test]
fn run_clears_inherited_identity() {
    let h = Home::new();
    h.write_env("work", "label = \"w\"\n");
    h.hydra()
        .env("AWS_PROFILE", "leaked")
        .args(run_args("work", show("AWS_PROFILE")))
        .assert()
        .success()
        .stdout(predicate::str::contains("[]"));
}

#[test]
fn run_clears_inherited_identity_whatever_the_case() {
    let h = Home::new();
    h.write_env("work", "label = \"w\"\n");
    h.hydra()
        .env("aws_profile", "leaked")
        .args(run_args("work", show("AWS_PROFILE")))
        .assert()
        .success()
        .stdout(predicate::str::contains("[]"));
}

#[test]
fn run_passes_exit_code_through() {
    let h = Home::new();
    h.write_env("work", "");
    h.hydra()
        .args(run_args(
            "work",
            vec![
                "pwsh".into(),
                "-NoProfile".into(),
                "-Command".into(),
                "exit 7".into(),
            ],
        ))
        .assert()
        .code(7);
}

#[test]
fn run_fails_closed_on_missing_secret() {
    let h = Home::new();
    h.write_env("work", "[env]\nTOKEN = \"secret:work/token\"\n");
    h.hydra()
        .args(run_args("work", show("TOKEN")))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("secret work/token isn't set")
                .and(predicate::str::contains("hydra secret set work/token")),
        );
}

#[test]
fn run_finds_cmd_launchers() {
    let h = Home::new();
    h.write_env("work", "");
    let bin = h.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("hello.cmd"), "@echo hello-from-cmd\r\n").unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    h.hydra()
        .env("PATH", path)
        .args(["run", "work", "--", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello-from-cmd"));
}

#[test]
fn whoami_reports_git_identity() {
    let h = Home::new();
    let key = h.dir.path().join("id_test");
    std::fs::write(&key, "not a real key").unwrap();
    let key = key.to_string_lossy().replace('\\', "/");
    h.write_env(
        "work",
        &format!(
            "label = \"acme work\"\n[git]\nemail = \"dev@example.com\"\nssh_key = \"{key}\"\n"
        ),
    );
    h.hydra()
        .args(["whoami", "--env", "work"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("work · acme work")
                .and(predicate::str::contains("dev@example.com"))
                .and(predicate::str::contains("ok")),
        );
}

#[test]
fn whoami_needs_an_environment() {
    let h = Home::new();
    h.hydra()
        .arg("whoami")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not in a hydra terminal"));
}

#[test]
fn shell_writes_init_and_exits_with_the_shell() {
    let h = Home::new();
    h.write_env("work", "color = \"#1f9a8a\"\n");
    h.hydra()
        .args(["shell", "work", "--shell", "pwsh"])
        .write_stdin("exit 0\n")
        .assert()
        .success();
    let init = std::fs::read_to_string(
        h.root()
            .join("state")
            .join("work")
            .join("shell")
            .join("init.ps1"),
    )
    .unwrap();
    assert!(init.contains("$([char]27)[38;2;31;154;138m[work]"));
}

#[test]
fn shell_requires_an_environment_name() {
    let h = Home::new();
    h.hydra()
        .arg("shell")
        .assert()
        .failure()
        .stderr(predicate::str::contains("hydra shell <env>"));
}

#[test]
fn auth_requires_configured_provider() {
    let h = Home::new();
    h.write_env("work", "");
    h.hydra()
        .args(["auth", "gh", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("add a [github] section"));
}
