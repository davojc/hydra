//! `hydra statusline [command]`: Claude Code's status line in a hydraui terminal. It passes the
//! session's status JSON (stdin) to hydraui, so the status bar can show the environment's usage,
//! then prints a status line: the user's own `command`'s, run with the same JSON, else a short
//! one of hydra's. Outside hydraui (no `HYDRA_MCP_URL`) only the printing happens; failures to
//! reach hydraui are silent.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

const TIMEOUT: Duration = Duration::from_millis(500);

/// The JSON-RPC notification hydraui's server reads for a status update.
pub fn message(status: &Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "method": "hydraui/usage", "params": status}).to_string()
}

/// hydra's own status line: the model, then how full the context is and the rate limits used,
/// where Claude gave them.
pub fn line(status: &Value) -> String {
    let pct = |v: &Value| v.as_f64().map(|p| format!("{}%", p.round()));
    let mut parts = Vec::new();
    if let Some(model) = status["model"]["display_name"].as_str() {
        parts.push(model.to_string());
    }
    if let Some(p) = pct(&status["context_window"]["used_percentage"]) {
        parts.push(format!("context {p}"));
    }
    for (key, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
        if let Some(p) = pct(&status["rate_limits"][key]["used_percentage"]) {
            parts.push(format!("{label} {p}"));
        }
    }
    parts.join(" · ")
}

fn tell_hydraui(status: &Value) {
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
        .send_string(&message(status));
}

/// Runs the user's status line `command` with `input` on its stdin, in the shell Claude Code
/// uses for it (Git Bash, else cmd), and passes its output on. Its exit code, 1 if it didn't run.
fn run_theirs(command: &str, input: &[u8]) -> i32 {
    // Claude Code's own setting first, then Git Bash where hydra finds it for shells; never a
    // bare `bash`, which on Windows can be WSL's.
    let bash = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            hydra_platform::shell::find_shell(hydra_platform::shell::ShellKind::Bash, None)
        });
    let child = match bash {
        Some(bash) => Command::new(bash)
            .args(["-c", command])
            .stdin(Stdio::piped())
            .spawn(),
        None => Command::new("cmd")
            .args(["/C", command])
            .stdin(Stdio::piped())
            .spawn(),
    };
    let Ok(mut child) = child else {
        return 1;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

pub fn run(theirs: Option<&str>) -> i32 {
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let status: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
    if !status.is_null() {
        tell_hydraui(&status);
    }
    match theirs {
        Some(command) => run_theirs(command, &input),
        None => {
            println!("{}", line(&status));
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_shows_what_claude_gave() {
        let full = serde_json::json!({
            "model": {"display_name": "Opus"},
            "context_window": {"used_percentage": 41.6},
            "rate_limits": {"five_hour": {"used_percentage": 12.2}, "seven_day": {"used_percentage": 70}},
        });
        assert_eq!(line(&full), "Opus · context 42% · 5h 12% · 7d 70%");
        let early = serde_json::json!({"model": {"display_name": "Opus"}, "context_window": {"used_percentage": null}});
        assert_eq!(line(&early), "Opus");
        assert_eq!(line(&Value::Null), "");
    }

    #[test]
    fn the_message_is_a_usage_notification_carrying_the_status() {
        let v: Value = serde_json::from_str(&message(&serde_json::json!({"a": 1}))).unwrap();
        assert_eq!(v["method"], "hydraui/usage");
        assert_eq!(v["params"]["a"], 1);
        assert!(v.get("id").is_none());
    }
}
