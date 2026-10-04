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

/// Path, length and content of every file under `dir` (sorted), plus `extra`.
fn fingerprint(dir: &std::path::Path, extra: &std::path::Path) -> Vec<(String, usize, Vec<u8>)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, usize, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                out.push((p.display().to_string(), bytes.len(), bytes));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    let bytes = std::fs::read(extra).unwrap();
    out.push((extra.display().to_string(), bytes.len(), bytes));
    out.sort();
    out
}

#[test]
fn run_rename_and_rm_never_change_the_base() {
    let h = Home::new();
    let user = fake_user(&h);
    let base = user.join(".claude");
    std::fs::create_dir_all(base.join("plugins")).unwrap();
    std::fs::write(base.join("plugins").join("installed_plugins.json"), "{}").unwrap();
    std::fs::write(
        base.join("settings.json"),
        r#"{"model":"opus","apiKeyHelper":"key.sh"}"#,
    )
    .unwrap();
    std::fs::write(base.join(".credentials.json"), "BASE-SECRET").unwrap();
    let before = fingerprint(&base, &user.join(".claude.json"));
    h.write_env("work", "[claude]\n");
    hydra_as(&h, &user)
        .args(["run", "work", "--", "cmd", "/c", "exit", "0"])
        .assert()
        .success();
    hydra_as(&h, &user)
        .args(["env", "rename", "work", "iov"])
        .assert()
        .success();
    hydra_as(&h, &user)
        .args(["env", "rm", "iov", "--yes"])
        .assert()
        .success();
    assert_eq!(fingerprint(&base, &user.join(".claude.json")), before);
}

#[test]
fn new_environments_start_without_tools() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    let toml = h.env_toml("work");
    assert!(!toml.contains("\n[claude]"), "{toml}");
    assert!(toml.contains("hydra add <tool> work"), "{toml}");
    h.hydra()
        .args(["add", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added claude to work"));
}
