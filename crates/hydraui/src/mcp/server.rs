//! The localhost MCP endpoint (spec §2): Streamable HTTP with plain JSON replies, bound to
//! loopback only, every request checked for a terminal's bearer token and a local Origin.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};

use super::registry::{Peer, Registry, Terminal};
use super::tools::{INSTRUCTIONS, Outcome, call_with, list};
use super::{SessionState, Usage};

/// Largest request body read; a tool call is a few lines of JSON.
const MAX_BODY: u64 = 1 << 20;

/// Something the server did that the app should show.
#[derive(Debug, Clone, PartialEq)]
pub enum McpEvent {
    /// A terminal made a project's task board; carries the project folder.
    BoardCreated(PathBuf),
    /// A Claude session's hook (`hydra hook <state>`) said what it is doing: the terminal's
    /// token, the state, and whether the session just started by resuming a conversation (it
    /// is then idle, but not a new session).
    Session {
        token: String,
        state: SessionState,
        resumed: bool,
        /// Claude's own session id, when the hook passed it (orchestrator spec §13).
        claude: Option<String>,
    },
    /// A Claude session's status line (`hydra statusline`) said how much of its account's
    /// limits are used: the terminal's token and the usage.
    Usage(String, Usage),
    /// A Claude session moved a card to Verify (`tasks_verify`), so its turn is finished: the
    /// terminal's token.
    Verified(String),
    /// The gate let a stage start: open its terminal and link it (orchestrator spec §3.2).
    StageStart(crate::runs::StartStage),
    /// Runs changed: re-draw them, and tell their orchestrators (orchestrator spec §6).
    RunChanged(Vec<crate::runs::Change>),
    /// A Claude session invoked a skill (`hydra hook skill`): the terminal's token and the
    /// skill's name.
    Skill(String, String),
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
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
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
    if let Some(done) = run_call(registry, token, body) {
        return done;
    }
    let peers = lock(registry).peers(token);
    let (reply, event) = answer(terminal, token, &peers, body);
    match &event {
        Some(McpEvent::Session {
            token,
            state,
            claude,
            ..
        }) => {
            let mut reg = lock(registry);
            reg.set_session(token, *state);
            let new = claude
                .as_deref()
                .and_then(|id| reg.claude_session(token, id));
            if let (Some((run, who)), Some(root)) = (new, reg.hydra_root.clone()) {
                let file = crate::run_log::file_for(&root, &run);
                let id = claude.as_deref().unwrap_or_default();
                if let Err(e) =
                    crate::run_log::append(&file, "claude session", json!({"who": who, "id": id}))
                {
                    crate::action_log::write(format!("run log {run}: {e}"));
                }
            }
        }
        // A stage reports only after using its skill (orchestrator §4); the run log keeps it.
        Some(McpEvent::Skill(token, skill)) => {
            let mut reg = lock(registry);
            reg.runs.skill_used(token, skill);
            let stage = match reg.runs.role(token) {
                crate::runs::Role::Stage(r, i) => {
                    Some((r.id.clone(), r.pipeline.stages[i].id.clone()))
                }
                _ => None,
            };
            if let (Some((run, stage)), Some(root)) = (stage, reg.hydra_root.clone()) {
                let file = crate::run_log::file_for(&root, &run);
                let _ = crate::run_log::append(
                    &file,
                    "skill used",
                    json!({"stage": stage, "skill": skill}),
                );
            }
        }
        // The weekly check reads what the run's sessions last reported (orchestrator §5.1).
        Some(McpEvent::Usage(token, usage)) => {
            if let Some(week) = usage.seven_day {
                lock(registry).runs.usage(token, week.used);
            }
        }
        _ => {}
    }
    (reply, event)
}

/// A `tools/call` of a run tool, answered under the registry's lock; `None` for any other
/// message.
fn run_call(
    registry: &Mutex<Registry>,
    token: &str,
    body: &str,
) -> Option<(Reply, Option<McpEvent>)> {
    let msg: Value = serde_json::from_str(body).ok()?;
    let id = msg.get("id")?;
    if msg.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    let params = msg.get("params")?;
    let name = params.get("name")?.as_str()?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    // The pipeline's branch rule, checked against GitHub before the report is recorded and
    // outside the lock (it asks gh).
    if name == "stage_report"
        && let Some(why) = branch_gate(registry, token, args)
    {
        let out = Outcome::err(format!("Not recorded: {why}"));
        return Some((ok(id, tool_result(&out)), None));
    }
    let (out, event) = super::run_tools::call(&mut lock(registry), token, name, args)?;
    Some((ok(id, tool_result(&out)), event))
}

