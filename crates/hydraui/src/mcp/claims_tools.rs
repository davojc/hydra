//! The `claims_*` tools (swarm claims spec §5.3): a session's claims, tied to its terminal. Asking
//! the swarms can take many seconds, so the server calls these on a thread of their own, and
//! nothing here holds its lock while it asks.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hydra_core::paths::HydraPaths;
use hydra_swarms::claims::{Holder, HolderKind};
use hydra_swarms::service::NO_LONGER_YOURS;
use hydra_swarms::{ClaimsClient, ContextView, LostReason, TakeReply};
use serde_json::{Value, json};

use super::registry::Terminal;
use super::server::{Server, lock};
use super::tools::Outcome;

/// Where the window running the swarms says how to reach them, under `running_dir`.
pub const CLAIMS_FILE: &str = "swarms-claims.json";

/// The bridge file, for the service that writes it and the client that reads it.
pub fn bridge_file(paths: &HydraPaths) -> PathBuf {
    crate::claim::running_dir(paths).join(CLAIMS_FILE)
}

/// Gives `server` this process's way to the swarms, once. Blocks on nothing: the client connects
/// on its own threads, whichever window runs the swarms and whenever it starts.
pub fn connect(server: &Arc<Server>, paths: &HydraPaths) {
    if server.claims().is_some() {
        return;
    }
    let losing = Arc::clone(server);
    let on_lost = Box::new(move |holder_id, reason: LostReason| {
        losing.claim_lost(holder_id, &reason);
    });
    let client = ClaimsClient::start(bridge_file(paths), on_lost);
    server.set_claims(Arc::new(ClaimsSessions::new(Arc::new(client))));
}

/// What the tools need of the swarms.
pub trait ClaimsApi: Send + Sync {
    fn take(
        &self,
        swarm: Option<&str>,
        context: &str,
        key: &str,
        holder: Holder,
    ) -> Result<TakeReply, String>;
    fn release(&self, holder_id: u64) -> Result<(), String>;
    fn done(&self, holder_id: u64) -> Result<(), String>;
    /// Never blocks.
    fn end(&self, holder_id: u64);
    fn list(&self, swarm: Option<&str>, context: &str) -> Result<ContextView, String>;
}

impl ClaimsApi for ClaimsClient {
    fn take(
        &self,
        swarm: Option<&str>,
        context: &str,
        key: &str,
        holder: Holder,
    ) -> Result<TakeReply, String> {
        ClaimsClient::take(self, swarm, context, key, holder)
    }

    fn release(&self, holder_id: u64) -> Result<(), String> {
        ClaimsClient::release(self, holder_id)
    }

    fn done(&self, holder_id: u64) -> Result<(), String> {
        ClaimsClient::done(self, holder_id)
    }

    fn end(&self, holder_id: u64) {
        ClaimsClient::end(self, holder_id);
    }

    fn list(&self, swarm: Option<&str>, context: &str) -> Result<ContextView, String> {
        ClaimsClient::list(self, swarm, context)
    }
}

/// A claim a terminal's session holds.
struct SessionClaim {
    holder_id: u64,
    /// The swarm's id, as the swarms answered.
    swarm: String,
    /// The swarm as the session named it, which can be its name rather than its id.
    asked_swarm: Option<String>,
    context: String,
    key: String,
}

#[derive(Default)]
struct Sessions {
    /// By terminal token.
    held: HashMap<String, Vec<SessionClaim>>,
    /// Tokens of terminals that have closed. A call can be on its way when its terminal closes,
    /// and a claim it gets then has nothing left to end it. Tokens are never used again, and a
    /// process closes few enough terminals for this not to matter.
    closed: HashSet<String>,
    /// Takes waiting for an answer.
    taking: usize,
    /// Losses of holders no take has come back with yet: the client can tell of a loss just after
    /// it hands a take its answer, before that take has got here.
    lost_early: HashMap<u64, LostReason>,
}

/// Which claims each terminal's session holds.
pub struct ClaimsSessions {
    api: Arc<dyn ClaimsApi>,
    sessions: Mutex<Sessions>,
}

impl ClaimsSessions {
    pub fn new(api: Arc<dyn ClaimsApi>) -> Self {
        ClaimsSessions {
            api,
            sessions: Mutex::default(),
        }
    }

