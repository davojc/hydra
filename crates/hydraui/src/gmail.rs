//! Gmail for email triage (spec `2026-11-10-hydraui-email-triage-design.md` §3): signing in to
//! the shared mailbox's own Google account with OAuth (PKCE, a loopback redirect), and reading
//! its threads. hydraui calls Google itself; Claude never touches the mailbox. Every call
//! blocks: run it off the UI thread. No error or log line ever carries a token.

use std::time::{Duration, Instant};

/// What hydraui asks Google for: read, label and draft. Never send.
pub const SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";

/// How long the browser has to come back from Google's consent page.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

/// The longest one call to Google may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Where Google's pages and APIs are: the real ones, or a fake in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The consent page.
    pub auth: String,
    /// Where a code or refresh token is traded for an access token.
    pub token: String,
    /// The Gmail API, up to `/gmail/v1`.
    pub api: String,
}

impl Endpoints {
    pub fn google() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            api: "https://gmail.googleapis.com/gmail/v1".into(),
        }
    }
}

/// The team's Google OAuth client, a Desktop-type one (config `gmail_client_id` and
/// `gmail_client_secret`). Google says a desktop client's secret can't be kept secret, so it
/// lives in config, not the credential store; PKCE is what protects the sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

/// A PKCE pair: the verifier hydraui keeps, and the challenge the consent page is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// What a code or refresh token is traded for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    /// Only when trading a code (Google gives one with `access_type=offline`).
    pub refresh: Option<String>,
}

/// One thread, as a check lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    /// The first message's subject.
    pub subject: String,
    /// Who wrote the last message.
    pub from: String,
    /// The last message's `Date` header, as sent.
    pub date: String,
    pub messages: usize,
    /// The first message's `Message-ID` header: the same in every mailbox holding a copy (a
    /// group's members), unlike `id`. Empty when it has none.
    pub message_id: String,
}

/// The S256 challenge of `verifier`: base64url, unpadded, of its SHA-256.
pub fn challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier))
}

/// A fresh PKCE pair, its verifier 32 random bytes as base64url (43 characters).
pub fn pkce() -> Result<Pkce, String> {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("no randomness: {e}"))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    Ok(Pkce {
        challenge: challenge(&verifier),
        verifier,
    })
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `k=v&…`, each part encoded.
fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Google's consent page for `client_id`, coming back to `redirect` with `state`. `hint` is
/// the mailbox, so Google offers that account first.
pub fn auth_url(
    ep: &Endpoints,
    client_id: &str,
    redirect: &str,
    challenge: &str,
    state: &str,
    hint: Option<&str>,
) -> String {
    let mut pairs = vec![
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("access_type", "offline"),
        // Always the consent screen, so Google always hands back a refresh token.
        ("prompt", "consent"),
    ];
    if let Some(h) = hint {
        pairs.push(("login_hint", h));
    }
    format!("{}?{}", ep.auth, query(&pairs))
}

/// The code in the browser's return to `/?code=…&state=…` (tiny_http's `url()`), when it
/// carries `state`; Google's own error, such as `access_denied`, when it says no.
pub fn read_redirect(url: &str, state: &str) -> Result<String, String> {
    let fields = crate::slack_connect::form(url.split_once('?').map_or("", |(_, q)| q));
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("state") != Some(state) {
        return Err("this isn't the sign-in hydraui started".into());
    }
    if let Some(e) = get("error") {
        return Err(format!("Google said {e}"));
    }
    get("code")
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Google sent no code".into())
}

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font:15px system-ui,sans-serif;max-width:32rem;margin:15vh auto\">\
         <h1 style=\"font-size:20px\">{title}</h1><p>{text}</p></body></html>"
    )
}

