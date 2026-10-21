//! The Jev API (TypeSafe's decision model): its key, kept in the OS credential store, and the
//! question asking whether a Claude session is free.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Credential Manager user the key is stored under.
const USER: &str = "jev";

/// The stored Jev API key, under one credential-store service name.
pub struct JevKey {
    service: String,
}

fn failed(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl JevKey {
    /// The key hydraui itself uses.
    pub fn app() -> Self {
        Self::with_service("hydraui")
    }

    /// A key under another service name, so tests never touch the real one.
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, USER).map_err(failed)
    }

    /// The stored key, or `None` when none is set.
    pub fn get(&self) -> Result<Option<String>, String> {
        match self.entry()?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(failed(e)),
        }
    }

    /// Stores the key without its surrounding spaces; a blank one is refused.
    pub fn set(&self, key: &str) -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("the Jev API key is empty".to_string());
        }
        self.entry()?.set_password(key).map_err(failed)
    }

    /// Forgets the key; false when there was none.
    pub fn remove(&self) -> Result<bool, String> {
        match self.entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(failed(e)),
        }
    }
}

/// Where Jev answers.
pub const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";

/// The questions auto-pickup asks Jev, and the answers that make a session free: read from
/// `<HYDRA_HOME>/hydraui/jev.toml` on every ask, so wording can be tried without a restart.
/// A setting left out keeps its default.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JevSettings {
    pub model: String,
    /// A session is free from this much "finished" up...
    pub finished_min: f64,
    /// ...while "waiting for the user" is no more than this...
    pub waiting_max: f64,
    /// ...and "unsent draft" no more than this.
    pub draft_max: f64,
    pub waiting: Question,
    pub finished: Question,
    pub draft: Question,
}

/// One yes/no ("noul") question about the terminal's screen.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub instructions: String,
    /// What a yes and a no mean, passed to Jev as written (`true = "…"`, `false = "…"`).
    #[serde(default)]
    pub criteria: Option<toml::Table>,
}

impl Question {
    fn new(instructions: &str) -> Self {
        Self {
            instructions: instructions.to_string(),
            criteria: None,
        }
    }

    fn json(&self) -> serde_json::Value {
        let mut q = serde_json::json!({"type": "noul", "instructions": self.instructions});
        if let Some(criteria) = &self.criteria {
            q["criteria"] = serde_json::to_value(criteria).unwrap_or_default();
        }
        q
    }
}

impl Default for JevSettings {
    fn default() -> Self {
        Self {
            model: "jev-latest".into(),
            finished_min: 0.8,
            waiting_max: 0.2,
            draft_max: 0.2,
            waiting: Question::new(
                "Is the assistant waiting for the user to answer a question, make a decision or give more information?",
            ),
            finished: Question::new(
                "Has the assistant finished its work and is it ready for a new, unrelated task?",
            ),
            draft: Question::new("Is there unsent text typed in the input box at the bottom?"),
        }
    }
}

/// jev.toml for `hydra_root` (`HydraPaths::root`).
pub fn settings_file(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("jev.toml")
}

/// What a new jev.toml holds: the defaults, written out to be edited.
pub const DEFAULT_SETTINGS_TEXT: &str = r#"# The questions hydraui's auto-pickup asks Jev about a Claude terminal's last 60 lines,
# and the answers that make the session free to take a card. Read on every ask: save and
# move a card into Pickup to try new wording. Each answer is Jev's probability of "yes",
# from 0 to 1; hydraui.log shows them, with the lines sent. Delete this file for the defaults.

model = "jev-latest"

# Free when finished >= finished_min, waiting <= waiting_max and draft <= draft_max.
finished_min = 0.8
waiting_max = 0.2
draft_max = 0.2

[waiting]
instructions = "Is the assistant waiting for the user to answer a question, make a decision or give more information?"

[finished]
instructions = "Has the assistant finished its work and is it ready for a new, unrelated task?"

[draft]
instructions = "Is there unsent text typed in the input box at the bottom?"

# Each question may also say what a yes and a no mean:
# [finished.criteria]
# true = "The last message reports work done and asks nothing of the user"
# false = "The assistant is still working, or asks the user something"
"#;

/// The settings in `path`: the defaults when there is no file, and also, with the problem,
/// when it can't be read or makes no sense.
pub fn load_settings(path: &Path) -> (JevSettings, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (JevSettings::default(), None);
        }
        Err(e) => return (JevSettings::default(), Some(e.to_string())),
    };
    match parse_settings(&text) {
        Ok(s) => (s, None),
        Err(e) => (JevSettings::default(), Some(e)),
    }
}

