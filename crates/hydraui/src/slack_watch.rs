//! A `[slack]` pipeline's watcher: which messages in its channel to take, marking each with the
//! reaction, and the pull requests that become its runs, one at a time. The calls are
//! `slack::Http`'s, made off the UI thread; what waits to run is kept in a file, so a restart
//! doesn't lose pull requests whose message is already marked.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::linear::LinearError;
use crate::pipeline::{Pipeline, SlackSettings};
use crate::slack::{Message, Pr, SlackApi, pr_links};

/// A pull request waiting for its run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    pub url: String,
    pub ticket: String,
    pub title: String,
    /// Who posted it (a Slack user id).
    pub by: String,
}

impl Queued {
    fn of(pr: &Pr, by: &str) -> Self {
        Queued {
            url: pr.url.clone(),
            ticket: pr.ticket(),
            title: pr.title(),
            by: by.to_string(),
        }
    }
}

/// The messages to take, oldest first, each with its pull requests not yet run or queued. A
/// message carrying `reaction` from anyone is someone's already; one without a pull request
/// isn't for this pipeline.
pub fn pick<'a>(
    messages: &'a [Message],
    reaction: &str,
    taken: impl Fn(&str) -> bool,
) -> Vec<(&'a Message, Vec<Pr>)> {
    messages
        .iter()
        .filter(|m| !m.reactions.iter().any(|r| r == reaction))
        .filter_map(|m| {
            let prs: Vec<Pr> = pr_links(&m.text)
                .into_iter()
                .filter(|p| !taken(&p.ticket()))
                .collect();
            (!prs.is_empty()).then_some((m, prs))
        })
        .collect()
}

/// One look: the channel's messages from the last `lookback_hours`, each one to take marked
/// with the reaction, and their pull requests in order. `ran` are tickets hydraui has run
/// before; `queued` are waiting already.
pub fn find_and_mark(
    api: &dyn SlackApi,
    settings: &SlackSettings,
    now: SystemTime,
    ran: &[String],
    queued: &[String],
) -> Result<Vec<Queued>, LinearError> {
    let since = now
        .checked_sub(Duration::from_secs(
            u64::from(settings.lookback_hours) * 3600,
        ))
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let messages = api.history(&settings.channel, since)?;
    let taken = |t: &str| ran.iter().chain(queued).any(|r| r.eq_ignore_ascii_case(t));
    let mut found = Vec::new();
    for (m, prs) in pick(&messages, &settings.reaction, taken) {
        // Marked first: if that fails, nothing is queued, and the next look tries again.
        api.react(&settings.channel, &m.ts, &settings.reaction)?;
        found.extend(prs.iter().map(|p| Queued::of(p, &m.user)));
    }
    Ok(found)
}

/// What the orchestrator is told to start the next pull request.
pub fn start_prompt(pipeline: &Pipeline, settings: &SlackSettings, pr: &Queued) -> String {
    format!(
        "A pull request was posted in {channel}: {url}. Start it: call run_start with pipeline \
         \"{file}\", ticket \"{ticket}\", summary \"{title}\" and link \"{url}\".",
        channel = settings.label,
        url = pr.url,
        file = pipeline.file,
        ticket = pr.ticket,
        title = pr.title,
    )
}

/// The card's line while a look is out.
pub fn looking(settings: &SlackSettings) -> String {
    format!("Looking in {} for new pull requests…", settings.label)
}

/// The switch's tooltip.
pub fn tooltip(settings: &SlackSettings) -> String {
    format!(
        "On: every {} min, takes each message in {} with a pull request link and no :{}: \
         (adding yours), and runs its pull requests one at a time. Off: nothing starts by \
         itself.",
        settings.every_minutes, settings.label, settings.reaction
    )
}

/// Where pipeline `file`'s waiting pull requests are kept.
pub fn queue_file(hydra_root: &Path, file: &str) -> PathBuf {
    hydra_root
        .join("hydraui")
        .join("slack-queue")
        .join(format!("{file}.json"))
}