/// Waits on `server` until the browser comes back from Google with `state`, or `wait` passes.
/// Any other request gets a 404 and is ignored.
pub fn receive(server: &tiny_http::Server, state: &str, wait: Duration) -> Result<String, String> {
    let html = |status: u16, body: String| {
        tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header(
                "Content-Type: text/html; charset=utf-8"
                    .parse::<tiny_http::Header>()
                    .expect("a valid header"),
            )
    };
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the browser didn't come back within 10 minutes".into());
        }
        let Ok(Some(req)) = server.recv_timeout(left) else {
            continue;
        };
        let url = req.url().to_string();
        if req.method() != &tiny_http::Method::Get || !url.starts_with("/?") {
            let _ = req.respond(html(404, page("Not here", "")));
            continue;
        }
        match read_redirect(&url, state) {
            Ok(code) => {
                let _ = req.respond(html(
                    200,
                    page(
                        "Gmail is connected to hydraui",
                        "You can close this tab and go back to hydraui.",
                    ),
                ));
                return Ok(code);
            }
            Err(e) => {
                let _ = req.respond(html(400, page("Gmail isn't connected", &e)));
            }
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(TIMEOUT).build()
}

/// A short reason for a failed call: the status and Google's own message, never the request.
fn failed(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body: serde_json::Value = r
                .into_string()
                .ok()
                .and_then(|b| serde_json::from_str(&b).ok())
                .unwrap_or_default();
            let said = body["error"]["message"]
                .as_str()
                .or(body["error_description"].as_str())
                .or(body["error"].as_str());
            match (body["error"].as_str(), said) {
                (Some("invalid_grant"), _) => {
                    format!(
                        "HTTP {code}: the sign-in has expired or was revoked. Connect Gmail again"
                    )
                }
                (_, Some(m)) => format!("HTTP {code}: {m}"),
                _ => format!("HTTP {code}"),
            }
        }
        ureq::Error::Transport(_) => "Google isn't reachable".into(),
    }
}

fn json(r: ureq::Response) -> Result<serde_json::Value, String> {
    r.into_string()
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .ok_or_else(|| "Google sent something unexpected".to_string())
}

fn token_call(ep: &Endpoints, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let r = agent()
        .post(&ep.token)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&query(form))
        .map_err(failed)?;
    json(r)
}

fn access_of(v: &serde_json::Value) -> Result<String, String> {
    v["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Google sent no access token".into())
}

/// Trades the consent page's `code` for tokens.
pub fn exchange(
    ep: &Endpoints,
    client: &Client,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<Tokens, String> {
    let v = token_call(
        ep,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("redirect_uri", redirect),
        ],
    )?;
    Ok(Tokens {
        access: access_of(&v)?,
        refresh: v["refresh_token"].as_str().map(str::to_string),
    })
}

/// A fresh access token from the stored refresh token.
pub fn refresh(ep: &Endpoints, client: &Client, refresh_token: &str) -> Result<String, String> {
    let v = token_call(
        ep,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &client.id),
            ("client_secret", &client.secret),
        ],
    )?;
    access_of(&v)
}

fn api_get(ep: &Endpoints, access: &str, path: &str) -> Result<serde_json::Value, String> {
    let r = agent()
        .get(&format!("{}/{path}", ep.api))
        .set("Authorization", &format!("Bearer {access}"))
        .call()
        .map_err(failed)?;
    json(r)
}

fn api_post(
    ep: &Endpoints,
    access: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let r = agent()
        .post(&format!("{}/{path}", ep.api))
        .set("Authorization", &format!("Bearer {access}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(failed)?;
    json(r)
}

/// The mailbox's labels, as (id, name).
pub fn labels(ep: &Endpoints, access: &str) -> Result<Vec<(String, String)>, String> {
    let v = api_get(ep, access, "users/me/labels")?;
    Ok(v["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            Some((
                l["id"].as_str()?.to_string(),
                l["name"].as_str()?.to_string(),
            ))
        })
        .collect())
}

/// Label `name`'s id, made when the mailbox hasn't got it. `known` is `labels`' answer, and
/// gains the new one.
pub fn label_id(
    ep: &Endpoints,
    access: &str,
    name: &str,
    known: &mut Vec<(String, String)>,
) -> Result<String, String> {
    if let Some((id, _)) = known.iter().find(|(_, n)| n == name) {
        return Ok(id.clone());
    }
    let v = api_post(
        ep,
        access,
        "users/me/labels",
        &serde_json::json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show",
        }),
    )?;
    let id = v["id"]
        .as_str()
        .ok_or_else(|| format!("Google made no label {name}"))?
        .to_string();
    known.push((id.clone(), name.to_string()));
    Ok(id)
}