fn parse_settings(text: &str) -> Result<JevSettings, String> {
    let s: JevSettings = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|e| e.message().to_string())?;
    for (name, v) in [
        ("finished_min", s.finished_min),
        ("waiting_max", s.waiting_max),
        ("draft_max", s.draft_max),
    ] {
        if !(0.0..=1.0).contains(&v) {
            return Err(format!("{name} must be from 0 to 1"));
        }
    }
    for (name, q) in [
        ("waiting", &s.waiting),
        ("finished", &s.finished),
        ("draft", &s.draft),
    ] {
        if q.instructions.trim().is_empty() {
            return Err(format!("[{name}] needs instructions"));
        }
    }
    Ok(s)
}

/// Jev's three answers about a terminal screen, each from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub waiting: f64,
    pub finished: f64,
    pub draft: f64,
}

impl Verdict {
    /// Finished, not waiting on the user and no unsent draft, by `s`'s thresholds.
    pub fn is_free(&self, s: &JevSettings) -> bool {
        self.finished >= s.finished_min
            && self.waiting <= s.waiting_max
            && self.draft <= s.draft_max
    }
}

/// The request asking Jev `s`'s three questions about `state`.
pub fn request_body(state: &str, s: &JevSettings) -> serde_json::Value {
    serde_json::json!({
        "model": s.model,
        "state": state,
        "questions": {
            "waiting": s.waiting.json(),
            "finished": s.finished.json(),
            "draft": s.draft.json(),
        },
    })
}

/// The longest a call may take, connecting and reading included.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Jev's reply: the answers, and the body as it came, for the log.
#[derive(Debug, Clone)]
pub struct Reply {
    pub verdict: Verdict,
    pub body: String,
}

