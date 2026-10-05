# Borrowed Logins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an environment borrow a tool's login (its whole state folder) from another environment, so one sign-in serves several environments.

**Architecture:** `[tool] from = "<owner>"` in env.toml is resolved at load time. The owner's section replaces the borrower's, and `EnvConfig.borrowed` records `tool -> owner`. Every provider already gets its folder through `Ctx::provider_dir`, which now maps to the owner's state folder. The CLI layers on top: owner-aware sign-in notes, a whoami tag, `add --from`, and `remove` / `env rm` / `env rename` that respect borrowers.

**Tech Stack:** Rust 2024 workspace (hydra-core, hydra-providers, hydra-cli), toml + toml_edit, clap, assert_cmd tests.

**Spec:** `docs/superpowers/specs/2026-10-05-hydra-borrowed-logins-design.md` (extends `docs/superpowers/specs/2026-10-03-hydra-design.md`).

## Global Constraints

- **Borrowable:** claude, github, aws, azure, gcloud, gws, kube, codex and gemini. `git` is never borrowable.
- **A borrowed section has only `from`.** No chains, no self-borrow. The owner must have the section without `from`.
- **Invalid borrowing is a config error, and launch fails closed** (spec §8). Error lines are user-facing; fixes go on `  -> ` lines, which `print_error` dims.
- **Exact messages** (`<b>` = borrower, `<o>` = owner, `<t>` = tool, `<n>` = next owner in a chain):
  - `<b> borrows [<t>] from <o>, but <o> has no [<t>]` / `  -> add it there: hydra add <t> <o>`
  - `<b> borrows [<t>] from <o>, which borrows it from <n>` / `  -> borrow from the owner: from = "<n>"`
  - `<b> borrows [<t>] from <o>, but there's no environment <o>` / `  -> fix from in [<t>] in <b>'s env.toml`
  - `<b> can't borrow [<t>] from itself` / `  -> remove from = "<b>"`
  - `[<t>] borrows from <o>, so it can't also set <key>` / `  -> remove <key>; <o>'s [<t>] settings are used`
  - `[git] can't be borrowed: commit author and SSH key stay per environment` / `  -> set [git] in <b> itself`
  - **Sign-in note:** `hydra: <t>: not signed in - <b> borrows it from <o>` / `  -> sign in there: hydra shell <o>, then <login command>`
  - **whoami detail:** `from <o> · <normal detail>`
- **Guards are unchanged:** HYDRA_ENV, bindings and the guard still use the borrower's name.
- **Tests** must use the existing fences (`common::Home::hydra()`, `HYDRA_USER_HOME` fake user). Never touch the real `~/.claude`, `.claude.json`, `C:\Users\davoc\.git` or the global git config.
- **Commits:**
  - Use the repo identity (davojc). No `--no-verify`. No employer names.
  - Every commit message ends with:
    ```
    Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
    Claude-Session: https://claude.ai/code/session_01RpaHxsXvDcvbn6U57wncBU
    ```
- **Before every commit, these must pass:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - the task's tests

## Review Focus

1. **Owner env.toml with a BOM or CRLF, or a borrow written as an inline table** (`claude = { from = "personal" }`): resolution works the same. Pinned in Task 1.
2. **Borrowing `[github]` in an env that has its own `[git]`:** git's gh credential helper must use the owner's `GH_CONFIG_DIR`, while the git author stays the borrower's. Pinned in Task 3.
3. **`hydra add claude work --from personal` when personal has no `[claude]`:** refused, and work's env.toml is unchanged. Pinned in Task 4.
4. **Renaming or removing the owner while a borrower terminal is open:** refused, so the open terminal's folder can't vanish under it. Pinned in Task 5.
5. **Claude prepared by a borrower launch:** the generated CLAUDE.md, settings and backups name the owner, so owner and borrower launches don't keep rewriting each other's files. Pinned in Task 2.

---

### Task 1: Borrow resolution when loading env.toml

**Files:**
- Create: `crates/hydra-core/src/borrow.rs`
- Modify: `crates/hydra-core/src/lib.rs` (add `pub mod borrow;`)
- Modify: `crates/hydra-core/src/config.rs` (`ConfigError::Borrow`, `EnvConfig.borrowed`, `parse_env`, `load_env`)
- Test: unit tests in `borrow.rs` and `config.rs`

**Interfaces:**
- Produces:
  - `hydra_core::borrow::NOT_BORROWABLE: &[&str]`
  - `hydra_core::borrow::resolve(paths: &HydraPaths, name: &EnvName, table: &mut toml::Table) -> Result<BTreeMap<String, EnvName>, String>`
  - `hydra_core::config::ConfigError::Borrow(String)`
  - `EnvConfig.borrowed: BTreeMap<String, EnvName>` (`#[serde(skip)]`; key = tool id as in `envedit::TOOLS`)
  - `hydra_core::config::parse_env(paths: &HydraPaths, name: &EnvName, text: &str) -> Result<EnvConfig, ConfigError>`

- [ ] **Step 1: Write the failing tests.** Add at the bottom of `crates/hydra-core/src/config.rs`'s `mod tests`; the existing `setup` helper writes `work`:

```rust
    fn add_env(paths: &HydraPaths, name: &str, toml: &str) {
        let n = EnvName::parse(name).unwrap();
        std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
        std::fs::write(paths.env_file(&n), toml).unwrap();
    }

    fn borrow_err(work: &str, personal: Option<&str>) -> String {
        let (_d, paths, name) = setup(work);
        if let Some(p) = personal {
            add_env(&paths, "personal", p);
        }
        load_env(&paths, &name).unwrap_err().to_string()
    }

    #[test]
    fn borrowed_section_takes_the_owners_settings() {
        let (_d, paths, name) =
            setup("[claude]\nfrom = \"personal\"\n\n[github]\nfrom = \"personal\"\n");
        add_env(
            &paths,
            "personal",
            "[claude]\nmcp.exclude = [\"x\"]\n\n[github]\nowners = [\"me\"]\n",
        );
        let c = load_env(&paths, &name).unwrap();
        assert_eq!(c.claude.unwrap().mcp.exclude, vec!["x"]);
        assert_eq!(c.github.unwrap().owners, vec!["me"]);
        assert_eq!(c.borrowed["claude"].as_str(), "personal");
        assert_eq!(c.borrowed["github"].as_str(), "personal");
        assert!(!c.borrowed.contains_key("git"));
    }

    #[test]
    fn inline_borrow_and_bom_crlf_owner_work() {
        let (_d, paths, name) = setup("claude = { from = \"personal\" }\r\n");
        add_env(&paths, "personal", "\u{feff}[claude]\r\n");
        let c = load_env(&paths, &name).unwrap();
        assert!(c.claude.is_some());
        assert_eq!(c.borrowed["claude"].as_str(), "personal");
    }

    #[test]
    fn owner_without_the_section_is_an_error() {
        let e = borrow_err("[claude]\nfrom = \"personal\"\n", Some("label = \"p\"\n"));
        assert_eq!(
            e,
            "work borrows [claude] from personal, but personal has no [claude]\n  -> add it there: hydra add claude personal"
        );
    }

    #[test]
    fn chains_are_an_error() {
        let e = borrow_err("[claude]\nfrom = \"personal\"\n", Some("[claude]\nfrom = \"client\"\n"));
        assert_eq!(
            e,
            "work borrows [claude] from personal, which borrows it from client\n  -> borrow from the owner: from = \"client\""
        );
    }

    #[test]
    fn unknown_owner_is_an_error() {
        let e = borrow_err("[codex]\nfrom = \"ghost\"\n", None);
        assert_eq!(
            e,
            "work borrows [codex] from ghost, but there's no environment ghost\n  -> fix from in [codex] in work's env.toml"
        );
    }

    #[test]
    fn self_borrow_is_an_error() {
        let e = borrow_err("[codex]\nfrom = \"work\"\n", None);
        assert_eq!(e, "work can't borrow [codex] from itself\n  -> remove from = \"work\"");
    }

    #[test]
    fn borrowed_section_with_other_keys_is_an_error() {
        let e = borrow_err(
            "[github]\nfrom = \"personal\"\nstrict = true\n",
            Some("[github]\n"),
        );
        assert_eq!(
            e,
            "[github] borrows from personal, so it can't also set strict\n  -> remove strict; personal's [github] settings are used"
        );
    }

    #[test]
    fn git_cannot_be_borrowed() {
        let e = borrow_err("[git]\nfrom = \"personal\"\n", Some("[git]\nname = \"x\"\n"));
        assert_eq!(
            e,
            "[git] can't be borrowed: commit author and SSH key stay per environment\n  -> set [git] in work itself"
        );
    }

    #[test]
    fn from_must_be_a_name() {
        let e = borrow_err("[codex]\nfrom = 3\n", None);
        assert!(e.contains("[codex] from must be an environment name"), "{e}");
    }

    #[test]
    fn parse_errors_keep_their_line_numbers() {
        let e = borrow_err("[claude]\nnope = 1\n", None);
        assert!(e.contains("line 2"), "{e}");
    }
```

- [ ] **Step 2: Run the tests to make sure they fail.**
Run: `cargo test -p hydra-core config::tests::borrow`
Expected: compile error (no `borrowed` field).

- [ ] **Step 3: Create `crates/hydra-core/src/borrow.rs`.**

