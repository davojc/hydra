//! The Jev API (TypeSafe's decision model): its key, kept in the OS credential store, and the
//! choice question asking what state a Claude session is in.

use std::collections::BTreeMap;
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

/// The option meaning the session can take a card.
pub const READY: &str = "ready";
/// The option Jev can fall back on when it can't tell: always offered, or Jev would pick one
/// of the others anyway.
pub const NONE: &str = "none";

/// The one question auto-pickup asks Jev, a choice between states of the session, and how sure
/// Jev must be: read from `<HYDRA_HOME>/hydraui/jev.toml` on every ask, so wording can be tried
/// without a restart. A setting left out keeps its default.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JevSettings {
    pub model: String,
    /// A session is free when Jev picks `ready` with at least this confidence.
    pub min_confidence: f64,
    pub instructions: String,
    /// Each option's name and what it means. Must include `ready`; `none` is added when left
    /// out.
    pub options: BTreeMap<String, String>,
}

/// What `none` means when jev.toml doesn't say.
const NONE_TEXT: &str = "None of these, or it can't be told from the screen";

impl Default for JevSettings {
    fn default() -> Self {
        let options = [
            (
                READY,
                "Finished its work and asks nothing of the user: it may mention optional next steps or things to try, but nothing must be answered before it can start a new task",
            ),
            (
                "asking",
                "Needs the user to answer a question, approve something or make a choice before it can continue",
            ),
            (
                "working",
                "Still working: running tools or thinking, or its last message says it is carrying on",
            ),
            (
                "draft",
                "Unsent text is typed in the input box at the bottom",
            ),
            (NONE, NONE_TEXT),
        ];
        Self {
            model: "jev-latest".into(),
            min_confidence: 0.7,
            instructions: "This is the end of a terminal running an AI coding assistant (Claude Code). What state is the assistant in?".into(),
            options: options
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

/// jev.toml for `hydra_root` (`HydraPaths::root`).
pub fn settings_file(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("jev.toml")
}

/// What a new jev.toml holds: the defaults, written out to be edited.
pub const DEFAULT_SETTINGS_TEXT: &str = r#"# The question hydraui's auto-pickup asks Jev about a Claude terminal's last 60 lines: one
# choice between the states below. The session can take a card when Jev picks "ready" with
# at least min_confidence. Read on every ask: save and move a card into Pickup to try new
# wording; hydraui.log shows Jev's pick, confidence and each option's probability, with the
# lines sent. Delete this file for the defaults.
#
# "ready" must be an option. "none" is always offered (added if you leave it out), so Jev
# can say it can't tell rather than pick something anyway. Add or rename the others freely.

model = "jev-latest"
min_confidence = 0.7
instructions = "This is the end of a terminal running an AI coding assistant (Claude Code). What state is the assistant in?"

[options]
ready = "Finished its work and asks nothing of the user: it may mention optional next steps or things to try, but nothing must be answered before it can start a new task"
asking = "Needs the user to answer a question, approve something or make a choice before it can continue"
working = "Still working: running tools or thinking, or its last message says it is carrying on"
draft = "Unsent text is typed in the input box at the bottom"
none = "None of these, or it can't be told from the screen"
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
    let mut s: JevSettings = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|e| e.message().to_string())?;
    if !(0.0..=1.0).contains(&s.min_confidence) {
        return Err("min_confidence must be from 0 to 1".into());
    }
    if s.instructions.trim().is_empty() {
        return Err("instructions can't be empty".into());
    }
    if !s.options.contains_key(READY) {
        return Err("[options] needs a \"ready\" option".into());
    }
    s.options
        .entry(NONE.to_string())
        .or_insert_with(|| NONE_TEXT.to_string());
    Ok(s)
}

/// Jev's answer about a terminal screen: the option it picked, how sure it is, and each
/// option's probability.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}

impl Verdict {
    /// Jev picked `ready`, at least as sure as `s` asks.
    pub fn is_free(&self, s: &JevSettings) -> bool {
        self.choice == READY && self.confidence >= s.min_confidence
    }
}

/// The request asking Jev `s`'s question about `state`.
pub fn request_body(state: &str, s: &JevSettings) -> serde_json::Value {
    serde_json::json!({
        "model": s.model,
        "state": state,
        "questions": {
            "state": {
                "type": "choice",
                "instructions": s.instructions,
                "criteria": s.options,
            },
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
    let verdict = read_verdict(&reply["answers"]["state"], s)
        .ok_or_else(|| failure("unexpected reply", Some(body.clone())))?;
    Ok(Reply { verdict, body })
}

/// The choice answer: an option `s` offered and a confidence from 0 to 1, or `None` for
/// anything else, a reply hydraui can't trust. Probabilities outside 0 to 1 are left out.
fn read_verdict(answer: &serde_json::Value, s: &JevSettings) -> Option<Verdict> {
    let unit = |v: &serde_json::Value| {
        v.as_f64()
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
    };
    let choice = answer["choice"].as_str()?;
    if !s.options.contains_key(choice) {
        return None;
    }
    let probabilities = answer["probabilities"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), unit(v)?)))
                .collect()
        })
        .unwrap_or_default();
    Some(Verdict {
        choice: choice.to_string(),
        confidence: unit(&answer["confidence"])?,
        probabilities,
    })
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
        body: impl Into<String>,
    ) -> (String, std::thread::JoinHandle<Option<String>>) {
        let body: String = body.into();
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

    fn verdict(choice: &str, confidence: f64) -> Verdict {
        Verdict {
            choice: choice.into(),
            confidence,
            probabilities: BTreeMap::new(),
        }
    }

    /// A choice reply picking `choice` at `confidence`.
    fn reply(choice: &str, confidence: f64) -> String {
        format!(
            r#"{{"answers":{{"state":{{"type":"choice","choice":"{choice}","confidence":{confidence},"probabilities":{{"ready":0.8,"asking":0.1,"none":0.1}}}}}}}}"#
        )
    }

    #[test]
    fn free_needs_ready_with_enough_confidence() {
        let s = d();
        assert!(verdict("ready", 0.7).is_free(&s));
        assert!(!verdict("ready", 0.69).is_free(&s));
        assert!(!verdict("asking", 0.95).is_free(&s));
        assert!(!verdict("none", 0.95).is_free(&s));
        let lenient = JevSettings {
            min_confidence: 0.5,
            ..d()
        };
        assert!(verdict("ready", 0.6).is_free(&lenient));
    }

    #[test]
    fn the_request_asks_one_choice_with_none_among_the_options() {
        let b = request_body("screen", &d());
        assert_eq!(b["model"], "jev-latest");
        assert_eq!(b["state"], "screen");
        let q = &b["questions"]["state"];
        assert_eq!(q["type"], "choice");
        assert!(q["instructions"].as_str().unwrap().contains("state"));
        for option in ["ready", "asking", "working", "draft", "none"] {
            assert!(q["criteria"][option].is_string(), "{option}");
        }
    }

    #[test]
    fn the_default_file_holds_the_defaults() {
        assert_eq!(parse_settings(DEFAULT_SETTINGS_TEXT).unwrap(), d());
    }

    #[test]
    fn the_file_changes_wording_options_and_confidence() {
        let s = parse_settings(
            "min_confidence = 0.6\ninstructions = \"State?\"\n[options]\nready = \"Done\"\nbusy = \"Not done\"\n",
        )
        .unwrap();
        assert_eq!(s.min_confidence, 0.6);
        assert_eq!(s.model, "jev-latest", "left out: the default");
        let b = request_body("x", &s);
        assert_eq!(b["questions"]["state"]["instructions"], "State?");
        assert_eq!(b["questions"]["state"]["criteria"]["ready"], "Done");
        assert_eq!(b["questions"]["state"]["criteria"]["busy"], "Not done");
        assert!(
            b["questions"]["state"]["criteria"]["none"].is_string(),
            "none is always offered"
        );
        assert_eq!(
            b["questions"]["state"]["criteria"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn a_file_that_makes_no_sense_says_why() {
        assert!(
            parse_settings("min_confidence = 2")
                .unwrap_err()
                .contains("min_confidence")
        );
        assert!(
            parse_settings("instructions = \" \"")
                .unwrap_err()
                .contains("instructions")
        );
        assert!(
            parse_settings("[options]\nasking = \"x\"")
                .unwrap_err()
                .contains("ready")
        );
        assert!(
            parse_settings("min_confidense = 0.5").is_err(),
            "a misspelt setting"
        );
        assert!(
            parse_settings("finished_min = 0.8").is_err(),
            "the old three-question file"
        );
        assert!(parse_settings("not toml").is_err());
    }

    #[test]
    fn no_file_is_the_defaults_and_a_bad_one_too_with_the_problem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jev.toml");
        assert_eq!(load_settings(&path), (d(), None));
        std::fs::write(&path, "min_confidence = -1").unwrap();
        let (s, problem) = load_settings(&path);
        assert_eq!(s, d());
        assert!(problem.unwrap().contains("min_confidence"));
    }

    #[test]
    fn a_choice_reply_is_read_with_its_probabilities() {
        let answer: serde_json::Value = serde_json::from_str(&reply("ready", 0.82)).unwrap();
        let v = read_verdict(&answer["answers"]["state"], &d()).unwrap();
        assert_eq!(v.choice, "ready");
        assert_eq!(v.confidence, 0.82);
        assert_eq!(v.probabilities.get("asking"), Some(&0.1));
    }

    #[test]
    fn a_choice_not_offered_or_a_confidence_outside_zero_to_one_is_untrusted() {
        let read = |r: String| {
            let answer: serde_json::Value = serde_json::from_str(&r).unwrap();
            read_verdict(&answer["answers"]["state"], &d())
        };
        assert!(read(reply("sleeping", 0.9)).is_none());
        assert!(read(reply("ready", 1.5)).is_none());
        assert!(read(reply("ready", -0.1)).is_none());
        assert!(read(r#"{"answers":{"state":{"choice":"ready"}}}"#.into()).is_none());
    }

    #[test]
    fn a_reply_is_read_and_the_key_sent() {
        let (url, h) = serve_once(200, reply("ready", 0.9));
        let r = ask(&url, "k", "s", &d()).unwrap();
        assert!(r.body.contains("answers"), "the body is kept for the log");
        assert_eq!(r.verdict.choice, "ready");
        assert_eq!(r.verdict.confidence, 0.9);
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