/// Why a stage's report breaks its pipeline's branch rule, if it does: a pull request in it on
/// another branch, or one whose branch can't be read.
fn branch_gate(registry: &Mutex<Registry>, token: &str, args: &Value) -> Option<String> {
    let (expected, env, lookup, exe) = {
        let reg = lock(registry);
        let crate::runs::Role::Stage(run, _) = reg.runs.role(token) else {
            return None;
        };
        let expected = run.pipeline.branch_for(&run.ticket)?;
        let env = reg.get(token).and_then(|t| t.env).unwrap_or_default();
        (expected, env, reg.branch_lookup, reg.hydra_exe.clone())
    };
    let prs = crate::branch_rule::report_prs(args);
    if prs.is_empty() {
        return None;
    }
    crate::branch_rule::check(&expected, &prs, |url| match (lookup, &exe) {
        (Some(f), _) => f(&env, url),
        (None, Some(hydra)) => crate::branch_rule::gh_branch_of(hydra, &env, url),
        (None, None) => Err("hydraui can't find hydra to ask GitHub".into()),
    })
    .err()
}

fn tool_result(out: &Outcome) -> Value {
    json!({
        "content": [{"type": "text", "text": out.text}],
        "isError": out.is_error,
    })
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
                .and_then(SessionState::parse_report)
                .map(|(state, resumed)| McpEvent::Session {
                    token: token.to_string(),
                    state,
                    resumed,
                    claude: params
                        .get("session")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                }),
            Some("hydraui/usage") => Some(McpEvent::Usage(token.to_string(), Usage::parse(params))),
            Some("hydraui/skill") => params
                .get("skill")
                .and_then(Value::as_str)
                .map(|s| McpEvent::Skill(token.to_string(), s.to_string())),
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
                    let result = tool_result(&out);
                    // Only tasks_add makes a board, and only tasks_verify verifies.
                    let event = if out.verified {
                        Some(McpEvent::Verified(token.to_string()))
                    } else {
                        out.created.map(McpEvent::BoardCreated)
                    };
                    (ok(id, result), event)
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

    /// `<HYDRA_HOME>`, for the run tools' pipelines and logs.
    pub fn set_hydra_root(&self, root: PathBuf) {
        lock(&self.registry).hydra_root = Some(root);
    }

    /// The `hydra` the branch rule asks GitHub through.
    pub fn set_hydra_exe(&self, exe: Option<PathBuf>) {
        lock(&self.registry).hydra_exe = exe;
    }

    /// Sends `event` to every subscriber still listening.
    pub fn broadcast(&self, event: McpEvent) {
        lock(&self.events).retain(|tx| tx.unbounded_send(event.clone()).is_ok());
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
            self.broadcast(event);
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
            Some(McpEvent::Session {
                token: token.clone(),
                state: SessionState::Waiting,
                resumed: false,
                claude: None,
            })
        );
        let (_, event) =
            send(r#"{"jsonrpc":"2.0","method":"hydraui/status","params":{"state":"resumed"}}"#);
        assert_eq!(
            event,
            Some(McpEvent::Session {
                token: token.clone(),
                state: SessionState::Idle,
                resumed: true,
                claude: None,
            }),
            "a resumed session is idle, but not a new one"
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

    /// A registry whose hydra home holds the pipeline `two.toml`, and an orchestrator's token.
    fn with_pipeline() -> (tempfile::TempDir, Mutex<Registry>, String) {
        let home = tempfile::tempdir().unwrap();
        write_two(home.path(), 80);
        let reg = Mutex::new(Registry::default());
        reg.lock().unwrap().hydra_root = Some(home.path().to_path_buf());
        let orch = reg
            .lock()
            .unwrap()
            .register("orchestrator", home.path().to_path_buf());
        (home, reg, orch)
    }

    /// Writes `two.toml`: stages a (bar `bar_a`) and b (bar 90).
    fn write_two(home: &std::path::Path, bar_a: u8) {
        let dir = crate::pipeline::folder(home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("two.toml"),
            format!(
                "name = \"Two\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nskill = \"QA-Skills\"\n\
                 bar = {bar_a}\n[[stage]]\nid = \"b\"\ntitle = \"B\"\nbar = 90\n"
            ),
        )
        .unwrap();
    }

    /// Calls tool `name` as `token`: the result's text, whether it is an error, and the event.
    fn call(
        reg: &Mutex<Registry>,
        token: &str,
        name: &str,
        args: Value,
    ) -> (String, bool, Option<McpEvent>) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}})
        .to_string();
        let auth = format!("Bearer {token}");
        let (reply, event) = handle(reg, "POST", "/mcp", Some(&auth), None, &body);
        let msg: Value = serde_json::from_str(&reply.body.unwrap()).unwrap();
        let r = &msg["result"];
        (
            r["content"][0]["text"].as_str().unwrap().to_string(),
            r["isError"].as_bool().unwrap(),
            event,
        )
    }

    fn report(score: u8) -> Value {
        json!({"outcome": "done", "score": score, "summary": "Done it.",
               "evidence": [{"kind": "test", "detail": "t"}],
               "reasons": [{"side": "for", "text": "r"}], "handoff": "h"})
    }

    /// What the workspace does on `StageStart`: opens a terminal for the stage and links it.
    fn open_stage(reg: &Mutex<Registry>, event: Option<McpEvent>) -> String {
        let Some(McpEvent::StageStart(s)) = event else {
            panic!("a stage start: {event:?}");
        };
        let mut r = reg.lock().unwrap();
        let name = format!("{} · {}", s.label, s.title);
        let tok = r.register(&name, PathBuf::from("."));
        r.runs.link(&s.run, s.stage, &tok, &name);
        // A real session invokes its stage's skill first (`hydra hook skill`).
        r.runs.skill_used(&tok, "QA-Skills");
        tok
    }

    /// A run of `two` started by `orch`, with its first stage open: the stage's token.
    fn first_stage_open(reg: &Mutex<Registry>, orch: &str) -> String {
        let args = json!({"pipeline": "two", "ticket": "INTH-7", "summary": "Export drops rows"});
        assert!(!call(reg, orch, "run_start", args).1);
        let (_, _, event) = call(
            reg,
            orch,
            "stage_start",
            json!({"run": "INTH-7-1", "stage": "a"}),
        );
        open_stage(reg, event)
    }

    #[test]
    fn a_run_goes_through_two_stages_over_json_rpc() {
        use crate::runs::Decided;
        let (home, reg, orch) = with_pipeline();
        let args = json!({"pipeline": "two", "ticket": "INTH-7", "summary": "Export drops rows"});
        let (text, err, event) = call(&reg, &orch, "run_start", args);
        assert!(!err, "{text}");
        let started: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (started["run"].as_str(), started["first"].as_str()),
            (Some("INTH-7-1"), Some("a"))
        );
        assert_eq!(started["stages"][0]["skill"], "QA-Skills");
        assert_eq!(started["stages"][1]["bar"], 90);
        assert!(
            matches!(&event, Some(McpEvent::RunChanged(c)) if c[0].what == Decided::RunStarted)
        );
        let args = json!({"run": "INTH-7-1", "stage": "a", "brief": "Start with the CSV."});
        let (text, err, event) = call(&reg, &orch, "stage_start", args);
        assert!(!err, "{text}");
        assert!(text.contains("s-01"), "{text}");
        assert!(
            matches!(&event, Some(McpEvent::StageStart(s)) if s.prompt.contains("Start with the CSV."))
        );
        let (text, err, again) = call(
            &reg,
            &orch,
            "stage_start",
            json!({"run": "INTH-7-1", "stage": "a"}),
        );
        assert!(err && text.contains("running"), "{text}");
        assert_eq!(again, None);
        let stage = open_stage(&reg, event);
        let (text, err, _) = call(&reg, &stage, "stage_brief", json!({}));
        assert!(!err, "{text}");
        let brief: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (
                brief["ticket"].as_str(),
                brief["attempt"].as_str(),
                brief["stage"]["bar"].as_u64()
            ),
            (Some("INTH-7"), Some("s-01"), Some(80))
        );
        assert!(call(&reg, &orch, "stage_brief", json!({})).1, "not a stage");
        assert!(
            call(&reg, &orch, "stage_report", report(95)).1,
            "not a stage"
        );
        let args = json!({"run": "INTH-7-1", "stage": "b"});
        assert!(
            call(&reg, &stage, "stage_start", args).1,
            "a stage can't orchestrate"
        );
        assert!(
            call(&reg, &stage, "run_status", json!({})).1,
            "nor read the run"
        );
        let (text, err, event) = call(&reg, &stage, "stage_report", report(85));
        assert!(!err && text.contains("passed"), "{text}");
        assert!(
            matches!(&event, Some(McpEvent::RunChanged(c)) if c.iter().any(|c| c.what == Decided::Passed))
        );
        let (text, err, _) = call(&reg, &orch, "run_status", json!({}));
        assert!(!err, "{text}");
        let st: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(st["stages"][0]["status"], "passed");
        assert_eq!(st["stages"][0]["attempts"][0]["rating"]["score"], 85);
        assert_eq!(
            (
                st["next"]["stage"].as_str(),
                st["next"]["may_start"].as_bool()
            ),
            (Some("b"), Some(true))
        );
        let (_, _, event) = call(
            &reg,
            &orch,
            "stage_start",
            json!({"run": "INTH-7-1", "stage": "b"}),
        );
        let second = open_stage(&reg, event);
        let brief: Value =
            serde_json::from_str(&call(&reg, &second, "stage_brief", json!({})).0).unwrap();
        assert_eq!(
            (
                brief["previous"]["summary"].as_str(),
                brief["previous"]["handoff"].as_str()
            ),
            (Some("Done it."), Some("h"))
        );
        let (_, _, event) = call(&reg, &second, "stage_report", report(92));
        assert!(
            matches!(&event, Some(McpEvent::RunChanged(c)) if c.iter().any(|c| c.what == Decided::RunFinished))
        );
        let log =
            std::fs::read_to_string(crate::run_log::file_for(home.path(), "INTH-7-1")).unwrap();
        for event in ["run started", "stage started", "report", "run ended"] {
            assert!(
                log.contains(&format!("\"event\":\"{event}\"")),
                "{event}: {log}"
            );
        }
    }

    #[test]
    fn claudes_session_ids_go_to_the_run_log_once() {
        let (home, reg, orch) = with_pipeline();
        let stage = first_stage_open(&reg, &orch);
        let status = |token: &str, session: &str| {
            let body = json!({"jsonrpc": "2.0", "method": "hydraui/status",
                              "params": {"state": "working", "session": session}})
            .to_string();
            let auth = format!("Bearer {token}");
            handle(&reg, "POST", "/mcp", Some(&auth), None, &body)
        };
        status(&orch, "orch-1");
        status(&orch, "orch-1");
        status(&stage, "stage-1");
        let log =
            std::fs::read_to_string(crate::run_log::file_for(home.path(), "INTH-7-1")).unwrap();
        let lines: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("\"claude session\""))
            .collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(lines[0].contains("\"who\":\"orchestrator\"") && lines[0].contains("orch-1"));
        assert!(lines[1].contains("\"who\":\"s-01\"") && lines[1].contains("stage-1"));
    }

    #[test]
    fn a_report_whose_pr_is_on_another_branch_is_refused() {
        let (home, reg, orch) = with_pipeline();
        // The pipeline's rule: every branch is the ticket.
        let file = crate::pipeline::folder(home.path()).join("two.toml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("branch = \"{{ticket}}\"\n{text}")).unwrap();
        fn on_github(_env: &str, url: &str) -> Result<String, String> {
            Ok(if url.ends_with("/9") {
                "jordan/inth-7-export"
            } else {
                "INTH-7"
            }
            .into())
        }
        reg.lock().unwrap().branch_lookup = Some(on_github);
        let stage = first_stage_open(&reg, &orch);
        let with_pr = |n: u32| {
            let mut r = report(95);
            r["summary"] = json!(format!("Opened https://github.com/iov42/fe/pull/{n}."));
            r
        };
        let (text, err, event) = call(&reg, &stage, "stage_report", with_pr(9));
        assert!(err, "{text}");
        assert!(
            text.contains("Not recorded") && text.contains("jordan/inth-7-export"),
            "{text}"
        );
        assert_eq!(event, None);
        let st: Value =
            serde_json::from_str(&call(&reg, &orch, "run_status", json!({})).0).unwrap();
        assert_eq!(st["stages"][0]["status"], "running", "nothing recorded");
        let (text, err, _) = call(&reg, &stage, "stage_report", with_pr(1));
        assert!(!err, "{text}");
    }

    #[test]
    fn a_run_id_already_in_the_logs_is_not_reused() {
        let (home, reg, orch) = with_pipeline();
        let old = crate::run_log::file_for(home.path(), "INTH-7-1");
        crate::run_log::append(&old, "run started", json!({"ticket": "INTH-7"})).unwrap();
        let args = json!({"pipeline": "two", "ticket": "INTH-7", "summary": "Again"});
        let (text, err, _) = call(&reg, &orch, "run_start", args);
        assert!(!err, "{text}");
        let started: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(started["run"], "INTH-7-2");
    }

    #[test]
    fn a_report_missing_reasons_is_refused_and_changes_nothing() {
        let (_home, reg, orch) = with_pipeline();
        let stage = first_stage_open(&reg, &orch);
        let mut bad = report(95);
        bad.as_object_mut().unwrap().remove("reasons");
        let (text, err, event) = call(&reg, &stage, "stage_report", bad);
        assert!(err && text.contains("reasons"), "{text}");
        assert_eq!(event, None);
        let st: Value =
            serde_json::from_str(&call(&reg, &orch, "run_status", json!({})).0).unwrap();
        assert_eq!(st["stages"][0]["status"], "running");
        // Fixed, it counts.
        assert!(!call(&reg, &stage, "stage_report", report(95)).1);
    }

    #[test]
    fn a_bar_changed_in_the_file_applies_at_the_report() {
        let (home, reg, orch) = with_pipeline();
        let stage = first_stage_open(&reg, &orch);
        write_two(home.path(), 99);
        let (text, err, _) = call(&reg, &stage, "stage_report", report(95));
        assert!(!err && text.contains("99"), "{text}");
        let st: Value =
            serde_json::from_str(&call(&reg, &orch, "run_status", json!({})).0).unwrap();
        assert_eq!(
            (st["stages"][0]["status"].as_str(), st["state"].as_str()),
            (Some("stopped"), Some("needs_you"))
        );
        assert_eq!(st["stages"][0]["bar"], 99);
    }

    #[test]
    fn usage_reports_feed_the_weekly_check() {
        use crate::runs::Decided;
        let (_home, reg, orch) = with_pipeline();
        let stage = first_stage_open(&reg, &orch);
        let usage = r#"{"jsonrpc":"2.0","method":"hydraui/usage","params":{"rate_limits":{"seven_day":{"used_percentage":75}}}}"#;
        let auth = format!("Bearer {stage}");
        assert_eq!(
            handle(&reg, "POST", "/mcp", Some(&auth), None, usage)
                .0
                .status,
            202
        );
        assert!(!call(&reg, &stage, "stage_report", report(95)).1);
        let (text, err, event) = call(
            &reg,
            &orch,
            "stage_start",
            json!({"run": "INTH-7-1", "stage": "b"}),
        );
        assert!(err && text.contains("25%"), "{text}");
        assert!(
            matches!(&event, Some(McpEvent::RunChanged(c)) if c.iter().any(|c| matches!(c.what, Decided::RunNeedsYou(_))))
        );
    }

    #[test]
    fn closing_a_stage_terminal_fails_its_stage() {
        use crate::runs::Decided;
        let (_home, reg, orch) = with_pipeline();
        let stage = first_stage_open(&reg, &orch);
        let changes = reg.lock().unwrap().revoke(&stage);
        assert!(changes.iter().any(|c| matches!(c.what, Decided::Failed(_))));
        let st: Value =
            serde_json::from_str(&call(&reg, &orch, "run_status", json!({})).0).unwrap();
        assert_eq!(st["stages"][0]["status"], "failed");
    }

    #[test]
    fn run_tools_need_a_hydra_home() {
        let dir = repo();
        let reg = Mutex::new(Registry::default());
        let tok = reg.lock().unwrap().register("c", dir.path().to_path_buf());
        let args = json!({"pipeline": "two", "ticket": "INTH-7", "summary": "x"});
        let (text, err, _) = call(&reg, &tok, "run_start", args);
        assert!(err && text.contains("hydra home"), "{text}");
    }

    #[test]
    fn a_two_stage_run_over_http() {
        use crate::runs::Decided;
        let home = tempfile::tempdir().unwrap();
        write_two(home.path(), 80);
        let server = Server::start().unwrap();
        server.set_hydra_root(home.path().to_path_buf());
        let register = |label: &str| {
            server
                .registry()
                .lock()
                .unwrap()
                .register(label, home.path().to_path_buf())
        };
        let orch = register("orchestrator");
        let post = |token: &str, body: Value| -> (u16, Value) {
            let resp = ureq::post(server.url())
                .set("Authorization", &format!("Bearer {token}"))
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
                .unwrap();
            let status = resp.status();
            let text = resp.into_string().unwrap();
            (status, serde_json::from_str(&text).unwrap_or(Value::Null))
        };
        let tool = |token: &str, name: &str, args: Value| -> (String, bool) {
            let (_, v) = post(
                token,
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": args}}),
            );
            let r = &v["result"];
            (
                r["content"][0]["text"].as_str().unwrap().to_string(),
                r["isError"].as_bool().unwrap(),
            )
        };
        let mut events = server.subscribe();
        let mut next = || events.try_recv().ok();
        // What the workspace does on StageStart: a terminal for the stage, linked to the run.
        let open = |event: Option<McpEvent>| -> String {
            let Some(McpEvent::StageStart(s)) = event else {
                panic!("a stage start: {event:?}");
            };
            let name = format!("{} · {}", s.label, s.title);
            let tok = register(&name);
            server
                .registry()
                .lock()
                .unwrap()
                .runs
                .link(&s.run, s.stage, &tok, &name);
            tok
        };

        let args = json!({"pipeline": "two", "ticket": "INTH-9", "summary": "Rows dropped"});
        let (text, err) = tool(&orch, "run_start", args);
        assert!(!err, "{text}");
        assert!(
            matches!(next(), Some(McpEvent::RunChanged(c)) if c[0].what == Decided::RunStarted)
        );
        assert!(
            !tool(
                &orch,
                "stage_start",
                json!({"run": "INTH-9-1", "stage": "a"})
            )
            .1
        );
        let a = open(next());
        // Stage a's skill is reported over HTTP, as `hydra hook skill` does.
        let (_, refused) = tool(&a, "stage_report", report(90));
        assert!(refused, "no report before the stage's skill is used");
        let skill = json!({"jsonrpc": "2.0", "method": "hydraui/skill",
                           "params": {"skill": "QA-Skills"}});
        assert_eq!(post(&a, skill).0, 202);
        assert!(matches!(next(), Some(McpEvent::Skill(t, k)) if t == a && k == "QA-Skills"));
        let usage = json!({"jsonrpc": "2.0", "method": "hydraui/usage",
                           "params": {"rate_limits": {"seven_day": {"used_percentage": 40}}}});
        assert_eq!(post(&a, usage).0, 202);
        assert!(matches!(next(), Some(McpEvent::Usage(t, _)) if t == a));
        let (text, err) = tool(&a, "stage_report", report(90));
        assert!(!err && text.contains("passed"), "{text}");
        assert!(
            matches!(next(), Some(McpEvent::RunChanged(c)) if c.iter().any(|c| c.what == Decided::Passed))
        );
        assert!(
            !tool(
                &orch,
                "stage_start",
                json!({"run": "INTH-9-1", "stage": "b"})
            )
            .1
        );
        let b = open(next());
        let (text, err) = tool(&b, "stage_report", report(95));
        assert!(!err, "{text}");
        assert!(
            matches!(next(), Some(McpEvent::RunChanged(c)) if c.iter().any(|c| c.what == Decided::RunFinished))
        );
        let (text, _) = tool(&orch, "run_status", json!({}));
        let st: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(st["state"], "finished");
        assert_eq!(st["week_used"], 40.0);
        // A closed stage terminal after the run ended changes nothing.
        assert!(server.registry().lock().unwrap().revoke(&b).is_empty());
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
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 11);
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
    fn a_verified_card_names_its_terminal() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(d.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&["init", "-q"]) {
            eprintln!("skipped: git isn't installed");
            return;
        }
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        assert!(git(&["add", "a.txt"]) && git(&["commit", "-q", "-m", "x"]));
        std::fs::create_dir(d.path().join(".hydra")).unwrap();
        std::fs::write(d.path().join(".hydra/tasks.md"), "## InProgress\n- [c] x\n").unwrap();
        let reg = Mutex::new(Registry::default());
        let token = reg.lock().unwrap().register("c", d.path().to_path_buf());
        let auth = format!("Bearer {token}");
        let verify = |commit: &str| {
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"tasks_verify","arguments":{{"text":"x","commit":"{commit}"}}}}}}"#
            );
            handle(&reg, "POST", "/mcp", Some(&auth), None, &body)
        };
        let (reply, event) = verify("deadbeef");
        assert!(reply.body.unwrap().contains(r#""isError":true"#));
        assert_eq!(event, None, "an unknown commit");
        let (reply, event) = verify("HEAD");
        assert!(reply.body.unwrap().contains(r#""isError":false"#));
        assert_eq!(event, Some(McpEvent::Verified(token.clone())));
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
