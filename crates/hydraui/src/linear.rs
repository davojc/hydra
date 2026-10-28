//! Linear's GraphQL API, as the orchestrator uses it (phase 2 spec §3, §4, §6): the key's user,
//! a team's states, the tickets a pipeline may pick, one ticket, and the two writes (move or
//! assign a ticket, comment on it). Requests are built and replies read by pure functions; `Http`
//! sends them, blocking, so call it off the UI thread.

use serde_json::{Value, json};

/// Linear's GraphQL endpoint.
pub const URL: &str = "https://api.linear.app/graphql";

/// Why a call to Linear failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearError {
    /// The API key is missing, wrong or revoked: watching stops at once (spec §3).
    Rejected(String),
    /// Anything else (network, a 5xx, an error Linear names): retried at the next check.
    Failed(String),
}

impl std::fmt::Display for LinearError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinearError::Rejected(why) | LinearError::Failed(why) => f.write_str(why),
        }
    }
}

/// A workflow state of a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub id: String,
    pub name: String,
}

/// A ticket, as much of it as the orchestrator needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub id: String,
    /// `INTH-170`.
    pub identifier: String,
    pub title: String,
    pub url: String,
    /// RFC 3339 times, as Linear sends them.
    pub sla_breaches_at: Option<String>,
    pub sla_high_risk_at: Option<String>,
    pub assignee: Option<String>,
    pub state: String,
    /// Linear's priority: 1 Urgent … 4 Low, 0 none.
    pub priority: u8,
    /// When it was made, RFC 3339.
    pub created_at: String,
}

/// What the orchestrator asks of Linear, so the watcher can be tested with a fake.
pub trait LinearApi {
    /// The id of the user the key belongs to.
    fn viewer(&self) -> Result<String, LinearError>;
    fn states(&self, team: &str) -> Result<Vec<State>, LinearError>;
    /// The team's tickets in `state_id`; unassigned ones only when `unassigned_only`.
    fn pickable(
        &self,
        team: &str,
        state_id: &str,
        unassigned_only: bool,
    ) -> Result<Vec<Ticket>, LinearError>;
    /// One ticket by its identifier; `None` when Linear has none.
    fn ticket(&self, identifier: &str) -> Result<Option<Ticket>, LinearError>;
    /// Moves ticket `id` to `state_id`, assigning it to `assignee` when given.
    fn update(&self, id: &str, state_id: &str, assignee: Option<&str>) -> Result<(), LinearError>;
    fn comment(&self, id: &str, body: &str) -> Result<(), LinearError>;
}

const TICKET_FIELDS: &str = "id identifier title url slaBreachesAt slaHighRiskAt priority createdAt assignee { id } \
     state { name }";

pub fn viewer_request() -> Value {
    json!({"query": "query { viewer { id } }"})
}

pub fn states_request(team: &str) -> Value {
    json!({
        "query": "query($team: String!) { teams(filter: { key: { eq: $team } }) \
                  { nodes { states { nodes { id name } } } } }",
        "variables": {"team": team},
    })
}

pub fn pickable_request(team: &str, state_id: &str, unassigned_only: bool) -> Value {
    let mut filter = json!({
        "team": {"key": {"eq": team}},
        "state": {"id": {"eq": state_id}},
    });
    if unassigned_only {
        filter["assignee"] = json!({"null": true});
    }
    json!({
        "query": format!(
            "query($filter: IssueFilter!) {{ issues(filter: $filter, first: 100) \
             {{ nodes {{ {TICKET_FIELDS} }} }} }}"
        ),
        "variables": {"filter": filter},
    })
}

pub fn ticket_request(identifier: &str) -> Value {
    json!({
        "query": format!("query($id: String!) {{ issue(id: $id) {{ {TICKET_FIELDS} }} }}"),
        "variables": {"id": identifier},
    })
}

pub fn update_request(id: &str, state_id: &str, assignee: Option<&str>) -> Value {
    let mut input = json!({"stateId": state_id});
    if let Some(a) = assignee {
        input["assigneeId"] = json!(a);
    }
    json!({
        "query": "mutation($id: String!, $input: IssueUpdateInput!) \
                  { issueUpdate(id: $id, input: $input) { success } }",
        "variables": {"id": id, "input": input},
    })
}

