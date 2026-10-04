mod common;
use common::Home;
use predicates::prelude::*;

/// The test runs hydra with HYDRA_USER_HOME pointed at a fake user folder, so the
/// real ~/.claude is never touched.
fn fake_user(h: &Home) -> std::path::PathBuf {
    let user = h.dir.path().join("user");
    let base = user.join(".claude");
    std::fs::create_dir_all(base.join("skills").join("s1")).unwrap();
    std::fs::write(base.join("skills").join("s1").join("SKILL.md"), "precious").unwrap();
    std::fs::write(base.join("CLAUDE.md"), "# shared").unwrap();
    std::fs::write(
        user.join(".claude.json"),
        r#"{"mcpServers":{"codegraph":{}}}"#,
    )
    .unwrap();
    user
}

fn hydra_as(h: &Home, user: &std::path::Path) -> assert_cmd::Command {
    let mut c = h.hydra();
    c.env("HYDRA_USER_HOME", user);
    c
}

#[test]
fn each_environment_gets_its_own_claude_folder() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("work", "[claude]\n");
    h.write_env("personal", "[claude]\n");
    for env in ["work", "personal"] {
        hydra_as(&h, &user)
            .args([
                "run",
                env,
                "--",
                "pwsh",
                "-NoProfile",
                "-Command",
                "Write-Output \"[$env:CLAUDE_CONFIG_DIR]\"",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("state\\{env}\\claude]")));
        let dir = h.root().join("state").join(env).join("claude");
        assert_eq!(
            std::fs::read_to_string(dir.join("skills").join("s1").join("SKILL.md")).unwrap(),
            "precious"
        );
        assert!(
            std::fs::read_to_string(dir.join(".claude.json"))
                .unwrap()
                .contains("codegraph")
        );
    }
}

#[test]
fn shell_says_how_to_sign_in_to_claude() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("work", "[claude]\n");
    hydra_as(&h, &user)
        .args(["shell", "work", "--shell", "pwsh"])
        .write_stdin("exit 0\n")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hydra: claude: not signed in - run claude auth login",
        ));
}

#[test]
fn env_rm_keeps_the_claude_base_intact() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("work", "[claude]\n");
    hydra_as(&h, &user)
        .args(["run", "work", "--", "cmd", "/c", "exit", "0"])
        .assert()
        .success();
    hydra_as(&h, &user)
        .args(["env", "rename", "work", "iov"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(
            h.root()
                .join("state")
                .join("iov")
                .join("claude")
                .join("skills")
                .join("s1")
                .join("SKILL.md")
        )
        .unwrap(),
        "precious"
    );
    hydra_as(&h, &user)
        .args(["env", "rm", "iov", "--yes"])
        .assert()
        .success();
    assert!(!h.root().join("state").join("iov").exists());
    assert_eq!(
        std::fs::read_to_string(
            user.join(".claude")
                .join("skills")
                .join("s1")
                .join("SKILL.md")
        )
        .unwrap(),
        "precious"
    );
    assert_eq!(
        std::fs::read_to_string(user.join(".claude").join("CLAUDE.md")).unwrap(),
        "# shared"
    );
}

#[test]
fn new_environments_use_claude_by_default() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    assert!(h.env_toml("work").contains("\n[claude]"));
}
