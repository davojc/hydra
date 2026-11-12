//! Scheduled pipelines (core spec §58): `[schedule] every = "mon 18:00"` starts a run at that
//! local time on those days while hydraui is open, or once when it next opens that day.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// When a pipeline runs: on these days (Monday first), at this local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub days: [bool; 7],
    pub hour: u8,
    pub minute: u8,
}

const DAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Reads `every`: days, then a 24-hour time. Days are `daily`, `weekdays`, `weekends`, or
/// names joined by commas (`mon,wed,fri`; full names too).
pub fn parse(every: &str) -> Result<When, String> {
    let usage = "e.g. \"mon 18:00\", \"weekdays 09:00\", \"daily 08:30\" or \"mon,thu 17:00\"";
    let mut parts = every.split_whitespace();
    let (Some(days), Some(time), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!(
            "[schedule] every {every:?} needs days and a time, {usage}"
        ));
    };
    let mut on = [false; 7];
    match days.to_ascii_lowercase().as_str() {
        "daily" | "everyday" => on = [true; 7],
        "weekdays" => on[..5].fill(true),
        "weekends" => on[5..].fill(true),
        list => {
            for d in list.split(',').filter(|d| !d.is_empty()) {
                let i = DAY_NAMES
                    .iter()
                    .position(|n| d.get(..3) == Some(*n) && is_day(d))
                    .ok_or(format!("[schedule] {d:?} isn't a day, {usage}"))?;
                on[i] = true;
            }
        }
    }
    if !on.contains(&true) {
        return Err(format!("[schedule] every {every:?} names no day, {usage}"));
    }
    let (h, m) = time
        .split_once(':')
        .ok_or(format!("[schedule] time {time:?} must be HH:MM, {usage}"))?;
    let (hour, minute) = (
        h.parse::<u8>().ok().filter(|h| *h < 24),
        m.parse::<u8>().ok().filter(|m| *m < 60),
    );
    let (Some(hour), Some(minute)) = (hour, minute) else {
        return Err(format!(
            "[schedule] time {time:?} must be HH:MM, 00:00 to 23:59"
        ));
    };
    Ok(When {
        days: on,
        hour,
        minute,
    })
}

fn is_day(d: &str) -> bool {
    const FULL: [&str; 7] = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    FULL.iter().any(|f| f.starts_with(d))
}

impl When {
    /// How it reads: `Mondays at 18:00`, `Weekdays at 09:00`.
    pub fn text(&self) -> String {
        const PLURAL: [&str; 7] = [
            "Mondays",
            "Tuesdays",
            "Wednesdays",
            "Thursdays",
            "Fridays",
            "Saturdays",
            "Sundays",
        ];
        let days = if self.days == [true; 7] {
            "Every day".to_string()
        } else if self.days == [true, true, true, true, true, false, false] {
            "Weekdays".to_string()
        } else if self.days == [false, false, false, false, false, true, true] {
            "Weekends".to_string()
        } else {
            let on: Vec<&str> = (0..7)
                .filter(|&i| self.days[i])
                .map(|i| PLURAL[i])
                .collect();
            on.join(", ")
        };
        format!("{days} at {:02}:{:02}", self.hour, self.minute)
    }

    /// Whether a run is due at log time `now` (`2026-11-16 18:00:05.000`), when the last one
    /// started on date `last` (`2026-11-09`): today is one of its days, its time has passed,
    /// and none started today. So a day hydraui opens late still gets its one run.
    pub fn due(&self, now: &str, last: Option<&str>) -> bool {
        let Some((date, time)) = now.split_once(' ') else {
            return false;
        };
        let Some(day) = weekday(date) else {
            return false;
        };
        let hm = time.get(..5).unwrap_or("");
        self.days[day]
            && hm >= format!("{:02}:{:02}", self.hour, self.minute).as_str()
            && last != Some(date)
    }
}

/// Where the clock stands between a schedule's runs, in seconds: since the run time before
/// (or since its time today, while that run is due and not yet started), and to the next.
/// Counted on the wall clock, so a daylight-saving change is an hour out for that one wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub since: u64,
    pub left: u64,
}

impl Wait {
    /// How far through the wait, 0.0 just after a run time to 1.0 when the next is due.
    pub fn progress(&self) -> f32 {
        match self.since + self.left {
            0 => 1.0,
            all => self.since as f32 / all as f32,
        }
    }
}