    /// Runs a tool for the terminal holding `token`; `None` for a name that isn't one of these.
    /// Blocks while the swarms answer.
    pub fn call(
        &self,
        token: &str,
        terminal: &Terminal,
        name: &str,
        args: &Value,
    ) -> Option<Outcome> {
        let answered = match name {
            "claims_take" => self.take(token, terminal, args),
            "claims_release" => self.finish(token, args, false),
            "claims_done" => self.finish(token, args, true),
            "claims_list" => self.list(args),
            _ => return None,
        };
        Some(answered.unwrap_or_else(|refused| refused))
    }

    /// Ends the terminal's claims: renewals stop and they lapse. Never blocks.
    pub fn terminal_closed(&self, token: &str) {
        let claims = {
            let mut sessions = lock(&self.sessions);
            sessions.closed.insert(token.to_string());
            sessions.held.remove(token)
        };
        for claim in claims.into_iter().flatten() {
            self.api.end(claim.holder_id);
        }
    }

    /// Forgets the holder and gives (token, text to type) for a loss.
    pub fn lost(&self, holder_id: u64, reason: &LostReason) -> Option<(String, String)> {
        let mut sessions = lock(&self.sessions);
        for (token, claims) in &mut sessions.held {
            if let Some(i) = claims.iter().position(|c| c.holder_id == holder_id) {
                let claim = claims.remove(i);
                return Some((token.clone(), lost_text(&claim.key, &claim.context, reason)));
            }
        }
        if sessions.taking > 0 {
            sessions.lost_early.insert(holder_id, reason.clone());
        }
        None
    }

    fn take(&self, token: &str, terminal: &Terminal, args: &Value) -> Result<Outcome, Outcome> {
        let context = required(args, "context")?;
        let key = required(args, "key")?;
        let swarm = optional(args, "swarm");
        {
            let mut sessions = lock(&self.sessions);
            // Asked again, the swarms would say this machine holds it, in another terminal, and
            // a session that lost track (a resume, a compacted context) would drop its own work.
            let own = sessions.held.get(token).is_some_and(|claims| {
                claims.iter().any(|c| {
                    c.context == context
                        && c.key == key
                        && swarm.is_none_or(|s| s == c.swarm || c.asked_swarm.as_deref() == Some(s))
                })
            });
            if own {
                return Ok(Outcome::ok(you_hold(key, context)));
            }
            sessions.taking += 1;
        }
        let holder = Holder {
            kind: HolderKind::Session,
            label: terminal.name.clone(),
        };
        let reply = self.api.take(swarm, context, key, holder);

        let mut sessions = lock(&self.sessions);
        sessions.taking -= 1;
        let mut lost_early = None;
        let mut closed = None;
        if let Ok(TakeReply::Yours {
            holder_id,
            swarm: swarm_id,
            ..
        }) = &reply
        {
            lost_early = sessions.lost_early.remove(holder_id);
            if sessions.closed.contains(token) {
                closed = Some(*holder_id);
            } else if lost_early.is_none() {
                sessions
                    .held
                    .entry(token.to_string())
                    .or_default()
                    .push(SessionClaim {
                        holder_id: *holder_id,
                        swarm: swarm_id.clone(),
                        asked_swarm: swarm.map(str::to_string),
                        context: context.to_string(),
                        key: key.to_string(),
                    });
            }
        }
        if sessions.taking == 0 {
            sessions.lost_early.clear();
        }
        drop(sessions);

        if let Some(holder_id) = closed {
            self.api.end(holder_id);
            return Err(Outcome::err("This terminal has closed."));
        }
        if let Some(reason) = lost_early {
            return Ok(Outcome::ok(lost_text(key, context, &reason)));
        }
        reply
            .map(|reply| Outcome::ok(take_text(key, context, &reply)))
            .map_err(Outcome::err)
    }

    fn finish(&self, token: &str, args: &Value, done: bool) -> Result<Outcome, Outcome> {
        let key = required(args, "key")?;
        let context = optional(args, "context");
        let holder_id = self.own_claim(token, key, context)?;
        let finished = if done {
            self.api.done(holder_id)
        } else {
            self.api.release(holder_id)
        };
        match finished {
            Ok(()) => {
                self.forget(token, holder_id);
                Ok(Outcome::ok(if done {
                    format!("{key} is done.")
                } else {
                    format!("Released {key}.")
                }))
            }
            Err(why) => {
                // Any other failure leaves the claim held, and the session can try again.
                if why == NO_LONGER_YOURS {
                    self.forget(token, holder_id);
                }
                Err(Outcome::err(why))
            }
        }
    }