```rust
//! Borrowing a tool's login from another environment: `[claude]` `from = "personal"`.
use std::collections::BTreeMap;

use crate::envedit::TOOLS;
use crate::name::EnvName;
use crate::paths::HydraPaths;

/// Tools that can't be borrowed: commit author and keys stay per environment.
pub const NOT_BORROWABLE: &[&str] = &["git"];

/// Replaces each `[tool] from = "<owner>"` section in `table` with the owner's
/// section and returns which tools are borrowed from whom. Errors are
/// user-facing, with `  -> ` fix lines.
pub fn resolve(
    paths: &HydraPaths,
    name: &EnvName,
    table: &mut toml::Table,
) -> Result<BTreeMap<String, EnvName>, String> {
    let mut borrowed = BTreeMap::new();
    for tool in TOOLS {
        let Some(toml::Value::Table(section)) = table.get(*tool) else {
            continue;
        };
        let Some(from) = section.get("from") else {
            continue;
        };
        if NOT_BORROWABLE.contains(tool) {
            return Err(format!(
                "[{tool}] can't be borrowed: commit author and SSH key stay per environment\n  -> set [{tool}] in {name} itself"
            ));
        }
        let owner = owner_name(name, tool, from)?;
        if let Some(extra) = section.keys().find(|k| *k != "from") {
            return Err(format!(
                "[{tool}] borrows from {owner}, so it can't also set {extra}\n  -> remove {extra}; {owner}'s [{tool}] settings are used"
            ));
        }
        let owned = owner_section(paths, name, tool, &owner)?;
        table.insert(tool.to_string(), toml::Value::Table(owned));
        borrowed.insert(tool.to_string(), owner);
    }
    Ok(borrowed)
}

fn owner_name(name: &EnvName, tool: &str, from: &toml::Value) -> Result<EnvName, String> {
    let s = from.as_str().ok_or_else(|| {
        format!("[{tool}] from must be an environment name, like from = \"personal\"")
    })?;
    let owner = EnvName::parse(s).map_err(|e| format!("[{tool}] from: {e}"))?;
    if &owner == name {
        return Err(format!(
            "{name} can't borrow [{tool}] from itself\n  -> remove from = \"{name}\""
        ));
    }
    Ok(owner)
}

/// The owner's own `[tool]` table, read straight from its env.toml.
fn owner_section(
    paths: &HydraPaths,
    name: &EnvName,
    tool: &str,
    owner: &EnvName,
) -> Result<toml::Table, String> {
    let path = paths.env_file(owner);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{name} borrows [{tool}] from {owner}, but there's no environment {owner}\n  -> fix from in [{tool}] in {name}'s env.toml"
            ));
        }
        Err(e) => return Err(format!("can't read {}: {e}", path.display())),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut doc: toml::Table =
        toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;
    match doc.remove(tool) {
        Some(toml::Value::Table(t)) => match t.get("from") {
            Some(next) => {
                let next = next.as_str().unwrap_or("?");
                Err(format!(
                    "{name} borrows [{tool}] from {owner}, which borrows it from {next}\n  -> borrow from the owner: from = \"{next}\""
                ))
            }
            None => Ok(t),
        },
        _ => Err(format!(
            "{name} borrows [{tool}] from {owner}, but {owner} has no [{tool}]\n  -> add it there: hydra add {tool} {owner}"
        )),
    }
}
```

Add `pub mod borrow;` to `crates/hydra-core/src/lib.rs`, in alphabetical order with the other modules.

- [ ] **Step 4: Wire it into `config.rs`.**

Add a variant to `ConfigError`:

```rust
    #[error("{0}")]
    Borrow(String),
```

Add the last field of `EnvConfig`:

```rust
    /// Tools borrowed from another environment (tool id -> owner). Filled by `parse_env`.
    #[serde(skip)]
    pub borrowed: BTreeMap<String, EnvName>,
```

Replace `load_env` and add `parse_env`:

```rust
/// Parses env.toml text for `name`: borrowed sections are filled in from their owners,
/// then the result is validated.
pub fn parse_env(paths: &HydraPaths, name: &EnvName, text: &str) -> Result<EnvConfig, ConfigError> {
    let path = paths.env_file(name);
    let invalid = |message: String| ConfigError::Invalid {
        path: path.clone(),
        message,
    };
    let mut table: toml::Table = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let borrowed = crate::borrow::resolve(paths, name, &mut table).map_err(ConfigError::Borrow)?;
    // Without borrowing, parse the text itself so errors keep their line numbers.
    let parsed: Result<EnvConfig, toml::de::Error> = if borrowed.is_empty() {
        toml::from_str(text)
    } else {
        table.try_into()
    };
    let mut cfg = parsed.map_err(|e| invalid(e.to_string()))?;
    cfg.borrowed = borrowed;
    cfg.validate().map_err(invalid)?;
    Ok(cfg)
}

pub fn load_env(paths: &HydraPaths, name: &EnvName) -> Result<EnvConfig, ConfigError> {
    let Some(text) = read_toml(&paths.env_file(name))? else {
        return Err(ConfigError::UnknownEnv(name.clone()));
    };
    parse_env(paths, name, &text)
}
```

`EnvName` must be `Clone + PartialEq + Debug` for the derive. It already is (it is used in `ConfigError` and compared in envs.rs); check before relying on it.

- [ ] **Step 5: Use `parse_env` in the CLI's pre-write check.** In `crates/hydra-cli/src/commands/tools.rs`, replace the body of `check`:

```rust
/// Parses and validates the edited text (borrowed sections included); nothing is written if it fails.
fn check(app: &App, env: &EnvName, text: &str) -> anyhow::Result<EnvConfig> {
    Ok(hydra_core::config::parse_env(&app.paths, env, text)?)
}
```

- [ ] **Step 6: Run the tests.**
Run: `cargo test --workspace`
Expected: all pass, including the 10 new tests.

- [ ] **Step 7: Commit.**

```bash
git add crates/hydra-core crates/hydra-cli/src/commands/tools.rs
git commit -m "feat: [tool] from = \"<env>\" borrows another environment's section"
```

---

### Task 2: Providers use the owner's folder

**Files:**
- Modify: `crates/hydra-core/src/provider.rs` (`Ctx::tool_owner`, `Ctx::provider_dir`)
- Modify: `crates/hydra-providers/src/claude.rs` (`layout`, `materialise`)
- Test: unit tests in `provider.rs` and `claude.rs`

**Interfaces:**
- Consumes: `EnvConfig.borrowed` (Task 1).
- Produces: `Ctx::tool_owner(&self, tool: &str) -> &EnvName`. `Ctx::provider_dir(sub)` now returns `state/<owner>/<sub>` for a borrowed tool (`gh` maps to tool `github`).