/// The ids of the labels on any message of thread `id`.
pub fn thread_label_ids(ep: &Endpoints, access: &str, id: &str) -> Result<Vec<String>, String> {
    let v = api_get(
        ep,
        access,
        &format!("users/me/threads/{}?format=minimal", enc(id)),
    )?;
    let mut out: Vec<String> = Vec::new();
    for m in v["messages"].as_array().into_iter().flatten() {
        for l in m["labelIds"].as_array().into_iter().flatten() {
            if let Some(l) = l.as_str()
                && !out.iter().any(|o| o == l)
            {
                out.push(l.to_string());
            }
        }
    }
    Ok(out)
}

/// Adds and removes labels (by id) on every message of thread `id`.
pub fn modify_thread(
    ep: &Endpoints,
    access: &str,
    id: &str,
    add: &[String],
    remove: &[String],
) -> Result<(), String> {
    api_post(
        ep,
        access,
        &format!("users/me/threads/{}/modify", enc(id)),
        &serde_json::json!({"addLabelIds": add, "removeLabelIds": remove}),
    )
    .map(|_| ())
}

/// The signed-in mailbox's address.
pub fn profile(ep: &Endpoints, access: &str) -> Result<String, String> {
    api_get(ep, access, "users/me/profile")?["emailAddress"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "Google sent no address".into())
}

