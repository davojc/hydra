# Hydra Plan 2: Claude Accounts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each hydra environment gets its own Claude Code sign-in, while every environment shares the user's existing `~/.claude` setup: skills, commands, plugins, hooks, settings, `CLAUDE.md` and user MCP servers.

**Architecture:**
- A new `claude` provider sets `CLAUDE_CONFIG_DIR=state/<env>/claude` and builds that folder on every launch:
  - Directory junctions point at the base for shared folders.
  - `settings.json` and `CLAUDE.md` are merged (base plus environment overrides).
  - Only the `mcpServers` key of the environment's `.claude.json` is synced from the base's global state file.
- Everything else Claude writes (sign-in, history, projects) stays private to the environment.
- Junction helpers live in `hydra-platform`. The JSON and merge logic is pure functions in the provider module.

**Tech Stack:** existing workspace, plus `serde_json` (with `preserve_order`) and `junction` 1.x.

**Spec:** `docs/superpowers/specs/2026-10-03-hydra-design.md` (§5 claude row, §6 Claude layering, §11 items 1, 4 and 5, all verified 2026-10-04 on Claude Code 2.1.289). Roadmap: `docs/superpowers/plans/2026-10-03-hydra-roadmap.md`.

## Global Constraints

- The constraints from Plan 1 still apply:
  - edition 2024, `rust-version = "1.89"`
  - let-chains instead of nested `if let`
  - no `std::env::set_var` in tests
  - case-insensitive variable names
  - generated hydra text is ASCII-only (user content passes through unchanged)
  - `cargo test --workspace`, `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` must pass after every task
- **Never delete, move or modify anything inside the base (`~/.claude`) or `~/.claude.json`.** Hydra only reads them. Removing a junction must remove only the link.
- Shared folders linked from the base, each only if it exists there: `skills`, `commands`, `agents`, `hooks`, `output-styles`, `plugins`. An environment override at `envs/<env>/claude/<name>/` wins over the base.
- Base: `claude_base` from `config.toml` (tilde-expanded) if set, else `<user_home>/.claude`. Base global state file: `<user_home>/.claude.json` when `claude_base` is unset, else `<base>/.claude.json`.
- Variables: set `CLAUDE_CONFIG_DIR`. Unset `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN`, because they override the sign-in. All four are the provider's managed variables.
- Sign-in hint: `not signed in - run claude auth login` when `state/<env>/claude/.credentials.json` is missing. The auth command is `claude auth login`.
- Identity check (`whoami`) reads files only: `oauthAccount.emailAddress` (plus `organizationName` when present) from `state/<env>/claude/.claude.json`.
- The provider is active only when the environment has a `[claude]` section. `hydra env new` writes an active `[claude]` section.
- Tests must never touch the developer's real `~/.claude` or `~/.claude.json`. Provider tests use `Fixture`'s temporary home. CLI tests set `HYDRA_USER_HOME` (added in Task 4) to a fake user folder.

## Review Focus

1. **A real folder sits where a link should go.** For example, the user ran Claude with a hand-set `CLAUDE_CONFIG_DIR` and it created `state/<env>/claude/plugins/` as a normal folder. An empty folder is replaced by the link. A non-empty one stops the launch with a fix, and its contents are never deleted. Test: Task 3 `real_folder_in_place_of_link_is_never_deleted`.
2. **`env rm --yes` and `env rename` with junctions inside the state folder.** The base's contents must survive both. Tests: Task 1 `remove_dir_all_does_not_follow_junctions`, Task 4 `env_rm_keeps_the_claude_base_intact`.
3. **The environment's `.claude.json` is not valid JSON**, for example because Claude was mid-write. Hydra must not overwrite it, and the launch fails with a "try again" message. Test: Task 2 `sync_refuses_unparseable_state_file`.
4. **No `~/.claude` at all** (Claude never used on this machine). The environment still opens, with an empty, working config folder. Test: Task 3 `works_without_a_base`.
5. **The user's real `.claude.json` keys** (`oauthAccount`, `projects`, …) survive a sync byte-for-byte apart from `mcpServers`. Test: Task 2 `sync_touches_only_mcp_servers`.

---

## File Structure

```
Cargo.toml                                   + serde_json, junction workspace deps
crates/hydra-platform/Cargo.toml             + junction (windows), tempfile dev
crates/hydra-platform/src/links.rs           NEW: link_dir, is_link, unlink
crates/hydra-platform/src/lib.rs             + pub mod links;
crates/hydra-core/src/config.rs              + GlobalConfig.claude_base
crates/hydra-core/src/envs.rs                template: active [claude]
crates/hydra-providers/Cargo.toml            + serde_json
crates/hydra-providers/src/claude_files.rs   NEW: deep_merge, glob_match, merge_settings, render_claude_md, sync_mcp (pure)
crates/hydra-providers/src/claude.rs         NEW: Claude provider
crates/hydra-providers/src/lib.rs            + modules, registry entry
crates/hydra-cli/tests/claude_cli.rs         NEW: end-to-end tests
```