- [ ] **Step 1: Write the failing tests.** Add a `#[cfg(test)] mod tests` at the end of `crates/hydra-core/src/provider.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::MemoryStore;

    #[test]
    fn borrowed_tools_use_the_owners_state_folder() {
        let paths = HydraPaths::new("C:/h");
        let work = EnvName::parse("work").unwrap();
        let mut env = EnvConfig::default();
        env.borrowed
            .insert("github".into(), EnvName::parse("personal").unwrap());
        env.borrowed
            .insert("claude".into(), EnvName::parse("personal").unwrap());
        let store = MemoryStore::default();
        let ctx = Ctx {
            name: &work,
            env: &env,
            paths: &paths,
            user_home: Path::new("C:/u"),
            secrets: &store,
        };
        let state = |e: &str| paths.state_dir(&EnvName::parse(e).unwrap());
        assert_eq!(ctx.provider_dir("gh"), state("personal").join("gh"));
        assert_eq!(ctx.provider_dir("claude"), state("personal").join("claude"));
        assert_eq!(ctx.provider_dir("git"), state("work").join("git"));
        assert_eq!(ctx.provider_dir("codex"), state("work").join("codex"));
        assert_eq!(ctx.tool_owner("claude").as_str(), "personal");
        assert_eq!(ctx.tool_owner("aws").as_str(), "work");
        assert_eq!(ctx.state_dir(), state("work"));
    }
}
```

`MemoryStore` is used by hydra-providers' `testutil`. If it is behind a feature or `cfg(test)` in hydra-core, use whatever that testutil imports (`hydra_core::secret::MemoryStore`).

In `crates/hydra-providers/src/claude.rs` tests, add this next to `materialise_shares_the_base_and_keeps_the_sign_in_private`. It uses that module's existing `Fixture`, `with_base` and `EnvName` imports; add `use hydra_core::name::EnvName;` to the test module if it isn't imported.

```rust
    #[test]
    fn borrowed_claude_prepares_the_owners_folder_as_the_owner() {
        let mut f = Fixture::new("[claude]
");
        with_base(&f);
        let personal = EnvName::parse("personal").unwrap();
        f.env.borrowed.insert("claude".into(), personal.clone());
        Claude.materialise(&f.ctx()).unwrap();
        let dir = f.paths.state_dir(&personal).join("claude");
        assert!(dir.join("settings.json").is_file());
        assert!(!f.paths.state_dir(&f.name).join("claude").exists());
        // Generated files name the owner, so owner and borrower launches write the same thing.
        if let Ok(md) = std::fs::read_to_string(dir.join("CLAUDE.md")) {
            assert!(!md.contains("work"), "{md}");
        }
    }
```

- [ ] **Step 2: Run the tests to make sure they fail.**
Run: `cargo test -p hydra-core provider::tests && cargo test -p hydra-providers borrowed_claude`
Expected: FAIL (`tool_owner` not found).

- [ ] **Step 3: Implement it in `provider.rs`.** Replace the `impl Ctx<'_>` block:

```rust
impl Ctx<'_> {
    pub fn state_dir(&self) -> PathBuf {
        self.paths.state_dir(self.name)
    }
    /// The environment whose saved logins `tool` uses: its owner when borrowed, else this one.
    pub fn tool_owner(&self, tool: &str) -> &EnvName {
        self.env.borrowed.get(tool).unwrap_or(self.name)
    }
    /// The tool's state folder; a borrowed tool's lives in its owner's state.
    pub fn provider_dir(&self, sub: &str) -> PathBuf {
        let tool = if sub == "gh" { "github" } else { sub };
        self.paths.state_dir(self.tool_owner(tool)).join(sub)
    }
}
```

- [ ] **Step 4: Make Claude prepare the folder as its owner.** In `crates/hydra-providers/src/claude.rs`:
  - In `layout`: `over: ctx.paths.env_dir(ctx.tool_owner("claude")).join("claude"),`
  - In `materialise`: `refuse_linked(&ctx.paths.state_dir(ctx.tool_owner("claude")))?;` replaces `refuse_linked(&ctx.state_dir())?;`
  - In `materialise`: `let env = ctx.tool_owner("claude").as_str();` replaces `let env = ctx.name.as_str();`

Leave the guard hook command alone. It reads `HYDRA_ENV` when it runs, so it still guards as the borrower.

- [ ] **Step 5: Run the tests.**
Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 6: Commit.**

```bash
git add crates/hydra-core/src/provider.rs crates/hydra-providers/src/claude.rs
git commit -m "feat: borrowed tools use their owner's state folder"
```

---

### Task 3: Sign-in note and whoami name the owner

**Files:**
- Modify: `crates/hydra-cli/src/commands/launch.rs` (`print_sign_in_hints`)
- Modify: `crates/hydra-cli/src/commands/whoami.rs` (row detail)
- Create: `crates/hydra-cli/tests/borrow_cli.rs`

**Interfaces:**
- Consumes: `EnvConfig.borrowed` (Task 1), `Ctx::provider_dir` (Task 2).
- Produces: user-visible lines only.

- [ ] **Step 1: Write the failing e2e tests** in `crates/hydra-cli/tests/borrow_cli.rs`:

```rust
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
    for (var, sub) in [("CLAUDE_CONFIG_DIR", "claude"), ("GH_CONFIG_DIR", "gh"), ("CODEX_HOME", "codex")] {
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
        h.root().join("state").join("personal").join("claude").join("settings.json"),
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
    hydra_as(&h, &user)
        .args(["run", "work", "--", "git", "config", "user.email"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work@example.com"));
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
        .stderr(predicate::str::contains("-> add it there: hydra add claude personal"));
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
```