    /// The holder of the one claim of this terminal's that matches.
    fn own_claim(&self, token: &str, key: &str, context: Option<&str>) -> Result<u64, Outcome> {
        let sessions = lock(&self.sessions);
        let matching: Vec<&SessionClaim> = sessions
            .held
            .get(token)
            .into_iter()
            .flatten()
            .filter(|c| c.key == key && context.is_none_or(|x| x == c.context))
            .collect();
        match matching.as_slice() {
            [] => Err(Outcome::err(match context {
                Some(context) => format!("You don't hold {key} in {context}."),
                None => format!("You don't hold {key}."),
            })),
            [claim] => Ok(claim.holder_id),
            several => {
                let mut contexts: Vec<&str> = several.iter().map(|c| c.context.as_str()).collect();
                contexts.sort_unstable();
                Err(Outcome::err(format!(
                    "Say which context: {}.",
                    contexts.join(", ")
                )))
            }
        }
    }

    fn forget(&self, token: &str, holder_id: u64) {
        if let Some(claims) = lock(&self.sessions).held.get_mut(token) {
            claims.retain(|c| c.holder_id != holder_id);
        }
    }

    fn list(&self, args: &Value) -> Result<Outcome, Outcome> {
        let context = required(args, "context")?;
        let view = self
            .api
            .list(optional(args, "swarm"), context)
            .map_err(Outcome::err)?;
        Ok(Outcome::ok(
            serde_json::to_string_pretty(&view).expect("json serialises"),
        ))
    }
}

/// A string argument that must be there and say something.
fn required<'a>(args: &'a Value, name: &str) -> Result<&'a str, Outcome> {
    optional(args, name).ok_or_else(|| {
        Outcome::err(format!(
            "the argument `{name}` is required and must be a string"
        ))
    })
}

/// A string argument, unless it is left out or blank.
fn optional<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn you_hold(key: &str, context: &str) -> String {
    format!(
        "You hold {key} in {context}. Work on it, then call claims_done. hydraui keeps the claim \
         alive while this terminal is open."
    )
}

/// What a take's answer tells the session. Only `Yours` means it may start, and the rest are
/// normal answers, not errors.
fn take_text(key: &str, context: &str, reply: &TakeReply) -> String {
    match reply {
        TakeReply::Yours { .. } => you_hold(key, context),
        TakeReply::Held {
            this_machine: true, ..
        } => format!("This machine already holds {key}, in another terminal. Don't work on it."),
        TakeReply::Held {
            person, machine, ..
        } => format!("{person} ({machine}) holds {key}. Don't work on it."),
        TakeReply::Done { person, .. } => {
            format!("{key} was finished by {person}. Don't work on it again.")
        }
        TakeReply::Paused => format!("{context} is paused."),
        TakeReply::AtLimit(limit) => {
            format!("This machine is at its limit of {limit} in {context}. Finish one first.")
        }
        // The swarms only answer this to a retake, which sessions never ask for.
        TakeReply::Gone(reason) => format!("{key} has gone: {}. Don't work on it.", why(reason)),
    }
}

/// The most of a person's, machine's or context's name that is typed into a session's prompt.
const NAME_MAX: usize = 80;
/// The most of a key that is typed into a session's prompt.
const KEY_MAX: usize = 200;

/// `text` as one short line. A loss is typed into a session's prompt and submitted, and these
/// names come from outside the terminal: a teammate chooses theirs, so a line break or an escape
/// in one must not reach the terminal, or end the prompt early.
fn one_line(text: &str, max: usize) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let cut: String = line.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

fn lost_text(key: &str, context: &str, reason: &LostReason) -> String {
    format!(
        "Your claim on {} in {} has gone: {}. Stop working on it.",
        one_line(key, KEY_MAX),
        one_line(context, NAME_MAX),
        why(reason)
    )
}

fn why(reason: &LostReason) -> String {
    let name = |text: &str| one_line(text, NAME_MAX);
    match reason {
        LostReason::Beaten { person, machine } => {
            format!("{} ({}) claimed it first", name(person), name(machine))
        }
        LostReason::Done { person } => format!("{} finished it", name(person)),
        LostReason::Released { person } => format!("{} released it", name(person)),
        LostReason::Lapsed => "it lapsed while this machine couldn't renew it".to_string(),
    }
}

