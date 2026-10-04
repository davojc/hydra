mod common;
use std::path::{Path, PathBuf};

use common::Home;
use predicates::prelude::*;

/// A home with `work` and `personal` (git identity only) and `<tmp>/repo` bound to `work`.
struct Setup {
    h: Home,
    repo: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let h = Home::new();
        h.write_env(
            "work",
            "[git]\nname = \"Work\"\nemail = \"work@example.com\"\n",
        );
        h.write_env(
            "personal",
            "[git]\nname = \"Me\"\nemail = \"me@example.com\"\n",
        );
        std::fs::create_dir_all(h.dir.path().join("user")).unwrap();
        let repo = h.dir.path().join("repo");
        let s = Self { h, repo };
        s.git().arg("init").arg(&s.repo).assert().success();
        s.h.hydra()
            .arg("bind")
            .arg(&s.repo)
            .arg("work")
            .assert()
            .success();
        s
    }

    fn tmp(&self) -> &Path {
        self.h.dir.path()
    }

    /// git run directly, fenced to the test folder and away from any real config.
    fn git(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::new("git");
        c.env("GIT_CEILING_DIRECTORIES", self.tmp())
            .env(
                "GIT_CONFIG_GLOBAL",
                self.tmp().join("user").join(".gitconfig"),
            )
            .env("HYDRA_USER_HOME", self.tmp().join("user"))
            .env_remove("HYDRA_ENV")
            .env_remove("HYDRA_ALLOW");
        c
    }

    /// `hydra run <env> -- git -C <repo> <args...>`
    fn run_git(&self, env: &str, args: &[&str]) -> assert_cmd::Command {
        let mut c = self.h.hydra();
        c.args(["run", env, "--", "git", "-C"])
            .arg(&self.repo)
            .args(args);
        c
    }

    fn commit_count(&self) -> String {
        let out = self
            .git()
            .arg("-C")
            .arg(&self.repo)
            .args(["rev-list", "--all", "--count"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn state_git(&self, env: &str) -> PathBuf {
        self.h.root().join("state").join(env).join("git")
    }

    fn write_repo_hook(&self, name: &str, body: &str) {
        std::fs::write(
            self.repo.join(".git").join("hooks").join(name),
            format!("#!/bin/sh\n{body}\n"),
        )
        .unwrap();
    }
}

#[test]
fn commit_in_right_environment_succeeds() {
    let s = Setup::new();
    s.run_git("work", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .success();
    assert_eq!(s.commit_count(), "1");
}

#[test]
fn commit_from_wrong_environment_is_blocked() {
    let s = Setup::new();
    s.run_git("personal", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "blocked git commit - this folder belongs to work",
        ))
        .stderr(predicate::str::contains("-> open a work terminal here"));
    assert_eq!(s.commit_count(), "0");
}

#[test]
fn allow_overrides_once() {
    let s = Setup::new();
    s.run_git("personal", &["commit", "--allow-empty", "-m", "x"])
        .env("HYDRA_ALLOW", "1")
        .assert()
        .success();
    assert_eq!(s.commit_count(), "1");
}

#[test]
fn repo_hooks_still_run() {
    let s = Setup::new();
    // Hooks run at the work-tree root, so a relative path lands in the repo.
    s.write_repo_hook("pre-commit", "echo ran > hook-ran\nexit 0");
    s.run_git("work", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .success();
    assert!(s.repo.join("hook-ran").is_file());

    s.write_repo_hook("commit-msg", "exit 1");
    s.run_git("work", &["commit", "--allow-empty", "-m", "y"])
        .assert()
        .failure();
    assert_eq!(s.commit_count(), "1");
}

#[test]
fn pre_push_stdin_reaches_repo_hook() {
    let s = Setup::new();
    let remote = s.tmp().join("remote.git");
    s.git()
        .args(["init", "--bare"])
        .arg(&remote)
        .assert()
        .success();
    s.git()
        .arg("-C")
        .arg(&s.repo)
        .args(["remote", "add", "origin"])
        .arg(&remote)
        .assert()
        .success();
    s.write_repo_hook("pre-push", "cat > push-stdin");
    s.run_git("work", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .success();
    s.run_git("work", &["push", "origin", "HEAD:refs/heads/main"])
        .assert()
        .success();
    let stdin = std::fs::read_to_string(s.repo.join("push-stdin")).unwrap();
    assert!(stdin.contains("refs/heads/"), "{stdin:?}");
}

#[test]
fn push_from_wrong_environment_is_blocked() {
    let s = Setup::new();
    let remote = s.tmp().join("remote.git");
    s.git()
        .args(["init", "--bare"])
        .arg(&remote)
        .assert()
        .success();
    s.run_git("work", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .success();
    s.run_git("personal", &["push"])
        .arg(&remote)
        .arg("HEAD:refs/heads/main")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "blocked git push - this folder belongs to work",
        ));
}

#[test]
fn broken_config_allows_with_warning() {
    let s = Setup::new();
    s.run_git("work", &["status"]).assert().success();
    std::fs::write(s.h.root().join("config.toml"), "this is = = not toml").unwrap();
    s.git()
        .arg("-C")
        .arg(&s.repo)
        .args(["commit", "--allow-empty", "-m", "y"])
        .env("GIT_CONFIG_GLOBAL", s.state_git("work").join("gitconfig"))
        .env("HYDRA_ENV", "personal")
        .env("HYDRA_HOME", s.h.root())
        .assert()
        .success()
        .stderr(predicate::str::contains("guard skipped"));
    assert_eq!(s.commit_count(), "1");
}

/// PATH without any folder holding a hydra executable.
fn path_without_hydra() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|d| !d.join("hydra.exe").exists() && !d.join("hydra").exists())
        .collect();
    std::env::join_paths(dirs).unwrap()
}

/// Commits in `repo` as `personal` (a wrong environment) through a copy of the generated
/// pre-commit wrapper whose `hydra_exe=` line is replaced by `exe_line`.
fn commit_through_wrapper(s: &Setup, exe_line: &str) -> assert_cmd::assert::Assert {
    s.run_git("work", &["status"]).assert().success();
    let wrapper =
        std::fs::read_to_string(s.state_git("work").join("hooks").join("pre-commit")).unwrap();
    let original = wrapper
        .lines()
        .find(|l| l.starts_with("hydra_exe="))
        .unwrap()
        .to_string();
    let hooks = s.tmp().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("pre-commit"),
        wrapper.replace(&original, exe_line),
    )
    .unwrap();
    s.git()
        .arg("-C")
        .arg(&s.repo)
        .arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            hooks.to_string_lossy().replace('\\', "/")
        ))
        .args(["commit", "--allow-empty", "-m", "z"])
        .env("GIT_CONFIG_GLOBAL", s.state_git("work").join("gitconfig"))
        .env("HYDRA_ENV", "personal")
        .env("HYDRA_HOME", s.h.root())
        .env("PATH", path_without_hydra())
        .assert()
}

