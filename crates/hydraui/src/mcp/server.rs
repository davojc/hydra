//! The localhost MCP endpoint (spec §2): Streamable HTTP with plain JSON replies, bound to
//! loopback only, every request checked for a terminal's bearer token and a local Origin.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};

use super::registry::{Peer, Registry, Terminal};
use super::tools::{INSTRUCTIONS, call_with, list};
use super::{SessionState, Usage};

/// Largest request body read; a tool call is a few lines of JSON.
const MAX_BODY: u64 = 1 << 20;

/// Something the server did that the app should show.
#[derive(Debug, Clone, PartialEq)]
pub enum McpEvent {
    /// A terminal made a project's task board; carries the project folder.
    BoardCreated(PathBuf),
    /// A Claude session's hook (`hydra hook <state>`) said what it is doing: the terminal's
    /// token and the state.
    Session(String, SessionState),
    /// A Claude session's status line (`hydra statusline`) said how much of its account's
    /// limits are used: the terminal's token and the usage.
    Usage(String, Usage),
}

/// What `handle` answers: an HTTP status and, for 200, the JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Option<String>,
}

fn status(status: u16) -> Reply {
    Reply { status, body: None }
}

/// Locks `m`, even after a panic poisoned it: the registry stays usable (and revocable).
pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// True for `http://127.0.0.1` or `http://localhost`, each optionally followed by `:<digits>`.
fn local_origin(origin: &str) -> bool {
    ["http://127.0.0.1", "http://localhost"]
        .iter()
        .filter_map(|host| origin.strip_prefix(host))
        .any(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix(':')
                    .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn json_reply(v: Value) -> Reply {
    Reply {
        status: 200,
        body: Some(v.to_string()),
    }
}

fn ok(id: &Value, result: Value) -> Reply {
    json_reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn fail(id: &Value, code: i64, message: &str) -> Reply {
    json_reply(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

/// One request, without any I/O: `gate`, then `answer`.
pub fn handle(
    registry: &Mutex<Registry>,
    method: &str,
    path: &str,
    auth: Option<&str>,
    origin: Option<&str>,
    body: &str,
) -> (Reply, Option<McpEvent>) {
    match gate(registry, method, path, auth, origin) {
        Ok(terminal) => reply_to(registry, &terminal, bearer(auth), body),
        Err(reply) => (reply, None),
    }
}

/// `answer`s the message from the terminal holding `token`, with the other Claude sessions it
/// may ask about, and keeps the registry's record of what each session is doing.
fn reply_to(
    registry: &Mutex<Registry>,
    terminal: &Terminal,
    token: &str,
    body: &str,
) -> (Reply, Option<McpEvent>) {
    let peers = lock(registry).peers(token);
    let (reply, event) = answer(terminal, token, &peers, body);
    if let Some(McpEvent::Session(token, state)) = &event {
        lock(registry).set_session(token, *state);
    }
    (reply, event)
}

/// The token in an `Authorization: Bearer <token>` header; empty without one.
fn bearer(auth: Option<&str>) -> &str {
    auth.and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("")
}

/// Checks the path, Origin, token and method from the headers alone, so a stranger is refused
/// before its body is read; the caller's terminal if it may go on.
pub fn gate(
    registry: &Mutex<Registry>,
    method: &str,
    path: &str,
    auth: Option<&str>,
    origin: Option<&str>,
) -> Result<Terminal, Reply> {
    if path != "/mcp" {
        return Err(status(404));
    }
    if origin.is_some_and(|o| !local_origin(o)) {
        return Err(status(403));
    }
    let terminal = Some(bearer(auth))
        .filter(|t| !t.is_empty())
        .and_then(|t| lock(registry).get(t))
        .ok_or(status(401))?;
    if method != "POST" {
        return Err(status(405));
    }
    Ok(terminal)
}

/// Answers the JSON-RPC message in `body` for a terminal `gate` let through, whose token is
/// `token`.
pub fn answer(
    terminal: &Terminal,
    token: &str,
    peers: &[Peer],
    body: &str,
) -> (Reply, Option<McpEvent>) {
    let msg: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return (fail(&Value::Null, -32700, "parse error"), None),
    };
    let Some(obj) = msg.as_object() else {
        return (
            fail(&Value::Null, -32600, "expected one JSON-RPC message"),
            None,
        );
    };
    let params = obj.get("params").unwrap_or(&Value::Null);
    let Some(id) = obj.get("id") else {
        // A notification. hydra's Claude hooks send `hydraui/status` with the session's state,
        // its status line `hydraui/usage` with Claude's status JSON.
        let event = match obj.get("method").and_then(Value::as_str) {
            Some("hydraui/status") => params
                .get("state")
                .and_then(Value::as_str)
                .and_then(SessionState::parse)
                .map(|state| McpEvent::Session(token.to_string(), state)),
            Some("hydraui/usage") => Some(McpEvent::Usage(token.to_string(), Usage::parse(params))),
            _ => None,
        };
        return (status(202), event);
    };
    match obj.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let version = params
                .get("protocolVersion")
                .cloned()
                .unwrap_or(Value::Null);
            let result = json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "hydraui", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            });
            (ok(id, result), None)
        }
        Some("ping") => (ok(id, json!({})), None),
        Some("tools/list") => (ok(id, json!({"tools": list()})), None),
        Some("tools/call") => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let empty = json!({});
            let args = params.get("arguments").unwrap_or(&empty);
            match call_with(terminal, peers, name, args) {
                None => (fail(id, -32602, "unknown tool"), None),
                Some(out) => {
                    let result = json!({
                        "content": [{"type": "text", "text": out.text}],
                        "isError": out.is_error,
                    });
                    (ok(id, result), out.created.map(McpEvent::BoardCreated))
                }
            }
        }
        _ => (fail(id, -32601, "method not found"), None),
    }
}

