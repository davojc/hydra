mod common;
use common::Home;
use predicates::prelude::*;

/// An environment with comments but no tool sections, like a hand-trimmed template.
const BARE: &str = "# my notes\nlabel = \"work\"\ncolor = \"#1f9a8a\"\n\n# [git]\n# name = \"x\"\n";

fn home_with_work() -> Home {
    let h = Home::new();
    h.write_env("work", BARE);
    h
}

fn show_claude_dir(h: &Home) -> assert_cmd::assert::Assert {
    h.hydra()
        .args([
            "run",
            "work",
            "--",
            "pwsh",
            "-NoProfile",
            "-Command",
            "Write-Output \"[$env:CLAUDE_CONFIG_DIR]\"",
        ])
        .assert()
}

#[test]
fn add_claude_writes_config_only_and_points_at_the_environment() {
    let h = home_with_work();
    h.hydra()
        .args(["add", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added claude to work"))
        .stdout(predicate::str::contains("next: open a new work terminal"))
        .stdout(predicate::str::contains("and sign in there"));
    let text = h.env_toml("work");
    assert!(text.contains("\n[claude]"), "{text}");
    assert!(
        text.contains("# my notes") && text.contains("# [git]"),
        "{text}"
    );
    show_claude_dir(&h)
        .success()
        .stdout(predicate::str::is_match(r"state\\work\\claude\]").unwrap());
}

#[test]
fn add_twice_says_already_on() {
    let h = home_with_work();
    h.hydra().args(["add", "claude", "work"]).assert().success();
    let before = h.env_toml("work");
    h.hydra()
        .args(["add", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude is already on in work"))
        .stdout(predicate::str::contains("next:").not());
    assert_eq!(h.env_toml("work"), before);
}

#[test]
fn add_git_identity_is_used_by_git() {
    let h = home_with_work();
    h.hydra()
        .args(["add", "git", "work", "--email", "a@b.c", "--name", "A"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added git to work"));
    h.hydra()
        .args([
            "run",
            "work",
            "--",
            "git",
            "config",
            "--global",
            "user.email",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("a@b.c"));
}

#[test]
fn add_aws_without_profile_fails_and_changes_nothing() {
    let h = home_with_work();
    h.hydra()
        .args(["add", "aws", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("aws needs --profile"));
    assert_eq!(h.env_toml("work"), BARE);
}

#[test]
fn add_unknown_tool_lists_the_tools() {
    let h = home_with_work();
    h.hydra()
        .args(["add", "nosuch", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("claude").and(predicate::str::contains("gemini")));
    assert_eq!(h.env_toml("work"), BARE);
}

#[test]
fn add_without_a_tool_lists_tools_and_marks_the_ones_on() {
    let h = home_with_work();
    h.hydra().args(["add", "git", "work"]).assert().success();
    h.hydra()
        .env("HYDRA_ENV", "work")
        .arg("add")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"(?m)^git\s+on$").unwrap())
        .stdout(predicate::str::is_match(r"(?m)^claude\s*$").unwrap());
    h.hydra()
        .arg("add")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"(?m)^claude$").unwrap())
        .stdout(predicate::str::contains(" on").not());
}

#[test]
fn remove_takes_the_section_out_and_keeps_logins() {
    let h = home_with_work();
    h.hydra().args(["add", "claude", "work"]).assert().success();
    h.hydra()
        .args(["remove", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed claude from work"))
        .stdout(predicate::str::contains("saved logins stay in"));
    assert!(!h.env_toml("work").contains("[claude]"));
    show_claude_dir(&h)
        .success()
        .stdout(predicate::str::contains("[]"));
    h.hydra()
        .args(["remove", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude isn't on in work"));
}

#[test]
fn add_needs_an_environment() {
    let h = home_with_work();
    h.hydra()
        .args(["add", "claude"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("name an environment"));
}