/// A thread from `threads.get` with `format=metadata`.
fn read_thread(v: &serde_json::Value) -> Option<Thread> {
    let messages = v["messages"].as_array()?;
    let header = |m: &serde_json::Value, name: &str| {
        m["payload"]["headers"]
            .as_array()
            .and_then(|hs| {
                hs.iter().find(|h| {
                    h["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                })
            })
            .and_then(|h| h["value"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let (first, last) = (messages.first()?, messages.last()?);
    Some(Thread {
        id: v["id"].as_str()?.to_string(),
        subject: header(first, "Subject"),
        from: header(last, "From"),
        date: header(last, "Date"),
        messages: messages.len(),
        message_id: header(first, "Message-ID"),
    })
}

/// One message of a thread, as a triage run reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub date: String,
    pub subject: String,
    /// Its plain-text body, or Gmail's snippet when it has none.
    pub body: String,
    /// The names of its attachments.
    pub attachments: Vec<String>,
}

/// The plain text of a `format=full` payload: its own `text/plain` body, else its parts'.
fn plain_text(payload: &serde_json::Value) -> Option<String> {
    use base64::Engine;
    if payload["mimeType"].as_str() == Some("text/plain")
        && let Some(data) = payload["body"]["data"].as_str()
    {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(data.trim_end_matches('='))
            .ok()?;
        return Some(String::from_utf8_lossy(&bytes).trim().to_string());
    }
    payload["parts"].as_array()?.iter().find_map(plain_text)
}

/// The file names of a payload's attachments, its parts' included.
fn attachments(payload: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(name) = payload["filename"].as_str().filter(|n| !n.is_empty()) {
        out.push(name.to_string());
    }
    for part in payload["parts"].as_array().into_iter().flatten() {
        attachments(part, out);
    }
}

/// Every message of thread `id`, oldest first.
pub fn thread_messages(ep: &Endpoints, access: &str, id: &str) -> Result<Vec<Message>, String> {
    let v = api_get(
        ep,
        access,
        &format!("users/me/threads/{}?format=full", enc(id)),
    )?;
    let messages = v["messages"]
        .as_array()
        .ok_or_else(|| format!("thread {id} came back unreadable"))?;
    Ok(messages
        .iter()
        .map(|m| {
            let header = |name: &str| {
                m["payload"]["headers"]
                    .as_array()
                    .and_then(|hs| {
                        hs.iter().find(|h| {
                            h["name"]
                                .as_str()
                                .is_some_and(|n| n.eq_ignore_ascii_case(name))
                        })
                    })
                    .and_then(|h| h["value"].as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let mut files = Vec::new();
            attachments(&m["payload"], &mut files);
            Message {
                from: header("From"),
                to: header("To"),
                date: header("Date"),
                subject: header("Subject"),
                body: plain_text(&m["payload"])
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| m["snippet"].as_str().unwrap_or("").to_string()),
                attachments: files,
            }
        })
        .collect())
}

/// Up to `max` threads matching Gmail search `query`, newest first.
pub fn list_threads(
    ep: &Endpoints,
    access: &str,
    q: &str,
    max: usize,
) -> Result<Vec<Thread>, String> {
    let list = api_get(
        ep,
        access,
        &format!(
            "users/me/threads?{}",
            query(&[("q", q), ("maxResults", &max.to_string())])
        ),
    )?;
    let ids: Vec<&str> = list["threads"]
        .as_array()
        .map(|ts| ts.iter().filter_map(|t| t["id"].as_str()).collect())
        .unwrap_or_default();
    ids.into_iter()
        .map(|id| {
            let v = api_get(
                ep,
                access,
                &format!(
                    "users/me/threads/{}?format=metadata&metadataHeaders=Subject\
                     &metadataHeaders=From&metadataHeaders=Date&metadataHeaders=Message-ID",
                    enc(id)
                ),
            )?;
            read_thread(&v).ok_or_else(|| format!("thread {id} came back unreadable"))
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod testing {
    //! A fake Google for tests, on a local port.
    use super::Endpoints;
    use std::sync::mpsc;

    /// A request the fake Google saw.
    #[derive(Debug)]
    pub struct Seen {
        pub method: String,
        pub url: String,
        pub auth: Option<String>,
        pub body: String,
    }

    /// A fake Google answering each request with the first `(path prefix, status, body)` that
    /// fits, for `n` requests; what it saw comes back on the channel. A route with a later one
    /// for the same prefix is used once, so a path can answer differently the next time.
    pub fn fake(
        mut routes: Vec<(&'static str, u16, String)>,
        n: usize,
    ) -> (Endpoints, mpsc::Receiver<Seen>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok(mut req) = server.recv() else { return };
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let auth = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.to_string());
                let url = req.url().to_string();
                let hit = routes.iter().position(|(p, _, _)| url.starts_with(p));
                let (status, reply) = hit.map_or((404, String::new()), |i| {
                    let (p, s, b) = routes[i].clone();
                    if routes[i + 1..].iter().any(|(q, _, _)| *q == p) {
                        routes.remove(i);
                    }
                    (s, b)
                });
                let _ = tx.send(Seen {
                    method: req.method().to_string(),
                    url,
                    auth,
                    body,
                });
                let _ =
                    req.respond(tiny_http::Response::from_string(reply).with_status_code(status));
            }
        });
        (
            Endpoints {
                auth: format!("{base}/auth"),
                token: format!("{base}/token"),
                api: format!("{base}/gmail/v1"),
            },
            rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::fake;
    use super::*;

    #[test]
    fn the_challenge_is_s256_base64url() {
        let v = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-._~0123456789";
        assert_eq!(challenge(v), "SCNC9wjP-7jzYH1kycLJ7OpdBnOChaiHSKNg_qFxuls");
    }

    #[test]
    fn a_pkce_pair_is_fresh_and_matches() {
        let (a, b) = (pkce().unwrap(), pkce().unwrap());
        assert_eq!(a.verifier.len(), 43);
        assert!(
            a.verifier
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_ne!(a.verifier, b.verifier);
        assert_eq!(a.challenge, challenge(&a.verifier));
    }

    #[test]
    fn the_consent_page_asks_for_modify_offline_with_pkce() {
        let ep = Endpoints::google();
        let url = auth_url(
            &ep,
            "id.apps",
            "http://127.0.0.1:5000/",
            "CH",
            "st",
            Some("integrations@iov42.com"),
        );
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{url}"
        );
        for part in [
            "client_id=id.apps",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2F",
            "response_type=code",
            "scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.modify",
            "code_challenge=CH",
            "code_challenge_method=S256",
            "state=st",
            "access_type=offline",
            "prompt=consent",
            "login_hint=integrations%40iov42.com",
        ] {
            assert!(url.contains(part), "{part} missing from {url}");
        }
        assert!(!auth_url(&ep, "i", "r", "c", "s", None).contains("login_hint"));
    }

    #[test]
    fn the_redirect_gives_the_code_only_with_our_state() {
        assert_eq!(
            read_redirect("/?state=st&code=4%2F0Ab", "st").unwrap(),
            "4/0Ab"
        );
        assert!(read_redirect("/?state=other&code=x", "st").is_err());
        assert!(read_redirect("/?state=st", "st").is_err());
        let denied = read_redirect("/?error=access_denied&state=st", "st").unwrap_err();
        assert!(denied.contains("access_denied"), "{denied}");
    }

    #[test]
    fn the_receiver_ignores_strays_and_takes_the_real_return() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let get = move |path: &str| {
            ureq::get(&format!("http://{addr}{path}"))
                .call()
                .map(|r| r.status())
                .unwrap_or_else(|e| match e {
                    ureq::Error::Status(code, _) => code,
                    _ => 0,
                })
        };
        let browser = std::thread::spawn(move || {
            [
                get("/favicon.ico"),
                get("/?state=wrong&code=x"),
                get("/?state=n1&code=good"),
            ]
        });
        assert_eq!(
            receive(&server, "n1", Duration::from_secs(10)).unwrap(),
            "good"
        );
        assert_eq!(browser.join().unwrap(), [404, 400, 200]);
    }

    #[test]
    fn the_receiver_gives_up() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let err = receive(&server, "n1", Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("didn't come back"), "{err}");
    }

    fn client() -> Client {
        Client {
            id: "cid".into(),
            secret: "csecret".into(),
        }
    }

    #[test]
    fn a_code_is_traded_with_its_verifier() {
        let (ep, seen) = fake(
            vec![(
                "/token",
                200,
                r#"{"access_token":"ya29.a","refresh_token":"1//r","expires_in":3599}"#.into(),
            )],
            1,
        );
        let t = exchange(&ep, &client(), "4/code", "VER", "http://127.0.0.1:1/").unwrap();
        assert_eq!(t.access, "ya29.a");
        assert_eq!(t.refresh.as_deref(), Some("1//r"));
        let s = seen.recv().unwrap();
        assert_eq!(s.method, "POST");
        for part in [
            "grant_type=authorization_code",
            "code=4%2Fcode",
            "code_verifier=VER",
            "client_id=cid",
            "client_secret=csecret",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2F",
        ] {
            assert!(s.body.contains(part), "{part} missing from {}", s.body);
        }
    }

    #[test]
    fn a_refresh_gives_an_access_token() {
        let (ep, seen) = fake(
            vec![("/token", 200, r#"{"access_token":"ya29.b"}"#.into())],
            1,
        );
        assert_eq!(refresh(&ep, &client(), "1//r").unwrap(), "ya29.b");
        let s = seen.recv().unwrap();
        assert!(s.body.contains("grant_type=refresh_token"), "{}", s.body);
        assert!(s.body.contains("refresh_token=1%2F%2Fr"), "{}", s.body);
    }

    #[test]
    fn a_revoked_sign_in_says_so_without_the_token() {
        let (ep, _) = fake(
            vec![(
                "/token",
                400,
                r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into(),
            )],
            1,
        );
        let err = refresh(&ep, &client(), "1//secret-refresh").unwrap_err();
        assert!(err.contains("Connect Gmail again"), "{err}");
        assert!(!err.contains("secret-refresh"), "{err}");
    }

    #[test]
    fn the_profile_names_the_mailbox() {
        let (ep, seen) = fake(
            vec![(
                "/gmail/v1/users/me/profile",
                200,
                r#"{"emailAddress":"integrations@iov42.com","messagesTotal":5}"#.into(),
            )],
            1,
        );
        assert_eq!(profile(&ep, "ya29.a").unwrap(), "integrations@iov42.com");
        assert_eq!(seen.recv().unwrap().auth.as_deref(), Some("Bearer ya29.a"));
    }

    fn thread_json(id: &str, subject: &str, from: &str) -> String {
        serde_json::json!({
            "id": id,
            "messages": [
                {"payload": {"headers": [
                    {"name": "Subject", "value": subject},
                    {"name": "From", "value": "Customer <c@example.com>"},
                    {"name": "Date", "value": "Mon, 9 Nov 2026 10:00:00 +0000"},
                    {"name": "Message-Id", "value": format!("<{id}@mail.example.com>")}
                ]}},
                {"payload": {"headers": [
                    {"name": "Subject", "value": format!("Re: {subject}")},
                    {"name": "From", "value": from},
                    {"name": "Date", "value": "Tue, 10 Nov 2026 09:30:00 +0000"}
                ]}}
            ]
        })
        .to_string()
    }

    #[test]
    fn threads_are_listed_with_their_subject_and_last_sender() {
        let (ep, seen) = fake(
            vec![
                (
                    "/gmail/v1/users/me/threads?",
                    200,
                    r#"{"threads":[{"id":"t1"},{"id":"t2"}],"resultSizeEstimate":2}"#.into(),
                ),
                (
                    "/gmail/v1/users/me/threads/t1",
                    200,
                    thread_json("t1", "API 401", "Ann <a@x.com>"),
                ),
                (
                    "/gmail/v1/users/me/threads/t2",
                    200,
                    thread_json("t2", "How to share", "Bo <b@x.com>"),
                ),
            ],
            3,
        );
        let got = list_threads(&ep, "ya29.a", "in:inbox newer_than:14d", 10).unwrap();
        assert_eq!(
            got[0],
            Thread {
                id: "t1".into(),
                subject: "API 401".into(),
                from: "Ann <a@x.com>".into(),
                date: "Tue, 10 Nov 2026 09:30:00 +0000".into(),
                messages: 2,
                // The first message's, whatever the header's case.
                message_id: "<t1@mail.example.com>".into(),
            }
        );
        assert_eq!(got[1].subject, "How to share");
        let list = seen.recv().unwrap();
        assert!(
            list.url.contains("q=in%3Ainbox%20newer_than%3A14d"),
            "{}",
            list.url
        );
        assert!(list.url.contains("maxResults=10"), "{}", list.url);
        let one = seen.recv().unwrap();
        assert!(one.url.contains("format=metadata"), "{}", one.url);
        assert!(
            one.url.contains("metadataHeaders=Message-ID"),
            "{}",
            one.url
        );
        assert_eq!(one.auth.as_deref(), Some("Bearer ya29.a"));
    }

    #[test]
    fn an_empty_mailbox_lists_nothing() {
        let (ep, _) = fake(
            vec![(
                "/gmail/v1/users/me/threads?",
                200,
                r#"{"resultSizeEstimate":0}"#.into(),
            )],
            1,
        );
        assert!(list_threads(&ep, "a", "in:inbox", 5).unwrap().is_empty());
    }

    fn b64(s: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE.encode(s)
    }

    #[test]
    fn a_threads_messages_come_with_their_plain_text() {
        let full = serde_json::json!({
            "id": "t1",
            "messages": [
                {"snippet": "Hi, how do I", "payload": {
                    "mimeType": "multipart/alternative",
                    "headers": [
                        {"name": "From", "value": "Ann <ann@acme.com>"},
                        {"name": "To", "value": "integrations@iov42.com"},
                        {"name": "Subject", "value": "Sharing"},
                        {"name": "Date", "value": "Mon, 9 Nov 2026 10:00:00 +0000"}
                    ],
                    "parts": [
                        {"mimeType": "text/plain", "body": {"data": b64("Hi, how do I share a record? é")}},
                        {"mimeType": "text/html", "body": {"data": b64("<p>Hi</p>")}},
                        {"mimeType": "application/pdf", "filename": "log.pdf", "body": {"attachmentId": "a1"}}
                    ]
                }},
                {"snippet": "Only a snippet", "payload": {
                    "mimeType": "text/html",
                    "headers": [{"name": "From", "value": "integrations@iov42.com"}],
                    "body": {"data": b64("<p>Thanks</p>")}
                }}
            ]
        })
        .to_string();
        let (ep, seen) = fake(vec![("/gmail/v1/users/me/threads/t1", 200, full)], 1);
        let got = thread_messages(&ep, "ya29.a", "t1").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0],
            Message {
                from: "Ann <ann@acme.com>".into(),
                to: "integrations@iov42.com".into(),
                date: "Mon, 9 Nov 2026 10:00:00 +0000".into(),
                subject: "Sharing".into(),
                body: "Hi, how do I share a record? é".into(),
                attachments: vec!["log.pdf".into()],
            }
        );
        // No plain text: Gmail's snippet.
        assert_eq!(got[1].body, "Only a snippet");
        assert!(seen.recv().unwrap().url.contains("format=full"));
    }

    #[test]
    fn labels_are_listed_made_when_missing_and_read_off_a_thread() {
        let (ep, seen) = fake(
            vec![
                (
                    "/gmail/v1/users/me/labels",
                    200,
                    r#"{"labels":[{"id":"INBOX","name":"INBOX"},{"id":"Label_1","name":"hydra/claimed"}]}"#
                        .into(),
                ),
            ],
            1,
        );
        let mut known = labels(&ep, "a").unwrap();
        assert_eq!(
            known[1],
            ("Label_1".to_string(), "hydra/claimed".to_string())
        );
        assert_eq!(seen.recv().unwrap().method, "GET");
        // Known: no call.
        assert_eq!(
            label_id(&ep, "a", "hydra/claimed", &mut known).unwrap(),
            "Label_1"
        );

        let (ep, seen) = fake(
            vec![(
                "/gmail/v1/users/me/labels",
                200,
                r#"{"id":"Label_2","name":"hydra/claimed/jordan"}"#.into(),
            )],
            1,
        );
        assert_eq!(
            label_id(&ep, "a", "hydra/claimed/jordan", &mut known).unwrap(),
            "Label_2"
        );
        assert_eq!(known.len(), 3);
        let made = seen.recv().unwrap();
        assert_eq!(made.method, "POST");
        assert!(
            made.body.contains("\"name\":\"hydra/claimed/jordan\""),
            "{}",
            made.body
        );

        let (ep, seen) = fake(
            vec![(
                "/gmail/v1/users/me/threads/t1",
                200,
                r#"{"id":"t1","messages":[{"labelIds":["INBOX","Label_1"]},{"labelIds":["Label_2","Label_1"]}]}"#
                    .into(),
            )],
            1,
        );
        let mut ids = thread_label_ids(&ep, "a", "t1").unwrap();
        ids.sort();
        assert_eq!(ids, ["INBOX", "Label_1", "Label_2"]);
        assert!(seen.recv().unwrap().url.contains("format=minimal"));
    }

    #[test]
    fn a_threads_labels_are_changed_in_one_call() {
        let (ep, seen) = fake(
            vec![(
                "/gmail/v1/users/me/threads/t1/modify",
                200,
                r#"{"id":"t1"}"#.into(),
            )],
            1,
        );
        modify_thread(&ep, "a", "t1", &["Label_3".into()], &["Label_1".into()]).unwrap();
        let s = seen.recv().unwrap();
        assert_eq!(s.method, "POST");
        let body: serde_json::Value = serde_json::from_str(&s.body).unwrap();
        assert_eq!(body["addLabelIds"], serde_json::json!(["Label_3"]));
        assert_eq!(body["removeLabelIds"], serde_json::json!(["Label_1"]));
    }

    #[test]
    fn an_api_refusal_is_named() {
        let (ep, _) = fake(
            vec![(
                "/gmail/v1/users/me/threads?",
                403,
                r#"{"error":{"code":403,"message":"Gmail API has not been used in project 1 before or it is disabled."}}"#.into(),
            )],
            1,
        );
        let err = list_threads(&ep, "a", "in:inbox", 5).unwrap_err();
        assert!(
            err.contains("HTTP 403") && err.contains("Gmail API has not been used"),
            "{err}"
        );
    }

    #[test]
    fn encoding_keeps_only_unreserved_characters() {
        assert_eq!(enc("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(enc("a b/c@d:e+é"), "a%20b%2Fc%40d%3Ae%2B%C3%A9");
    }
}
