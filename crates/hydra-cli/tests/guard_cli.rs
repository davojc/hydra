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
fn allow_runs_once_with_override() {
    let s = Setup::new();
    s.h.hydra()
        .args(["run", "personal", "--", env!("CARGO_BIN_EXE_hydra")])
        .args(["allow", "--", "git", "-C"])
        .arg(&s.repo)
        .args(["commit", "--allow-empty", "-m", "x"])
        .assert()
        .success();
    assert_eq!(s.commit_count(), "1");
    // Only that one command: the next one is guarded again.
    s.run_git("personal", &["commit", "--allow-empty", "-m", "y"])
        .assert()
        .failure();
    assert_eq!(s.commit_count(), "1");
}

/// `<tmp>/shims/{gh,ssh}.exe` (copies of hydra) and `<tmp>/bin` with fake gh.cmd and ssh.cmd.
fn shims(s: &Setup) -> PathBuf {
    let shims = s.tmp().join("shims");
    std::fs::create_dir_all(&shims).unwrap();
    for name in ["gh.exe", "ssh.exe"] {
        std::fs::copy(env!("CARGO_BIN_EXE_hydra"), shims.join(name)).unwrap();
    }
    let bin = s.tmp().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("gh.cmd"), "@echo REAL GH %*\r\n@exit /b 7\r\n").unwrap();
    std::fs::write(bin.join("ssh.cmd"), "@echo %*\r\n").unwrap();
    shims
}

/// A shim run from the repo bound to `work`, with PATH = shims;bin;<system PATH>.
fn run_shim(s: &Setup, exe: &str, env: &str, args: &[&str]) -> assert_cmd::assert::Assert {
    let shims = shims(s);
    let mut path = vec![shims.clone(), s.tmp().join("bin")];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    s.h.command(shims.join(exe))
        .args(args)
        .current_dir(&s.repo)
        .env("HYDRA_ENV", env)
        .env("PATH", std::env::join_paths(path).unwrap())
        .assert()
}

#[test]
fn gh_shim_blocks_wrong_environment() {
    let s = Setup::new();
    run_shim(&s, "gh.exe", "personal", &["pr", "create", "--fill"])
        .code(1)
        .stderr(predicate::str::contains("blocked gh pr create"))
        .stdout(predicate::str::contains("REAL GH").not());
}

#[test]
fn gh_shim_passes_reads_and_right_env() {
    let s = Setup::new();
    run_shim(&s, "gh.exe", "personal", &["pr", "list"])
        .code(7)
        .stdout(predicate::str::contains("REAL GH pr list"));
    run_shim(&s, "gh.exe", "work", &["pr", "create", "--fill"])
        .code(7)
        .stdout(predicate::str::contains("REAL GH pr create --fill"));
}

#[test]
fn ssh_shim_adds_the_key() {
    let s = Setup::new();
    let key = s.tmp().join("id_work");
    std::fs::write(&key, "key").unwrap();
    s.h.write_env(
        "work",
        &format!(
            "[git]\nname = \"Work\"\nemail = \"work@example.com\"\nssh_key = '{}'\n",
            key.display()
        ),
    );
    let out = run_shim(&s, "ssh.exe", "work", &["-T", "git@github.com"]).success();
    // Rust quotes `IdentitiesOnly=yes` for the batch-file stand-in; a real ssh.exe gets the
    // same argv either way.
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).replace('"', "");
    assert!(
        stdout.contains(&format!(
            "-i {} -o IdentitiesOnly=yes -T git@github.com",
            key.display()
        )),
        "{stdout:?}"
    );
}

#[test]
fn launch_installs_shims() {
    let s = Setup::new();
    s.h.hydra()
        .args(["run", "work", "--", "cmd", "/c", "exit 0"])
        .assert()
        .success();
    let shims = s.h.root().join("shims");
    assert!(shims.join("gh.exe").is_file());
    assert!(shims.join("ssh.exe").is_file());
    if gh_installed() {
        let out =
            s.h.hydra()
                .args(["run", "work", "--", "pwsh", "-NoProfile", "-Command"])
                .arg("(Get-Command gh -ErrorAction SilentlyContinue).Source")
                .output()
                .unwrap();
        let source = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let parent = PathBuf::from(&source).parent().map(|p| p.to_path_buf());
        assert_eq!(
            parent.map(|p| p.to_string_lossy().to_lowercase()),
            Some(shims.to_string_lossy().to_lowercase()),
            "{source:?}"
        );
    }
}

