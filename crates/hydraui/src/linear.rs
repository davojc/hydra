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
    /// Its labels' names.
    pub labels: Vec<String>,
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
     state { name } labels { nodes { name } }";

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
pub(crate) fn data(reply: &Value) -> Result<&Value, LinearError> {
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
        labels: t["labels"]["nodes"]
            .as_array()
            .map(|ls| {
                ls.iter()
                    .filter_map(|l| l["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
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

/// The teams the key sees, for the Tickets page's first choice (core spec §53).
pub fn teams_request() -> Value {
    json!({"query": "query { teams(first: 100) { nodes { key name } } }"})
}

/// Each team as (key, name), by key.
pub fn read_teams(reply: &Value) -> Result<Vec<(String, String)>, LinearError> {
    let mut teams: Vec<(String, String)> = data(reply)?["teams"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("teams"))?
        .iter()
        .filter_map(|t| {
            Some((
                t["key"].as_str()?.to_string(),
                t["name"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect();
    teams.sort();
    Ok(teams)
}

/// The workflow states of the teams keyed `teams`.
pub fn team_states_request(teams: &[String]) -> Value {
    json!({
        "query": "query($keys: [String!]) { teams(first: 20, filter: { key: { in: $keys } }) \
                  { nodes { key states(first: 50) { nodes { name type position color } } } } }",
        "variables": {"keys": teams},
    })
}

/// A workflow state as the Tickets page draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateInfo {
    pub name: String,
    /// Linear's type: `triage`, `backlog`, `unstarted`, `started`, `completed`, `canceled`.
    pub kind: String,
    /// Its colour, `#rrggbb`.
    pub color: String,
}

/// Their states, each name once (its first team's type and colour), in Linear's order:
/// triage, backlog, unstarted, started, completed, canceled, then by position.
pub fn read_team_states(reply: &Value) -> Result<Vec<StateInfo>, LinearError> {
    let teams = data(reply)?["teams"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("teams"))?;
    let rank = |t: &str| match t {
        "triage" => 0,
        "backlog" => 1,
        "unstarted" => 2,
        "started" => 3,
        "completed" => 4,
        "canceled" => 5,
        _ => 6,
    };
    let mut states: Vec<(usize, f64, StateInfo)> = teams
        .iter()
        .flat_map(|t| t["states"]["nodes"].as_array().cloned().unwrap_or_default())
        .filter_map(|n| {
            let kind = n["type"].as_str().unwrap_or("").to_string();
            Some((
                rank(&kind),
                n["position"].as_f64().unwrap_or(0.0),
                StateInfo {
                    name: n["name"].as_str()?.to_string(),
                    kind,
                    color: n["color"].as_str().unwrap_or("").to_string(),
                },
            ))
        })
        .collect();
    states.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut out: Vec<StateInfo> = Vec::new();
    for (_, _, s) in states {
        if !out.iter().any(|o| o.name == s.name) {
            out.push(s);
        }
    }
    Ok(out)
}

/// The states' names, in the same order.
pub fn read_team_state_names(reply: &Value) -> Result<Vec<String>, LinearError> {
    Ok(read_team_states(reply)?
        .into_iter()
        .map(|s| s.name)
        .collect())
}

/// The labels the teams keyed `teams` can use: their own and the workspace's.
pub fn team_labels_request(teams: &[String]) -> Value {
    json!({
        "query": "query($keys: [String!]) { issueLabels(first: 250, filter: { or: [ \
                  { team: { key: { in: $keys } } }, { team: { null: true } } ] }) \
                  { nodes { name isGroup } } }",
        "variables": {"keys": teams},
    })
}

/// Their names, each once, by name ignoring case; a group's own name is left out (its
/// labels are what tickets carry).
pub fn read_label_names(reply: &Value) -> Result<Vec<String>, LinearError> {
    let mut names: Vec<String> = data(reply)?["issueLabels"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("issueLabels"))?
        .iter()
        .filter(|l| l["isGroup"].as_bool() != Some(true))
        .filter_map(|l| l["name"].as_str().map(str::to_string))
        .collect();
    names.sort_by_key(|n| (n.to_lowercase(), n.clone()));
    names.dedup();
    Ok(names)
}

/// Whose tickets the Tickets page lists (core spec §53).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Assignee {
    /// Assigned or not.
    #[default]
    Anyone,
    Unassigned,
    /// Assigned to one of these users (by id); to anyone when empty.
    Assigned(Vec<String>),
}

/// The members of the teams keyed `teams`, for the Assigned filter.
pub fn team_members_request(teams: &[String]) -> Value {
    json!({
        "query": "query($keys: [String!]) { teams(first: 20, filter: { key: { in: $keys } }) \
                  { nodes { members(first: 100) { nodes { id name displayName active } } } } }",
        "variables": {"keys": teams},
    })
}

/// Each active member once, as (id, name), by name.
pub fn read_team_members(reply: &Value) -> Result<Vec<(String, String)>, LinearError> {
    let teams = data(reply)?["teams"]["nodes"]
        .as_array()
        .ok_or_else(|| unexpected("teams"))?;
    let mut people: Vec<(String, String)> = Vec::new();
    for m in teams.iter().flat_map(|t| {
        t["members"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }) {
        if m["active"].as_bool() == Some(false) {
            continue;
        }
        let (Some(id), Some(name)) = (
            m["id"].as_str(),
            m["name"].as_str().or_else(|| m["displayName"].as_str()),
        ) else {
            continue;
        };
        if !people.iter().any(|(i, _)| i == id) {
            people.push((id.to_string(), name.to_string()));
        }
    }
    people.sort_by_key(|p| p.1.to_lowercase());
    Ok(people)
}

/// What the Tickets page reads of each ticket: `TICKET_FIELDS` with its labels capped, so a
/// page of them stays under Linear's complexity limit (10,000 points a query; a nested list
/// multiplies by its `first`, 50 when left out).
const LISTED_TICKET_FIELDS: &str = "id identifier title url slaBreachesAt slaHighRiskAt priority createdAt assignee { id } \
     state { name } labels(first: 10) { nodes { name } }";

/// How many tickets the Tickets page lists at most.
pub const LISTED_TICKETS: usize = 100;

/// The tickets of teams `teams` in any of the states named `states`, assigned as `who` says,
/// most recently updated first, up to `LISTED_TICKETS`.
pub fn tickets_in_states_request(teams: &[String], states: &[String], who: &Assignee) -> Value {
    let mut filter = json!({
        "team": {"key": {"in": teams}},
        "state": {"name": {"in": states}},
    });
    match who {
        Assignee::Anyone => {}
        Assignee::Unassigned => filter["assignee"] = json!({"null": true}),
        Assignee::Assigned(ids) if ids.is_empty() => filter["assignee"] = json!({"null": false}),
        Assignee::Assigned(ids) => filter["assignee"] = json!({"id": {"in": ids}}),
    }
    json!({
        "query": format!(
            "query($filter: IssueFilter!) {{ issues(filter: $filter, first: {LISTED_TICKETS}, \
             orderBy: updatedAt) {{ nodes {{ {LISTED_TICKET_FIELDS} }} }} }}"
        ),
        "variables": {"filter": filter},
    })
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
    pub(crate) fn post(&self, body: &Value) -> Result<Value, LinearError> {
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
    #[test]
    fn labels_come_once_each_by_name_without_their_groups() {
        let reply = json!({"data": {"issueLabels": {"nodes": [
            {"name": "hydra-claimed", "isGroup": false},
            {"name": "Bug", "isGroup": false},
            {"name": "Type", "isGroup": true},
            {"name": "bug", "isGroup": false},
            {"name": "Bug", "isGroup": false},
            {"name": "Customer"}
        ]}}});
        assert_eq!(
            read_label_names(&reply).unwrap(),
            vec!["Bug", "bug", "Customer", "hydra-claimed"]
        );
        let req = team_labels_request(&["PRE".into()]);
        assert_eq!(req["variables"]["keys"], json!(["PRE"]));
    }

    #[test]
    fn teams_come_by_key() {
        let reply = json!({"data": {"teams": {"nodes": [
            {"key": "PRE", "name": "Product & Engineering"},
            {"key": "INTH", "name": "Interu Help"}
        ]}}});
        assert_eq!(
            read_teams(&reply).unwrap(),
            vec![
                ("INTH".to_string(), "Interu Help".to_string()),
                ("PRE".to_string(), "Product & Engineering".to_string())
            ]
        );
    }

    #[test]
    fn the_chosen_teams_states_come_once_each_in_workflow_order() {
        let reply = json!({"data": {"teams": {"nodes": [
            {"key": "PRE", "states": {"nodes": [
                {"name": "Done", "type": "completed", "position": 3.0, "color": "#5e6ad2"},
                {"name": "In Progress", "type": "started", "position": 2.0},
                {"name": "Todo", "type": "unstarted", "position": 1.0}
            ]}},
            {"key": "INTH", "states": {"nodes": [
                {"name": "In Review", "type": "started", "position": 2.5},
                {"name": "Todo", "type": "unstarted", "position": 1.0},
                {"name": "Backlog", "type": "backlog", "position": 0.0}
            ]}}
        ]}}});
        assert_eq!(
            read_team_state_names(&reply).unwrap(),
            vec!["Backlog", "Todo", "In Progress", "In Review", "Done"]
        );
        let done = read_team_states(&reply).unwrap().pop().unwrap();
        assert_eq!(
            done,
            StateInfo {
                name: "Done".into(),
                kind: "completed".into(),
                color: "#5e6ad2".into()
            }
        );
        let req = team_states_request(&["PRE".into(), "INTH".into()]);
        assert_eq!(req["variables"]["keys"], json!(["PRE", "INTH"]));
        let bad = json!({"errors": [{"message": "nope"}]});
        assert!(read_team_state_names(&bad).is_err());
    }

    #[test]
    fn tickets_are_asked_for_by_team_and_state_newest_first() {
        let req = tickets_in_states_request(
            &["PRE".into()],
            &["Todo".into(), "In Review".into()],
            &Assignee::Anyone,
        );
        assert_eq!(
            req["variables"]["filter"],
            json!({"team": {"key": {"in": ["PRE"]}},
                   "state": {"name": {"in": ["Todo", "In Review"]}}})
        );
        let q = req["query"].as_str().unwrap();
        assert!(
            q.contains("orderBy: updatedAt") && q.contains("identifier"),
            "{q}"
        );
    }

    /// Linear's estimate of a query's cost, roughly: every `{` opens an object worth 1, every
    /// field 0.1, and a list multiplies what is inside by its `first` (50 when left out).
    fn complexity(query: &str) -> f64 {
        fn block(chars: &mut std::iter::Peekable<std::str::Chars>) -> f64 {
            let mut total = 0.0;
            let mut word = String::new();
            let mut first: Option<f64> = None;
            let mut is_list = false;
            while let Some(c) = chars.next() {
                match c {
                    '(' => {
                        let mut args = String::new();
                        for a in chars.by_ref() {
                            if a == ')' {
                                break;
                            }
                            args.push(a);
                        }
                        if let Some(n) = args
                            .split("first:")
                            .nth(1)
                            .and_then(|r| r.trim().split(|c: char| !c.is_ascii_digit()).next())
                            .and_then(|n| n.parse().ok())
                        {
                            first = Some(n);
                        }
                        is_list = true;
                    }
                    '{' => {
                        let inner = 1.0 + block(chars);
                        let plural = word.ends_with('s') || word == "nodes";
                        total += if word == "nodes" {
                            inner
                        } else if is_list || plural {
                            inner * first.unwrap_or(50.0)
                        } else {
                            inner
                        };
                        word.clear();
                        first = None;
                        is_list = false;
                    }
                    '}' => {
                        if !word.is_empty() {
                            total += 0.1;
                        }
                        return total;
                    }
                    c if c.is_whitespace() => {
                        if !word.is_empty() && chars.peek().is_some_and(|n| *n != '{' && *n != '(')
                        {
                            total += 0.1;
                            word.clear();
                        }
                    }
                    c => word.push(c),
                }
            }
            total
        }
        let q = query.split_once('{').map_or(query, |(_, r)| r);
        block(&mut q.chars().peekable())
    }

    #[test]
    fn the_tickets_pages_queries_stay_under_linears_complexity_limit() {
        let teams = vec!["PRE".to_string(), "INTH".to_string()];
        let states = vec!["Todo".to_string()];
        for req in [
            teams_request(),
            team_states_request(&teams),
            team_members_request(&teams),
            team_labels_request(&teams),
            tickets_in_states_request(&teams, &states, &Assignee::Anyone),
        ] {
            let q = req["query"].as_str().unwrap();
            let c = complexity(q);
            assert!(c < 5_000.0, "{c} points: {q}");
        }
        // The estimate does catch a query that is too big: what was sent before.
        let old = "query { teams(filter: { key: { in: $keys } }) { nodes { members(first: 250) \
                   { nodes { id name displayName active } } } } }";
        assert!(complexity(old) > 10_000.0, "{}", complexity(old));
    }

    #[test]
    fn tickets_can_be_unassigned_assigned_or_assigned_to_someone() {
        let who = |w: &Assignee| {
            tickets_in_states_request(&["PRE".into()], &["Todo".into()], w)["variables"]["filter"]
                ["assignee"]
                .clone()
        };
        assert_eq!(who(&Assignee::Anyone), Value::Null);
        assert_eq!(who(&Assignee::Unassigned), json!({"null": true}));
        assert_eq!(who(&Assignee::Assigned(vec![])), json!({"null": false}));
        assert_eq!(
            who(&Assignee::Assigned(vec!["u1".into(), "u2".into()])),
            json!({"id": {"in": ["u1", "u2"]}})
        );
    }

    #[test]
    fn team_members_come_once_each_by_name_leaving_out_deactivated_ones() {
        let reply = json!({"data": {"teams": {"nodes": [
            {"members": {"nodes": [
                {"id": "u2", "name": "Jordan Miller", "active": true},
                {"id": "u1", "name": "aadam Z", "active": true}
            ]}},
            {"members": {"nodes": [
                {"id": "u2", "name": "Jordan Miller", "active": true},
                {"id": "u3", "name": "Gone Person", "active": false},
                {"id": "u4", "displayName": "kris", "active": true}
            ]}}
        ]}}});
        assert_eq!(
            read_team_members(&reply).unwrap(),
            vec![
                ("u1".to_string(), "aadam Z".to_string()),
                ("u2".to_string(), "Jordan Miller".to_string()),
                ("u4".to_string(), "kris".to_string()),
            ]
        );
        let req = team_members_request(&["PRE".into()]);
        assert_eq!(req["variables"]["keys"], json!(["PRE"]));
    }

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
            "labels": {"nodes": [{"name": "Bug"}, {"name": "Frontend"}]},
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
        assert_eq!(got[0].labels, ["Bug", "Frontend"]);
        assert!(
            pickable_request("INTH", "s1", true)["query"]
                .as_str()
                .unwrap()
                .contains("labels { nodes { name } }")
        );
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