`h.dir` must be public in `common::Home`; claude_cli.rs already uses `h.dir.path()`.

- [ ] **Step 2: Run the tests to make sure they fail.**
Run: `cargo test -p hydra-cli --test borrow_cli`
Expected:
- `not_signed_in_points_at_the_owner` and `whoami_tags_borrowed_tools` FAIL.
- The folder tests may already pass after Tasks 1–2. That's fine; they pin the behaviour.

- [ ] **Step 3: Implement the sign-in note.** In `launch.rs`, replace the loop body in `print_sign_in_hints`:

```rust
    for p in hydra_providers::all() {
        if !p.is_configured(&launch.config) {
            continue;
        }
        let Some(hint) = p.sign_in_hint(&ctx) else {
            continue;
        };
        match launch.config.borrowed.get(p.id()) {
            Some(owner) => {
                let fix = match hint.strip_prefix("not signed in - run ") {
                    Some(cmd) => {
                        anstream::eprintln!(
                            "{}",
                            style::warn(format!(
                                "hydra: {}: not signed in - {name} borrows it from {owner}",
                                p.id()
                            ))
                        );
                        format!("  -> sign in there: hydra shell {owner}, then {cmd}")
                    }
                    None => {
                        anstream::eprintln!(
                            "{}",
                            style::warn(format!(
                                "hydra: {}: {hint} - {name} borrows it from {owner}",
                                p.id()
                            ))
                        );
                        format!("  -> fix it there: hydra shell {owner}")
                    }
                };
                anstream::eprintln!("{}", style::dim(fix));
            }
            None => {
                anstream::eprintln!("{}", style::warn(format!("hydra: {}: {hint}", p.id())));
            }
        }
    }
```

- [ ] **Step 4: Implement the whoami tag.** In `whoami.rs`, change the `rows` construction:

```rust
    let mut rows: Vec<IdentityReport> = hydra_providers::all()
        .iter()
        .filter(|p| p.is_configured(&launch.config))
        .map(|p| {
            let mut r = p.check(&ctx, &runner);
            if let Some(owner) = launch.config.borrowed.get(p.id()) {
                r.detail = format!("from {owner} · {}", r.detail);
            }
            r
        })
        .collect();
```

- [ ] **Step 5: Run the tests.**
Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 6: Commit.**

```bash
git add crates/hydra-cli
git commit -m "feat: sign-in notes and whoami name the environment a tool is borrowed from"
```

---

### Task 4: `hydra add --from` and `hydra remove`

**Files:**
- Modify: `crates/hydra-core/src/envedit.rs` (`ToolValues.from`, `add_tool`, `check_applies`)
- Create (in `crates/hydra-core/src/borrow.rs`): `borrowers`
- Modify: `crates/hydra-cli/src/cli.rs` (`Add { from }`)
- Modify: `crates/hydra-cli/src/commands/mod.rs` (pass `from`)
- Modify: `crates/hydra-cli/src/commands/tools.rs` (`Flags.from`, `add`, `remove`)
- Test: `envedit.rs` unit tests, `borrow.rs` unit test, `crates/hydra-cli/tests/borrow_cli.rs`

**Interfaces:**
- Consumes: `parse_env` (Task 1).
- Produces:
  - `ToolValues.from: Option<String>`
  - `hydra_core::borrow::borrowers(paths: &HydraPaths, owner: &EnvName) -> Vec<(EnvName, Vec<String>)>`: each environment borrowing from `owner`, with the tool ids it borrows, sorted by env name. Unreadable or unparseable env files are skipped.

- [ ] **Step 1: Write the failing tests.**

In `envedit.rs` tests:

```rust
    #[test]
    fn add_from_writes_only_from_and_replaces_settings() {
        let v = ToolValues {
            from: Some("personal".into()),
            ..Default::default()
        };
        let (out, outcome) = add_tool("", "claude", &v).unwrap();
        assert_eq!(outcome, AddOutcome::Added);
        assert_eq!(out, "[claude]\nfrom = \"personal\"\n");
        let (out, outcome) =
            add_tool("[claude]\nmcp.exclude = [\"x\"]\n", "claude", &v).unwrap();
        assert_eq!(outcome, AddOutcome::Updated);
        assert_eq!(out, "[claude]\nfrom = \"personal\"\n");
        let (_, outcome) = add_tool(&out, "claude", &v).unwrap();
        assert_eq!(outcome, AddOutcome::AlreadyPresent);
    }

    #[test]
    fn add_from_refuses_git_and_other_flags() {
        let git = ToolValues {
            from: Some("personal".into()),
            ..Default::default()
        };
        assert_eq!(
            add_tool("", "git", &git).unwrap_err(),
            "git can't be borrowed: commit author and SSH key stay per environment"
        );
        let mixed = ToolValues {
            from: Some("personal".into()),
            owners: vec!["me".into()],
            ..Default::default()
        };
        assert_eq!(
            add_tool("", "github", &mixed).unwrap_err(),
            "--from can't be combined with other settings; the owner's are used"
        );
    }
```

