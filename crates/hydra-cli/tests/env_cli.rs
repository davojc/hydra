mod common;
use common::Home;
use predicates::prelude::*;

#[test]
fn init_creates_config_once() {
    let h = Home::new();
    h.hydra()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));
    assert!(h.root().join("config.toml").is_file());
    assert!(
        h.root().join("envs").is_dir()
            && h.root().join("base").is_dir()
            && h.root().join("state").is_dir()
    );
    h.hydra()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("already set up"));
}

#[test]
fn env_new_writes_template_and_rotates_colours() {
    let h = Home::new();
    h.hydra()
        .args(["env", "new", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hydra env edit work"));
    h.hydra()
        .args(["env", "new", "personal"])
        .assert()
        .success();
    assert!(h.env_toml("work").contains("color = \"#1f9a8a\""));
    assert!(h.env_toml("personal").contains("color = \"#c98a1e\""));
}

#[test]
fn env_new_rejects_bad_names_and_duplicates() {
    let h = Home::new();
    h.hydra()
        .args(["env", "new", "Work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid environment name"));
    h.hydra().args(["env", "new", "work"]).assert().success();
    h.hydra()
        .args(["env", "new", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn env_list_shows_envs_and_warns_about_bad_folders() {
    let h = Home::new();
    h.write_env("work", "label = \"acme work\"\ncolor = \"#1f9a8a\"\n");
    std::fs::create_dir_all(h.root().join("envs").join("Bad")).unwrap();
    h.hydra()
        .args(["env", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work").and(predicate::str::contains("acme work")))
        .stderr(predicate::str::contains("ignoring envs/Bad"));
}

#[test]
fn env_rm_removes_folder_state_and_secrets() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    h.hydra()
        .args(["secret", "set", "work/token"])
        .write_stdin("s3cret-value\n")
        .assert()
        .success();
    std::fs::create_dir_all(h.root().join("state").join("work").join("gh")).unwrap();
    h.hydra()
        .args(["env", "rm", "work", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed secrets: token"));
    assert!(!h.root().join("envs").join("work").exists());
    assert!(!h.root().join("state").join("work").exists());
}

#[test]
fn env_rm_keeps_state_without_confirmation() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    std::fs::create_dir_all(h.root().join("state").join("work")).unwrap();
    h.hydra()
        .args(["env", "rm", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kept saved logins"));
    assert!(h.root().join("state").join("work").exists());
}

#[test]
fn env_rename_updates_bindings_refs_and_secrets() {
    let h = Home::new();
    h.hydra().arg("init").assert().success();
    h.hydra().args(["env", "new", "work"]).assert().success();
    std::fs::write(
        h.root().join("config.toml"),
        "[bindings]\n\"E:/acme/**\" = \"work\"\n",
    )
    .unwrap();
    h.write_env("work", "[env]\nLINEAR_API_KEY = \"secret:work/linear\"\n");
    h.hydra()
        .args(["secret", "set", "work/linear"])
        .write_stdin("lin_api_123\n")
        .assert()
        .success();

    h.hydra()
        .args(["env", "rename", "work", "acme"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("renamed work to acme")
                .and(predicate::str::contains("moved secrets: linear")),
        );
    assert!(h.env_toml("acme").contains("secret:acme/linear"));
    assert!(
        std::fs::read_to_string(h.root().join("config.toml"))
            .unwrap()
            .contains("\"acme\"")
    );
    h.hydra()
        .args(["secret", "rm", "acme/linear"])
        .assert()
        .success();
}

#[test]
fn env_edit_validates_after_the_editor_closes() {
    let h = Home::new();
    h.write_env("work", "color = \"teal\"\n");
    h.hydra()
        .args(["env", "edit", "work"])
        .env_remove("VISUAL")
        .env("EDITOR", "cmd /c rem")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("must look like"));
    h.write_env("work", "color = \"#1f9a8a\"\n");
    h.hydra()
        .args(["env", "edit", "work"])
        .env_remove("VISUAL")
        .env("EDITOR", "cmd /c rem")
        .assert()
        .success()
        .stdout(predicate::str::contains("saved"));
}