impl When {
    /// The wait at log time `now`, when the last run started on date `last` (as `due`).
    pub fn wait(&self, now: &str, last: Option<&str>) -> Option<Wait> {
        let (date, time) = now.split_once(' ')?;
        let day = weekday(date)?;
        let mut hms = time.get(..8)?.split(':').map(|p| p.parse::<i64>().ok());
        let secs = hms.next()?? * 3600 + hms.next()?? * 60 + hms.next()??;
        let slot = i64::from(self.hour) * 3600 + i64::from(self.minute) * 60;
        let on = |d: i64| self.days[(day as i64 + d).rem_euclid(7) as usize];
        let since = (0..=7)
            .find(|&d| on(-d) && (d > 0 || slot <= secs))
            .map(|d| d * 86_400 + secs - slot)?;
        let left = if self.due(now, last) {
            0
        } else {
            (0..=7)
                .find(|&d| on(d) && (d > 0 || slot > secs))
                .map(|d| d * 86_400 + slot - secs)?
        };
        Some(Wait {
            since: u64::try_from(since).ok()?,
            left: u64::try_from(left).ok()?,
        })
    }
}

/// Seconds to go, as a person would say them: "30 seconds", "1 hour 25 minutes", "8 hours",
/// "1 day 5 hours", "6 days".
pub fn countdown(secs: u64) -> String {
    let n = |n: u64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match secs {
        0..60 => n(secs, "second"),
        60..3600 => n(m, "minute"),
        3600..7200 if m > 0 => format!("1 hour {}", n(m, "minute")),
        3600..86_400 => n(secs / 3600, "hour"),
        86_400..172_800 if h > 0 => format!("1 day {}", n(h, "hour")),
        _ => n(d, "day"),
    }
}

/// Monday 0 to Sunday 6, of `YYYY-MM-DD`.
pub fn weekday(date: &str) -> Option<usize> {
    let mut p = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (p.next()??, p.next()??, p.next()??);
    // Days since 1970-01-01 (a Thursday), by Howard Hinnant's civil-from-days inverse.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days + 3).rem_euclid(7) as usize)
}

/// The run's ticket for pipeline file `file` on `date`: `CS-SYNC-2026-11-16`. Plain letters,
/// digits and `-`, so its log's name is the run's id and a restart finds it.
pub fn ticket(file: &str, date: &str) -> String {
    let base: String = file
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-{date}", base.trim_matches('-'))
}

/// `<hydra_root>/hydraui/schedule.json`: the date each pipeline's last scheduled run started.
fn file(hydra_root: &Path) -> PathBuf {
    hydra_root.join("hydraui").join("schedule.json")
}