/// Why a call failed: a short reason for a toast ("HTTP 401", "not reachable", "unexpected
/// reply"), and the body Jev sent, if any, for the log. Neither mentions the key.
#[derive(Debug, Clone)]
pub struct Failure {
    pub reason: String,
    pub body: Option<String>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

fn failure(reason: impl Into<String>, body: Option<String>) -> Failure {
    Failure {
        reason: reason.into(),
        body,
    }
}

/// Asks Jev `s`'s questions about `state`. Blocks (up to 5 s), so call it off the UI thread.
pub fn ask(url: &str, key: &str, state: &str, s: &JevSettings) -> Result<Reply, Failure> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let resp = match agent
        .post(url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(&request_body(state, s).to_string())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            return Err(failure(format!("HTTP {code}"), r.into_string().ok()));
        }
        Err(_) => return Err(failure("not reachable", None)),
    };
    let body = resp
        .into_string()
        .map_err(|_| failure("unexpected reply", None))?;
    let reply: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| failure("unexpected reply", Some(body.clone())))?;
    // Each answer is a number from 0 to 1; anything else is a reply hydraui can't trust.
    let noul = |id: &str| {
        reply["answers"][id]["noul"]
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
            .ok_or_else(|| failure("unexpected reply", Some(body.clone())))
    };
    let verdict = Verdict {
        waiting: noul("waiting")?,
        finished: noul("finished")?,
        draft: noul("draft")?,
    };
    Ok(Reply { verdict, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_set_read_and_removed() {
        let k = JevKey::with_service(&format!("hydraui-test-{}", std::process::id()));
        assert_eq!(k.get().unwrap(), None);
        k.set("  tsk-123  ").unwrap();
        assert_eq!(k.get().unwrap().as_deref(), Some("tsk-123"));
        assert!(k.remove().unwrap());
        assert_eq!(k.get().unwrap(), None);
        assert!(!k.remove().unwrap());
    }

    #[test]
    fn a_blank_key_is_refused() {
        let k = JevKey::with_service("hydraui-test-blank");
        assert!(k.set("   ").is_err());
    }

    /// Serves one canned reply on a local port; returns the url and the request's Authorization header.
    fn serve_once(
        status: u16,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<Option<String>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let h = std::thread::spawn(move || {
            let mut req = server.recv().ok()?;
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string());
            let mut sent = String::new();
            std::io::Read::read_to_string(req.as_reader(), &mut sent).ok()?;
            let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(status));
            auth
        });
        (url, h)
    }

    fn d() -> JevSettings {
        JevSettings::default()
    }

    #[test]
    fn free_needs_finished_and_no_waiting_or_draft() {
        let v = |w, f, d| Verdict {
            waiting: w,
            finished: f,
            draft: d,
        };
        let s = d();
        assert!(v(0.1, 0.9, 0.0).is_free(&s));
        assert!(v(0.2, 0.8, 0.2).is_free(&s));
        assert!(!v(0.3, 0.9, 0.0).is_free(&s));
        assert!(!v(0.1, 0.7, 0.0).is_free(&s));
        assert!(!v(0.1, 0.9, 0.5).is_free(&s));
        let lenient = JevSettings {
            finished_min: 0.6,
            ..d()
        };
        assert!(v(0.1, 0.7, 0.0).is_free(&lenient));
    }

    #[test]
    fn the_request_asks_three_noul_questions() {
        let b = request_body("screen", &d());
        assert_eq!(b["model"], "jev-latest");
        assert_eq!(b["state"], "screen");
        for id in ["waiting", "finished", "draft"] {
            assert_eq!(b["questions"][id]["type"], "noul");
            assert!(b["questions"][id].get("criteria").is_none());
        }
    }

    #[test]
    fn the_default_file_holds_the_defaults() {
        assert_eq!(parse_settings(DEFAULT_SETTINGS_TEXT).unwrap(), d());
    }

    #[test]
    fn the_file_changes_wording_criteria_and_thresholds() {
        let s = parse_settings(
            "finished_min = 0.7\n[finished]\ninstructions = \"Done?\"\n[finished.criteria]\ntrue = \"yes\"\nfalse = \"no\"\n",
        )
        .unwrap();
        assert_eq!(s.finished_min, 0.7);
        assert_eq!(s.waiting, d().waiting, "left out: the default");
        let b = request_body("x", &s);
        assert_eq!(b["questions"]["finished"]["instructions"], "Done?");
        assert_eq!(b["questions"]["finished"]["criteria"]["true"], "yes");
        assert_eq!(b["questions"]["finished"]["criteria"]["false"], "no");
    }

    #[test]
    fn a_file_that_makes_no_sense_says_why() {
        assert!(
            parse_settings("finished_min = 2")
                .unwrap_err()
                .contains("finished_min")
        );
        assert!(
            parse_settings("[draft]\ninstructions = \" \"")
                .unwrap_err()
                .contains("[draft]")
        );
        assert!(
            parse_settings("finshed_min = 0.5").is_err(),
            "a misspelt setting"
        );
        assert!(parse_settings("not toml").is_err());
    }

    #[test]
    fn no_file_is_the_defaults_and_a_bad_one_too_with_the_problem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jev.toml");
        assert_eq!(load_settings(&path), (d(), None));
        std::fs::write(&path, "waiting_max = -1").unwrap();
        let (s, problem) = load_settings(&path);
        assert_eq!(s, d());
        assert!(problem.unwrap().contains("waiting_max"));
    }

    #[test]
    fn a_reply_is_read_and_the_key_sent() {
        let (url, h) = serve_once(
            200,
            r#"{"answers":{"waiting":{"type":"noul","noul":0.1},"finished":{"type":"noul","noul":0.9},"draft":{"type":"noul","noul":0.05}}}"#,
        );
        let r = ask(&url, "k", "s", &d()).unwrap();
        assert!(r.body.contains("answers"), "the body is kept for the log");
        assert_eq!(
            r.verdict,
            Verdict {
                waiting: 0.1,
                finished: 0.9,
                draft: 0.05
            }
        );
        assert_eq!(h.join().unwrap().as_deref(), Some("Bearer k"));
    }

    #[test]
    fn a_bad_reply_is_an_error() {
        let (url, _h) = serve_once(200, r#"{"answers":{}}"#);
        assert!(ask(&url, "secret-key", "s", &d()).is_err());
        let (url, _h) = serve_once(422, r#"{"detail":"bad"}"#);
        let e = ask(&url, "secret-key", "s", &d()).unwrap_err();
        assert!(!format!("{e:#}").contains("secret-key"));
        assert_eq!(
            e.body.as_deref(),
            Some(r#"{"detail":"bad"}"#),
            "Jev's why, for the log"
        );
    }

    #[test]
    fn an_answer_outside_zero_to_one_is_an_error() {
        let (url, _h) = serve_once(
            200,
            r#"{"answers":{"waiting":{"noul":0.1},"finished":{"noul":1.5},"draft":{"noul":0.0}}}"#,
        );
        assert!(ask(&url, "k", "s", &d()).is_err());
        let (url, _h) = serve_once(
            200,
            r#"{"answers":{"waiting":{"noul":-0.1},"finished":{"noul":0.9},"draft":{"noul":0.0}}}"#,
        );
        assert!(ask(&url, "k", "s", &d()).is_err());
    }

    #[test]
    fn an_error_says_the_http_status() {
        let (url, _h) = serve_once(401, r#"{"detail":"bad key"}"#);
        let e = ask(&url, "secret-key", "s", &d()).unwrap_err();
        assert_eq!(e.to_string(), "HTTP 401");
    }

    #[test]
    fn a_slow_drip_reply_is_cut_off() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Answers, then sends the body a byte every second: each read is quick, the whole slow.
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if s.write_all(b" ").is_err() {
                    return;
                }
            }
        });
        let start = std::time::Instant::now();
        assert!(ask(&url, "k", "s", &d()).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(8));
    }

    #[test]
    fn a_timeout_is_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Accept and hold the connection open without ever answering.
        std::thread::spawn(move || {
            let _held = listener.accept();
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        let start = std::time::Instant::now();
        assert!(ask(&url, "k", "s", &d()).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(7));
    }
}