pub fn comment_request(id: &str, body: &str) -> Value {
    json!({
        "query": "mutation($input: CommentCreateInput!) { commentCreate(input: $input) { success } }",
        "variables": {"input": {"issueId": id, "body": body}},
    })
}

/// The error a reply carries, if any: Linear's `errors`, an authentication one as `Rejected`.
pub fn reply_error(reply: &Value) -> Option<LinearError> {
    let errors = reply.get("errors")?.as_array().filter(|e| !e.is_empty())?;
    let auth = errors.iter().any(|e| {
        e["extensions"]["code"].as_str() == Some("AUTHENTICATION_ERROR")
            || e["extensions"]["type"].as_str() == Some("authentication error")
    });
    let message = errors[0]["message"].as_str().unwrap_or("an error").trim();
    Some(if auth {
        LinearError::Rejected(format!("Linear rejected the API key: {message}"))
    } else {
        LinearError::Failed(format!("Linear: {message}"))
    })
}

/// `reply`'s data, or the error it carries instead.
fn data(reply: &Value) -> Result<&Value, LinearError> {
    if let Some(e) = reply_error(reply) {
        return Err(e);
    }
    reply
        .get("data")
        .filter(|d| !d.is_null())
        .ok_or_else(|| LinearError::Failed("Linear sent no data".into()))
}

fn unexpected(what: &str) -> LinearError {
    LinearError::Failed(format!("Linear's reply had no {what}"))
}

fn read_one_ticket(t: &Value) -> Option<Ticket> {
    let text = |k: &str| t.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Ticket {
        id: text("id")?,
        identifier: text("identifier")?,
        title: text("title").unwrap_or_default(),
        url: text("url").unwrap_or_default(),
        sla_breaches_at: text("slaBreachesAt"),
        sla_high_risk_at: text("slaHighRiskAt"),
        assignee: t["assignee"]["id"].as_str().map(str::to_string),
        state: t["state"]["name"].as_str().unwrap_or_default().to_string(),
        priority: t["priority"].as_u64().map_or(0, |p| p.min(4) as u8),
        created_at: text("createdAt").unwrap_or_default(),
    })
}

pub fn read_viewer(reply: &Value) -> Result<String, LinearError> {
    data(reply)?["viewer"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| unexpected("viewer"))
}

pub fn read_states(reply: &Value, team: &str) -> Result<Vec<State>, LinearError> {
    let teams = data(reply)?["teams"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("teams"))?;
    let Some(t) = teams.first() else {
        return Err(LinearError::Failed(format!("Linear has no team {team}")));
    };
    Ok(t["states"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("states"))?
        .iter()
        .filter_map(|s| {
            Some(State {
                id: s["id"].as_str()?.to_string(),
                name: s["name"].as_str()?.to_string(),
            })
        })
        .collect())
}

/// The tickets in the reply; whether one without an SLA counts is the pipeline's choice
/// (`linear_watch::pick`).
pub fn read_pickable(reply: &Value) -> Result<Vec<Ticket>, LinearError> {
    Ok(data(reply)?["issues"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("issues"))?
        .iter()
        .filter_map(read_one_ticket)
        .collect())
}

pub fn read_ticket(reply: &Value) -> Result<Option<Ticket>, LinearError> {
    Ok(read_one_ticket(&data(reply)?["issue"]))
}

/// Whether a write's reply says it worked (`success: true` under `field`).
pub fn read_success(reply: &Value, field: &str) -> Result<(), LinearError> {
    match data(reply)?[field]["success"].as_bool() {
        Some(true) => Ok(()),
        _ => Err(LinearError::Failed(format!(
            "Linear didn't take the {field}"
        ))),
    }
}

/// How long one call may take.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Linear over HTTPS with a personal API key.
pub struct Http {
    pub key: String,
}