/// The `tools/list` entries for the claims tools.
pub fn schemas() -> Vec<Value> {
    let string = |what: &str| json!({ "type": "string", "description": what });
    let context = "The kind of work the key belongs to, as your team names it.";
    let finish_args = json!({
        "type": "object",
        "properties": {
            "key": string("The key, as you gave it to claims_take."),
            "context": string("Its context (optional: needed only if you hold the same key in \
                more than one)."),
        },
        "required": ["key"],
    });
    vec![
        json!({
            "name": "claims_take",
            "description": "Claim a job shared with your teammates before you start it, so no \
                two sessions work one job. Only start if the answer says you hold it. Any other \
                answer means don't work on it, and isn't an error. hydraui keeps the claim alive \
                while this terminal is open, and tells you if it goes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "context": string(context),
                    "key": string("What identifies the job within the context."),
                    "swarm": string("The swarm the context is in (optional: needed only if you \
                        are in more than one)."),
                },
                "required": ["context", "key"],
            },
        }),
        json!({
            "name": "claims_release",
            "description": "Give up a claim you hold because you stopped without finishing, so \
                someone else can take the job.",
            "inputSchema": finish_args,
        }),
        json!({
            "name": "claims_done",
            "description": "Mark the job of a claim you hold as finished, so nobody works on it \
                again.",
            "inputSchema": finish_args,
        }),
        json!({
            "name": "claims_list",
            "description": "See a context: its settings, who holds which key and what was \
                finished recently.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "context": string(context),
                    "swarm": string("The swarm the context is in (optional: needed only if you \
                        are in more than one)."),
                },
                "required": ["context"],
            },
        }),
    ]
}

/// Stands in for the swarms in tests, here and in `server` and `mod`.
#[cfg(test)]
pub(super) mod testing {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::Duration;

    use hydra_swarms::claims::Holder;
    use hydra_swarms::{ContextView, TakeReply};

    use super::ClaimsApi;

    /// What a take was asked for: the swarm, the context, the key and the holder.
    pub type Asked = (Option<String>, String, String, Holder);

    #[derive(Default)]
    pub struct Fake {
        /// What each take answers, in turn; after them, a new holder each time.
        pub replies: Mutex<VecDeque<Result<TakeReply, String>>>,
        /// How long a take takes.
        pub slow: Option<Duration>,
        /// A take says it has started on the first and waits for the second.
        pub pause: Mutex<Option<(Sender<()>, Receiver<()>)>>,
        pub finish_fails: Mutex<Option<String>>,
        pub holders: Mutex<Vec<Asked>>,
        pub released: Mutex<Vec<u64>>,
        pub done: Mutex<Vec<u64>>,
        pub ended: Mutex<Vec<u64>>,
        pub view: Option<ContextView>,
    }

    impl Fake {
        fn finish(&self, into: &Mutex<Vec<u64>>, holder_id: u64) -> Result<(), String> {
            if let Some(why) = self.finish_fails.lock().unwrap().take() {
                return Err(why);
            }
            into.lock().unwrap().push(holder_id);
            Ok(())
        }
    }

    impl ClaimsApi for Fake {
        fn take(
            &self,
            swarm: Option<&str>,
            context: &str,
            key: &str,
            holder: Holder,
        ) -> Result<TakeReply, String> {
            if let Some(slow) = self.slow {
                std::thread::sleep(slow);
            }
            if let Some((started, go)) = self.pause.lock().unwrap().take() {
                started.send(()).unwrap();
                go.recv().unwrap();
            }
            let reply = self.replies.lock().unwrap().pop_front();
            let mut holders = self.holders.lock().unwrap();
            holders.push((
                swarm.map(str::to_string),
                context.to_string(),
                key.to_string(),
                holder,
            ));
            reply.unwrap_or(Ok(TakeReply::Yours {
                holder_id: holders.len() as u64,
                // Always the id, as the swarms answer, whatever name the session gave.
                swarm: "swarm-id".to_string(),
                lapse_micros: 180_000_000,
            }))
        }

        fn release(&self, holder_id: u64) -> Result<(), String> {
            self.finish(&self.released, holder_id)
        }

        fn done(&self, holder_id: u64) -> Result<(), String> {
            self.finish(&self.done, holder_id)
        }

        fn end(&self, holder_id: u64) {
            self.ended.lock().unwrap().push(holder_id);
        }

