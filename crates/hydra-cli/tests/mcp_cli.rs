mod common;
use common::Home;

const INPUT: &str = concat!(
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    "\n",
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    "\n\n",
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    "\n",
);

fn lines(out: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn outside_hydraui_it_answers_as_a_server_with_no_tools() {
    let h = Home::new();
    let out = h
        .hydra()
        .arg("mcp")
        .env_remove("HYDRA_MCP_URL")
        .env_remove("HYDRA_MCP_TOKEN")
        .write_stdin(INPUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let replies = lines(&out);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "hydraui");
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(replies[1]["result"]["tools"], serde_json::json!([]));
}

#[test]
fn a_closed_hydraui_answers_requests_with_an_error_and_notifications_with_nothing() {
    let h = Home::new();
    let out = h
        .hydra()
        .arg("mcp")
        .env("HYDRA_MCP_URL", "http://127.0.0.1:9/mcp")
        .env("HYDRA_MCP_TOKEN", "x")
        .write_stdin(INPUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let replies = lines(&out);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["error"]["code"], -32000);
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(
        replies[1]["error"]["message"],
        "hydraui isn't reachable (closed?)"
    );
}

#[test]
fn a_line_that_isnt_json_gets_a_parse_error() {
    let h = Home::new();
    let out = h
        .hydra()
        .arg("mcp")
        .env_remove("HYDRA_MCP_URL")
        .env_remove("HYDRA_MCP_TOKEN")
        .write_stdin("nope\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let replies = lines(&out);
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert!(replies[0]["id"].is_null());
}

#[test]
fn it_is_hidden_from_help() {
    let h = Home::new();
    let out = h
        .hydra()
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&out).contains("mcp"));
}