pub fn load_queue(path: &Path) -> VecDeque<Queued> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_queue(path: &Path, queue: &VecDeque<Queued>) -> std::io::Result<()> {
    if queue.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(queue).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn msg(ts: &str, text: &str, reactions: &[&str]) -> Message {
        Message {
            ts: ts.into(),
            user: "U1".into(),
            text: text.into(),
            reactions: reactions.iter().map(|r| r.to_string()).collect(),
        }
    }

    fn settings() -> SlackSettings {
        SlackSettings {
            channel: "C0C217CER6G".into(),
            label: "#pull-requests".into(),
            reaction: "eyes".into(),
            every_minutes: 2,
            lookback_hours: 24,
            watch: true,
        }
    }

    struct Fake {
        messages: Vec<Message>,
        reacted: RefCell<Vec<String>>,
        fail_react: bool,
    }

    impl SlackApi for Fake {
        fn history(&self, _: &str, _: u64) -> Result<Vec<Message>, LinearError> {
            Ok(self.messages.clone())
        }
        fn react(&self, _: &str, ts: &str, name: &str) -> Result<(), LinearError> {
            if self.fail_react {
                return Err(LinearError::Failed("down".into()));
            }
            self.reacted.borrow_mut().push(format!("{ts}:{name}"));
            Ok(())
        }
    }

    const PR: &str = "<https://github.com/iov42/svc-usage/pull/";

    #[test]
    fn messages_someone_took_or_without_a_pr_are_left_alone() {
        let messages = vec![
            msg("1", &format!("{PR}60|x>"), &["eyes"]),
            msg("2", "lunch?", &[]),
            msg(
                "3",
                &format!("Reviews pls: {PR}56|a> {PR}61|b>"),
                &["thumbsup"],
            ),
        ];
        let got = pick(&messages, "eyes", |t| t == "svc-usage-56");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0.ts, "3");
        assert_eq!(
            got[0].1.iter().map(Pr::ticket).collect::<Vec<_>>(),
            ["svc-usage-61"]
        );
    }

    #[test]
    fn a_look_marks_each_message_it_takes_and_queues_every_pr_in_it() {
        let api = Fake {
            messages: vec![
                msg("1", &format!("{PR}60|x>"), &[]),
                msg("2", &format!("{PR}61|a> and {PR}62|b>"), &[]),
                msg("3", &format!("{PR}63|c>"), &[]),
            ],
            reacted: RefCell::default(),
            fail_react: false,
        };
        let found = find_and_mark(
            &api,
            &settings(),
            SystemTime::now(),
            &["svc-usage-60".into()],
            &["svc-usage-63".into()],
        )
        .unwrap();
        assert_eq!(
            found.iter().map(|q| q.ticket.as_str()).collect::<Vec<_>>(),
            ["svc-usage-61", "svc-usage-62"]
        );
        assert_eq!(
            *api.reacted.borrow(),
            ["2:eyes"],
            "only the message it took"
        );
        assert_eq!(found[0].url, "https://github.com/iov42/svc-usage/pull/61");
    }

    #[test]
    fn nothing_is_queued_when_marking_fails() {
        let api = Fake {
            messages: vec![msg("1", &format!("{PR}60|x>"), &[])],
            reacted: RefCell::default(),
            fail_react: true,
        };
        assert!(find_and_mark(&api, &settings(), SystemTime::now(), &[], &[]).is_err());
    }

    #[test]
    fn the_orchestrator_is_told_the_ticket_title_and_link() {
        let p = crate::pipeline::parse(
            "name = \"PRs\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"review {pr}\"\n",
        )
        .unwrap();
        let pr = Queued::of(&pr_links(&format!("{PR}62|x>"))[0], "U1");
        let text = start_prompt(
            &Pipeline {
                file: "review-pr-s".into(),
                ..p
            },
            &settings(),
            &pr,
        );
        assert!(text.contains("pipeline \"review-pr-s\""), "{text}");
        assert!(text.contains("ticket \"svc-usage-62\""), "{text}");
        assert!(
            text.contains("link \"https://github.com/iov42/svc-usage/pull/62\""),
            "{text}"
        );
    }

    #[test]
    fn the_queue_survives_a_restart_and_an_empty_one_leaves_no_file() {
        let d = tempfile::tempdir().unwrap();
        let path = queue_file(d.path(), "review-pr-s");
        let mut q = VecDeque::new();
        q.push_back(Queued::of(&pr_links(&format!("{PR}62|x>"))[0], "U1"));
        save_queue(&path, &q).unwrap();
        assert_eq!(load_queue(&path), q);
        save_queue(&path, &VecDeque::new()).unwrap();
        assert!(!path.exists());
        assert!(load_queue(&path).is_empty());
    }
}