        fn list(&self, _swarm: Option<&str>, context: &str) -> Result<ContextView, String> {
            self.view
                .clone()
                .ok_or_else(|| format!("nothing in {context}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use hydra_swarms::claims::ContextSettings;

    use super::testing::Fake;
    use super::*;

    fn terminal() -> Terminal {
        Terminal {
            name: "web".to_string(),
            cwd: PathBuf::from("C:/w/web"),
            env: None,
        }
    }

    fn with(fake: Fake) -> (Arc<Fake>, ClaimsSessions) {
        let fake = Arc::new(fake);
        (fake.clone(), ClaimsSessions::new(fake))
    }

    fn call(sessions: &ClaimsSessions, token: &str, name: &str, args: Value) -> Outcome {
        sessions
            .call(token, &terminal(), name, &args)
            .expect("one of the claims tools")
    }

    fn take(sessions: &ClaimsSessions, token: &str, context: &str, key: &str) -> Outcome {
        let args = json!({"context": context, "key": key});
        call(sessions, token, "claims_take", args)
    }

    #[test]
    fn take_tells_a_session_it_holds_the_key() {
        let (fake, sessions) = with(Fake::default());
        let out = take(&sessions, "t", "mail", "m-1");
        assert_eq!(
            out.text,
            "You hold m-1 in mail. Work on it, then call claims_done. hydraui keeps the claim \
             alive while this terminal is open."
        );
        assert!(!out.is_error);
        {
            let holders = fake.holders.lock().unwrap();
            let (swarm, context, key, holder) = &holders[0];
            assert_eq!(swarm, &None);
            assert_eq!((context.as_str(), key.as_str()), ("mail", "m-1"));
            assert_eq!(holder.kind, HolderKind::Session);
            assert_eq!(holder.label, "web");
        }
        // Remembered: finishing it reaches the holder the take was given.
        assert!(!call(&sessions, "t", "claims_done", json!({"key": "m-1"})).is_error);
        assert_eq!(*fake.done.lock().unwrap(), vec![1]);
    }

    #[test]
    fn a_take_passes_on_the_swarm_it_names() {
        let (fake, sessions) = with(Fake::default());
        let args = json!({"context": "mail", "key": "m-1", "swarm": "team"});
        assert!(!call(&sessions, "t", "claims_take", args).is_error);
        assert_eq!(fake.holders.lock().unwrap()[0].0.as_deref(), Some("team"));
    }

    #[test]
    fn take_says_who_holds_it_without_an_error() {
        let answers = [
            (
                TakeReply::Held {
                    person: "Ana".into(),
                    machine: "desk".into(),
                    this_machine: false,
                    since: 1,
                },
                "Ana (desk) holds m-1. Don't work on it.",
            ),
            (
                TakeReply::Held {
                    person: "Ana".into(),
                    machine: "desk".into(),
                    this_machine: true,
                    since: 1,
                },
                "This machine already holds m-1, in another terminal. Don't work on it.",
            ),
            (
                TakeReply::Done {
                    person: "Ana".into(),
                    at: 1,
                },
                "m-1 was finished by Ana. Don't work on it again.",
            ),
            (TakeReply::Paused, "mail is paused."),
            (
                TakeReply::AtLimit(2),
                "This machine is at its limit of 2 in mail. Finish one first.",
            ),
            (
                TakeReply::Gone(LostReason::Done {
                    person: "Ana".into(),
                }),
                "m-1 has gone: Ana finished it. Don't work on it.",
            ),
        ];
        for (reply, text) in answers {
            let fake = Fake::default();
            fake.replies.lock().unwrap().push_back(Ok(reply));
            let (_, sessions) = with(fake);
            let out = take(&sessions, "t", "mail", "m-1");
            assert_eq!(out.text, text);
            assert!(!out.is_error, "{text}");
            // Nothing to finish: none of these is a claim.
            let release = call(&sessions, "t", "claims_release", json!({"key": "m-1"}));
            assert!(release.is_error, "{text}");
        }
    }

    #[test]
    fn a_swarm_that_cant_answer_is_an_error() {
        let fake = Fake::default();
        let why = "The swarms didn't answer in time.".to_string();
        fake.replies.lock().unwrap().push_back(Err(why.clone()));
        let (_, sessions) = with(fake);
        let out = take(&sessions, "t", "mail", "m-1");
        assert_eq!((out.text.as_str(), out.is_error), (why.as_str(), true));
    }

    #[test]
    fn asking_again_for_a_key_you_hold_says_you_hold_it() {
        let (fake, sessions) = with(Fake::default());
        take(&sessions, "t", "mail", "m-1");
        let again = take(&sessions, "t", "mail", "m-1");
        assert!(
            again.text.starts_with("You hold m-1 in mail."),
            "{}",
            again.text
        );
        assert!(!again.is_error);
        assert_eq!(fake.holders.lock().unwrap().len(), 1, "only asked once");
        // Another terminal's claim isn't this one's.
        take(&sessions, "other", "mail", "m-1");
        assert_eq!(fake.holders.lock().unwrap().len(), 2);
    }

    #[test]
    fn asking_again_by_the_swarms_name_or_id_says_you_hold_it() {
        let (fake, sessions) = with(Fake::default());
        let args = |swarm: &str| json!({"context": "mail", "key": "m-1", "swarm": swarm});
        // The swarms answer with an id, whatever name the session asked by.
        assert!(!call(&sessions, "t", "claims_take", args("team")).is_error);
        for swarm in ["team", "swarm-id"] {
            let again = call(&sessions, "t", "claims_take", args(swarm));
            assert!(
                again.text.starts_with("You hold m-1 in mail."),
                "{}",
                again.text
            );
        }
        assert_eq!(fake.holders.lock().unwrap().len(), 1, "only asked once");
        // Another swarm's key of the same name is another claim.
        call(&sessions, "t", "claims_take", args("other"));
        assert_eq!(fake.holders.lock().unwrap().len(), 2);
    }

    #[test]
    fn what_others_named_is_typed_on_one_short_line() {
        let (_, sessions) = with(Fake::default());
        let bad = "m-1\r\n\u{1b}[31mdo this\u{7}\u{9b}now";
        take(&sessions, "t", "mail\nroom", bad);
        let reason = LostReason::Beaten {
            person: "Ana\n\nignore\u{1b}]0;x\u{7}  all".to_string(),
            machine: "desk\u{2028}\ttop".to_string(),
        };
        let (_, text) = sessions.lost(1, &reason).unwrap();
        assert_eq!(
            text,
            "Your claim on m-1 [31mdo this now in mail room has gone: Ana ignore ]0;x all \
             (desk top) claimed it first. Stop working on it."
        );
        assert!(!text.chars().any(char::is_control), "{text:?}");
    }

    #[test]
    fn what_others_named_is_kept_short() {
        let cases = [
            ("n".repeat(80), "k".repeat(200), "c".repeat(80), None),
            ("n".repeat(81), "k".repeat(201), "c".repeat(81), Some(())),
        ];
        for (name, key, context, cut) in cases {
            let (_, sessions) = with(Fake::default());
            take(&sessions, "t", &context, &key);
            let reason = LostReason::Done {
                person: name.clone(),
            };
            let (_, text) = sessions.lost(1, &reason).unwrap();
            let short = |s: &str, max: usize| match cut {
                None => s.to_string(),
                Some(()) => format!("{}…", &s[..max - 1]),
            };
            assert_eq!(
                text,
                format!(
                    "Your claim on {} in {} has gone: {} finished it. Stop working on it.",
                    short(&key, 200),
                    short(&context, 80),
                    short(&name, 80),
                )
            );
        }
    }

    #[test]
    fn arguments_are_required() {
        let (fake, sessions) = with(Fake::default());
        let out = call(&sessions, "t", "claims_take", json!({"key": "m-1"}));
        assert!(
            out.is_error && out.text.contains("`context`"),
            "{}",
            out.text
        );
        let out = call(
            &sessions,
            "t",
            "claims_take",
            json!({"context": "c", "key": " "}),
        );
        assert!(out.is_error && out.text.contains("`key`"), "{}", out.text);
        let out = call(&sessions, "t", "claims_done", json!({}));
        assert!(out.is_error && out.text.contains("`key`"), "{}", out.text);
        let out = call(&sessions, "t", "claims_list", json!({}));
        assert!(
            out.is_error && out.text.contains("`context`"),
            "{}",
            out.text
        );
        assert!(fake.holders.lock().unwrap().is_empty());
    }

    #[test]
    fn an_unknown_name_is_not_a_claims_tool() {
        let (_, sessions) = with(Fake::default());
        assert!(
            sessions
                .call("t", &terminal(), "claims_steal", &json!({}))
                .is_none()
        );
    }

    #[test]
    fn done_and_release_find_the_callers_own_claim() {
        let (fake, sessions) = with(Fake::default());
        take(&sessions, "mine", "mail", "m-1");
        take(&sessions, "mine", "mail", "m-2");
        let theirs = call(&sessions, "theirs", "claims_done", json!({"key": "m-1"}));
        assert!(theirs.is_error);
        assert_eq!(theirs.text, "You don't hold m-1.");
        assert!(fake.done.lock().unwrap().is_empty());

        let out = call(&sessions, "mine", "claims_release", json!({"key": "m-1"}));
        assert_eq!((out.text.as_str(), out.is_error), ("Released m-1.", false));
        assert_eq!(*fake.released.lock().unwrap(), vec![1]);

        let out = call(&sessions, "mine", "claims_done", json!({"key": "m-2"}));
        assert_eq!((out.text.as_str(), out.is_error), ("m-2 is done.", false));
        assert_eq!(*fake.done.lock().unwrap(), vec![2]);

        // Each is finished, so neither is held any more.
        let out = call(&sessions, "mine", "claims_done", json!({"key": "m-1"}));
        assert!(out.is_error);
        let args = json!({"key": "m-2", "context": "mail"});
        let out = call(&sessions, "mine", "claims_release", args);
        assert_eq!(
            (out.text.as_str(), out.is_error),
            ("You don't hold m-2 in mail.", true)
        );
    }

    #[test]
    fn the_same_key_in_two_contexts_asks_which() {
        let (fake, sessions) = with(Fake::default());
        take(&sessions, "t", "b", "k");
        take(&sessions, "t", "a", "k");
        for tool in ["claims_done", "claims_release"] {
            let out = call(&sessions, "t", tool, json!({"key": "k"}));
            assert_eq!(
                (out.text.as_str(), out.is_error),
                ("Say which context: a, b.", true)
            );
        }
        assert!(fake.done.lock().unwrap().is_empty());
        let out = call(
            &sessions,
            "t",
            "claims_done",
            json!({"key": "k", "context": "a"}),
        );
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(*fake.done.lock().unwrap(), vec![2]);
        // One left, so the key alone says which now.
        assert!(!call(&sessions, "t", "claims_release", json!({"key": "k"})).is_error);
        assert_eq!(*fake.released.lock().unwrap(), vec![1]);
    }

    #[test]
    fn a_claim_no_longer_yours_is_forgotten() {
        let (fake, sessions) = with(Fake::default());
        take(&sessions, "t", "mail", "m-1");
        *fake.finish_fails.lock().unwrap() = Some(NO_LONGER_YOURS.to_string());
        let out = call(&sessions, "t", "claims_done", json!({"key": "m-1"}));
        assert_eq!((out.text.as_str(), out.is_error), (NO_LONGER_YOURS, true));
        let out = call(&sessions, "t", "claims_done", json!({"key": "m-1"}));
        assert_eq!(out.text, "You don't hold m-1.");
        // So asking for it again really asks.
        take(&sessions, "t", "mail", "m-1");
        assert_eq!(fake.holders.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_finish_that_failed_can_be_tried_again() {
        let (fake, sessions) = with(Fake::default());
        take(&sessions, "t", "mail", "m-1");
        let why = "The swarms aren't running on this machine right now.";
        *fake.finish_fails.lock().unwrap() = Some(why.to_string());
        let out = call(&sessions, "t", "claims_done", json!({"key": "m-1"}));
        assert_eq!((out.text.as_str(), out.is_error), (why, true));
        let out = call(&sessions, "t", "claims_done", json!({"key": "m-1"}));
        assert_eq!((out.text.as_str(), out.is_error), ("m-1 is done.", false));
    }

    #[test]
    fn closing_a_terminal_ends_its_claims() {
        let (fake, sessions) = with(Fake::default());
        take(&sessions, "closing", "mail", "m-1");
        take(&sessions, "closing", "jobs", "j-1");
        take(&sessions, "staying", "mail", "m-2");
        sessions.terminal_closed("closing");
        assert_eq!(*fake.ended.lock().unwrap(), vec![1, 2]);
        sessions.terminal_closed("closing");
        assert_eq!(fake.ended.lock().unwrap().len(), 2, "only once");
        assert!(!call(&sessions, "staying", "claims_done", json!({"key": "m-2"})).is_error);
    }

    #[test]
    fn a_take_answered_after_its_terminal_closed_is_ended() {
        let (started_tx, started) = channel();
        let (go, go_rx) = channel();
        let fake = Fake::default();
        *fake.pause.lock().unwrap() = Some((started_tx, go_rx));
        let (fake, sessions) = with(fake);
        let sessions = Arc::new(sessions);
        let taking = {
            let sessions = sessions.clone();
            std::thread::spawn(move || take(&sessions, "t", "mail", "m-1"))
        };
        started.recv().unwrap();
        sessions.terminal_closed("t");
        assert!(fake.ended.lock().unwrap().is_empty());
        go.send(()).unwrap();
        let out = taking.join().unwrap();
        assert!(out.is_error, "{}", out.text);
        assert_eq!(*fake.ended.lock().unwrap(), vec![1], "nothing else would");
    }

    #[test]
    fn a_loss_gives_the_token_and_the_typed_text() {
        let person = || "Ana".to_string();
        let reasons = [
            (
                LostReason::Beaten {
                    person: person(),
                    machine: "desk".to_string(),
                },
                "Ana (desk) claimed it first",
            ),
            (LostReason::Done { person: person() }, "Ana finished it"),
            (LostReason::Released { person: person() }, "Ana released it"),
            (
                LostReason::Lapsed,
                "it lapsed while this machine couldn't renew it",
            ),
        ];
        for (reason, why) in reasons {
            let (_, sessions) = with(Fake::default());
            take(&sessions, "t", "mail", "m-1");
            let told = sessions.lost(1, &reason);
            let text = format!("Your claim on m-1 in mail has gone: {why}. Stop working on it.");
            assert_eq!(told, Some(("t".to_string(), text)));
            // Forgotten, so a second loss says nothing and it can't be finished.
            assert_eq!(sessions.lost(1, &reason), None);
            let out = call(&sessions, "t", "claims_done", json!({"key": "m-1"}));
            assert!(out.is_error);
        }
    }

    #[test]
    fn a_loss_while_the_take_is_answering_is_told_by_the_take() {
        let (started_tx, started) = channel();
        let (go, go_rx) = channel();
        let fake = Fake::default();
        *fake.pause.lock().unwrap() = Some((started_tx, go_rx));
        let (fake, sessions) = with(fake);
        let sessions = Arc::new(sessions);
        let taking = {
            let sessions = sessions.clone();
            std::thread::spawn(move || take(&sessions, "t", "mail", "m-1"))
        };
        started.recv().unwrap();
        // The client can tell of a loss the moment after it has given the take its answer.
        assert_eq!(sessions.lost(1, &LostReason::Lapsed), None);
        go.send(()).unwrap();
        let out = taking.join().unwrap();
        assert_eq!(
            out.text,
            "Your claim on m-1 in mail has gone: it lapsed while this machine couldn't renew \
             it. Stop working on it."
        );
        let out = call(&sessions, "t", "claims_done", json!({"key": "m-1"}));
        assert!(out.is_error, "not held");
        assert!(fake.done.lock().unwrap().is_empty());
    }

    #[test]
    fn a_list_is_the_contexts_view_as_json() {
        let view = ContextView {
            name: "mail".to_string(),
            settings: ContextSettings::default(),
            live: Vec::new(),
            done: Vec::new(),
        };
        let (_, sessions) = with(Fake {
            view: Some(view.clone()),
            ..Fake::default()
        });
        let out = call(&sessions, "t", "claims_list", json!({"context": "mail"}));
        assert!(!out.is_error);
        assert_eq!(
            serde_json::from_str::<ContextView>(&out.text).unwrap(),
            view
        );
        let (_, sessions) = with(Fake::default());
        let out = call(&sessions, "t", "claims_list", json!({"context": "mail"}));
        assert_eq!((out.text.as_str(), out.is_error), ("nothing in mail", true));
    }

    #[test]
    fn the_tools_are_listed() {
        let tools = schemas();
        let required = |name: &str| -> Vec<String> {
            let tool = tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("{name} is listed"));
            assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
            serde_json::from_value(tool["inputSchema"]["required"].clone()).unwrap()
        };
        assert_eq!(tools.len(), 4);
        assert_eq!(required("claims_take"), ["context", "key"]);
        assert_eq!(required("claims_release"), ["key"]);
        assert_eq!(required("claims_done"), ["key"]);
        assert_eq!(required("claims_list"), ["context"]);
    }
}