#[test]
fn hook_allows_when_hydra_is_missing() {
    let s = Setup::new();
    commit_through_wrapper(&s, "hydra_exe='/nonexistent'").success();
    assert_eq!(s.commit_count(), "1");
}

/// A stand-in hydra that exits with `code`.
fn fake_hydra(s: &Setup, code: i32) -> String {
    let f = s.tmp().join(format!("fake-hydra-{code}"));
    std::fs::write(&f, format!("#!/bin/sh\nexit {code}\n")).unwrap();
    format!("hydra_exe='{}'", f.to_string_lossy().replace('\\', "/"))
}

#[test]
fn only_the_block_code_stops_the_commit() {
    for code in [2, 101] {
        let s = Setup::new();
        commit_through_wrapper(&s, &fake_hydra(&s, code)).success();
        assert_eq!(s.commit_count(), "1", "exit {code} should allow");
    }
    let s = Setup::new();
    commit_through_wrapper(&s, &fake_hydra(&s, 1)).failure();
    assert_eq!(s.commit_count(), "0");
}

#[test]
fn main_repo_hooks_run_in_a_linked_worktree() {
    let s = Setup::new();
    s.run_git("work", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .success();
    let wt = s.tmp().join("wt");
    s.git()
        .arg("-C")
        .arg(&s.repo)
        .args(["worktree", "add"])
        .arg(&wt)
        .assert()
        .success();
    s.write_repo_hook("pre-commit", "echo ran > hook-ran\nexit 0");
    s.h.hydra()
        .args(["run", "work", "--", "git", "-C"])
        .arg(&wt)
        .args(["commit", "--allow-empty", "-m", "y"])
        .assert()
        .success();
    assert!(wt.join("hook-ran").is_file());
}

#[test]
fn the_users_own_global_config_is_included_and_carried() {
    let s = Setup::new();
    let global_hooks = s.tmp().join("global-hooks");
    std::fs::create_dir_all(&global_hooks).unwrap();
    std::fs::write(
        global_hooks.join("pre-commit"),
        "#!/bin/sh\necho ran > global-ran\nexit 0\n",
    )
    .unwrap();
    let mine = s.tmp().join("dev").join(".gitconfig");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(
        &mine,
        format!(
            "[alias]\n\thy = status\n[core]\n\thooksPath = {}\n",
            global_hooks.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();

    s.run_git("work", &["config", "--get", "alias.hy"])
        .env("GIT_CONFIG_GLOBAL", &mine)
        .assert()
        .success()
        .stdout("status\n");
    // A nested launch still finds it, though its GIT_CONFIG_GLOBAL is hydra's own file.
    s.h.hydra()
        .env("GIT_CONFIG_GLOBAL", &mine)
        .args([
            "run",
            "work",
            "--",
            env!("CARGO_BIN_EXE_hydra"),
            "run",
            "personal",
        ])
        .args(["--", "git", "-C"])
        .arg(&s.repo)
        .args(["config", "--get", "alias.hy"])
        .assert()
        .success()
        .stdout("status\n");
    // The user's global hooks folder is what the wrappers chain to.
    s.run_git("work", &["commit", "--allow-empty", "-m", "x"])
        .env("GIT_CONFIG_GLOBAL", &mine)
        .assert()
        .success();
    assert!(s.repo.join("global-ran").is_file());
}