---

### Task 1: Directory links in hydra-platform

**Files:**
- Modify: `Cargo.toml` (workspace deps), `crates/hydra-platform/Cargo.toml`, `crates/hydra-platform/src/lib.rs`
- Create: `crates/hydra-platform/src/links.rs`

**Interfaces:**
- Produces:
  - `hydra_platform::links::link_dir(target: &Path, link: &Path) -> io::Result<()>`: a junction on Windows, a symlink on Unix.
  - `is_link(path: &Path) -> bool`: true for junctions and symlinks, false otherwise or if the path is missing.
  - `unlink(link: &Path) -> io::Result<()>`: removes only the link. It returns an error if `link` is not a link.

- [ ] **Step 1: Add the dependencies**

In root `Cargo.toml` `[workspace.dependencies]` add:
```toml
junction = "1"
serde_json = { version = "1", features = ["preserve_order"] }
```
In `crates/hydra-platform/Cargo.toml` add:
```toml
[target.'cfg(windows)'.dependencies]
junction.workspace = true
```
(`tempfile` is already a dev-dependency there.) In `crates/hydra-platform/src/lib.rs` add `pub mod links;`.

- [ ] **Step 2: Write the failing tests**

`crates/hydra-platform/src/links.rs`:
```rust
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    fn base_with_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base").join("skills");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("keep.md"), "precious").unwrap();
        (dir, base)
    }

    #[test]
    fn link_reads_through_and_unlink_keeps_the_target() {
        let (dir, base) = base_with_file();
        let link = dir.path().join("env").join("skills");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        link_dir(&base, &link).unwrap();
        assert!(is_link(&link));
        assert_eq!(std::fs::read_to_string(link.join("keep.md")).unwrap(), "precious");
        unlink(&link).unwrap();
        assert!(!link.exists());
        assert_eq!(std::fs::read_to_string(base.join("keep.md")).unwrap(), "precious");
    }

    #[test]
    fn is_link_is_false_for_folders_and_missing_paths() {
        let (dir, base) = base_with_file();
        assert!(!is_link(&base));
        assert!(!is_link(&dir.path().join("nope")));
        assert!(unlink(&base).is_err(), "unlink must refuse a real folder");
        assert!(base.join("keep.md").exists());
    }

    #[test]
    fn remove_dir_all_does_not_follow_junctions() {
        let (dir, base) = base_with_file();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        link_dir(&base, &state.join("skills")).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
        assert_eq!(std::fs::read_to_string(base.join("keep.md")).unwrap(), "precious");
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p hydra-platform links`
Expected: compile errors, because `link_dir`, `is_link` and `unlink` are not found.

- [ ] **Step 4: Implement**

Add above the tests:
```rust
/// Makes `link` point at the directory `target`: a junction on Windows (no admin
/// rights needed), a symlink elsewhere.
pub fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        junction::create(target, link)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

/// True for junctions and symlinks; false for real files, folders and missing paths.
pub fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Removes a link without touching what it points at. Refuses anything that isn't a link.
pub fn unlink(link: &Path) -> std::io::Result<()> {
    if !is_link(link) {
        return Err(std::io::Error::other(format!("{} is not a link", link.display())));
    }
    #[cfg(windows)]
    {
        std::fs::remove_dir(link)
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(link)
    }
}
```

