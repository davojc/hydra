use std::path::Path;

use serde_json::{Map, Value};

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

/// Appended to errors about a `.claude.json`, which Claude rewrites while it runs.
const MAY_BE_WRITING: &str = " (if Claude is running it may be writing it; try again)";

/// Parses a JSON object; errors name the file's full path (plus `hint`).
fn parse_object(text: &str, path: &Path, hint: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text)
        .map_err(|e| format!("{} isn't valid JSON: {e}{hint}", path.display()))?;
    if v.is_object() {
        Ok(v)
    } else {
        Err(format!("{} must be a JSON object{hint}", path.display()))
    }
}

/// `env` variables that choose the account or endpoint Claude signs in to.
const SIGN_IN_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

/// Removes sign-in settings from the shared base, returning their names (`env.X` for variables).
fn drop_sign_in(settings: &mut Value) -> Vec<String> {
    let mut dropped = Vec::new();
    let Some(obj) = settings.as_object_mut() else {
        return dropped;
    };
    if obj.remove("apiKeyHelper").is_some() {
        dropped.push("apiKeyHelper".to_string());
    }
    if let Some(Value::Object(env)) = obj.get_mut("env") {
        for var in SIGN_IN_ENV {
            let keys: Vec<String> = env
                .keys()
                .filter(|k| k.eq_ignore_ascii_case(var))
                .cloned()
                .collect();
            for k in keys {
                env.remove(&k);
                dropped.push(format!("env.{k}"));
            }
        }
    }
    dropped
}

/// The generated settings.json, plus the sign-in settings left out of the shared base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedSettings {
    pub text: String,
    pub dropped: Vec<String>,
}

/// Base settings (minus sign-in settings) merged with the environment's overrides, minus
/// excluded MCP servers. Each side is `(path, text)`; the path is only used in errors.
pub fn merge_settings(
    base: Option<(&Path, &str)>,
    over: Option<(&Path, &str)>,
    exclude: &[String],
) -> Result<MergedSettings, String> {
    let mut merged = Value::Object(Map::new());
    let mut dropped = Vec::new();
    if let Some((path, text)) = base {
        let mut b = parse_object(text, path, "")?;
        dropped = drop_sign_in(&mut b);
        merged = deep_merge(merged, b);
    }
    if let Some((path, text)) = over {
        merged = deep_merge(merged, parse_object(text, path, "")?);
    }
    if let Some(Value::Object(servers)) = merged.get_mut("mcpServers") {
        drop_excluded(servers, exclude);
    }
    let mut text = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(MergedSettings { text, dropped })
}

/// The tools whose commands the guard hook checks.
pub const GUARD_MATCHER: &str = "Bash|PowerShell";

/// Appends a PreToolUse hook on the Bash and PowerShell tools running `command` to the settings JSON
/// (creating hooks/PreToolUse as needed, keeping every existing hook). Idempotent:
/// an existing entry whose command contains " guard claude" is replaced, not duplicated.
pub fn add_guard_hook(settings_json: &str, command: &str) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(settings_json).map_err(|e| e.to_string())?;
    let obj = root
        .as_object_mut()
        .ok_or("settings.json isn't a JSON object")?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("settings.json: \"hooks\" isn't an object")?;
    let pre = hooks
        .entry("PreToolUse")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("settings.json: hooks.PreToolUse isn't a list")?;
    let ours = |entry: &Value| {
        entry["hooks"].as_array().is_some_and(|hs| {
            hs.iter().any(|h| {
                h["command"]
                    .as_str()
                    .is_some_and(|c| c.contains(" guard claude"))
            })
        })
    };
    pre.retain(|e| !ours(e));
    pre.push(serde_json::json!({
        "matcher": GUARD_MATCHER,
        "hooks": [{"type": "command", "command": command}],
    }));
    let mut text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

