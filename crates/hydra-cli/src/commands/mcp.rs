//! `hydra mcp`: the stdio MCP server Claude Code runs. Inside hydraui it relays each
//! JSON-RPC line to hydraui's MCP endpoint; outside, it answers as a server with no tools.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

const FALLBACK_PROTOCOL: &str = "2025-06-18";

/// A JSON-RPC error reply; `id` is `Value::Null` when the request's id is unknown.
fn error_reply(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn parse_error() -> String {
    error_reply(Value::Null, -32700, "Parse error")
}

/// The reply for one stdin line when not running inside hydraui. `None` for a
/// notification (a message without an `id`).
pub fn local_reply(line: &str) -> Option<String> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(parse_error());
    };
    let id = msg.get("id")?.clone();
    let result = match msg.get("method").and_then(Value::as_str) {
        Some("initialize") => json!({
            "protocolVersion": msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "hydraui", "version": crate::VERSION},
            "instructions": "Not running inside hydraui: no tools.",
        }),
        Some("tools/list") => json!({"tools": []}),
        Some("ping") => json!({}),
        _ => return Some(error_reply(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string())
}

/// Posts `line` to hydraui. `None` for a notification or a 202; an error reply when
/// hydraui can't be reached or answers anything else.
fn relay_reply(agent: &ureq::Agent, url: &str, token: &str, line: &str) -> Option<String> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(parse_error());
    };
    let id = msg.get("id").cloned();
    let sent = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .set("Authorization", &format!("Bearer {token}"))
        .send_string(line);
    let body = match sent {
        Ok(resp) if resp.status() == 202 => return None,
        Ok(resp) if resp.status() == 200 => resp.into_string().ok(),
        _ => None,
    };
    // Re-serialised so a newline inside the body can't break the line framing.
    let compact = body
        .and_then(|b| serde_json::from_str::<Value>(&b).ok())
        .map(|v| v.to_string());
    match compact {
        Some(c) => Some(c),
        None => id.map(|id| error_reply(id, -32000, "hydraui isn't reachable (closed?)")),
    }
}

/// `hydra mcp`: serves stdin until EOF.
pub fn run() -> anyhow::Result<i32> {
    let target = match (
        std::env::var("HYDRA_MCP_URL"),
        std::env::var("HYDRA_MCP_TOKEN"),
    ) {
        (Ok(url), Ok(token)) => Some((url, token)),
        _ => None,
    };
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(30))
        .build();
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match &target {
            Some((url, token)) => relay_reply(&agent, url, token, &line),
            None => local_reply(&line),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn initialize_echoes_the_clients_version_and_has_empty_tools() {
        let r = v(&local_reply(
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .unwrap());
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["capabilities"]["tools"], json!({}));
        assert_eq!(r["result"]["serverInfo"]["version"], crate::VERSION);
    }

    #[test]
    fn initialize_without_a_version_falls_back() {
        let r = v(&local_reply(r#"{"id":1,"method":"initialize"}"#).unwrap());
        assert_eq!(r["result"]["protocolVersion"], FALLBACK_PROTOCOL);
    }

    #[test]
    fn tools_list_and_ping() {
        let r = v(&local_reply(r#"{"id":1,"method":"tools/list"}"#).unwrap());
        assert_eq!(r["result"]["tools"], json!([]));
        let r = v(&local_reply(r#"{"id":2,"method":"ping"}"#).unwrap());
        assert_eq!(r["result"], json!({}));
    }

    #[test]
    fn a_notification_gets_no_reply() {
        assert_eq!(
            local_reply(r#"{"method":"notifications/initialized"}"#),
            None
        );
    }

    #[test]
    fn an_unknown_request_is_method_not_found() {
        let r = v(&local_reply(r#"{"id":"a","method":"nope"}"#).unwrap());
        assert_eq!(r["id"], "a");
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        let r = v(&local_reply("{").unwrap());
        assert_eq!(r["error"]["code"], -32700);
        assert!(r["id"].is_null());
    }
}