#[test]
fn gh_shim_warns_about_a_repo_outside_the_owners() {
    let s = Setup::new();
    s.h.write_env(
        "work",
        "[git]
name = \"Work\"
email = \"work@example.com\"
[github]
owners = [\"acme\"]
",
    );
    run_shim(&s, "gh.exe", "work", &["pr", "create", "-R", "other/x"])
        .code(7)
        .stderr(predicate::str::contains(
            "hydra: warning: other isn't in work's github owners [acme]",
        ))
        .stdout(predicate::str::contains("REAL GH pr create -R other/x"));
}

/// `<root>/shims/<name>` as `len` zero bytes, last modified at `mtime`.
fn fake_shim(s: &Setup, name: &str, len: u64, mtime: std::time::SystemTime) -> PathBuf {
    let dir = s.h.root().join("shims");
    std::fs::create_dir_all(&dir).unwrap();
    let shim = dir.join(name);
    let f = std::fs::File::create(&shim).unwrap();
    f.set_len(len).unwrap();
    f.set_modified(mtime).unwrap();
    shim
}

fn hydra_exe() -> (Vec<u8>, std::time::SystemTime) {
    let exe = env!("CARGO_BIN_EXE_hydra");
    let modified = std::fs::metadata(exe).unwrap().modified().unwrap();
    (std::fs::read(exe).unwrap(), modified)
}

#[test]
fn launch_refreshes_a_same_size_shim_older_than_hydra() {
    let s = Setup::new();
    let (bytes, modified) = hydra_exe();
    let len = bytes.len() as u64;
    let hour = std::time::Duration::from_secs(3600);
    let old = fake_shim(&s, "gh.exe", len, modified - hour);
    let newer = fake_shim(&s, "ssh.exe", len, modified + hour);
    s.h.hydra()
        .args(["run", "work", "--", "cmd", "/c", "exit 0"])
        .assert()
        .success();
    assert!(
        std::fs::read(&old).unwrap() == bytes,
        "older shim not refreshed"
    );
    // The copy keeps hydra's time, so the next launch sees it as current.
    assert!(std::fs::metadata(&old).unwrap().modified().unwrap() >= modified);
    assert!(
        std::fs::read(&newer).unwrap().iter().all(|b| *b == 0),
        "a same-size shim newer than hydra is left alone"
    );
}

#[test]
#[cfg(windows)]
fn launch_keeps_a_locked_stale_shim_and_notes_it() {
    use std::os::windows::fs::OpenOptionsExt;
    let s = Setup::new();
    let shim = fake_shim(&s, "gh.exe", 10, std::time::SystemTime::now());
    // No sharing at all: nothing can replace the file while this handle is open.
    let _lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&shim)
        .unwrap();
    s.h.hydra()
        .args(["run", "work", "--", "cmd", "/c", "exit 0"])
        .assert()
        .success()
        .stderr(predicate::str::contains("gh/ssh shims not refreshed"));
    let dir = s.h.root().join("shims");
    assert_eq!(std::fs::metadata(&shim).unwrap().len(), 10);
    assert!(!dir.join("gh.exe.hydra-tmp").exists());
    assert!(dir.join("ssh.exe").is_file());
}

/// Whether a real gh is on the machine's PATH.
fn gh_installed() -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join("gh.exe").is_file())
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

#[test]
fn a_binding_to_an_unknown_environment_allows_with_warning() {
    for (env, shown) in [("ghost", "ghost"), ("", "\"\"")] {
        let s = Setup::new();
        // At the repo root: git runs hooks there, and the file beats the `work` rule.
        let dir = s.repo.clone();
        std::fs::write(dir.join(".hydra"), format!("env = \"{env}\"\n")).unwrap();
        let skipped = "hydra: warning: guard skipped (.hydra in ";
        let warning = format!(" names unknown environment {shown})");
        // The git hook.
        s.h.hydra()
            .args(["run", "personal", "--", "git", "-C"])
            .arg(&dir)
            .args(["commit", "--allow-empty", "-m", "x"])
            .assert()
            .success()
            .stderr(predicate::str::contains(skipped))
            .stderr(predicate::str::contains(&warning));
        assert_eq!(s.commit_count(), "1");
        // The gh shim.
        run_shim(&s, "gh.exe", "personal", &["pr", "create", "--fill"])
            .code(7)
            .stderr(predicate::str::contains(skipped))
            .stderr(predicate::str::contains(&warning));
        // Claude's hook.
        claude_hook(&s, "personal", &bash_input("git commit -m x", &dir))
            .success()
            .stderr(predicate::str::contains(skipped))
            .stderr(predicate::str::contains(&warning));
    }
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

#[test]
fn an_environment_without_a_git_section_still_gets_its_gitconfig_and_guard() {
    let s = Setup::new();
    s.h.write_env("plain", "label = \"plain\"\n");
    // The (fake) user's global config: Home::hydra points GIT_CONFIG_GLOBAL at it.
    std::fs::write(
        s.tmp().join("user").join(".gitconfig"),
        "[user]\n\tname = Me\n\temail = me@example.com\n[alias]\n\thy = status\n",
    )
    .unwrap();

    // GIT_CONFIG_GLOBAL is plain's generated file: --global reads only that file.
    let hooks = s.state_git("plain").join("hooks");
    s.run_git("plain", &["config", "--global", "--get", "core.hooksPath"])
        .assert()
        .success()
        .stdout(format!("{}\n", hooks.to_string_lossy().replace('\\', "/")));
    // ... and it includes the user's global config.
    s.run_git("plain", &["config", "--get", "alias.hy"])
        .assert()
        .success()
        .stdout("status\n");
    s.run_git("plain", &["config", "--get", "user.email"])
        .assert()
        .success()
        .stdout("me@example.com\n");

    s.run_git("plain", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "blocked git commit - this folder belongs to work",
        ));
    assert_eq!(s.commit_count(), "0");
}