/// The hooks that tell hydraui what a session is doing: each Claude Code event, its matcher
/// (`None`: every time) and the state `hydra hook` reports.
pub const STATUS_HOOKS: [(&str, Option<&str>, &str); 7] = [
    ("SessionStart", None, "idle"),
    ("UserPromptSubmit", None, "working"),
    // Asking the user a question or to approve a plan.
    (
        "PreToolUse",
        Some("AskUserQuestion|ExitPlanMode"),
        "waiting",
    ),
    // A tool ran: any question or permission before it has been answered.
    ("PostToolUse", Some("*"), "working"),
    (
        "Notification",
        Some("permission_prompt|elicitation_dialog"),
        "waiting",
    ),
    ("Stop", None, "idle"),
    ("SessionEnd", None, "ended"),
];

/// Whether a hook command is one of hydra's status hooks (`<hydra> hook <state>`).
fn is_status_hook(command: &str) -> bool {
    command
        .rsplit_once(" hook ")
        .is_some_and(|(_, state)| STATUS_HOOKS.iter().any(|(_, _, s)| *s == state))
}

/// Adds hydra's status hooks (`STATUS_HOOKS`, each running `<hydra> hook <state>`, `hydra`
/// being the quoted path or name in `hydra`) to the settings JSON, keeping every other hook.
/// Idempotent: status hooks already there, from any hydra, are replaced.
pub fn add_status_hooks(settings_json: &str, hydra: &str) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(settings_json).map_err(|e| e.to_string())?;
    let obj = root
        .as_object_mut()
        .ok_or("settings.json isn't a JSON object")?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("settings.json: \"hooks\" isn't an object")?;
    let ours = |entry: &Value| {
        entry["hooks"].as_array().is_some_and(|hs| {
            hs.iter()
                .any(|h| h["command"].as_str().is_some_and(is_status_hook))
        })
    };
    for (event, _, _) in STATUS_HOOKS {
        let list = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| format!("settings.json: hooks.{event} isn't a list"))?;
        list.retain(|e| !ours(e));
    }
    for (event, matcher, state) in STATUS_HOOKS {
        let mut entry = serde_json::json!({
            "hooks": [{"type": "command", "command": format!("{hydra} hook {state}")}],
        });
        if let Some(m) = matcher {
            entry["matcher"] = Value::String(m.to_string());
        }
        hooks[event]
            .as_array_mut()
            .expect("made a list above")
            .push(entry);
    }
    let mut text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

