//! `hydra hook <state>`: Claude Code's hooks in a hydraui terminal tell hydraui what the session
//! is doing, so its terminal list can show it, and `hydra hook skill` which skill it invokes.
//! Outside hydraui (no `HYDRA_MCP_URL`) it does nothing; every failure is silent, so a hook
//! never gets in the session's way.
use std::io::{IsTerminal, Read};
use std::time::Duration;

/// How long hydraui gets to take the message; it is on this machine.
const TIMEOUT: Duration = Duration::from_millis(500);

/// The JSON-RPC notification hydraui's server reads for `state`, with Claude's `session_id` from the hook's input, when it has one, so
/// hydraui can reopen the session after a restart (orchestrator spec §13).
pub fn status_message(state: &str, hook_input: &str) -> String {
    let session = serde_json::from_str::<serde_json::Value>(hook_input)
        .ok()
        .and_then(|v| v.get("session_id")?.as_str().map(str::to_string))
        .filter(|s| !s.is_empty());
    let mut params = serde_json::json!({"state": state});
    if let Some(session) = session {
        params["session"] = session.into();
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "hydraui/status",
        "params": params,
    })
    .to_string()
}

/// The notification naming the skill a PreToolUse hook on the Skill tool is about, read from
/// the hook's input (`tool_input.skill`, or `command` in older Claude Codes); `None` without
/// one.
pub fn skill_message(hook_input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(hook_input).ok()?;
    let input = v.get("tool_input")?;
    let skill = ["skill", "command"]
        .iter()
        .find_map(|k| input.get(*k).and_then(|s| s.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "hydraui/skill",
            "params": {"skill": skill},
        })
        .to_string(),
    )
}

pub fn run(state: &str) {
    let (Ok(url), Ok(token)) = (
        std::env::var("HYDRA_MCP_URL"),
        std::env::var("HYDRA_MCP_TOKEN"),
    ) else {
        return;
    };
    // Claude pipes the hook's input as JSON; run by hand from a terminal there is none to wait for.
    let mut input = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    }
    let body = if state == "skill" {
        match skill_message(&input) {
            Some(body) => body,
            None => return,
        }
    } else {
        status_message(state, &input)
    };
    let _ = ureq::post(&url)
        .timeout(TIMEOUT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_string(&body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_hook_names_the_skill_claude_invokes() {
        let input =
            r#"{"hook_event_name":"PreToolUse","tool_name":"Skill","tool_input":{"skill":"qa"}}"#;
        let v: serde_json::Value = serde_json::from_str(&skill_message(input).unwrap()).unwrap();
        assert_eq!(v["method"], "hydraui/skill");
        assert_eq!(v["params"]["skill"], "qa");
        assert!(v.get("id").is_none());
        // Older Claude Codes name it `command`.
        let older = r#"{"tool_name":"Skill","tool_input":{"command":"qa-pipeline"}}"#;
        let v: serde_json::Value = serde_json::from_str(&skill_message(older).unwrap()).unwrap();
        assert_eq!(v["params"]["skill"], "qa-pipeline");
        assert_eq!(skill_message(r#"{"tool_input":{}}"#), None);
        assert_eq!(skill_message("not json"), None);
    }

    #[test]
    fn the_message_is_a_status_notification() {
        let v: serde_json::Value = serde_json::from_str(&status_message("waiting", "")).unwrap();
        assert_eq!(v["method"], "hydraui/status");
        assert_eq!(v["params"]["state"], "waiting");
        assert!(v.get("id").is_none(), "a notification has no id");
        assert!(v["params"].get("session").is_none());
    }

    #[test]
    fn the_status_carries_claudes_session_id_from_the_hook_input() {
        let input = r#"{"session_id":"5c1e-77","hook_event_name":"Stop"}"#;
        let v: serde_json::Value = serde_json::from_str(&status_message("idle", input)).unwrap();
        assert_eq!(v["params"]["state"], "idle");
        assert_eq!(v["params"]["session"], "5c1e-77");
        // No input (or not JSON): the state alone.
        let v: serde_json::Value = serde_json::from_str(&status_message("idle", "")).unwrap();
        assert!(v["params"].get("session").is_none());
    }
}
