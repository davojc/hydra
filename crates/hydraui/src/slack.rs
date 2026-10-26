//! Slack's Web API, as a `[slack]` pipeline uses it: a channel's recent messages and adding a
//! reaction to one. Replies are read by pure functions; `Http` sends them, blocking, so call it
//! off the UI thread. Errors reuse the trigger's `LinearError` split: `Rejected` stops watching
//! at once, `Failed` is retried.

use serde_json::{Value, json};

use crate::linear::LinearError;

const API: &str = "https://slack.com/api";

/// A message in the channel, as a pick needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Its timestamp, Slack's id for it.
    pub ts: String,
    /// Who posted it (a user id).
    pub user: String,
    pub text: String,
    /// The names of the reactions on it.
    pub reactions: Vec<String>,
}

/// A GitHub pull request named in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub url: String,
    pub owner: String,
    pub repo: String,
    pub number: u32,
}

impl Pr {
    /// The run's ticket: `<repo>-<number>`, e.g. `svc-usage-62`.
    pub fn ticket(&self) -> String {
        format!("{}-{}", self.repo, self.number)
    }

    /// `svc-usage #62`, for the card.
    pub fn title(&self) -> String {
        format!("{} #{}", self.repo, self.number)
    }
}

/// The pull requests `text` links to, in order, each once. Slack writes a link as
/// `<https://…|label>`; a bare URL is read too.
pub fn pr_links(text: &str) -> Vec<Pr> {
    let mut out: Vec<Pr> = Vec::new();
    for (i, _) in text.match_indices("https://github.com/") {
        let rest = &text[i + "https://github.com/".len()..];
        let end = rest
            .find(|c: char| c == '|' || c == '>' || c.is_whitespace())
            .unwrap_or(rest.len());
        let parts: Vec<&str> = rest[..end].split('/').collect();
        let [owner, repo, "pull", number, ..] = parts.as_slice() else {
            continue;
        };
        let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
        let Ok(number) = digits.parse::<u32>() else {
            continue;
        };
        let pr = Pr {
            url: format!("https://github.com/{owner}/{repo}/pull/{number}"),
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        };
        if !out.contains(&pr) {
            out.push(pr);
        }
    }
    out
}

/// Why a reply says `ok: false`, sorted: a token or channel that can't work stops watching.
pub fn reply_error(reply: &Value) -> Option<LinearError> {
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let code = reply
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("no reply");
    let rejected = |why: String| Some(LinearError::Rejected(why));
    match code {
        "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" | "account_inactive" => {
            rejected(format!("Slack rejected the token ({code})"))
        }
        "missing_scope" => rejected(format!(
            "the Slack token lacks a scope ({}): it needs channels:history, reactions:read and \
             reactions:write",
            reply
                .get("needed")
                .and_then(Value::as_str)
                .unwrap_or("see the token's app")
        )),
        "channel_not_found" | "not_in_channel" | "is_archived" => {
            rejected(format!("Slack can't read the channel ({code})"))
        }
        _ => Some(LinearError::Failed(format!("Slack answered {code}"))),
    }
}

/// The channel's messages in a `conversations.history` reply, oldest first. Joins, bot
/// posts and other subtypes are left out: only people post pull requests.
pub fn read_history(reply: &Value) -> Result<Vec<Message>, LinearError> {
    if let Some(e) = reply_error(reply) {
        return Err(e);
    }
    let text = |m: &Value, k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut out: Vec<Message> = reply
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|m| m.get("subtype").is_none())
        .map(|m| Message {
            ts: text(m, "ts"),
            user: text(m, "user"),
            text: text(m, "text"),
            reactions: m
                .get("reactions")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|r| r.get("name")?.as_str().map(str::to_string))
                .collect(),
        })
        .filter(|m| !m.ts.is_empty())
        .collect();
    // Slack lists newest first; picks go oldest first, as people posted them.
    out.sort_by(|a, b| {
        let n = |ts: &str| ts.parse::<f64>().unwrap_or(0.0);
        n(&a.ts).total_cmp(&n(&b.ts))
    });
    Ok(out)
}