In `borrow.rs`, add a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowers_lists_who_borrows_what() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(dir.path());
        for (n, t) in [
            ("personal", "[claude]\n[github]\n"),
            ("work", "[claude]\nfrom = \"personal\"\n[github]\nfrom = \"personal\"\n"),
            ("client", "codex = { from = \"personal\" }\n"),
            ("other", "[claude]\nfrom = \"work\"\n"),
            ("broken", "[[[ nope"),
        ] {
            let n = EnvName::parse(n).unwrap();
            std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
            std::fs::write(paths.env_file(&n), t).unwrap();
        }
        let got = borrowers(&paths, &EnvName::parse("personal").unwrap());
        let got: Vec<(String, Vec<String>)> =
            got.into_iter().map(|(e, t)| (e.to_string(), t)).collect();
        assert_eq!(
            got,
            vec![
                ("client".to_string(), vec!["codex".to_string()]),
                ("work".to_string(), vec!["claude".to_string(), "github".to_string()]),
            ]
        );
    }
}
```

In `borrow_cli.rs`:

```rust
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
    h.hydra().args(["add", "claude", "personal"]).assert().success();
    h.hydra()
        .args(["add", "claude", "work", "--from", "personal"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added claude to work (borrowed from personal)"));
    assert!(h.env_toml("work").contains("[claude]\nfrom = \"personal\"\n"));
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
        .stderr(predicate::str::contains("-> remove it there first: hydra remove claude work"));
    h.hydra()
        .args(["remove", "claude", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed claude from work; it was borrowed from personal, whose login is untouched",
        ));
    h.hydra().args(["remove", "claude", "personal"]).assert().success();
}
```

- [ ] **Step 2: Run the tests to make sure they fail.**
Run: `cargo test -p hydra-core envedit borrow && cargo test -p hydra-cli --test borrow_cli`
Expected: compile errors or FAIL.

- [ ] **Step 3: Implement `envedit`.**

Add `pub from: Option<String>, // any borrowable tool: from (owner env)` as the last field of `ToolValues`.

At the start of `check_applies`, before the existing loop:

```rust
    if v.from.is_some() {
        if crate::borrow::NOT_BORROWABLE.contains(&tool) {
            return Err(format!(
                "{tool} can't be borrowed: commit author and SSH key stay per environment"
            ));
        }
        let only_from = ToolValues {
            from: v.from.clone(),
            ..Default::default()
        };
        if *v != only_from {
            return Err(
                "--from can't be combined with other settings; the owner's are used".to_string(),
            );
        }
    }
```

In `add_tool`, right after `check_applies(tool, v)?; let mut doc = parse(text)?;`:

```rust
    if let Some(owner) = &v.from {
        let mut table = Table::new();
        table.insert("from", value(owner.as_str()));
        let existed = has_table(&doc, tool);
        if !existed && doc.contains_key(tool) {
            return Err(format!("{tool} in env.toml isn't a table"));
        }
        if existed {
            let old = doc.get(tool).and_then(|i| i.as_table()).map(|t| t.decor().clone());
            if let Some(d) = old {
                *table.decor_mut() = d;
            }
        } else if !text.is_empty() && !text.ends_with("\n\n") {
            let prefix = if text.ends_with('\n') { "\n" } else { "\n\n" };
            table.decor_mut().set_prefix(prefix);
        }
        doc.insert(tool, Item::Table(table));
        let out = doc.to_string();
        let outcome = match (existed, out == text) {
            (true, true) => AddOutcome::AlreadyPresent,
            (true, false) => AddOutcome::Updated,
            (false, _) => AddOutcome::Added,
        };
        return Ok((out, outcome));
    }
```

If the existing section is an inline table (`claude = {...}`), `as_table` is `None`, and the replacement becomes a normal `[claude]` table. That is fine.

- [ ] **Step 4: Implement `borrow::borrowers`.**

```rust
/// Every environment that borrows something from `owner`, with the tools, sorted by name.
/// Unreadable or broken env files are skipped (they fail on their own launch).
pub fn borrowers(paths: &HydraPaths, owner: &EnvName) -> Vec<(EnvName, Vec<String>)> {
    let Ok(list) = crate::envs::list(paths) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in list.valid {
        let Ok(text) = std::fs::read_to_string(paths.env_file(&n)) else {
            continue;
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let Ok(doc) = toml::from_str::<toml::Table>(text) else {
            continue;
        };
        let tools: Vec<String> = TOOLS
            .iter()
            .filter(|t| {
                doc.get(**t)
                    .and_then(|s| s.get("from"))
                    .and_then(|f| f.as_str())
                    == Some(owner.as_str())
            })
            .map(|t| t.to_string())
            .collect();
        if !tools.is_empty() {
            out.push((n, tools));
        }
    }
    out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    out
}
```

Check the actual signature of `crate::envs::list` (it is used as `list(paths)?.valid` inside envs.rs) and adapt the `let Ok(list)` line if the error type differs.

- [ ] **Step 5: Implement the CLI.**

In `cli.rs` `Add`, after `api_key_secret`:

```rust
        /// Borrow the tool's login and settings from another environment, e.g. --from personal
        #[arg(long)]
        from: Option<String>,
```

In `commands/mod.rs`, destructure `from` in `Cmd::Add { .. }` and pass `from` into `tools::Flags { .., from }`.

In `tools.rs`:
- Add `pub from: Option<String>,` to `Flags`.
- In `into_values`, add `from: self.from,`.
- In `add`, after `write_env_file(...)`, replace the `added` message line with:

```rust
    let borrowed = cfg.borrowed.get(tool);
    let tail = borrowed.map(|o| format!(" (borrowed from {o})")).unwrap_or_default();
    anstream::println!("{}", style::ok(format!("{verb} {tool} {prep} {env}{tail}")));
```

  When `borrowed` is `Some(owner)`, replace the `next:` line with `next: open a new {env} terminal (hydra shell {env}); sign in from {owner} if it isn't already`.

In `remove`, after `require_env` and before reading the file:

```rust
    let lent_to: Vec<String> = hydra_core::borrow::borrowers(&app.paths, &env)
        .into_iter()
        .filter(|(_, tools)| tools.iter().any(|t| t == tool))
        .map(|(n, _)| n.to_string())
        .collect();
    if let Some(first) = lent_to.first() {
        anyhow::bail!(
            "{env} lends {tool} to {}\n  -> remove it there first: hydra remove {tool} {first}",
            lent_to.join(", ")
        );
    }
```

  Also in `remove`: read the borrowed owner before removing. Call `check(app, &env, &text)` on the *old* text and remember `old_cfg.borrowed.get(tool).cloned()`. If the old config fails to parse, treat it as not borrowed. After writing, print `removed {tool} from {env}; it was borrowed from {owner}, whose login is untouched` for a borrowed tool, and keep the existing "saved logins stay in" message otherwise.

- [ ] **Step 6: Run the tests.**
Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 7: Commit.**

```bash
git add crates/hydra-core crates/hydra-cli
git commit -m "feat: hydra add <tool> <env> --from <owner>; remove respects borrowers"
```

---

### Task 5: `env rm` and `env rename` respect borrowers

**Files:**
- Modify: `crates/hydra-core/src/rewrite.rs` (`rename_borrow_owner`)
- Modify: `crates/hydra-core/src/envs.rs` (`EnvError::Lends`, `EnvError::BorrowerRunning`, `remove`, `rename`, `RenameReport.borrowers`)
- Modify: `crates/hydra-cli/src/commands/env.rs` (print updated borrowers)
- Test: `rewrite.rs` / `envs.rs` unit tests and `borrow_cli.rs`

**Interfaces:**
- Consumes: `borrow::borrowers` (Task 4).
- Produces:
  - `rewrite::rename_borrow_owner(text: &str, old: &str, new: &str) -> Result<(String, usize), TomlError>`
  - `RenameReport.borrowers: Vec<String>` (env names whose `from` changed)

- [ ] **Step 1: Write the failing tests.**

In `rewrite.rs` tests (create `#[cfg(test)] mod tests` if absent):

```rust
    #[test]
    fn rename_borrow_owner_rewrites_only_matching_from() {
        let text = "# keep\n[claude]\nfrom = \"personal\" # mine\n\n[github]\nfrom = \"other\"\n\ncodex = { from = \"personal\" }\n[env]\nfrom = \"personal\"\n";
        let (out, n) = rename_borrow_owner(text, "personal", "home").unwrap();
        assert_eq!(n, 2);
        assert!(out.contains("[claude]\nfrom = \"home\" # mine\n"), "{out}");
        assert!(out.contains("codex = { from = \"home\" }"), "{out}");
        assert!(out.contains("from = \"other\""), "{out}");
        assert!(out.contains("[env]\nfrom = \"personal\""), "{out}");
    }
```

In `borrow_cli.rs`:

```rust
#[test]
fn env_rm_refuses_an_owner_with_borrowers() {
    let h = Home::new();
    h.write_env("personal", "[claude]\n[github]\n");
    h.write_env("work", "[claude]\nfrom = \"personal\"\n[github]\nfrom = \"personal\"\n");
    h.hydra()
        .args(["env", "rm", "personal", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("work borrows claude, github from personal"));
    assert!(h.root().join("envs").join("personal").join("env.toml").is_file());
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

```

In `crates/hydra-core/src/envs.rs` tests, next to `remove_refuses_while_a_shell_is_open`, using that module's `home`, `create`, `n` helpers:

```rust
    #[test]
    fn owner_rename_refuses_while_a_borrower_is_open() {
        let (_d, paths) = home();
        create(&paths, &n("personal")).unwrap();
        create(&paths, &n("work")).unwrap();
        std::fs::write(paths.env_file(&n("personal")), "[codex]
").unwrap();
        std::fs::write(paths.env_file(&n("work")), "[codex]
from = \"personal\"
").unwrap();
        let _lock = lock::hold_shared(&paths, &n("work")).unwrap();
        let err = rename(&paths, &n("personal"), &n("home"), &MemoryStore::default()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "a hydra terminal for work (which borrows from personal) is still open; close it first"
        );
    }
```

(`env rm` of an owner is already refused whenever it has borrowers, open or not.)

- [ ] **Step 2: Run the tests to make sure they fail.**
Run: `cargo test -p hydra-core rewrite && cargo test -p hydra-cli --test borrow_cli`
Expected: FAIL.

- [ ] **Step 3: Implement `rename_borrow_owner` in `rewrite.rs`.**

```rust
/// Rewrites `from = "<old>"` in every tool section (table or inline table). Returns the count.
pub fn rename_borrow_owner(text: &str, old: &str, new: &str) -> Result<(String, usize), TomlError> {
    let mut doc: DocumentMut = text.parse()?;
    let mut count = 0;
    for tool in crate::envedit::TOOLS {
        let Some(t) = doc.get_mut(tool).and_then(Item::as_table_like_mut) else {
            continue;
        };
        if let Some(Value::String(s)) = t.get_mut("from").and_then(Item::as_value_mut)
            && s.value() == old
        {
            replace_string(s, new.to_string());
            count += 1;
        }
    }
    Ok((doc.to_string(), count))
}
```

- [ ] **Step 4: Implement it in `envs.rs`.**

Add the variants:

```rust
    #[error("{0}")]
    Lends(String),
    #[error("a hydra terminal for {borrower} (which borrows from {owner}) is still open; close it first")]
    BorrowerRunning { borrower: EnvName, owner: EnvName },
```

Add a helper:

```rust
/// Refuses while a terminal of an environment that borrows from `owner` is open.
fn refuse_open_borrowers(paths: &HydraPaths, owner: &EnvName) -> Result<(), EnvError> {
    for (b, _) in crate::borrow::borrowers(paths, owner) {
        if lock::is_running(paths, &b)? {
            return Err(EnvError::BorrowerRunning {
                borrower: b,
                owner: owner.clone(),
            });
        }
    }
    Ok(())
}
```

In `remove`, after the `Running` check:

```rust
    let lent = crate::borrow::borrowers(paths, name);
    if !lent.is_empty() {
        let lines: Vec<String> = lent
            .iter()
            .map(|(b, tools)| format!("{b} borrows {} from {name}", tools.join(", ")))
            .collect();
        let first = &lent[0];
        return Err(EnvError::Lends(format!(
            "{}\n  -> remove those first, e.g. hydra remove {} {}",
            lines.join("\n"),
            first.1[0],
            first.0
        )));
    }
```

In `rename`, after the `Running` check: `refuse_open_borrowers(paths, old)?;`.

In the rename loop over env files, apply both rewrites. Replace the loop with:

```rust
    let mut env_files = Vec::new();
    let mut borrowers = Vec::new();
    for n in list(paths)?.valid {
        let path = paths.env_file(&n);
        let text = std::fs::read_to_string(&path)?;
        let rewrite_err = |e: toml_edit::TomlError| EnvError::Rewrite(format!("{}: {e}", path.display()));
        let (out, refs) = rewrite::rename_secret_refs(&text, old.as_str(), new.as_str()).map_err(rewrite_err)?;
        let (out, lent) = rewrite::rename_borrow_owner(&out, old.as_str(), new.as_str()).map_err(rewrite_err)?;
        if lent > 0 {
            borrowers.push(n.to_string());
        }
        if refs > 0 || lent > 0 {
            env_files.push((if &n == old { new.clone() } else { n }, out, refs));
        }
    }
```

The existing write loop sums the third tuple element into `references`, which stays the secret-reference count. Change its failure fix text to `change secret:{old}/ references and from = "{old}" to {new} by hand`. Add `pub borrowers: Vec<String>` to `RenameReport`, and set `borrowers` in the returned value. Check whether `TomlError` is `toml_edit::TomlError` (rewrite.rs imports it from `toml_edit`), and adjust the closure's type annotation if needed.

- [ ] **Step 5: Print it in the CLI.** In `commands/env.rs`, in `EnvCmd::Rename` after the bindings line:

```rust
            if !r.borrowers.is_empty() {
                anstream::println!("updated borrowers: {}", r.borrowers.join(", "));
            }
```

- [ ] **Step 6: Run the tests.**
Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 7: Commit.**

```bash
git add crates/hydra-core crates/hydra-cli
git commit -m "feat: env rm refuses owners with borrowers; env rename updates them"
```

---

### Task 6: Docs and version

**Files:**
- Modify: `README.md`, `site/index.html`, `Cargo.toml` (workspace version), `docs/superpowers/specs/2026-10-03-hydra-design.md` (§3 concepts: one line pointing to the borrowed-logins spec), `docs/superpowers/plans/2026-10-03-hydra-roadmap.md` (a row for this plan: done, 0.6.0)

- [ ] **Step 1: README.** Add a "Borrowing a login" section after the tools section:
  - What borrowing does, with the `from = "personal"` example and `hydra add claude work --from personal`.
  - Whole tool folder shared: login, history, settings.
  - Sign in from the owner, with the exact not-signed-in note.
  - `git` can't be borrowed; borrowing `github` covers git over HTTPS.
  - The `remove` / `env rm` / `env rename` behaviour.

  Also:
  - Add `--from <env>` to the `hydra add` row of the commands table.
  - Add troubleshooting rows for "borrows [x] from y, but y has no [x]" and the chain error.
  - Set the status line to 0.6.

- [ ] **Step 2: site/index.html.** Make the same content changes in the page's existing classes:
  - a `#borrow` section with a nav link after "Tools";
  - the commands row;
  - the troubleshooting rows.

  Keep the HTML valid and phone-width safe.

- [ ] **Step 3: Version.** Set `version = "0.6.0"` in the workspace `Cargo.toml`, then run `cargo build` so `Cargo.lock` updates.

- [ ] **Step 4: Run the checks.**
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass.

- [ ] **Step 5: Commit.**

```bash
git add README.md site/index.html Cargo.toml Cargo.lock docs
git commit -m "docs: borrowing a login; version 0.6.0"
```

---

### Task 7 (manual, user): real-machine check after release

1. `hydra add claude <second-env> --from <env-with-claude>`, then open a second-env terminal. `claude` is signed in with no new login.
2. In both terminals, `$env:CLAUDE_CONFIG_DIR` points at the owner's state folder.
3. Do the same for `github`: `gh auth status` in the borrower shows the owner's account. `git config user.email` still shows the borrower's.
4. `hydra whoami` in the borrower shows `from <owner> ·`.
5. `hydra env rm <owner>` refuses and lists the borrower.