pub fn last_runs(hydra_root: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(file(hydra_root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Notes that pipeline `pipeline`'s scheduled run started on `date`.
pub fn mark_run(hydra_root: &Path, pipeline: &str, date: &str) -> std::io::Result<()> {
    let mut all = last_runs(hydra_root);
    all.insert(pipeline.to_string(), date.to_string());
    let path = file(hydra_root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&all).map_err(std::io::Error::other)?,
    )
}

/// Where a scheduled run goes: the environment `[schedule]` names, else `[autostart]`'s, else
/// one where the pipeline already has an orchestrator, else the default, else the first;
/// the first of those that exists in `envs`.
pub fn pick_env(
    named: Option<&str>,
    autostart: Option<&str>,
    running: Option<&str>,
    default: Option<&str>,
    envs: &[String],
) -> Option<String> {
    [named, autostart, running, default]
        .into_iter()
        .flatten()
        .find(|e| envs.iter().any(|x| x == e))
        .map(str::to_string)
        .or_else(|| envs.first().cloned())
}

/// What the orchestrator is told when a scheduled run is due.
pub fn start_prompt(name: &str, file: &str, when: &When, ticket: &str, date: &str) -> String {
    format!(
        "It's time for the scheduled run of pipeline \"{name}\" ({}). Start it: call run_start \
         with pipeline \"{file}\", ticket {ticket} and summary \"{name} {date}\".",
        when.text()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_a_time_read_in_several_ways() {
        let w = parse("mon 18:00").unwrap();
        assert_eq!(w.days, [true, false, false, false, false, false, false]);
        assert_eq!((w.hour, w.minute), (18, 0));
        assert_eq!(w.text(), "Mondays at 18:00");
        assert_eq!(parse("weekdays 09:05").unwrap().text(), "Weekdays at 09:05");
        assert_eq!(parse("Daily 8:30").unwrap().text(), "Every day at 08:30");
        assert_eq!(parse("weekends 10:00").unwrap().text(), "Weekends at 10:00");
        assert_eq!(
            parse("Monday,thu 17:00").unwrap().text(),
            "Mondays, Thursdays at 17:00"
        );
    }

    #[test]
    fn a_bad_schedule_says_why() {
        for bad in [
            "",
            "mon",
            "mon 25:00",
            "mon 18:60",
            "funday 18:00",
            "mon 6pm",
            "mo 18:00",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(parse("mon 18:00 extra").is_err());
    }

    #[test]
    fn weekdays_count_from_monday() {
        assert_eq!(weekday("2026-11-16"), Some(0)); // a Monday
        assert_eq!(weekday("2026-11-10"), Some(1)); // a Tuesday
        assert_eq!(weekday("2026-11-15"), Some(6)); // a Sunday
        assert_eq!(weekday("2024-02-29"), Some(3)); // a Thursday
        assert_eq!(weekday("nope"), None);
    }

    #[test]
    fn a_run_is_due_once_its_time_passes_on_its_day_and_only_once() {
        let w = parse("mon 18:00").unwrap();
        assert!(!w.due("2026-11-16 17:59:59.000", None));
        assert!(w.due("2026-11-16 18:00:00.000", None));
        // Opened late that day: still its one run.
        assert!(w.due("2026-11-16 22:15:00.000", Some("2026-11-09")));
        assert!(!w.due("2026-11-16 22:15:00.000", Some("2026-11-16")));
        // Not on Tuesday, however late.
        assert!(!w.due("2026-11-17 19:00:00.000", Some("2026-11-09")));
    }

    #[test]
    fn the_wait_runs_from_the_run_before_to_the_next() {
        let w = parse("daily 09:00").unwrap();
        // Before today's run: since yesterday's, an hour to go.
        let t = w
            .wait("2026-11-16 08:00:00.000", Some("2026-11-15"))
            .unwrap();
        assert_eq!((t.since, t.left), (23 * 3600, 3600));
        // Just after today's run started: tomorrow's is next.
        let t = w
            .wait("2026-11-16 09:00:30.000", Some("2026-11-16"))
            .unwrap();
        assert_eq!((t.since, t.left), (30, 24 * 3600 - 30));
        // Started on the second of its time: the next is tomorrow's, not this one.
        let t = w
            .wait("2026-11-16 09:00:00.000", Some("2026-11-16"))
            .unwrap();
        assert_eq!((t.since, t.left), (0, 24 * 3600));
        // Due and not started yet: nothing left to wait, the bar is full.
        let t = w
            .wait("2026-11-16 09:00:10.000", Some("2026-11-15"))
            .unwrap();
        assert_eq!((t.since, t.left), (10, 0));
        assert!((t.progress() - 1.0).abs() < f32::EPSILON);
        // Weekly: Tuesday to Thursday 19:26 is 2 days, since the Thursday before 5.
        let w = parse("thu 19:26").unwrap();
        let t = w.wait("2026-11-10 19:26:00.000", None).unwrap();
        assert_eq!((t.since, t.left), (5 * 86_400, 2 * 86_400));
        assert!((t.progress() - 5.0 / 7.0).abs() < 1e-6);
        // Run that Thursday: a week to the next.
        let t = w
            .wait("2026-11-12 19:27:00.000", Some("2026-11-12"))
            .unwrap();
        assert_eq!((t.since, t.left), (60, 7 * 86_400 - 60));
        assert!(w.wait("not a time", None).is_none());
    }

    #[test]
    fn the_time_left_reads_like_a_person_would_say_it() {
        assert_eq!(countdown(1), "1 second");
        assert_eq!(countdown(30), "30 seconds");
        assert_eq!(countdown(90), "1 minute");
        assert_eq!(countdown(59 * 60 + 59), "59 minutes");
        assert_eq!(countdown(3600), "1 hour");
        assert_eq!(countdown(3600 + 25 * 60), "1 hour 25 minutes");
        assert_eq!(countdown(8 * 3600 + 100), "8 hours");
        assert_eq!(countdown(86_400 + 5 * 3600), "1 day 5 hours");
        assert_eq!(countdown(86_400 + 10), "1 day");
        assert_eq!(countdown(6 * 86_400 + 23 * 3600), "6 days");
    }

    #[test]
    fn its_ticket_is_a_plain_name_with_the_date() {
        assert_eq!(ticket("cs-sync", "2026-11-16"), "CS-SYNC-2026-11-16");
        assert_eq!(
            ticket("weekly review", "2026-11-16"),
            "WEEKLY-REVIEW-2026-11-16"
        );
    }

    #[test]
    fn a_run_goes_to_the_first_environment_that_exists() {
        let envs = ["iov42".to_string(), "personal".to_string()];
        assert_eq!(
            pick_env(Some("gone"), Some("personal"), None, None, &envs).as_deref(),
            Some("personal")
        );
        assert_eq!(
            pick_env(None, None, Some("iov42"), Some("personal"), &envs).as_deref(),
            Some("iov42")
        );
        assert_eq!(
            pick_env(None, None, None, None, &envs).as_deref(),
            Some("iov42")
        );
        assert_eq!(pick_env(Some("x"), None, None, None, &[]), None);
    }

    #[test]
    fn the_last_run_dates_are_kept_per_pipeline() {
        let d = tempfile::tempdir().unwrap();
        assert!(last_runs(d.path()).is_empty());
        mark_run(d.path(), "cs-sync", "2026-11-16").unwrap();
        mark_run(d.path(), "other", "2026-11-10").unwrap();
        mark_run(d.path(), "cs-sync", "2026-11-23").unwrap();
        let all = last_runs(d.path());
        assert_eq!(all["cs-sync"], "2026-11-23");
        assert_eq!(all["other"], "2026-11-10");
    }
}