/// The command of the guard hook already in the settings JSON, if any.
pub fn guard_hook_command(settings_json: &str) -> Option<String> {
    let root: Value = serde_json::from_str(settings_json).ok()?;
    root["hooks"]["PreToolUse"]
        .as_array()?
        .iter()
        .flat_map(|e| e["hooks"].as_array().into_iter().flatten())
        .filter_map(|h| h["command"].as_str())
        .find(|c| c.contains(" guard claude"))
        .map(str::to_string)
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

/// The MCP server hydra registers in every environment (§3).
pub const HYDRAUI_SERVER: &str = "hydraui";

/// The tools that server offers; Claude Code may call them without asking.
pub const HYDRAUI_TOOLS: [&str; 5] = [
    "tasks_read",
    "tasks_add",
    "tasks_take",
    "tasks_verify",
    "tasks_mine",
];

/// Appends each missing `mcp__hydraui__<tool>` to `permissions.allow` in the settings JSON
/// (creating permissions/allow as needed, keeping every other key and rule). Idempotent.
pub fn add_hydraui_allow(settings_json: &str) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(settings_json).map_err(|e| e.to_string())?;
    let obj = root
        .as_object_mut()
        .ok_or("settings.json isn't a JSON object")?;
    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("settings.json: \"permissions\" isn't an object")?;
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("settings.json: permissions.allow isn't a list")?;
    for tool in HYDRAUI_TOOLS {
        let rule = format!("mcp__{HYDRAUI_SERVER}__{tool}");
        if !allow.iter().any(|r| r.as_str() == Some(&rule)) {
            allow.push(Value::String(rule));
        }
    }
    let mut text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

/// Copies the base's `mcpServers` (minus exclusions) into the environment's
/// `.claude.json`, leaving every other key alone, and adds the `hydraui` server. `Ok(None)` means nothing to write.
/// Each side is `(path, text)`; the path is only used in errors.
pub fn sync_mcp(
    state_json: Option<(&Path, &str)>,
    base_global_json: Option<(&Path, &str)>,
    exclude: &[String],
) -> Result<Option<String>, String> {
    let mut servers = match base_global_json {
        Some((path, text)) => match parse_object(text, path, MAY_BE_WRITING)?.get("mcpServers") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        },
        None => Map::new(),
    };
    servers.insert(
        HYDRAUI_SERVER.to_string(),
        serde_json::json!({
            "type": "http",
            "url": "${HYDRA_MCP_URL}",
            "headers": {"Authorization": "Bearer ${HYDRA_MCP_TOKEN}"},
        }),
    );
    drop_excluded(&mut servers, exclude);
    let mut state = match state_json {
        Some((path, text)) => parse_object(text, path, MAY_BE_WRITING)?,
        None if servers.is_empty() => return Ok(None),
        None => Value::Object(Map::new()),
    };
    let wanted = Value::Object(servers);
    if state.get("mcpServers") == Some(&wanted) {
        return Ok(None);
    }
    state
        .as_object_mut()
        .expect("checked object")
        .insert("mcpServers".to_string(), wanted);
    let mut out = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn every_environment_gets_the_hydraui_server() {
        let out = sync_mcp(None, None, &[]).unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["hydraui"],
            json!({"type": "http", "url": "${HYDRA_MCP_URL}", "headers": {"Authorization": "Bearer ${HYDRA_MCP_TOKEN}"}})
        );
        let base = r#"{"mcpServers":{"hydraui":{"command":"old"},"codegraph":{}}}"#;
        let v: Value = serde_json::from_str(
            &sync_mcp(None, Some((Path::new("b"), base)), &[])
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["mcpServers"]["hydraui"]["type"], "http");
        assert!(v["mcpServers"]["codegraph"].is_object());
        assert_eq!(sync_mcp(None, None, &["hydraui".into()]).unwrap(), None);
    }

    #[test]
    fn hydraui_tools_are_allowed_once_keeping_the_users_rules() {
        let user = r#"{"permissions":{"allow":["Bash(git status)"],"deny":["Read(.env)"]}}"#;
        let once = add_hydraui_allow(user).unwrap();
        let twice = add_hydraui_allow(&once).unwrap();
        assert_eq!(once, twice);
        let v: Value = serde_json::from_str(&once).unwrap();
        assert_eq!(v["permissions"]["deny"], json!(["Read(.env)"]));
        assert_eq!(
            v["permissions"]["allow"],
            json!([
                "Bash(git status)",
                "mcp__hydraui__tasks_read",
                "mcp__hydraui__tasks_add",
                "mcp__hydraui__tasks_take",
                "mcp__hydraui__tasks_verify",
                "mcp__hydraui__tasks_mine"
            ])
        );
        assert!(
            add_hydraui_allow("{}")
                .unwrap()
                .contains("mcp__hydraui__tasks_take")
        );
    }

    #[test]
    fn status_hooks_are_added_once_keeping_other_hooks() {
        let user = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"notify-me"}]}]}}"#;
        let once = add_status_hooks(user, "\"C:/h/hydra.exe\"").unwrap();
        // Another hydra's path replaces the old hooks rather than adding more.
        let again = add_status_hooks(&once, "hydra").unwrap();
        let v: Value = serde_json::from_str(&again).unwrap();
        assert_eq!(
            v["hooks"]["Stop"],
            json!([
                {"hooks": [{"type": "command", "command": "notify-me"}]},
                {"hooks": [{"type": "command", "command": "hydra hook idle"}]}
            ])
        );
        assert_eq!(
            v["hooks"]["Notification"],
            json!([{
                "matcher": "permission_prompt|elicitation_dialog",
                "hooks": [{"type": "command", "command": "hydra hook waiting"}]
            }])
        );
        assert_eq!(add_status_hooks(&again, "hydra").unwrap(), again);
        for (event, _, state) in STATUS_HOOKS {
            let cmd = &v["hooks"][event].as_array().unwrap().last().unwrap()["hooks"][0]["command"];
            assert_eq!(cmd, &json!(format!("hydra hook {state}")), "{event}");
        }
        assert!(!is_status_hook("hydra hook later"));
        assert!(!is_status_hook("\"C:/h/hydra.exe\" guard claude"));
    }

    #[test]
    fn guard_hook_is_added_to_empty_settings() {
        let out = add_guard_hook("{}", "\"C:/h/hydra.exe\" guard claude").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hooks"]["PreToolUse"],
            json!([{"matcher": "Bash|PowerShell", "hooks": [{"type": "command", "command": "\"C:/h/hydra.exe\" guard claude"}]}])
        );
    }

    #[test]
    fn guard_hook_from_an_older_hydra_is_replaced() {
        let before = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"\"C:/h/hydra.exe\" guard claude"}]}]}}"#;
        let out = add_guard_hook(before, "\"C:/h/hydra.exe\" guard claude").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"], "Bash|PowerShell");
    }

    #[test]
    fn guard_hook_keeps_existing_hooks() {
        let before = r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"mine.sh"}]}],"Stop":[]}}"#;
        let out = add_guard_hook(before, "h guard claude").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "mine.sh");
        assert_eq!(v["model"], "opus");
        assert_eq!(v["hooks"]["Stop"], json!([]));
    }

    #[test]
    fn finds_the_guard_hook_command() {
        let with = add_guard_hook("{}", "\"C:/x/hydra.exe\" guard claude").unwrap();
        assert_eq!(
            guard_hook_command(&with).as_deref(),
            Some("\"C:/x/hydra.exe\" guard claude")
        );
        assert_eq!(guard_hook_command("{}"), None);
        assert_eq!(guard_hook_command("not json"), None);
    }

    #[test]
    fn guard_hook_twice_is_not_duplicated_and_is_replaced() {
        let once = add_guard_hook("{}", "old guard claude").unwrap();
        let twice = add_guard_hook(&once, "new guard claude").unwrap();
        let v: Value = serde_json::from_str(&twice).unwrap();
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], "new guard claude");
    }

    fn base_path() -> PathBuf {
        PathBuf::from("C:/u/.claude/settings.json")
    }
    fn over_path() -> PathBuf {
        PathBuf::from("C:/h/envs/work/claude/settings.json")
    }
    fn state_path() -> PathBuf {
        PathBuf::from("C:/h/state/work/claude/.claude.json")
    }
    fn global_path() -> PathBuf {
        PathBuf::from("C:/u/.claude.json")
    }

    fn merge(
        base: Option<&str>,
        over: Option<&str>,
        exclude: &[String],
    ) -> Result<MergedSettings, String> {
        let (b, o) = (base_path(), over_path());
        merge_settings(
            base.map(|t| (b.as_path(), t)),
            over.map(|t| (o.as_path(), t)),
            exclude,
        )
    }

    fn sync(
        state: Option<&str>,
        global: Option<&str>,
        exclude: &[String],
    ) -> Result<Option<String>, String> {
        let (s, g) = (state_path(), global_path());
        sync_mcp(
            state.map(|t| (s.as_path(), t)),
            global.map(|t| (g.as_path(), t)),
            exclude,
        )
    }

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
        let m = merge(Some(base), Some(over), &["acme*".to_string()]).unwrap();
        assert!(m.dropped.is_empty());
        let out = m.text;
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["model"], "sonnet");
        assert_eq!(v["permissions"]["allow"][0], "Bash");
        assert!(v["mcpServers"].get("acme-x").is_none());
        assert!(v["mcpServers"].get("codegraph").is_some());
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn settings_merge_without_files_is_an_empty_object() {
        assert_eq!(merge(None, None, &[]).unwrap().text, "{}\n");
    }

    #[test]
    fn settings_merge_reports_bad_json_with_the_full_path() {
        let err = merge(Some("{nope"), None, &[]).unwrap_err();
        assert!(err.contains(&base_path().display().to_string()), "{err}");
        let err = merge(None, Some("[1]"), &[]).unwrap_err();
        assert!(err.contains(&over_path().display().to_string()), "{err}");
        assert!(!err.contains("try again"), "{err}");
    }

    #[test]
    fn sign_in_settings_from_the_base_are_dropped() {
        let base = r#"{"model":"opus","apiKeyHelper":"get-key.sh","env":{"ANTHROPIC_API_KEY":"k","ANTHROPIC_AUTH_TOKEN":"t","CLAUDE_CODE_OAUTH_TOKEN":"o","ANTHROPIC_BASE_URL":"u","CLAUDE_CODE_USE_BEDROCK":"1","CLAUDE_CODE_USE_VERTEX":"1","KEEP_ME":"yes"}}"#;
        let m = merge(Some(base), None, &[]).unwrap();
        let v: Value = serde_json::from_str(&m.text).unwrap();
        assert_eq!(v["model"], "opus");
        assert!(v.get("apiKeyHelper").is_none());
        assert_eq!(v["env"], json!({"KEEP_ME": "yes"}));
        assert_eq!(
            m.dropped,
            vec![
                "apiKeyHelper",
                "env.ANTHROPIC_API_KEY",
                "env.ANTHROPIC_AUTH_TOKEN",
                "env.CLAUDE_CODE_OAUTH_TOKEN",
                "env.ANTHROPIC_BASE_URL",
                "env.CLAUDE_CODE_USE_BEDROCK",
                "env.CLAUDE_CODE_USE_VERTEX",
            ]
        );
    }

    #[test]
    fn sign_in_settings_from_the_environment_are_kept() {
        let base = r#"{"apiKeyHelper":"base.sh","env":{"ANTHROPIC_BASE_URL":"base"}}"#;
        let over = r#"{"apiKeyHelper":"work.sh","env":{"ANTHROPIC_API_KEY":"work","ANTHROPIC_BASE_URL":"work"}}"#;
        let m = merge(Some(base), Some(over), &[]).unwrap();
        let v: Value = serde_json::from_str(&m.text).unwrap();
        assert_eq!(v["apiKeyHelper"], "work.sh");
        assert_eq!(
            v["env"],
            json!({"ANTHROPIC_API_KEY": "work", "ANTHROPIC_BASE_URL": "work"})
        );
        assert_eq!(m.dropped, vec!["apiKeyHelper", "env.ANTHROPIC_BASE_URL"]);
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
        let out = sync(Some(state), Some(base), &["acme*".to_string()])
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["oauthAccount"]["emailAddress"], "a@b.c",
            "account never copied from base"
        );
        assert_eq!(v["projects"]["E:/x"]["k"], 1);
        assert_eq!(
            v["mcpServers"],
            serde_json::json!({"codegraph": {"command": "cg"}, "hydraui": {"type": "http", "url": "${HYDRA_MCP_URL}", "headers": {"Authorization": "Bearer ${HYDRA_MCP_TOKEN}"}}})
        );
    }

    #[test]
    fn sync_is_a_no_op_when_already_in_sync() {
        let state = r#"{"mcpServers":{"codegraph":{"command":"cg"},"hydraui":{"type":"http","url":"${HYDRA_MCP_URL}","headers":{"Authorization":"Bearer ${HYDRA_MCP_TOKEN}"}}}}"#;
        let base = r#"{"mcpServers":{"codegraph":{"command":"cg"}}}"#;
        assert_eq!(sync(Some(state), Some(base), &[]).unwrap(), None);
    }

    #[test]
    fn sync_creates_state_file_when_missing() {
        let base = r#"{"mcpServers":{"dash":{}}}"#;
        let out = sync(None, Some(base), &[]).unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            serde_json::json!({"mcpServers": {"dash": {}, "hydraui": {"type": "http", "url": "${HYDRA_MCP_URL}", "headers": {"Authorization": "Bearer ${HYDRA_MCP_TOKEN}"}}}})
        );
        assert_eq!(
            sync(None, None, &["hydraui".to_string()]).unwrap(),
            None,
            "nothing to sync, nothing written"
        );
    }

    #[test]
    fn sync_refuses_unparseable_state_file() {
        let err = sync(Some("{\"half"), Some(r#"{"mcpServers":{}}"#), &[]).unwrap_err();
        assert!(
            err.contains(&state_path().display().to_string())
                && err.contains("(if Claude is running it may be writing it; try again)"),
            "{err}"
        );
    }

    #[test]
    fn sync_names_the_base_state_file_in_errors() {
        let err = sync(None, Some("{nope"), &[]).unwrap_err();
        assert!(
            err.contains(&global_path().display().to_string())
                && err.contains("(if Claude is running it may be writing it; try again)"),
            "{err}"
        );
    }
}