/// What the watcher needs from Slack; `Http` is the real one, tests use a fake.
pub trait SlackApi {
    /// The channel's messages since `oldest` (seconds since the epoch), oldest first.
    fn history(&self, channel: &str, oldest: u64) -> Result<Vec<Message>, LinearError>;
    /// Adds reaction `name` to message `ts`; one already there counts as added.
    fn react(&self, channel: &str, ts: &str, name: &str) -> Result<(), LinearError>;
}

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Slack over HTTPS with a token (a user token's reactions show as that person).
pub struct Http {
    pub token: String,
}

impl Http {
    fn call(&self, req: ureq::Request, body: Option<Value>) -> Result<Value, LinearError> {
        let req = req.set("Authorization", &format!("Bearer {}", self.token.trim()));
        let resp = match body {
            Some(b) => req
                .set("Content-Type", "application/json; charset=utf-8")
                .send_string(&b.to_string()),
            None => req.call(),
        };
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(429, _)) => {
                return Err(LinearError::Failed("Slack is rate limiting".into()));
            }
            Err(ureq::Error::Status(code, _)) => {
                return Err(LinearError::Failed(format!("Slack answered HTTP {code}")));
            }
            Err(e) => return Err(LinearError::Failed(format!("Slack isn't reachable: {e}"))),
        };
        let text = resp
            .into_string()
            .map_err(|e| LinearError::Failed(format!("Slack's reply: {e}")))?;
        serde_json::from_str(&text)
            .map_err(|_| LinearError::Failed("Slack's reply isn't JSON".into()))
    }
}

impl SlackApi for Http {
    fn history(&self, channel: &str, oldest: u64) -> Result<Vec<Message>, LinearError> {
        if self.token.trim().is_empty() {
            return Err(LinearError::Rejected("no Slack token".into()));
        }
        let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
        let req = agent
            .get(&format!("{API}/conversations.history"))
            .query("channel", channel)
            .query("oldest", &oldest.to_string())
            .query("limit", "200");
        read_history(&self.call(req, None)?)
    }

    fn react(&self, channel: &str, ts: &str, name: &str) -> Result<(), LinearError> {
        let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
        let reply = self.call(
            agent.post(&format!("{API}/reactions.add")),
            Some(json!({"channel": channel, "timestamp": ts, "name": name})),
        )?;
        if reply.get("error").and_then(Value::as_str) == Some("already_reacted") {
            return Ok(());
        }
        match reply_error(&reply) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_requests_are_read_from_slack_links_and_bare_urls() {
        let text = "Three Lingo PRs:\n1. INFRA-1183 <https://github.com/iov42/iov42-lingo-app/pull/44|github.com/iov42/iov42-lingo-app/pull/44>\n\
                    2. https://github.com/iov42/iov42-lingo-app/pull/45/files and again \
                    <https://github.com/iov42/iov42-lingo-app/pull/44>\n\
                    not one: https://github.com/iov42/iov42-lingo-app/issues/3";
        let prs = pr_links(text);
        assert_eq!(
            prs.iter().map(Pr::ticket).collect::<Vec<_>>(),
            ["iov42-lingo-app-44", "iov42-lingo-app-45"]
        );
        assert_eq!(
            prs[1].url,
            "https://github.com/iov42/iov42-lingo-app/pull/45"
        );
        assert_eq!(prs[0].title(), "iov42-lingo-app #44");
        assert!(pr_links("no links here").is_empty());
    }

    #[test]
    fn history_is_people_messages_oldest_first_with_their_reactions() {
        let reply = json!({"ok": true, "messages": [
            {"ts": "1793023484.850479", "user": "U2", "text": "<https://github.com/iov42/svc-usage/pull/62|x>",
             "reactions": [{"name": "eyes", "count": 1}]},
            {"ts": "1793020000.000001", "subtype": "channel_join", "user": "U3", "text": "joined"},
            {"ts": "1793013365.003299", "user": "U2", "text": "<https://github.com/iov42/svc-usage/pull/60|y>"},
        ]});
        let got = read_history(&reply).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].ts, "1793013365.003299");
        assert!(got[0].reactions.is_empty());
        assert_eq!(got[1].reactions, ["eyes"]);
    }

    #[test]
    fn a_bad_token_or_channel_stops_watching_and_others_retry() {
        let e = |code: &str| reply_error(&json!({"ok": false, "error": code})).unwrap();
        assert!(matches!(e("invalid_auth"), LinearError::Rejected(_)));
        assert!(matches!(e("not_in_channel"), LinearError::Rejected(_)));
        let scope = reply_error(
            &json!({"ok": false, "error": "missing_scope", "needed": "reactions:write"}),
        );
        assert!(matches!(scope, Some(LinearError::Rejected(w)) if w.contains("reactions:write")));
        assert!(matches!(e("internal_error"), LinearError::Failed(_)));
        assert_eq!(reply_error(&json!({"ok": true})), None);
    }
}
