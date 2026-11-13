//! One taker per job (core spec §87): when several people run a pipeline that takes its work
//! from a shared source (a Slack channel, a Linear team, a group's mail), each new item is
//! claimed through the swarm's claims service (swarm claims spec) before anyone marks it in
//! the source. These are the pure parts: how each kind of job is named, which context a
//! pipeline claims in, and what an answer from the service means for a trigger.

use hydra_swarms::TakeReply;
use hydra_swarms::claims::valid_context_name;

/// The longest key the claims service takes, in bytes.
const MAX_KEY: usize = 200;

/// A Slack message, by channel and timestamp: the job is the message, as its reaction marks.
pub fn slack_key(channel: &str, ts: &str) -> String {
    format!("slack:{channel}:{ts}")
}

/// A Linear issue, by identifier (`INTH-123`).
pub fn linear_key(identifier: &str) -> String {
    format!("linear:{}", identifier.trim().to_uppercase())
}

/// An email, by its Message-ID: every copy of a group's email shares it.
pub fn mail_key(message_id: &str) -> String {
    let id = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    let key = format!("mail:{}", id.to_lowercase());
    if key.len() <= MAX_KEY {
        return key;
    }
    // Too long for a key: its FNV-1a fingerprint, as the claim files use.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("mail:{h:016x}")
}

/// The context a pipeline claims in: `[claims] context`, else its file name made into a
/// context name (lowercase letters, digits, `-` and `_`).
pub fn context_for(file: &str, named: Option<&str>) -> String {
    if let Some(c) = named.map(str::trim).filter(|c| !c.is_empty()) {
        return c.to_string();
    }
    let name: String = file
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if valid_context_name(&name) {
        name
    } else {
        "pipeline".to_string()
    }
}

/// How a trigger takes each job through the swarm: `take` asks for its key, `give_back`
/// releases a claim it won't use after all.
pub struct Claims<'a> {
    pub take: &'a dyn Fn(&str) -> Taken,
    pub give_back: &'a dyn Fn(u64),
}

/// Each person's source marks alone: no swarm.
pub const UNSHARED: Claims<'static> = Claims {
    take: &|_| Taken::Unsettled(String::new()),
    give_back: &|_| {},
};

/// What a take means for a trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// It's this person's: mark it in the source and start. The claim is held as this id.
    Yours(u64),
    /// Someone has it (`ours`: this machine already does), or finished it: leave it.
    Theirs {
        person: String,
        ours: bool,
    },
    Done {
        person: String,
    },
    /// Not now, and why: the context is paused, or this machine is at its limit.
    Wait(String),
    /// The swarm can't settle it (no such context, the swarm isn't open): the source's own
    /// check decides, and the card says why.
    Unsettled(String),
}

impl Taken {
    /// Whether the trigger goes on to mark and start it.
    pub fn go(&self) -> bool {
        matches!(self, Taken::Yours(_) | Taken::Unsettled(_))
    }
}

/// What the service's answer to a take means.
pub fn says(reply: Result<TakeReply, String>, context: &str) -> Taken {
    match reply {
        Ok(TakeReply::Yours { holder_id, .. }) => Taken::Yours(holder_id),
        Ok(TakeReply::Held {
            person,
            this_machine,
            ..
        }) => Taken::Theirs {
            person,
            ours: this_machine,
        },
        Ok(TakeReply::Done { person, .. }) => Taken::Done { person },
        Ok(TakeReply::Paused) => Taken::Wait(format!("{context} is paused in the swarm")),
        Ok(TakeReply::AtLimit(n)) => Taken::Wait(format!(
            "this machine is at its limit of {n} in {context}: a swarm owner can raise it"
        )),
        Ok(TakeReply::Gone(_)) => Taken::Wait("the claim had ended".into()),
        Err(e) if e.contains("has no context called") => Taken::Unsettled(format!(
            "{e} Ask a swarm owner to create it, so teammates never take the same job."
        )),
        Err(e) => Taken::Unsettled(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_job_has_its_own_key() {
        assert_eq!(
            slack_key("C0C217CER6G", "1731499200.1234"),
            "slack:C0C217CER6G:1731499200.1234"
        );
        assert_eq!(linear_key(" inth-12 "), "linear:INTH-12");
        assert_eq!(mail_key("<AbC@mail.gmail.com>"), "mail:abc@mail.gmail.com");
        let long = format!("<{}@x>", "a".repeat(300));
        let k = mail_key(&long);
        assert!(k.starts_with("mail:") && k.len() == 21, "{k}");
        assert_eq!(
            k,
            mail_key(&long.to_uppercase()),
            "copies of one email share it"
        );
    }

    #[test]
    fn a_pipeline_claims_in_its_own_context_unless_it_names_one() {
        assert_eq!(context_for("review-pr-s", None), "review-pr-s");
        assert_eq!(
            context_for("Integrations Emails", None),
            "integrations-emails"
        );
        assert_eq!(context_for("x", Some(" prs ")), "prs");
    }

    #[test]
    fn the_answer_says_whether_to_go() {
        let yours = Ok(TakeReply::Yours {
            holder_id: 7,
            swarm: "team".into(),
            lapse_micros: 1,
        });
        assert_eq!(says(yours, "prs"), Taken::Yours(7));
        let held = says(
            Ok(TakeReply::Held {
                person: "Kris".into(),
                machine: "kris-mbp".into(),
                this_machine: false,
                since: 1,
            }),
            "prs",
        );
        assert_eq!(
            held,
            Taken::Theirs {
                person: "Kris".into(),
                ours: false
            }
        );
        assert!(!held.go());
        assert!(!says(Ok(TakeReply::AtLimit(1)), "prs").go());
        assert!(
            says(Ok(TakeReply::AtLimit(1)), "prs")
                == Taken::Wait(
                    "this machine is at its limit of 1 in prs: a swarm owner can raise it".into()
                )
        );
        // No such context: the source's own check decides, and the card says what to do.
        let none = says(Err("team has no context called prs.".into()), "prs");
        assert!(none.go());
        assert!(matches!(&none, Taken::Unsettled(w) if w.contains("swarm owner")));
    }
}
