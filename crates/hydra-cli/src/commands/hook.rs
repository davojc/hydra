//! `hydra hook <state>`: Claude Code's hooks in a hydraui terminal tell hydraui what the session
//! is doing, so its terminal list can show it. Outside hydraui (no `HYDRA_MCP_URL`) it does
//! nothing; every failure is silent, so a hook never gets in the session's way.
use std::time::Duration;

/// How long hydraui gets to take the message; it is on this machine.
const TIMEOUT: Duration = Duration::from_millis(500);

/// The JSON-RPC notification hydraui's server reads for `state`.
pub fn message(state: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "hydraui/status",
        "params": {"state": state},
    })
    .to_string()
}

pub fn run(state: &str) {
    let (Ok(url), Ok(token)) = (
        std::env::var("HYDRA_MCP_URL"),
        std::env::var("HYDRA_MCP_TOKEN"),
    ) else {
        return;
    };
    let _ = ureq::post(&url)
        .timeout(TIMEOUT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_string(&message(state));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_is_a_status_notification() {
        let v: serde_json::Value = serde_json::from_str(&message("waiting")).unwrap();
        assert_eq!(v["method"], "hydraui/status");
        assert_eq!(v["params"]["state"], "waiting");
        assert!(v.get("id").is_none(), "a notification has no id");
    }
}