If `is_symlink()` returns false for junctions on this Rust version, use `junction::exists(path).unwrap_or(false)` on Windows instead, and record it in the report.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hydra-platform links && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: 3 pass, clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hydra-platform
git commit -m "feat(platform): directory links (junctions on Windows)"
```

---

### Task 2: Pure Claude file logic

**Files:**
- Modify: `crates/hydra-providers/Cargo.toml` (add `serde_json.workspace = true` under `[dependencies]`), `crates/hydra-providers/src/lib.rs` (add `pub mod claude_files;`)
- Create: `crates/hydra-providers/src/claude_files.rs`

**Interfaces:**
- Produces, in `hydra_providers::claude_files`:
  - `deep_merge(base: Value, over: Value) -> Value`
  - `glob_match(pattern: &str, name: &str) -> bool`
  - `drop_excluded(servers: &mut Map<String, Value>, exclude: &[String])`
  - `merge_settings(base: Option<&str>, over: Option<&str>, exclude: &[String]) -> Result<String, String>`: returns pretty JSON plus a trailing newline.
  - `render_claude_md(env: &str, base: Option<&str>, over: Option<&str>) -> Option<String>`
  - `sync_mcp(state_json: Option<&str>, base_global_json: Option<&str>, exclude: &[String]) -> Result<Option<String>, String>`: returns `None` when nothing changes.

- [ ] **Step 1: Write the failing tests**

`crates/hydra-providers/src/claude_files.rs`:
```rust
use serde_json::{Map, Value};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deep_merge_merges_objects_and_replaces_the_rest() {
        let base = json!({"a": {"x": 1, "y": [1, 2]}, "b": "keep", "c": 1});
        let over = json!({"a": {"y": [9], "z": true}, "c": 2});
        assert_eq!(
            deep_merge(base, over),
            json!({"a": {"x": 1, "y": [9], "z": true}, "b": "keep", "c": 2})
        );
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("acme*", "acme-brain"));
        assert!(glob_match("*brain", "ACME-BRAIN"), "case-insensitive");
        assert!(glob_match("dash", "dash"));
        assert!(glob_match("d?sh", "dash"));
        assert!(!glob_match("acme*", "codegraph"));
        assert!(!glob_match("dash", "dashboard"));
    }

    #[test]
    fn settings_merge_with_exclusions() {
        let base = r#"{"model":"opus","mcpServers":{"acme-x":{},"codegraph":{}},"permissions":{"allow":["Bash"]}}"#;
        let over = r#"{"model":"sonnet"}"#;
        let out = merge_settings(Some(base), Some(over), &["acme*".to_string()]).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["model"], "sonnet");
        assert_eq!(v["permissions"]["allow"][0], "Bash");
        assert!(v["mcpServers"].get("acme-x").is_none());
        assert!(v["mcpServers"].get("codegraph").is_some());
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn settings_merge_without_files_is_an_empty_object() {
        assert_eq!(merge_settings(None, None, &[]).unwrap(), "{}\n");
    }

    #[test]
    fn settings_merge_reports_bad_json() {
        let err = merge_settings(Some("{nope"), None, &[]).unwrap_err();
        assert!(err.contains("settings.json"), "{err}");
    }

    #[test]
    fn claude_md_concatenates_with_marker() {
        let out = render_claude_md("work", Some("# Base\n"), Some("# Work only\n")).unwrap();
        assert!(out.starts_with("<!-- generated by hydra for environment \"work\""));
        assert!(out.contains("# Base\n\n\n# Work only\n"));
        assert_eq!(render_claude_md("work", None, None), None);
        let only_base = render_claude_md("work", Some("# Base"), None).unwrap();
        assert!(only_base.ends_with("# Base"));
    }

    #[test]
    fn sync_touches_only_mcp_servers() {
        let state = r#"{"oauthAccount":{"emailAddress":"a@b.c"},"projects":{"E:/x":{"k":1}},"mcpServers":{"old":{}}}"#;
        let base = r#"{"mcpServers":{"codegraph":{"command":"cg"},"acme-brain":{}},"oauthAccount":{"emailAddress":"base@x"}}"#;
        let out = sync_mcp(Some(state), Some(base), &["acme*".to_string()]).unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["oauthAccount"]["emailAddress"], "a@b.c", "account never copied from base");
        assert_eq!(v["projects"]["E:/x"]["k"], 1);
        assert_eq!(v["mcpServers"], serde_json::json!({"codegraph": {"command": "cg"}}));
    }

    #[test]
    fn sync_is_a_no_op_when_already_in_sync() {
        let state = r#"{"mcpServers":{"codegraph":{"command":"cg"}}}"#;
        let base = r#"{"mcpServers":{"codegraph":{"command":"cg"}}}"#;
        assert_eq!(sync_mcp(Some(state), Some(base), &[]).unwrap(), None);
    }

    #[test]
    fn sync_creates_state_file_when_missing() {
        let base = r#"{"mcpServers":{"dash":{}}}"#;
        let out = sync_mcp(None, Some(base), &[]).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), serde_json::json!({"mcpServers": {"dash": {}}}));
        assert_eq!(sync_mcp(None, None, &[]).unwrap(), None, "nothing to sync, nothing written");
    }

    #[test]
    fn sync_refuses_unparseable_state_file() {
        let err = sync_mcp(Some("{\"half"), Some(r#"{"mcpServers":{}}"#), &[]).unwrap_err();
        assert!(err.contains("try again"), "{err}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p hydra-providers claude_files`
Expected: compile errors, because the functions are not found.

- [ ] **Step 3: Implement**

Add above the tests:
```rust
/// Objects merge key by key (recursively); anything else in `over` replaces `base`.
pub fn deep_merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, ov) in o {
                let merged = match b.remove(&k) {
                    Some(bv) => deep_merge(bv, ov),
                    None => ov,
                };
                b.insert(k, merged);
            }
            Value::Object(b)
        }
        (_, over) => over,
    }
}

/// Case-insensitive glob with `*` (any run) and `?` (one character).
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

pub fn drop_excluded(servers: &mut Map<String, Value>, exclude: &[String]) {
    servers.retain(|name, _| !exclude.iter().any(|pat| glob_match(pat, name)));
}

fn parse_object(text: &str, what: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{what} isn't valid JSON: {e}"))?;
    if v.is_object() { Ok(v) } else { Err(format!("{what} must be a JSON object")) }
}

/// Base settings merged with the environment's overrides, minus excluded MCP servers.
pub fn merge_settings(base: Option<&str>, over: Option<&str>, exclude: &[String]) -> Result<String, String> {
    let mut merged = Value::Object(Map::new());
    if let Some(b) = base {
        merged = deep_merge(merged, parse_object(b, "your Claude settings.json")?);
    }
    if let Some(o) = over {
        merged = deep_merge(merged, parse_object(o, "the environment's claude/settings.json")?);
    }
    if let Some(Value::Object(servers)) = merged.get_mut("mcpServers") {
        drop_excluded(servers, exclude);
    }
    let mut out = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(out)
}

/// Base CLAUDE.md followed by the environment's own section. `None` when neither exists.
pub fn render_claude_md(env: &str, base: Option<&str>, over: Option<&str>) -> Option<String> {
    if base.is_none() && over.is_none() {
        return None;
    }
    let mut s = format!(
        "<!-- generated by hydra for environment \"{env}\" - edit ~/.claude/CLAUDE.md or ~/.hydra/envs/{env}/claude/CLAUDE.md instead -->\n"
    );
    if let Some(b) = base {
        s.push_str(b);
    }
    if let Some(o) = over {
        if base.is_some() {
            s.push_str("\n\n");
        }
        s.push_str(o);
    }
    Some(s)
}

/// Copies the base's `mcpServers` (minus exclusions) into the environment's
/// `.claude.json`, leaving every other key alone. `Ok(None)` means nothing to write.
pub fn sync_mcp(state_json: Option<&str>, base_global_json: Option<&str>, exclude: &[String]) -> Result<Option<String>, String> {
    let mut servers = match base_global_json {
        Some(text) => match parse_object(text, "your ~/.claude.json")?.get("mcpServers") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        },
        None => Map::new(),
    };
    drop_excluded(&mut servers, exclude);
    let mut state = match state_json {
        Some(text) => serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| "the environment's .claude.json isn't valid JSON (Claude may be writing it); try again".to_string())?,
        None if servers.is_empty() => return Ok(None),
        None => Value::Object(Map::new()),
    };
    let wanted = Value::Object(servers);
    if state.get("mcpServers") == Some(&wanted) {
        return Ok(None);
    }
    state.as_object_mut().expect("checked object").insert("mcpServers".to_string(), wanted);
    let mut out = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(Some(out))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydra-providers claude_files && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: 10 pass, clean. (The let-chain constraint: rewrite the `if let Some(o) = over { if base.is_some()` block only if clippy flags it. It is an `if` inside an `if let` with a statement after, so it is not collapsible.)

- [ ] **Step 5: Commit**

```bash
git add crates/hydra-providers
git commit -m "feat(providers): pure Claude settings, CLAUDE.md and MCP sync logic"
```

---

### Task 3: The claude provider

**Files:**
- Modify: `crates/hydra-core/src/config.rs` (add `pub claude_base: Option<String>,` to `GlobalConfig`, after `git_bash`)
- Create: `crates/hydra-providers/src/claude.rs`
- Modify: `crates/hydra-providers/src/lib.rs` (add `pub mod claude;` and `Box::new(claude::Claude)` as the **first** entry of `all()`)

**Interfaces:**
- Consumes:
  - `claude_files::{merge_settings, render_claude_md, sync_mcp}` (Task 2)
  - `hydra_platform::links::{link_dir, is_link, unlink}` (Task 1)
  - `hydra_core::config::load_global`, `hydra_core::paths::expand_tilde`
  - the `Provider` trait and `Ctx`
  - `report::report`
- Produces: `hydra_providers::claude::{Claude, SHARED_DIRS}`.

- [ ] **Step 1: Write the failing tests**

`crates/hydra-providers/src/claude.rs`:
```rust
use std::path::{Path, PathBuf};

use hydra_core::config::{EnvConfig, load_global};
use hydra_core::contribution::Contribution;
use hydra_core::paths::expand_tilde;
use hydra_core::provider::{CommandRunner, Ctx, IdentityReport, Provider, ProviderError, Status};
use hydra_platform::links::{is_link, link_dir, unlink};

use crate::claude_files::{merge_settings, render_claude_md, sync_mcp};
use crate::report::report;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeRunner, Fixture};
    use hydra_core::contribution::VarValue;

    /// A user ~/.claude with a skill, plugins, settings, CLAUDE.md, plus ~/.claude.json.
    fn with_base(f: &Fixture) {
        let base = f.home.join(".claude");
        std::fs::create_dir_all(base.join("skills").join("my-skill")).unwrap();
        std::fs::write(base.join("skills").join("my-skill").join("SKILL.md"), "skill").unwrap();
        std::fs::create_dir_all(base.join("plugins")).unwrap();
        std::fs::write(base.join("plugins").join("installed_plugins.json"), "{}").unwrap();
        std::fs::write(base.join("settings.json"), r#"{"model":"opus","mcpServers":{"acme-x":{}}}"#).unwrap();
        std::fs::write(base.join("CLAUDE.md"), "# Base rules").unwrap();
        std::fs::write(base.join(".credentials.json"), "BASE-SECRET").unwrap();
        std::fs::write(f.home.join(".claude.json"), r#"{"mcpServers":{"codegraph":{"command":"cg"},"acme-brain":{}},"oauthAccount":{"emailAddress":"base@x"}}"#).unwrap();
    }

    fn cfg_dir(f: &Fixture) -> PathBuf {
        f.paths.state_dir(&f.name).join("claude")
    }

    #[test]
    fn contributes_config_dir_and_clears_overriding_keys() {
        let f = Fixture::new("[claude]\n");
        let c = Claude.contribute(&f.ctx()).unwrap();
        assert_eq!(c.vars["CLAUDE_CONFIG_DIR"], VarValue::Literal(cfg_dir(&f).to_string_lossy().into_owned()));
        for v in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"] {
            assert!(c.unset.contains(v), "{v}");
        }
        assert!(Claude.is_configured(&f.env));
        assert!(!Claude.is_configured(&Fixture::new("").env));
    }

    #[test]
    fn materialise_shares_the_base_and_keeps_the_sign_in_private() {
        let f = Fixture::new("[claude]\nmcp.exclude = [\"acme*\"]\n");
        with_base(&f);
        Claude.materialise(&f.ctx()).unwrap();
        let dir = cfg_dir(&f);
        assert!(is_link(&dir.join("skills")) && is_link(&dir.join("plugins")));
        assert_eq!(std::fs::read_to_string(dir.join("skills").join("my-skill").join("SKILL.md")).unwrap(), "skill");
        assert!(!dir.join("commands").exists(), "only folders that exist in the base are linked");
        assert!(!dir.join(".credentials.json").exists(), "the base sign-in is never copied");
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["model"], "opus");
        assert!(settings["mcpServers"].get("acme-x").is_none());
        assert!(std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap().contains("# Base rules"));
        let state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(state["mcpServers"], serde_json::json!({"codegraph": {"command": "cg"}}));
        assert!(state.get("oauthAccount").is_none(), "the base account is never copied");
        assert!(dir.join("README-hydra.txt").is_file());
    }

    #[test]
    fn environment_overrides_win() {
        let f = Fixture::new("[claude]\n");
        with_base(&f);
        let over = f.paths.env_dir(&f.name).join("claude");
        std::fs::create_dir_all(over.join("skills").join("work-skill")).unwrap();
        std::fs::write(over.join("settings.json"), r#"{"model":"sonnet"}"#).unwrap();
        std::fs::write(over.join("CLAUDE.md"), "# Work only").unwrap();
        Claude.materialise(&f.ctx()).unwrap();
        let dir = cfg_dir(&f);
        assert!(dir.join("skills").join("work-skill").is_dir());
        assert!(!dir.join("skills").join("my-skill").exists());
        assert!(std::fs::read_to_string(dir.join("settings.json")).unwrap().contains("\"sonnet\""));
        let md = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(md.contains("# Base rules") && md.contains("# Work only"));
    }

    #[test]
    fn materialise_is_repeatable_and_follows_base_changes() {
        let f = Fixture::new("[claude]\n");
        with_base(&f);
        Claude.materialise(&f.ctx()).unwrap();
        std::fs::create_dir_all(f.home.join(".claude").join("commands")).unwrap();
        Claude.materialise(&f.ctx()).unwrap();
        assert!(is_link(&cfg_dir(&f).join("commands")));
    }

    #[test]
    fn real_folder_in_place_of_link_is_never_deleted() {
        let f = Fixture::new("[claude]\n");
        with_base(&f);
        let dir = cfg_dir(&f);
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(dir.join("plugins").join("mine.json"), "x").unwrap();
        std::fs::create_dir_all(dir.join("skills")).unwrap(); // empty: replaced by the link
        let e = Claude.materialise(&f.ctx()).unwrap_err();
        assert!(e.message.contains("plugins") && e.message.contains("real folder"), "{}", e.message);
        assert!(e.fix.is_some());
        assert_eq!(std::fs::read_to_string(dir.join("plugins").join("mine.json")).unwrap(), "x");
    }

    #[test]
    fn works_without_a_base() {
        let f = Fixture::new("[claude]\n");
        Claude.materialise(&f.ctx()).unwrap();
        let dir = cfg_dir(&f);
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), "{}\n");
        assert!(!dir.join("CLAUDE.md").exists());
    }

    #[test]
    fn claude_base_setting_moves_the_base() {
        let f = Fixture::new("[claude]\n");
        let alt = f.home.join("dotfiles").join("claude");
        std::fs::create_dir_all(alt.join("skills")).unwrap();
        std::fs::write(alt.join(".claude.json"), r#"{"mcpServers":{"alt":{}}}"#).unwrap();
        std::fs::create_dir_all(f.paths.root()).unwrap();
        let alt_toml = alt.to_string_lossy().replace('\\', "/");
        std::fs::write(f.paths.config_file(), format!("claude_base = \"{alt_toml}\"\n")).unwrap();
        Claude.materialise(&f.ctx()).unwrap();
        assert!(is_link(&cfg_dir(&f).join("skills")));
        assert!(std::fs::read_to_string(cfg_dir(&f).join(".claude.json")).unwrap().contains("\"alt\""));
    }

    #[test]
    fn sign_in_hint_and_identity_come_from_files() {
        let f = Fixture::new("[claude]\n");
        Claude.materialise(&f.ctx()).unwrap();
        assert_eq!(Claude.sign_in_hint(&f.ctx()).as_deref(), Some("not signed in - run claude auth login"));
        let r = Claude.check(&f.ctx(), &FakeRunner::default());
        assert_eq!(r.status, Status::Missing);

        let dir = cfg_dir(&f);
        std::fs::write(dir.join(".credentials.json"), "{}").unwrap();
        std::fs::write(dir.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"work@example.com","organizationName":"acme"}}"#).unwrap();
        assert_eq!(Claude.sign_in_hint(&f.ctx()), None);
        let r = Claude.check(&f.ctx(), &FakeRunner::default());
        assert_eq!((r.status, r.detail.as_str()), (Status::Ok, "work@example.com (acme)"));
        assert_eq!(Claude.auth_command(&f.ctx()).unwrap(), vec!["claude", "auth", "login"]);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p hydra-providers claude::`
Expected: compile error, because `Claude` is not found.

- [ ] **Step 3: Implement**

Add above the tests:
```rust
pub struct Claude;

/// Folders shared from the base into every environment (when they exist there).
pub const SHARED_DIRS: &[&str] = &["skills", "commands", "agents", "hooks", "output-styles", "plugins"];

const MANAGED: &[&str] = &["CLAUDE_CONFIG_DIR", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"];

const README: &str = "generated by hydra\r\n\
\r\n\
This folder is Claude Code's config folder for one hydra environment (CLAUDE_CONFIG_DIR).\r\n\
- skills, commands, agents, hooks, output-styles, plugins: links to your shared ~/.claude folders\r\n\
- settings.json and CLAUDE.md: rebuilt on every launch; edit ~/.claude/... or ~/.hydra/envs/<env>/claude/... instead\r\n\
- .claude.json: Claude's own state; hydra only refreshes its mcpServers key from ~/.claude.json\r\n\
- everything else (sign-in, history, projects) belongs to this environment only\r\n";

struct Layout {
    base: PathBuf,
    base_global: PathBuf,
    over: PathBuf,
    dir: PathBuf,
}

fn layout(ctx: &Ctx) -> Result<Layout, ProviderError> {
    let global = load_global(ctx.paths).map_err(|e| ProviderError::new(e.to_string()))?;
    let (base, base_global) = match &global.claude_base {
        Some(b) => {
            let base = expand_tilde(b, ctx.user_home);
            let g = base.join(".claude.json");
            (base, g)
        }
        None => (ctx.user_home.join(".claude"), ctx.user_home.join(".claude.json")),
    };
    Ok(Layout { base, base_global, over: ctx.paths.env_dir(ctx.name).join("claude"), dir: ctx.provider_dir("claude") })
}

fn read_opt(path: &Path) -> Result<Option<String>, ProviderError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ProviderError::new(format!("can't read {}: {e}", path.display()))),
    }
}

/// Writes only when the content changed, via a temp file + rename so Claude never sees half a file.
fn write_if_changed(path: &Path, content: &str) -> Result<(), ProviderError> {
    if read_opt(path)?.as_deref() == Some(content) {
        return Ok(());
    }
    let tmp = path.with_extension("hydra-tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn link_shared(l: &Layout, name: &str, env_file: &Path) -> Result<(), ProviderError> {
    let target = [l.over.join(name), l.base.join(name)].into_iter().find(|p| p.is_dir());
    let link = l.dir.join(name);
    if is_link(&link) {
        unlink(&link)?;
    } else if link.is_dir() {
        let empty = std::fs::read_dir(&link)?.next().is_none();
        if !empty {
            return Err(ProviderError::new(format!(
                "{} is a real folder, not a link to your shared ~/.claude/{name}",
                link.display()
            ))
            .with_fix(format!(
                "move anything you want to keep into ~/.claude/{name} (or {}), then delete {}",
                env_file.display(),
                link.display()
            )));
        }
        std::fs::remove_dir(&link)?;
    }
    if let Some(t) = target {
        link_dir(&t, &link)?;
    }
    Ok(())
}

impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn is_configured(&self, env: &EnvConfig) -> bool {
        env.claude.is_some()
    }
    fn managed_vars(&self) -> &'static [&'static str] {
        MANAGED
    }

    fn materialise(&self, ctx: &Ctx) -> Result<(), ProviderError> {
        let l = layout(ctx)?;
        std::fs::create_dir_all(&l.dir)?;
        let exclude = ctx.env.claude.as_ref().map(|c| c.mcp.exclude.clone()).unwrap_or_default();
        let over_dir = l.over.clone();
        for name in SHARED_DIRS {
            link_shared(&l, name, &over_dir.join(name))?;
        }
        let settings = merge_settings(
            read_opt(&l.base.join("settings.json"))?.as_deref(),
            read_opt(&l.over.join("settings.json"))?.as_deref(),
            &exclude,
        )
        .map_err(ProviderError::new)?;
        write_if_changed(&l.dir.join("settings.json"), &settings)?;
        let md_path = l.dir.join("CLAUDE.md");
        match render_claude_md(
            ctx.name.as_str(),
            read_opt(&l.base.join("CLAUDE.md"))?.as_deref(),
            read_opt(&l.over.join("CLAUDE.md"))?.as_deref(),
        ) {
            Some(md) => write_if_changed(&md_path, &md)?,
            None => {
                if md_path.is_file() {
                    std::fs::remove_file(&md_path)?;
                }
            }
        }
        let state_path = l.dir.join(".claude.json");
        if let Some(updated) = sync_mcp(read_opt(&state_path)?.as_deref(), read_opt(&l.base_global)?.as_deref(), &exclude)
            .map_err(ProviderError::new)?
        {
            write_if_changed(&state_path, &updated)?;
        }
        write_if_changed(&l.dir.join("README-hydra.txt"), README)?;
        Ok(())
    }

    fn contribute(&self, ctx: &Ctx) -> Result<Contribution, ProviderError> {
        Ok(Contribution::new()
            .path("CLAUDE_CONFIG_DIR", &ctx.provider_dir("claude"))
            .unset("ANTHROPIC_API_KEY")
            .unset("ANTHROPIC_AUTH_TOKEN")
            .unset("CLAUDE_CODE_OAUTH_TOKEN"))
    }

    fn check(&self, ctx: &Ctx, _run: &dyn CommandRunner) -> IdentityReport {
        let dir = ctx.provider_dir("claude");
        if !dir.join(".credentials.json").is_file() {
            return report("claude", Status::Missing, "not signed in");
        }
        let account = std::fs::read_to_string(dir.join(".claude.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("oauthAccount").cloned());
        match account.as_ref().and_then(|a| a.get("emailAddress")).and_then(|e| e.as_str()) {
            Some(email) => {
                let org = account.as_ref().and_then(|a| a.get("organizationName")).and_then(|o| o.as_str());
                let detail = match org {
                    Some(o) if !o.is_empty() => format!("{email} ({o})"),
                    _ => email.to_string(),
                };
                report("claude", Status::Ok, detail)
            }
            None => report("claude", Status::Info, "signed in"),
        }
    }

    fn auth_command(&self, _ctx: &Ctx) -> Option<Vec<String>> {
        Some(["claude", "auth", "login"].map(String::from).to_vec())
    }

    fn sign_in_hint(&self, ctx: &Ctx) -> Option<String> {
        (!ctx.provider_dir("claude").join(".credentials.json").is_file())
            .then(|| "not signed in - run claude auth login".to_string())
    }
}
```

Add `serde_json` usage: it is already a providers dependency from Task 2.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hydra-providers && cargo test -p hydra-core && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all pass, including the registry `provider_ids_are_unique` and the 8 new claude tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hydra-core crates/hydra-providers
git commit -m "feat(providers): claude provider - per-environment sign-in, shared ~/.claude setup"
```

---

### Task 4: Template, end-to-end tests and safety

**Files:**
- Modify: `crates/hydra-core/src/envs.rs` (`template`): add an active `[claude]` section, followed by a commented `# mcp.exclude = ["work-only-*"]` example line.
- Modify: `crates/hydra-cli/src/app.rs` (`App::from_env`): `user_home` = `HYDRA_USER_HOME` if set, else `dirs::home_dir()`. On Windows `dirs` uses the known-folder API and ignores `USERPROFILE`, so tests need this override to keep away from the real `~/.claude`.
- Create: `crates/hydra-cli/tests/claude_cli.rs`

**Interfaces:**
- Consumes: the `hydra` binary, `tests/common/mod.rs` (`Home`).

- [ ] **Step 1: Update the template**

In `template()`, after the `# home = ...` line and its blank line, insert:
```
[claude]                            # own Claude sign-in; shares your ~/.claude setup
# mcp.exclude = ["work-only-*"]     # MCP servers to hide in this environment

```
Update the existing envs/CLI tests only if they assert the template's exact text. Existing tests assert `label` and `color` lines only, so none should need to change.

- [ ] **Step 2: Write the failing end-to-end tests**

`crates/hydra-cli/tests/claude_cli.rs`:
```rust
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
    std::fs::write(user.join(".claude.json"), r#"{"mcpServers":{"codegraph":{}}}"#).unwrap();
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
            .args(["run", env, "--", "pwsh", "-NoProfile", "-Command", "Write-Output \"[$env:CLAUDE_CONFIG_DIR]\""])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("state\\{env}\\claude]")));
        let dir = h.root().join("state").join(env).join("claude");
        assert_eq!(std::fs::read_to_string(dir.join("skills").join("s1").join("SKILL.md")).unwrap(), "precious");
        assert!(std::fs::read_to_string(dir.join(".claude.json")).unwrap().contains("codegraph"));
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
        .stderr(predicate::str::contains("hydra: claude: not signed in - run claude auth login"));
}

#[test]
fn env_rm_keeps_the_claude_base_intact() {
    let h = Home::new();
    let user = fake_user(&h);
    h.write_env("work", "[claude]\n");
    hydra_as(&h, &user).args(["run", "work", "--", "cmd", "/c", "exit", "0"]).assert().success();
    hydra_as(&h, &user).args(["env", "rename", "work", "iov"]).assert().success();
    assert_eq!(std::fs::read_to_string(h.root().join("state").join("iov").join("claude").join("skills").join("s1").join("SKILL.md")).unwrap(), "precious");
    hydra_as(&h, &user).args(["env", "rm", "iov", "--yes"]).assert().success();
    assert!(!h.root().join("state").join("iov").exists());
    assert_eq!(std::fs::read_to_string(user.join(".claude").join("skills").join("s1").join("SKILL.md")).unwrap(), "precious");
    assert_eq!(std::fs::read_to_string(user.join(".claude").join("CLAUDE.md")).unwrap(), "# shared");
}

#[test]
fn new_environments_use_claude_by_default() {
    let h = Home::new();
    h.hydra().args(["env", "new", "work"]).assert().success();
    assert!(h.env_toml("work").contains("\n[claude]"));
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p hydra-cli --test claude_cli`
Expected: `new_environments_use_claude_by_default` fails until the template changes. **Do the `HYDRA_USER_HOME` part of Step 4 before this first run**, so the other tests never resolve to the real `~/.claude`. Hydra only reads the base, but nothing should be left to chance.

- [ ] **Step 4: Implement**

In `crates/hydra-cli/src/app.rs`, replace the `user_home` line in `App::from_env` with:
```rust
        let user_home = match std::env::var_os("HYDRA_USER_HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => dirs::home_dir().context("can't find your home folder")?,
        };
```
Apply the template change from Step 1. Then run:
`cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all pass. Then confirm by hand that the real `~/.claude` folder's file count and modification times are unchanged (compare `Get-ChildItem ~\.claude -Recurse | Measure-Object` before and after the test run).

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: [claude] on by default for new environments; end-to-end Claude tests"
```

---

### Task 5: Real-account check (manual, by the user)

Run in your own Windows Terminal after `Copy-Item E:\AI\hydra\target\release\hydra.exe E:\Tools\ -Force`:

- [ ] Add `[claude]` to `envs/iov/env.toml` and `envs/davojc/env.toml` (`hydra env edit iov` / `davojc`).
- [ ] `hydra shell iov`. The banner shows `hydra: claude: not signed in - run claude auth login`. Run `claude auth login` with the iov account, then `exit`.
- [ ] `hydra shell davojc`, then `claude auth login` with the davojc account, then `exit`.
- [ ] `hydra whoami --env iov` and `hydra whoami --env davojc` show different `claude` rows.
- [ ] Run `claude` in both environments side by side: `/status` shows each account, and your skills, plugins and MCP servers appear in both.
- [ ] Plain `claude` outside hydra still uses your original `~/.claude` login.