fn claude_hook(s: &Setup, env: &str, input: &str) -> assert_cmd::assert::Assert {
    let mut c = s.h.hydra();
    c.args(["guard", "claude"]).env("HYDRA_ENV", env);
    c.write_stdin(input.to_string()).assert()
}

fn bash_input(command: &str, cwd: &Path) -> String {
    serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}, "cwd": cwd})
        .to_string()
}

#[test]
fn claude_hook_blocks_wrong_environment() {
    let s = Setup::new();
    claude_hook(&s, "personal", &bash_input("git commit -m x", &s.repo))
        .code(2)
        .stderr(predicate::str::contains("blocked git commit"));
    claude_hook(&s, "work", &bash_input("git commit -m x", &s.repo)).success();
    claude_hook(&s, "personal", "not json")
        .success()
        .stderr(predicate::str::contains("guard skipped"));
}

#[test]
fn claude_hook_allows_other_commands() {
    let s = Setup::new();
    claude_hook(&s, "personal", &bash_input("ls -la", &s.repo)).success();
    let other = serde_json::json!({"tool_name": "Edit", "tool_input": {"command": "git commit"}, "cwd": s.repo}).to_string();
    claude_hook(&s, "personal", &other).success();
}

#[test]
fn claude_is_told_to_ask_the_user_and_cannot_allow_itself() {
    let s = Setup::new();
    claude_hook(
        &s,
        "personal",
        &bash_input("hydra allow -- git commit -m x", &s.repo),
    )
    .code(2)
    .stderr(predicate::str::contains("blocked git commit"))
    .stderr(predicate::str::contains(
        "\n  -> or ask the user to run it once in their terminal: hydra allow -- git commit",
    ))
    .stderr(predicate::str::contains("run it once anyway").not());
    claude_hook(
        &s,
        "personal",
        &bash_input("hydra.exe allow -- gh pr create --fill", &s.repo),
    )
    .code(2)
    .stderr(predicate::str::contains("blocked gh pr create"));
    // The git hook keeps the usual wording.
    s.run_git("personal", &["commit", "--allow-empty", "-m", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "-> or run it once anyway: hydra allow -- git commit",
        ));
}

/// `C:\a\b` as Git Bash writes it: `/c/a/b`.
fn msys(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let (drive, rest) = s.split_once(':').unwrap();
    format!("/{}{rest}", drive.to_lowercase())
}

#[test]
fn claude_hook_follows_shell_paths_and_cds() {
    let s = Setup::new();
    let side = s.tmp().join("side");
    std::fs::create_dir_all(&side).unwrap();
    std::fs::create_dir_all(s.repo.join("sub")).unwrap();
    let repo = msys(&s.repo);
    for (command, cwd) in [
        (format!("cd {repo} && git commit --no-verify -m x"), &side),
        (format!("git -C {repo} push"), &side),
        // `..` is relative to `sub`, not to the starting folder (whose parent is unbound).
        ("cd sub && git -C .. commit -m x".to_string(), &s.repo),
    ] {
        claude_hook(&s, "personal", &bash_input(&command, cwd))
            .code(2)
            .stderr(predicate::str::contains("blocked git"));
    }
    s.h.hydra()
        .args(["guard", "claude"])
        .env("HYDRA_ENV", "personal")
        .env("HYDRA_USER_HOME", s.tmp())
        .write_stdin(bash_input("cd ~/repo && git commit -m x", &side))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("blocked git commit"));
    // Still allowed where nothing is bound.
    claude_hook(&s, "personal", &bash_input("git -C .. commit -m x", &side)).success();
}

#[test]
fn allow_reaches_only_its_own_command() {
    let s = Setup::new();
    let echo = ["cmd", "/c", "echo [%HYDRA_ALLOW%]"];
    // `hydra allow -- <cmd>` hands the override to its child ...
    s.h.hydra()
        .args([
            "run",
            "personal",
            "--",
            env!("CARGO_BIN_EXE_hydra"),
            "allow",
            "--",
        ])
        .args(echo)
        .assert()
        .success()
        .stdout(predicate::str::contains("[1]"));
    // ... but a launch from inside it (e.g. `hydra allow -- pwsh`, then hydra run) drops it.
    s.h.hydra()
        .env("HYDRA_ALLOW", "1")
        .args(["run", "personal", "--"])
        .args(echo)
        .assert()
        .success()
        .stdout(predicate::str::contains("[%HYDRA_ALLOW%]"));
}

#[test]
fn claude_hook_checks_powershell_commands_too() {
    let s = Setup::new();
    let input = |command: &str| {
        serde_json::json!({"tool_name": "PowerShell", "tool_input": {"command": command}, "cwd": s.tmp()})
            .to_string()
    };
    let repo = s.repo.to_string_lossy();
    claude_hook(&s, "personal", &input(&format!("cd '{repo}'; git push")))
        .code(2)
        .stderr(predicate::str::contains("blocked git push"));
    claude_hook(&s, "personal", &input("Get-ChildItem")).success();
}
