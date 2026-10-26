//! **Connect Slack**: a person's own Slack user token for `[slack]` pipelines, without each
//! person making a Slack app. hydraui listens on a local port and opens the team's Canopy
//! (`slack_connect_url`), which holds the one shared app's secret: Canopy sends the browser
//! through Slack's consent screen and posts the token back to `http://127.0.0.1:<port>/slack`
//! with the nonce hydraui made. Only a post carrying that nonce is taken. The token is then
//! stored as the secret `<env>/slack` (`Hydra::slack_token`).

use std::io::Read;
use std::time::{Duration, Instant};

/// How long the browser has to come back.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

/// What came back from Slack, through Canopy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    pub token: String,
    /// The Slack workspace's name.
    pub team: String,
    /// The person's Slack user id.
    pub user: String,
}

/// A fresh nonce: 24 random bytes as hex.
pub fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| format!("no randomness: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Where the browser starts: Canopy's connect page, told hydraui's port and nonce.
pub fn start_url(canopy: &str, port: u16, state: &str) -> String {
    format!(
        "{}/api/slack/connect?port={port}&state={state}",
        canopy.trim_end_matches('/')
    )
}

/// `application/x-www-form-urlencoded` text as pairs.
pub fn form(body: &str) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                    match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        Some(b) => {
                            out.push(b);
                            i += 2;
                        }
                        None => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    body.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(p), String::new()),
        })
        .collect()
}

/// The token in a post from Canopy, when it carries `state`.
pub fn read_post(body: &str, state: &str) -> Result<Connected, String> {
    let fields = form(body);
    let get = |k: &str| {
        fields
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    };
    if get("state") != state {
        return Err("this sign-in wasn't started by this hydraui".into());
    }
    let token = get("token");
    if !token.starts_with("xoxp-") {
        return Err("Slack sent no user token".into());
    }
    Ok(Connected {
        token,
        team: get("team"),
        user: get("user"),
    })
}

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font:15px system-ui,sans-serif;max-width:32rem;margin:15vh auto\">\
         <h1 style=\"font-size:20px\">{title}</h1><p>{text}</p></body></html>"
    )
}

/// Waits on `server` until Canopy posts the token with `state`, or `wait` passes. Anything
/// else gets a 404 and is ignored. Blocking: call it off the UI thread.
pub fn receive(
    server: &tiny_http::Server,
    state: &str,
    wait: Duration,
) -> Result<Connected, String> {
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
        let Ok(Some(mut req)) = server.recv_timeout(left) else {
            continue;
        };
        if req.method() != &tiny_http::Method::Post || req.url() != "/slack" {
            let _ = req.respond(html(404, page("Not here", "")));
            continue;
        }
        let mut body = String::new();
        let _ = req.as_reader().take(64 * 1024).read_to_string(&mut body);
        match read_post(&body, state) {
            Ok(c) => {
                let _ = req.respond(html(
                    200,
                    page(
                        "Slack is connected to hydraui",
                        "You can close this tab. Pipelines that watch Slack use your account now.",
                    ),
                ));
                return Ok(c);
            }
            Err(e) => {
                let _ = req.respond(html(400, page("Slack isn't connected", &e)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonce_is_long_and_fresh() {
        let (a, b) = (nonce().unwrap(), nonce().unwrap());
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
    }

    #[test]
    fn the_browser_starts_at_canopy_with_the_port_and_nonce() {
        assert_eq!(
            start_url("https://canopy.dev.iov42.net/", 51234, "ab12"),
            "https://canopy.dev.iov42.net/api/slack/connect?port=51234&state=ab12"
        );
    }

    #[test]
    fn form_fields_are_decoded() {
        assert_eq!(
            form("state=a1&token=xoxp-1%2D2&team=iov42+ltd&user=U1&bad=%zz&x"),
            [
                ("state".into(), "a1".into()),
                ("token".into(), "xoxp-1-2".into()),
                ("team".into(), "iov42 ltd".into()),
                ("user".into(), "U1".into()),
                ("bad".into(), "%zz".into()),
                ("x".into(), String::new()),
            ]
        );
        assert_eq!(form("t=%4"), [("t".into(), "%4".into())]);
    }

    #[test]
    fn only_a_post_with_this_nonce_and_a_user_token_counts() {
        let ok = read_post("state=n1&token=xoxp-9&team=iov42&user=U1", "n1").unwrap();
        assert_eq!((ok.token.as_str(), ok.user.as_str()), ("xoxp-9", "U1"));
        assert!(read_post("state=other&token=xoxp-9", "n1").is_err());
        assert!(
            read_post("state=n1&token=xoxb-9", "n1").is_err(),
            "a bot token"
        );
        assert!(read_post("state=n1", "n1").is_err());
    }

    #[test]
    fn the_receiver_ignores_strays_and_takes_the_real_post() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let send = move |path: &str, body: &str| {
            ureq::post(&format!("http://{addr}{path}"))
                .set("Content-Type", "application/x-www-form-urlencoded")
                .send_string(body)
                .map(|r| r.status())
                .unwrap_or_else(|e| match e {
                    ureq::Error::Status(code, _) => code,
                    _ => 0,
                })
        };
        let poster = std::thread::spawn(move || {
            [
                send("/other", "state=n1&token=xoxp-1"),
                send("/slack", "state=wrong&token=xoxp-1"),
                send("/slack", "state=n1&token=xoxp-1&team=iov42&user=U1"),
            ]
        });
        let got = receive(&server, "n1", Duration::from_secs(10)).unwrap();
        assert_eq!(got.token, "xoxp-1");
        assert_eq!(poster.join().unwrap(), [404, 400, 200]);
    }

    #[test]
    fn the_receiver_gives_up() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let err = receive(&server, "n1", Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("didn't come back"), "{err}");
    }
}
