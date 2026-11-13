//! Pipeline settings read back in words (core spec §63): the one-line summaries a folded card
//! shows, and the choices its dropdowns offer. Pure, so the page stays a view of these.

use crate::pipeline::{Assignee, Edit, EditStage, KeepChecking};

/// Minutes as a dropdown says them: "5 minutes", "1 hour".
pub fn minutes(m: u32) -> String {
    match m {
        0 => "Off".into(),
        1 => "1 minute".into(),
        60 => "1 hour".into(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

/// Hours as a dropdown says them: "1 hour", "1 day", "1 week".
pub fn hours(h: u32) -> String {
    match h {
        1 => "1 hour".into(),
        24 => "1 day".into(),
        168 => "1 week".into(),
        h if h % 24 == 0 => format!("{} days", h / 24),
        h => format!("{h} hours"),
    }
}

/// Days: "1 day", "7 days".
pub fn days(d: u32) -> String {
    if d == 1 {
        "1 day".into()
    } else {
        format!("{d} days")
    }
}

/// The shares of the seven-day Claude limit Settings offers as a stage's floor (orchestrator
/// spec §5.1); 0 turns the check off.
pub const WEEK_LEFT_CHOICES: [u8; 6] = [0, 10, 20, 30, 40, 50];

/// The week-limit choices, with `current` added in order when the file set another.
pub fn week_left_choices(current: u8) -> Vec<u8> {
    let mut all = WEEK_LEFT_CHOICES.to_vec();
    if !all.contains(&current) {
        all.push(current);
        all.sort_unstable();
    }
    all
}

/// A week-limit choice as its button says it: "Off", "20%", "30% (default)".
pub fn week_left(n: u8) -> String {
    match n {
        0 => "Off".into(),
        n if n == crate::runs::MIN_WEEK_LEFT => format!("{n}% (default)"),
        n => format!("{n}%"),
    }
}

/// What the week-limit setting does, at `n`.
pub fn week_left_about(n: u8) -> String {
    match n {
        0 => "Off: a pipeline's stages start however little of the seven-day Claude limit is \
              left."
            .into(),
        n => format!(
            "A pipeline's next stage starts only while at least {n}% of the seven-day Claude \
             limit is left, so pipelines don't use up the week you need yourself. Below it, the \
             ticket waits for you."
        ),
    }
}

/// `options`, with `current` added in order when it isn't one (a value set in the file).
pub fn with_current(options: &[u32], current: u32) -> Vec<u32> {
    let mut all = options.to_vec();
    if !all.contains(&current) {
        all.push(current);
        all.sort_unstable();
    }
    all
}

/// How often a source checks, in minutes.
pub const CHECK_EVERY: [u32; 7] = [1, 2, 5, 10, 15, 30, 60];
/// How far back Slack looks, in hours.
pub const LOOK_BACK_HOURS: [u32; 6] = [1, 6, 12, 24, 72, 168];
/// How far back mail looks, in days.
pub const LOOK_BACK_DAYS: [u32; 5] = [1, 2, 3, 7, 14];
/// How often a step below its bar checks again, in minutes.
pub const KEEP_EVERY: [u32; 5] = [5, 10, 15, 30, 60];
/// For how long, in hours.
pub const KEEP_FOR: [u32; 5] = [1, 2, 4, 8, 24];
/// How often a waiting step looks again, in minutes; 0 never.
pub const LOOK_EVERY: [u32; 5] = [0, 5, 15, 30, 60];
/// For how long, in hours; 0 until someone acts.
pub const LOOK_FOR: [u32; 5] = [0, 1, 4, 8, 24];

/// The folded source card: what starts a run, then how it looks.
pub fn source(edit: &Edit) -> (String, String) {
    if let Some(l) = &edit.linear {
        let whose = match l.assignee {
            Assignee::Unassigned => "unassigned",
            Assignee::Any => "any",
        };
        let which = if l.sla_only {
            "with an SLA"
        } else {
            "by priority"
        };
        let labels = if l.labels.is_empty() {
            String::new()
        } else {
            format!(", labelled {}", l.labels.join(" or "))
        };
        return (
            format!(
                "A {} ticket reaches \"{}\"",
                or(&l.team, "Linear"),
                or(&l.pick, "…")
            ),
            format!(
                "Takes {whose} tickets {which}{labels}; checks every {}",
                minutes(l.every_minutes)
            ),
        );
    }
    if let Some(s) = &edit.slack {
        let channel = if s.label.trim().is_empty() {
            or(&s.channel, "a channel").to_string()
        } else {
            format!("#{}", s.label.trim().trim_start_matches('#'))
        };
        return (
            format!("A pull request is posted in {channel}"),
            format!(
                "Checks every {}, looking back {}",
                minutes(s.every_minutes),
                hours(s.lookback_hours)
            ),
        );
    }
    if let Some(sc) = &edit.schedule {
        return match sc.when() {
            Some(w) => (w.text(), "A run starts by itself at that time".into()),
            None => (
                "On a schedule".into(),
                "Say which days and what time".into(),
            ),
        };
    }
    if let Some(m) = &edit.mail {
        let when = m
            .when()
            .map_or_else(|| "on its schedule".to_string(), |w| w.text());
        return (
            "An email arrives in the mailbox".into(),
            format!(
                "Looks {}, back {}",
                lower_first(&when),
                days(u32::from(m.look_back_days))
            ),
        );
    }
    (
        "You give it work".into(),
        "Start a run yourself from the pipeline's card".into(),
    )
}

/// A folded step card's line: what it's told, in short.
pub fn instructions(s: &EditStage) -> String {
    let text = match &s.jev {
        Some(j) => j.instructions.as_str(),
        None => s.prompt.as_str(),
    };
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    match first {
        None if s.jev.is_some() => "Needs a question".into(),
        None => "Uses the default instructions".into(),
        Some(l) if l.chars().count() > 90 => {
            format!("{}…", l.chars().take(89).collect::<String>().trim_end())
        }
        Some(l) => l.to_string(),
    }
}

/// What More options holds, folded: "keeps checking · 95% · team skills".
pub fn options(s: &EditStage, default_bar: u8, router_on: bool) -> String {
    let mut parts = Vec::new();
    if s.jev.is_none() {
        parts.push(if s.keep.is_some() {
            "keeps checking".to_string()
        } else {
            "asks you if unsure".to_string()
        });
    }
    parts.push(format!("{}% sure", s.bar.unwrap_or(default_bar)));
    if s.jev.is_none() {
        match s.skill.as_deref() {
            Some(k) => parts.push(format!("skill {k}")),
            None if router_on => parts.push("team skills".into()),
            None => {}
        }
        if let Some(f) = &s.folder {
            let name = std::path::Path::new(f)
                .file_name()
                .map_or(f.clone(), |n| n.to_string_lossy().into_owned());
            parts.push(format!("in {name}"));
        }
    }
    parts.join(" · ")
}

/// A step's "not sure enough" rule in a sentence.
pub fn unsure(keep: Option<KeepChecking>, bar: u8) -> String {
    match keep {
        None => format!("If it's less than {bar}% sure, it stops and asks you."),
        Some(k) => format!(
            "If it's less than {bar}% sure, it checks again every {} for up to {}, then asks \
             you. It asks at once when it says a person must act.",
            minutes(k.every),
            hours(k.hours)
        ),
    }
}

/// A pipeline's colour and icon by name, e.g. "Violet · Pull request"; a colour not among
/// `pipeline::COLORS` is its hex, and no icon is the Diamond.
pub fn look(color: Option<u32>, icon: Option<&str>) -> String {
    let c = color.unwrap_or(crate::orchestrator_theme::TEAL);
    let colour = crate::pipeline::COLORS
        .iter()
        .find(|(_, h)| *h == c)
        .map_or_else(|| crate::pipeline::color_text(c), |(n, _)| n.to_string());
    let shape = icon
        .and_then(|i| crate::pipeline_icon::ICONS.iter().find(|(n, _)| *n == i))
        .map_or("Diamond", |(_, l)| *l);
    format!("{colour} · {shape}")
}

/// Where a pipeline's work comes from, in a few words for its card in Start a pipeline.
pub fn source_short(p: &crate::pipeline::Pipeline) -> String {
    if let Some(l) = &p.linear {
        format!("Linear · {}", or(&l.team, "a team"))
    } else if let Some(s) = &p.slack {
        let label = s.label.trim().trim_start_matches('#');
        if label.is_empty() {
            "Slack".into()
        } else {
            format!("Slack · #{label}")
        }
    } else if p.mail.is_some() {
        "Gmail".into()
    } else if p.schedule.is_some() {
        "On a schedule".into()
    } else {
        "Started by you".into()
    }
}

/// "1 stage", "3 stages".
pub fn stages(n: usize) -> String {
    if n == 1 {
        "1 stage".into()
    } else {
        format!("{n} stages")
    }
}

fn or<'a>(s: &'a str, empty: &'a str) -> &'a str {
    if s.trim().is_empty() { empty } else { s.trim() }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_week_limit_reads_as_its_choices_say_it() {
        assert_eq!(week_left(0), "Off");
        assert_eq!(week_left(20), "20%");
        assert_eq!(week_left(crate::runs::MIN_WEEK_LEFT), "30% (default)");
        assert_eq!(week_left_choices(30), WEEK_LEFT_CHOICES.to_vec());
        assert_eq!(week_left_choices(25), vec![0, 10, 20, 25, 30, 40, 50]);
        assert!(week_left_about(0).starts_with("Off"));
        assert!(week_left_about(25).contains("at least 25%"));
    }
    use crate::pipeline::{self, KeepChecking};

    fn edit(toml: &str) -> Edit {
        Edit::from(&pipeline::parse(toml).unwrap())
    }

    const STEP: &str = "name = \"P\"\n[[stage]]\nid = \"a\"\ntitle = \"A\"\n";

    #[test]
    fn a_look_reads_as_its_colour_and_icon() {
        assert_eq!(look(None, None), "Teal · Diamond");
        assert_eq!(
            look(Some(0x9b7cf0), Some("git-pull-request")),
            "Violet · Pull request"
        );
        assert_eq!(look(Some(0x123456), Some("mail")), "#123456 · Mail");
    }

    #[test]
    fn a_card_says_where_work_comes_from() {
        let p = |extra: &str| crate::pipeline::parse(&format!("{STEP}{extra}")).unwrap();
        assert_eq!(source_short(&p("")), "Started by you");
        assert_eq!(
            source_short(&p("[linear]\nteam = \"INTH\"\n")),
            "Linear · INTH"
        );
        assert_eq!(
            source_short(&p(
                "[slack]\nchannel = \"C0C217CER6G\"\nlabel = \"#pull-requests\"\n"
            )),
            "Slack · #pull-requests"
        );
        assert_eq!(
            source_short(&p("[mail]\ncheck = \"weekdays 09:00\"\n")),
            "Gmail"
        );
        assert_eq!(
            source_short(&p("[schedule]\nevery = \"mon 18:00\"\n")),
            "On a schedule"
        );
        assert_eq!(
            (stages(1).as_str(), stages(3).as_str()),
            ("1 stage", "3 stages")
        );
    }

    #[test]
    fn durations_read_as_people_say_them() {
        assert_eq!(minutes(0), "Off");
        assert_eq!(minutes(1), "1 minute");
        assert_eq!(minutes(5), "5 minutes");
        assert_eq!(minutes(60), "1 hour");
        assert_eq!(minutes(120), "2 hours");
        assert_eq!(hours(1), "1 hour");
        assert_eq!(hours(4), "4 hours");
        assert_eq!(hours(24), "1 day");
        assert_eq!(hours(72), "3 days");
        assert_eq!(hours(168), "1 week");
        assert_eq!(days(1), "1 day");
        assert_eq!(days(7), "7 days");
    }

    #[test]
    fn a_value_from_the_file_joins_the_choices_in_order() {
        assert_eq!(with_current(&[5, 10, 30], 10), [5, 10, 30]);
        assert_eq!(with_current(&[5, 10, 30], 20), [5, 10, 20, 30]);
    }

    #[test]
    fn each_source_reads_as_what_starts_a_run() {
        assert_eq!(source(&edit(STEP)).0, "You give it work");
        let slack = edit(&format!(
            "{STEP}[slack]\nchannel = \"C0C217CER6G\"\nlabel = \"pull-requests\"\n\
             every_minutes = 1\nlookback_hours = 24\n"
        ));
        assert_eq!(
            source(&slack),
            (
                "A pull request is posted in #pull-requests".to_string(),
                "Checks every 1 minute, looking back 1 day".to_string()
            )
        );
        let linear = edit(&format!(
            "{STEP}[linear]\nteam = \"INTH\"\npick = \"Ready\"\nlabels = [\"bug\"]\n"
        ));
        let (head, more) = source(&linear);
        assert_eq!(head, "A INTH ticket reaches \"Ready\"");
        assert!(
            more.contains("unassigned") && more.contains("labelled bug"),
            "{more}"
        );
        let mail = edit(&format!(
            "{STEP}[mail]\ncheck = \"weekdays 09:00\"\nlook_back_days = 3\n"
        ));
        let (head, more) = source(&mail);
        assert_eq!(head, "An email arrives in the mailbox");
        assert!(more.ends_with("back 3 days"), "{more}");
    }

    fn step(prompt: &str) -> EditStage {
        EditStage {
            id: None,
            title: "Review".into(),
            skill: None,
            folder: None,
            bar: None,
            prompt: prompt.into(),
            jev: None,
            keep: None,
        }
    }

    #[test]
    fn a_folded_step_shows_its_first_line() {
        assert_eq!(instructions(&step("")), "Uses the default instructions");
        assert_eq!(
            instructions(&step("\n  Review {pr}.\nThen…")),
            "Review {pr}."
        );
        let long = "word ".repeat(40);
        let shown = instructions(&step(&long));
        assert!(
            shown.ends_with('…') && shown.chars().count() <= 90,
            "{shown}"
        );
    }

    #[test]
    fn more_options_folded_says_what_is_set() {
        let mut s = step("x");
        assert_eq!(options(&s, 80, false), "asks you if unsure · 80% sure");
        assert_eq!(
            options(&s, 80, true),
            "asks you if unsure · 80% sure · team skills"
        );
        s.keep = Some(KeepChecking { every: 5, hours: 1 });
        s.bar = Some(95);
        s.skill = Some("angular".into());
        s.folder = Some("~/Projects/iov42-agents".into());
        assert_eq!(
            options(&s, 80, true),
            "keeps checking · 95% sure · skill angular · in iov42-agents"
        );
    }

    #[test]
    fn the_unsure_rule_reads_as_a_sentence() {
        assert_eq!(
            unsure(None, 80),
            "If it's less than 80% sure, it stops and asks you."
        );
        let k = unsure(Some(KeepChecking { every: 5, hours: 1 }), 95);
        assert!(
            k.starts_with(
                "If it's less than 95% sure, it checks again every 5 minutes for up to 1 hour"
            ),
            "{k}"
        );
    }
}
