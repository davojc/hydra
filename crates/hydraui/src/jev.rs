//! The Jev API (TypeSafe's decision model): its key, kept in the OS credential store, and the
//! question asking whether a Claude session is free.

use std::time::Duration;

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

/// A finished session counts as free from this much "finished" up.
const FINISHED_MIN: f64 = 0.8;
/// ...and while "waiting for the user" is no more than this.
const WAITING_MAX: f64 = 0.2;
/// ...and while "unsent draft" is no more than this.
const DRAFT_MAX: f64 = 0.2;

/// Jev's three answers about a terminal screen, each from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub waiting: f64,
    pub finished: f64,
    pub draft: f64,
}

impl Verdict {
    /// Finished, not waiting on the user and no unsent draft.
    pub fn is_free(&self) -> bool {
        self.finished >= FINISHED_MIN && self.waiting <= WAITING_MAX && self.draft <= DRAFT_MAX
    }
}

/// The request asking Jev the three questions about `state`.
pub fn request_body(state: &str) -> serde_json::Value {
    let q = |instructions: &str| serde_json::json!({"type": "noul", "instructions": instructions});
    serde_json::json!({
        "model": "jev-latest",
        "state": state,
        "questions": {
            "waiting": q("Is the assistant waiting for the user to answer a question, make a decision or give more information?"),
            "finished": q("Has the assistant finished its work and is it ready for a new, unrelated task?"),
            "draft": q("Is there unsent text typed in the input box at the bottom?"),
        },
    })
}

/// The longest a call may take, connecting and reading included.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Asks Jev about `state`. Blocks (up to 5 s), so call it off the UI thread. The error is a
/// short reason for a toast ("HTTP 401", "not reachable", "unexpected reply") and never
/// mentions the key.
pub fn ask(url: &str, key: &str, state: &str) -> anyhow::Result<Verdict> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let resp = match agent
        .post(url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(&request_body(state).to_string())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => anyhow::bail!("HTTP {code}"),
        Err(_) => anyhow::bail!("not reachable"),
    };
    let reply: serde_json::Value = serde_json::from_reader(resp.into_reader())
        .map_err(|_| anyhow::anyhow!("unexpected reply"))?;
    // Each answer is a number from 0 to 1; anything else is a reply hydraui can't trust.
    let noul = |id: &str| {
        reply["answers"][id]["noul"]
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
            .ok_or_else(|| anyhow::anyhow!("unexpected reply"))
    };
    Ok(Verdict {
        waiting: noul("waiting")?,
        finished: noul("finished")?,
        draft: noul("draft")?,
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

    #[test]
    fn free_needs_finished_and_no_waiting_or_draft() {
        let v = |w, f, d| Verdict {
            waiting: w,
            finished: f,
            draft: d,
        };
        assert!(v(0.1, 0.9, 0.0).is_free());
        assert!(v(0.2, 0.8, 0.2).is_free());
        assert!(!v(0.3, 0.9, 0.0).is_free());
        assert!(!v(0.1, 0.7, 0.0).is_free());
        assert!(!v(0.1, 0.9, 0.5).is_free());
    }

    #[test]
    fn the_request_asks_three_noul_questions() {
        let b = request_body("screen");
        assert_eq!(b["model"], "jev-latest");
        assert_eq!(b["state"], "screen");
        for id in ["waiting", "finished", "draft"] {
            assert_eq!(b["questions"][id]["type"], "noul");
        }
    }

    #[test]
    fn a_reply_is_read_and_the_key_sent() {
        let (url, h) = serve_once(
            200,
            r#"{"answers":{"waiting":{"type":"noul","noul":0.1},"finished":{"type":"noul","noul":0.9},"draft":{"type":"noul","noul":0.05}}}"#,
        );
        let v = ask(&url, "k", "s").unwrap();
        assert_eq!(
            v,
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
        assert!(ask(&url, "secret-key", "s").is_err());
        let (url, _h) = serve_once(422, r#"{"detail":"bad"}"#);
        let e = ask(&url, "secret-key", "s").unwrap_err();
        assert!(!format!("{e:#}").contains("secret-key"));
    }

    #[test]
    fn an_answer_outside_zero_to_one_is_an_error() {
        let (url, _h) = serve_once(
            200,
            r#"{"answers":{"waiting":{"noul":0.1},"finished":{"noul":1.5},"draft":{"noul":0.0}}}"#,
        );
        assert!(ask(&url, "k", "s").is_err());
        let (url, _h) = serve_once(
            200,
            r#"{"answers":{"waiting":{"noul":-0.1},"finished":{"noul":0.9},"draft":{"noul":0.0}}}"#,
        );
        assert!(ask(&url, "k", "s").is_err());
    }

    #[test]
    fn an_error_says_the_http_status() {
        let (url, _h) = serve_once(401, r#"{"detail":"bad key"}"#);
        let e = ask(&url, "secret-key", "s").unwrap_err();
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
        assert!(ask(&url, "k", "s").is_err());
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
        assert!(ask(&url, "k", "s").is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(7));
    }
}
