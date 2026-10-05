mod common;
use common::Home;
use predicates::prelude::*;

/// A fake user folder (so the real ~/.claude is never touched), as in claude_cli.rs.
fn fake_user(h: &Home) -> std::path::PathBuf {
    let user = h.dir.path().join("user");
    std::fs::create_dir_all(user.join(".claude")).unwrap();
    std::fs::write(user.join(".claude.json"), "{}").unwrap();
    user
}

fn hydra_as(h: &Home, user: &std::path::Path) -> assert_cmd::Command {
    let mut c = h.hydra();
    c.env("HYDRA_USER_HOME", user);
    c
}

fn echo(var: &str) -> [String; 4] {
    [
        "pwsh".into(),
        "-NoProfile".into(),
        "-Command".into(),
        format!("Write-Output \"[$env:{var}]\""),
    ]
}

#[test]
fn borrowed_tools_point_at_the_owners_folders() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("personal", "[claude]\n\n[github]\n\n[codex]\n");
    h.write_env(
        "work",
        "[claude]\nfrom = \"personal\"\n\n[github]\nfrom = \"personal\"\n\n[codex]\nfrom = \"personal\"\n",
    );
    for (var, sub) in [
        ("CLAUDE_CONFIG_DIR", "claude"),
        ("GH_CONFIG_DIR", "gh"),
        ("CODEX_HOME", "codex"),
    ] {
        hydra_as(&h, &user)
            .args(["run", "work", "--"])
            .args(echo(var))
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("state\\personal\\{sub}]")));
    }
    assert!(!h.root().join("state").join("work").join("claude").exists());
    // The guard hook is in the shared folder and still guards as the borrower (HYDRA_ENV=work).
    let settings = std::fs::read_to_string(
        h.root()
            .join("state")
            .join("personal")
            .join("claude")
            .join("settings.json"),
    )
    .unwrap();
    assert!(settings.contains("guard claude"), "{settings}");
    hydra_as(&h, &user)
        .args(["run", "work", "--"])
        .args(echo("HYDRA_ENV"))
        .assert()
        .success()
        .stdout(predicate::str::contains("[work]"));
}

#[test]
fn borrowed_github_keeps_the_borrowers_git_author() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("personal", "[github]\n");
    h.write_env(
        "work",
        "[github]\nfrom = \"personal\"\n\n[git]\nname = \"Work Me\"\nemail = \"work@example.com\"\n",
    );
    // The effective author a commit would record, run away from the hydra checkout's own config.
    hydra_as(&h, &user)
        .current_dir(h.dir.path())
        .args(["run", "work", "--", "git", "var", "GIT_AUTHOR_IDENT"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Work Me <work@example.com>"));
    hydra_as(&h, &user)
        .args(["run", "work", "--"])
        .args(echo("GH_CONFIG_DIR"))
        .assert()
        .success()
        .stdout(predicate::str::contains("state\\personal\\gh]"));
}

#[test]
fn launch_fails_closed_when_the_owner_lacks_the_tool() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("personal", "label = \"p\"\n");
    h.write_env("work", "[claude]\nfrom = \"personal\"\n");
    hydra_as(&h, &user)
        .args(["run", "work", "--", "cmd", "/c", "exit", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "work borrows [claude] from personal, but personal has no [claude]",
        ))
        .stderr(predicate::str::contains(
            "-> add it there: hydra add claude personal",
        ));
}

#[test]
fn not_signed_in_points_at_the_owner() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("personal", "[claude]\n");
    h.write_env("work", "[claude]\nfrom = \"personal\"\n");
    hydra_as(&h, &user)
        .args(["shell", "work", "--shell", "pwsh"])
        .write_stdin("exit 0\n")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hydra: claude: not signed in - work borrows it from personal",
        ))
        .stderr(predicate::str::contains(
            "-> sign in there: hydra shell personal, then claude auth login",
        ));
}

#[test]
fn whoami_tags_borrowed_tools() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("personal", "[codex]\n");
    h.write_env("work", "[codex]\nfrom = \"personal\"\n");
    let out = hydra_as(&h, &user)
        .args(["whoami", "--env", "work"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("from personal · "), "{stdout}");
}

#[test]
fn add_from_writes_the_borrow_and_checks_the_owner() {
    let h = Home::new();
    h.write_env("personal", "label = \"p\"\n");
    h.write_env("work", "label = \"w\"\n");
    h.hydra()
        .args(["add", "claude", "work", "--from", "personal"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("personal has no [claude]"));
    assert_eq!(h.env_toml("work"), "label = \"w\"\n");
    h.hydra()
        .args(["add", "claude", "personal"])
        .assert()
        .success();
    h.hydra()
        .args(["add", "claude", "work", "--from", "personal"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "added claude to work (borrowed from personal)",
        ));
    assert!(
        h.env_toml("work")
            .contains("[claude]\nfrom = \"personal\"\n")
    );
}

#[test]
fn remove_refuses_an_owner_that_lends_and_frees_a_borrower() {
    let h = Home::new();
    h.write_env("personal", "[claude]\n");
    h.write_env("work", "[claude]\nfrom = \"personal\"\n");
    h.hydra()
        .args(["remove", "claude", "personal"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("personal lends claude to work"))
        .stderr(predicate::str::contains(
            "-> remove it there first: hydra remove claude work",
        ));
    h.hydra()
        .args(["remove", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed claude from work; it was borrowed from personal, whose login is untouched",
        ));
    h.hydra()
        .args(["remove", "claude", "personal"])
        .assert()
        .success();
}

#[test]
fn env_rm_refuses_an_owner_with_borrowers() {
    let h = Home::new();
    h.write_env("personal", "[claude]\n[github]\n");
    h.write_env(
        "work",
        "[claude]\nfrom = \"personal\"\n[github]\nfrom = \"personal\"\n",
    );
    h.hydra()
        .args(["env", "rm", "personal", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "work borrows claude, github from personal",
        ));
    assert!(
        h.root()
            .join("envs")
            .join("personal")
            .join("env.toml")
            .is_file()
    );
}

#[test]
fn env_rename_updates_borrowers() {
    let h = Home::new();
    h.write_env("personal", "[claude]\n");
    h.write_env("work", "[claude]\nfrom = \"personal\"\n");
    h.hydra()
        .args(["env", "rename", "personal", "home"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated borrowers: work"));
    assert_eq!(h.env_toml("work"), "[claude]\nfrom = \"home\"\n");
}