pub struct Server {
    url: String,
    registry: Arc<Mutex<Registry>>,
    events: Mutex<Vec<UnboundedSender<McpEvent>>>,
}

impl Server {
    /// Binds `127.0.0.1` on a free port and serves on a background thread.
    pub fn start() -> std::io::Result<Arc<Server>> {
        let http = tiny_http::Server::http("127.0.0.1:0").map_err(std::io::Error::other)?;
        let port = http
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .ok_or_else(|| std::io::Error::other("not an IP socket"))?;
        let server = Arc::new(Server {
            url: format!("http://127.0.0.1:{port}/mcp"),
            registry: Arc::default(),
            events: Mutex::default(),
        });
        let worker = server.clone();
        std::thread::Builder::new()
            .name("hydraui-mcp".into())
            .spawn(move || {
                for request in http.incoming_requests() {
                    worker.serve(request);
                }
            })?;
        Ok(server)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn registry(&self) -> &Mutex<Registry> {
        &self.registry
    }

    pub fn subscribe(&self) -> UnboundedReceiver<McpEvent> {
        let (tx, rx) = unbounded();
        lock(&self.events).push(tx);
        rx
    }

    fn serve(&self, mut request: tiny_http::Request) {
        let header = |name: &'static str| {
            request
                .headers()
                .iter()
                .find(|h| h.field.equiv(name))
                .map(|h| h.value.as_str().to_string())
        };
        let (auth, origin) = (header("Authorization"), header("Origin"));
        let method = request.method().as_str().to_string();
        let path = request.url().split('?').next().unwrap_or("").to_string();
        let gated = catch_unwind(AssertUnwindSafe(|| {
            gate(
                &self.registry,
                &method,
                &path,
                auth.as_deref(),
                origin.as_deref(),
            )
        }));
        let terminal = match gated {
            Ok(Ok(terminal)) => terminal,
            Ok(Err(reply)) => return refuse(request, reply),
            Err(_) => return refuse(request, status(500)),
        };
        let mut body = String::new();
        let read = request.as_reader().take(MAX_BODY).read_to_string(&mut body);
        let handled = match read {
            Err(_) => Some((status(400), None)),
            Ok(_) => catch_unwind(AssertUnwindSafe(|| {
                reply_to(&self.registry, &terminal, bearer(auth.as_deref()), &body)
            }))
            .ok(),
        };
        let (reply, event) = handled.unwrap_or_else(|| (status(500), None));
        if let Some(event) = event {
            lock(&self.events).retain(|tx| tx.unbounded_send(event.clone()).is_ok());
        }
        respond(request, reply);
    }
}

/// Dropping a request reads the rest of its body, which a stranger may never send: refuse it on
/// a thread of its own so the serving thread stays free.
fn refuse(request: tiny_http::Request, reply: Reply) {
    let _ = std::thread::Builder::new()
        .name("hydraui-mcp-refuse".into())
        .spawn(move || respond(request, reply));
}

