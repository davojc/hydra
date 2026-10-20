//! The localhost MCP endpoint (spec §2): Streamable HTTP with plain JSON replies, bound to
//! loopback only, every request checked for a terminal's bearer token and a local Origin.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};

use super::registry::Registry;
use super::tools::{INSTRUCTIONS, call, list};

/// Largest request body read; a tool call is a few lines of JSON.
const MAX_BODY: u64 = 1 << 20;

/// Something the server did that the app should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpEvent {
    /// A terminal made a project's task board; carries the project folder.
    BoardCreated(PathBuf),
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
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

/// One request, without any I/O: checks the path, Origin, token and method, then answers the
/// JSON-RPC message in `body`.
pub fn handle(
    registry: &Mutex<Registry>,
    method: &str,
    path: &str,
    auth: Option<&str>,
    origin: Option<&str>,
    body: &str,
) -> (Reply, Option<McpEvent>) {
    if path != "/mcp" {
        return (status(404), None);
    }
    if origin.is_some_and(|o| !local_origin(o)) {
        return (status(403), None);
    }
    let terminal = auth
        .and_then(|a| a.strip_prefix("Bearer "))
        .and_then(|t| lock(registry).get(t));
    let Some(terminal) = terminal else {
        return (status(401), None);
    };
    if method != "POST" {
        return (status(405), None);
    }
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
    let Some(id) = obj.get("id") else {
        return (status(202), None);
    };
    let params = obj.get("params").unwrap_or(&Value::Null);
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
            match call(&terminal, name, args) {
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
        let mut body = String::new();
        let read = request.as_reader().take(MAX_BODY).read_to_string(&mut body);
        let handled = if read.is_err() {
            Some((status(400), None))
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                handle(
                    &self.registry,
                    &method,
                    &path,
                    auth.as_deref(),
                    origin.as_deref(),
                    &body,
                )
            }))
            .ok()
        };
        let (reply, event) = handled.unwrap_or_else(|| (status(500), None));
        if let Some(event) = event {
            lock(&self.events).retain(|tx| tx.unbounded_send(event.clone()).is_ok());
        }
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
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 4);
        let bad: Value = serde_json::from_str(&rpc(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tasks_delete","arguments":{}}}"#).body.unwrap()).unwrap();
        assert_eq!(bad["error"]["code"], -32602);
        let junk: Value = serde_json::from_str(&rpc("{not json").body.unwrap()).unwrap();
        assert_eq!(junk["error"]["code"], -32700);
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
        let resp = ureq::post(server.url())
            .set("Authorization", &format!("Bearer {tok}"))
            .set("Content-Type", "application/json")
            .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert!(resp.into_string().unwrap().contains("tasks_take"));
        assert!(server.url().starts_with("http://127.0.0.1:") && server.url().ends_with("/mcp"));
    }
}