impl Http {
    /// Sends `body`; the reply as JSON, or why there is none. Linear answers errors with a
    /// JSON body too, so a 4xx is read for them.
    fn post(&self, body: &Value) -> Result<Value, LinearError> {
        if self.key.trim().is_empty() {
            return Err(LinearError::Rejected("no Linear API key".into()));
        }
        let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
        let resp = match agent
            .post(URL)
            .set("Authorization", self.key.trim())
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
        {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let reply: Value = r
                    .into_string()
                    .ok()
                    .and_then(|b| serde_json::from_str(&b).ok())
                    .unwrap_or(Value::Null);
                return Err(reply_error(&reply).unwrap_or_else(|| match code {
                    401 | 403 => {
                        LinearError::Rejected(format!("Linear rejected the API key (HTTP {code})"))
                    }
                    _ => LinearError::Failed(format!("Linear answered HTTP {code}")),
                }));
            }
            Err(e) => return Err(LinearError::Failed(format!("Linear isn't reachable: {e}"))),
        };
        let text = resp
            .into_string()
            .map_err(|e| LinearError::Failed(format!("Linear's reply: {e}")))?;
        serde_json::from_str(&text)
            .map_err(|_| LinearError::Failed("Linear's reply isn't JSON".into()))
    }
}

impl LinearApi for Http {
    fn viewer(&self) -> Result<String, LinearError> {
        read_viewer(&self.post(&viewer_request())?)
    }
    fn states(&self, team: &str) -> Result<Vec<State>, LinearError> {
        read_states(&self.post(&states_request(team))?, team)
    }
    fn pickable(
        &self,
        team: &str,
        state_id: &str,
        unassigned_only: bool,
    ) -> Result<Vec<Ticket>, LinearError> {
        read_pickable(&self.post(&pickable_request(team, state_id, unassigned_only))?)
    }
    fn ticket(&self, identifier: &str) -> Result<Option<Ticket>, LinearError> {
        read_ticket(&self.post(&ticket_request(identifier))?)
    }
    fn update(&self, id: &str, state_id: &str, assignee: Option<&str>) -> Result<(), LinearError> {
        read_success(
            &self.post(&update_request(id, state_id, assignee))?,
            "issueUpdate",
        )
    }
    fn comment(&self, id: &str, body: &str) -> Result<(), LinearError> {
        read_success(&self.post(&comment_request(id, body))?, "commentCreate")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket_json(identifier: &str, sla: Option<&str>, assignee: Option<&str>) -> Value {
        json!({
            "id": format!("uuid-{identifier}"),
            "identifier": identifier,
            "title": "Dutch translation switches languages",
            "url": format!("https://linear.app/iov42/issue/{identifier}"),
            "slaBreachesAt": sla,
            "slaHighRiskAt": sla.map(|_| "2026-11-03T14:04:05.535Z"),
            "assignee": assignee.map(|a| json!({"id": a})),
            "state": {"name": "Ready for Investigation"},
            "priority": 2,
            "createdAt": "2026-10-01T00:00:00Z",
        })
    }

    #[test]
    fn a_teams_states_are_read_and_a_missing_team_is_named() {
        let reply = json!({"data": {"teams": {"nodes": [{"states": {"nodes": [
            {"id": "s1", "name": "Ready for Investigation"},
            {"id": "s2", "name": "In Investigation"},
        ]}}]}}});
        assert_eq!(
            read_states(&reply, "INTH").unwrap(),
            [
                State {
                    id: "s1".into(),
                    name: "Ready for Investigation".into()
                },
                State {
                    id: "s2".into(),
                    name: "In Investigation".into()
                },
            ]
        );
        let none = json!({"data": {"teams": {"nodes": []}}});
        assert_eq!(
            read_states(&none, "INTH").unwrap_err(),
            LinearError::Failed("Linear has no team INTH".into())
        );
        assert!(states_request("INTH")["variables"]["team"] == "INTH");
    }

    #[test]
    fn every_ticket_is_read_with_its_priority_and_age() {
        let reply = json!({"data": {"issues": {"nodes": [
            ticket_json("INTH-170", Some("2026-11-04T14:04:05.535Z"), None),
            ticket_json("INTH-171", None, None),
            ticket_json("INTH-172", Some("2026-11-01T09:00:00.000Z"), Some("u-9")),
        ]}}});
        let got = read_pickable(&reply).unwrap();
        let ids: Vec<&str> = got.iter().map(|t| t.identifier.as_str()).collect();
        assert_eq!(ids, ["INTH-170", "INTH-171", "INTH-172"]);
        assert_eq!(got[2].assignee.as_deref(), Some("u-9"));
        assert_eq!(
            (got[1].priority, got[1].created_at.as_str()),
            (2, "2026-10-01T00:00:00Z")
        );
        assert_eq!(
            got[0].sla_breaches_at.as_deref(),
            Some("2026-11-04T14:04:05.535Z")
        );
        assert_eq!(got[0].url, "https://linear.app/iov42/issue/INTH-170");
        assert_eq!(got[0].state, "Ready for Investigation");
    }

    #[test]
    fn unassigned_only_is_asked_of_linear_only_when_wanted() {
        let only = pickable_request("INTH", "s1", true);
        let any = pickable_request("INTH", "s1", false);
        let filter = |r: &Value| r["variables"]["filter"].clone();
        assert_eq!(filter(&only)["team"]["key"]["eq"], "INTH");
        assert_eq!(filter(&only)["state"]["id"]["eq"], "s1");
        assert_eq!(filter(&only)["assignee"]["null"], true);
        assert!(filter(&any).get("assignee").is_none());
        assert!(only["query"].as_str().unwrap().contains("slaBreachesAt"));
    }

    #[test]
    fn a_claim_assigns_only_when_told_to() {
        let mine = update_request("uuid-1", "s2", Some("u-me"));
        assert_eq!(mine["variables"]["id"], "uuid-1");
        assert_eq!(mine["variables"]["input"]["stateId"], "s2");
        assert_eq!(mine["variables"]["input"]["assigneeId"], "u-me");
        let keep = update_request("uuid-1", "s2", None);
        assert!(keep["variables"]["input"].get("assigneeId").is_none());
        assert!(
            mine["query"]
                .as_str()
                .unwrap()
                .contains("issueUpdate(id: $id, input: $input)")
        );
    }

    #[test]
    fn a_rejected_key_is_told_apart_from_other_errors() {
        let auth = json!({"errors": [{"message": "Authentication required, not authenticated",
            "extensions": {"code": "AUTHENTICATION_ERROR"}}]});
        assert!(matches!(reply_error(&auth), Some(LinearError::Rejected(_))));
        let other = json!({"errors": [{"message": "Entity not found: Issue"}], "data": null});
        assert_eq!(
            reply_error(&other),
            Some(LinearError::Failed(
                "Linear: Entity not found: Issue".into()
            ))
        );
        assert_eq!(reply_error(&json!({"data": {"viewer": {"id": "u"}}})), None);
        // An error reply reads as that error, whatever is asked of it.
        assert!(matches!(read_viewer(&auth), Err(LinearError::Rejected(_))));
        assert!(matches!(read_pickable(&other), Err(LinearError::Failed(_))));
    }

    #[test]
    fn the_viewer_one_ticket_and_writes_are_read() {
        assert_eq!(
            read_viewer(&json!({"data": {"viewer": {"id": "u-me"}}})).unwrap(),
            "u-me"
        );
        let one = json!({"data": {"issue": ticket_json("INTH-170", None, None)}});
        assert_eq!(read_ticket(&one).unwrap().unwrap().identifier, "INTH-170");
        assert_eq!(
            read_ticket(&json!({"data": {"issue": null}})).unwrap(),
            None
        );
        let ok = json!({"data": {"commentCreate": {"success": true}}});
        assert_eq!(read_success(&ok, "commentCreate"), Ok(()));
        let no = json!({"data": {"issueUpdate": {"success": false}}});
        assert!(read_success(&no, "issueUpdate").is_err());
        assert_eq!(
            comment_request("uuid-1", "hi")["variables"]["input"]["body"],
            "hi"
        );
    }
}