fn respond(request: tiny_http::Request, reply: Reply) {
    let _ = match reply.body {
        Some(text) => request.respond(
            tiny_http::Response::from_string(text).with_header(
                tiny_http::Header::from_bytes("Content-Type", "application/json")
                    .expect("a valid header"),
            ),
        ),
        None => {
            request.respond(tiny_http::Response::from_string("").with_status_code(reply.status))
        }
    };
}

static GLOBAL: OnceLock<Option<Arc<Server>>> = OnceLock::new();

/// Starts the app's server on first call; `None` if it could not bind.
pub fn init() -> Option<&'static Arc<Server>> {
    GLOBAL.get_or_init(|| Server::start().ok()).as_ref()
}

/// The app's server, if `init` started it.
pub fn get() -> Option<&'static Arc<Server>> {
    GLOBAL.get().and_then(Option::as_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    /// A folder that is a repo root, so project lookup stays inside it.
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        d
    }

    #[test]
    fn refuses_strangers() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let tok = reg.lock().unwrap().register("c", dir.path().to_path_buf());
        let ok = format!("Bearer {tok}");
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        assert_eq!(handle(&reg, "POST", "/mcp", None, None, init).0.status, 401);
        assert_eq!(
            handle(&reg, "POST", "/mcp", Some("Bearer nope"), None, init)
                .0
                .status,
            401
        );
        assert_eq!(
            handle(
                &reg,
                "POST",
                "/mcp",
                Some(&ok),
                Some("https://evil.example"),
                init
            )
            .0
            .status,
            403
        );
        assert_eq!(
            handle(
                &reg,
                "POST",
                "/mcp",
                Some(&ok),
                Some("http://localhost.evil.com"),
                init
            )
            .0
            .status,
            403
        );
        assert_eq!(
            handle(
                &reg,
                "POST",
                "/mcp",
                Some(&ok),
                Some("http://localhost:5173"),
                init
            )
            .0
            .status,
            200
        );
        assert_eq!(
            handle(&reg, "POST", "/other", Some(&ok), None, init)
                .0
                .status,
            404
        );
        assert_eq!(
            handle(&reg, "GET", "/mcp", Some(&ok), None, "").0.status,
            405
        );
        reg.lock().unwrap().revoke(&tok);
        assert_eq!(
            handle(&reg, "POST", "/mcp", Some(&ok), None, init).0.status,
            401
        );
    }

    #[test]
    fn a_session_sees_the_others_that_said_they_run() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let me = reg.lock().unwrap().register("me", dir.path().to_path_buf());
        let other = reg
            .lock()
            .unwrap()
            .register("other", dir.path().join("src"));
        let auth = |t: &str| format!("Bearer {t}");
        let status = r#"{"jsonrpc":"2.0","method":"hydraui/status","params":{"state":"working"}}"#;
        handle(&reg, "POST", "/mcp", Some(&auth(&other)), None, status);
        let read = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tasks_read","arguments":{}}}"#;
        let others = |token: &str| -> Value {
            let reply = handle(&reg, "POST", "/mcp", Some(&auth(token)), None, read).0;
            let msg: Value = serde_json::from_str(&reply.body.unwrap()).unwrap();
            let text = msg["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string();
            serde_json::from_str::<Value>(&text).unwrap()["other_sessions"].clone()
        };
        assert_eq!(others(&me)[0]["name"], "other");
        assert_eq!(others(&other), serde_json::json!([]), "not itself");
        let ended = r#"{"jsonrpc":"2.0","method":"hydraui/status","params":{"state":"ended"}}"#;
        handle(&reg, "POST", "/mcp", Some(&auth(&other)), None, ended);
        assert_eq!(others(&me), serde_json::json!([]));
    }

    #[test]
    fn a_hooks_status_names_its_terminal() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let token = reg.lock().unwrap().register("c", dir.path().to_path_buf());
        let auth = format!("Bearer {token}");
        let send = |body: &str| handle(&reg, "POST", "/mcp", Some(&auth), None, body);
        let (reply, event) =
            send(r#"{"jsonrpc":"2.0","method":"hydraui/status","params":{"state":"waiting"}}"#);
        assert_eq!(reply.status, 202);
        assert_eq!(
            event,
            Some(McpEvent::Session(token.clone(), SessionState::Waiting))
        );
        let (_, event) =
            send(r#"{"jsonrpc":"2.0","method":"hydraui/status","params":{"state":"asleep"}}"#);
        assert_eq!(event, None, "an unknown state");
        let (_, event) = send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(event, None);
        let (_, event) = send(
            r#"{"jsonrpc":"2.0","method":"hydraui/usage","params":{"rate_limits":{"seven_day":{"used_percentage":80}}}}"#,
        );
        let Some(McpEvent::Usage(t, u)) = event else {
            panic!("a usage event: {event:?}");
        };
        assert_eq!(
            (t, u.seven_day.map(|l| l.used)),
            (token.clone(), Some(80.0))
        );
        let (reply, event) = handle(
            &reg,
            "POST",
            "/mcp",
            Some("Bearer nope"),
            None,
            r#"{"jsonrpc":"2.0","method":"hydraui/status","params":{"state":"idle"}}"#,
        );
        assert_eq!((reply.status, event), (401, None), "a stranger");
    }

    #[test]
    fn speaks_json_rpc() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let tok = format!(
            "Bearer {}",
            reg.lock().unwrap().register("c", dir.path().to_path_buf())
        );
        let rpc = |body: &str| handle(&reg, "POST", "/mcp", Some(&tok), None, body).0;
        let init: Value = serde_json::from_str(&rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#).body.unwrap()).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("Pickup")
        );
        assert_eq!(
            rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).status,
            202
        );
        let list: Value = serde_json::from_str(
            &rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
                .body
                .unwrap(),
        )
        .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 5);
        let bad: Value = serde_json::from_str(&rpc(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tasks_delete","arguments":{}}}"#).body.unwrap()).unwrap();
        assert_eq!(bad["error"]["code"], -32602);
        let junk: Value = serde_json::from_str(&rpc("{not json").body.unwrap()).unwrap();
        assert_eq!(junk["error"]["code"], -32700);
    }

    #[test]
    fn strangers_are_refused_before_the_body() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let tok = format!(
            "Bearer {}",
            reg.lock().unwrap().register("c", dir.path().to_path_buf())
        );
        let refused = |auth: Option<&str>, origin: Option<&str>| {
            gate(&reg, "POST", "/mcp", auth, origin)
                .err()
                .map(|r| r.status)
        };
        assert_eq!(refused(None, None), Some(401));
        assert_eq!(refused(Some(&tok), Some("https://evil.example")), Some(403));
        assert_eq!(refused(Some(&tok), None), None);
    }

    #[test]
    fn a_tool_that_makes_a_board_says_so() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let tok = format!(
            "Bearer {}",
            reg.lock().unwrap().register("c", dir.path().to_path_buf())
        );
        let add = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tasks_add","arguments":{"text":"x"}}}"#;
        let (reply, event) = handle(&reg, "POST", "/mcp", Some(&tok), None, add);
        assert_eq!(reply.status, 200);
        assert_eq!(
            event,
            Some(McpEvent::BoardCreated(dir.path().to_path_buf()))
        );
    }

    #[test]
    fn an_idle_stranger_does_not_stall_the_server() {
        use std::io::Write;
        let dir = repo();
        let server = Server::start().unwrap();
        let tok = server
            .registry()
            .lock()
            .unwrap()
            .register("c", dir.path().to_path_buf());
        let addr = server
            .url()
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        // Promises a body too big to be read up front, then never sends it.
        let mut idle = std::net::TcpStream::connect(&addr).unwrap();
        idle.write_all(b"POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 2000\r\n\r\n")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let resp = ureq::post(server.url())
            .timeout(std::time::Duration::from_secs(5))
            .set("Authorization", &format!("Bearer {tok}"))
            .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .unwrap();
        assert_eq!(resp.status(), 200);
        drop(idle);
    }

    #[test]
    fn serves_over_http() {
        let dir = repo();
        let server = Server::start().unwrap();
        let tok = server
            .registry()
            .lock()
            .unwrap()
            .register("c", dir.path().to_path_buf());
        let rpc = |body: &str| -> Value {
            let resp = ureq::post(server.url())
                .set("Authorization", &format!("Bearer {tok}"))
                .set("Content-Type", "application/json")
                .send_string(body)
                .unwrap();
            assert_eq!(resp.status(), 200);
            serde_json::from_str(&resp.into_string().unwrap()).unwrap()
        };
        let init = rpc(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "hydraui");
        let list = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        assert!(list.to_string().contains("tasks_take"));
        let mut events = server.subscribe();
        let add = rpc(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tasks_add","arguments":{"text":"x"}}}"#,
        );
        assert_eq!(add["result"]["isError"], false);
        assert_eq!(
            events.try_recv().ok(),
            Some(McpEvent::BoardCreated(dir.path().to_path_buf()))
        );
        assert!(server.url().starts_with("http://127.0.0.1:") && server.url().ends_with("/mcp"));
    }
}
