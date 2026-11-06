//! The Claims section on a swarm's page: each context's live claims, recent done keys and
//! settings.
use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use hydra_swarms::claims::{ContextSettings, Holder, HolderKind, valid_context_name};
use hydra_swarms::{ClaimView, Command, ContextView, SwarmView};
use jiff::tz::TimeZone;

use super::Workspace;
use super::swarms_page::{section_label, when};

/// The least `keep_done` the service accepts, in seconds.
const LEAST_KEEP_DONE: u64 = 60;
/// The service stores times in microseconds; the page reads seconds.
const MICROS: u64 = 1_000_000;

/// One of the four settings an owner can change: what its button says, and how to read a
/// typed value into the settings.
struct Setting {
    button: &'static str,
    title: &'static str,
    placeholder: &'static str,
    show: fn(&ContextSettings) -> String,
    apply: fn(ContextSettings, &str) -> Result<ContextSettings, String>,
}

const SETTINGS: [Setting; 4] = [
    Setting {
        button: "Renew every…",
        title: "How often a holder renews, in seconds",
        placeholder: "Seconds, like 60",
        show: |s| s.renew_secs.to_string(),
        apply: |s, typed| {
            Ok(ContextSettings {
                renew_secs: parse_count(typed)?,
                ..s
            })
        },
    },
    Setting {
        button: "Lapse after…",
        title: "How many renewals a holder may miss",
        placeholder: "A number, like 3",
        show: |s| s.lapse_after.to_string(),
        apply: |s, typed| {
            Ok(ContextSettings {
                lapse_after: parse_count(typed)?,
                ..s
            })
        },
    },
    Setting {
        button: "Per machine…",
        title: "How many claims one machine may hold",
        placeholder: "1 or more, -1 for no limit, 0 to pause",
        show: |s| s.max_per_machine.to_string(),
        apply: |s, typed| {
            Ok(ContextSettings {
                max_per_machine: parse_limit(typed)?,
                ..s
            })
        },
    },
    Setting {
        button: "Keep done…",
        title: "How long done keys are kept",
        placeholder: "Like 30d, 12h or 90m",
        show: |s| {
            let secs = s.keep_done_secs;
            if secs.is_multiple_of(86_400) {
                format!("{}d", secs / 86_400)
            } else if secs.is_multiple_of(3_600) {
                format!("{}h", secs / 3_600)
            } else {
                format!("{}m", secs / 60)
            }
        },
        apply: |s, typed| {
            Ok(ContextSettings {
                keep_done_secs: parse_keep_done(typed)?,
                ..s
            })
        },
    },
];

impl Workspace {
    /// The Claims section: a block per context. Owners also get the settings and
    /// `+ New context`; for everyone else a swarm with no contexts has no section at all.
    pub(super) fn swarm_claims(
        &self,
        swarm: &SwarmView,
        owner: bool,
        tz: &TimeZone,
        now: u64,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if swarm.claims.is_empty() && !owner {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let new_context = owner.then(|| {
            let id = swarm.id.clone();
            let taken: Vec<String> = swarm.claims.iter().map(|c| c.name.clone()).collect();
            Button::new("claims-new-context")
                .label("+ New context")
                .ghost()
                .small()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    let (id, taken) = (id.clone(), taken.clone());
                    ws.text_dialog(
                        "New context".into(),
                        String::new(),
                        "A name, like inbox-support",
                        move |ws, typed, _, _| {
                            if !valid_context_name(&typed) {
                                return Err("Use 1 to 64 of a-z, 0-9, - and _.".into());
                            }
                            // Setting an existing context would put its settings back to the
                            // defaults.
                            if taken.contains(&typed) {
                                return Err("That context is already here.".into());
                            }
                            ws.swarms_command(Command::ClaimContextSet {
                                swarm: id.clone(),
                                name: typed,
                                settings: ContextSettings::default(),
                            });
                            Ok(())
                        },
                        window,
                        cx,
                    )
                }))
        });
        let blocks: Vec<AnyElement> = swarm
            .claims
            .iter()
            .map(|c| self.claims_context(swarm, c, owner, tz, now, cx))
            .collect();
        Some(
            v_flex()
                .gap_3()
                .child(section_label("CLAIMS", cx))
                .when(swarm.claims.is_empty(), |d| {
                    d.child(div().text_sm().text_color(muted).child(
                        "No contexts yet. A context names one kind of job, such as inbox-support.",
                    ))
                })
                .children(blocks)
                .when_some(new_context, |d, b| d.child(h_flex().child(b)))
                .into_any_element(),
        )
    }

    fn claims_context(
        &self,
        swarm: &SwarmView,
        context: &ContextView,
        owner: bool,
        tz: &TimeZone,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
        let name = context.name.clone();
        let live: Vec<AnyElement> = context
            .live
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let release = can_release(c, owner).then(|| {
                    let (id, ctx, claim) = (swarm.id.clone(), name.clone(), c.clone());
                    Button::new(SharedString::from(format!("claim-release-{name}-{i}")))
                        .label("Release")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.claim_release(id.clone(), ctx.clone(), claim.clone(), window, cx)
                        }))
                });
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(div().font_family("monospace").child(c.key.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .child(claim_line(c, now)),
                    )
                    .when_some(release, |d, b| d.child(b))
                    .into_any_element()
            })
            .collect();
        let done: Vec<AnyElement> = context
            .done
            .iter()
            .map(|d| {
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(format!(
                        "{} · {} · {}",
                        d.key,
                        d.person,
                        when(d.at / MICROS, now, tz)
                    ))
                    .into_any_element()
            })
            .collect();
        let settings = owner.then(|| {
            let buttons: Vec<Button> = SETTINGS
                .iter()
                .enumerate()
                .map(|(i, setting)| {
                    let (id, ctx, current) = (swarm.id.clone(), name.clone(), context.settings);
                    Button::new(SharedString::from(format!("claim-set-{name}-{i}")))
                        .label(setting.button)
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            let (id, ctx) = (id.clone(), ctx.clone());
                            ws.text_dialog(
                                format!("{} ({ctx})", setting.title),
                                (setting.show)(&current),
                                setting.placeholder,
                                move |ws, typed, _, _| {
                                    let settings = (setting.apply)(current, &typed)?;
                                    ws.swarms_command(Command::ClaimContextSet {
                                        swarm: id.clone(),
                                        name: ctx.clone(),
                                        settings,
                                    });
                                    Ok(())
                                },
                                window,
                                cx,
                            )
                        }))
                })
                .collect();
            h_flex()
                .gap_1()
                .flex_wrap()
                .child(div().text_sm().text_color(muted).child("Change settings…"))
                .children(buttons)
        });
        v_flex()
            .gap_1()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .font_family("monospace")
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(settings_line(&context.settings)),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(
                "Keep done keys longer than anything rescanning this context looks back, or an old item can be claimed again.",
            ))
            .when_some(settings, |d, s| d.child(s))
            .child(v_flex().gap_1().pt_1().children(live))
            .when(!done.is_empty(), |d| {
                d.child(
                    v_flex()
                        .gap_1()
                        .pt_1()
                        .child(div().text_xs().text_color(muted).child("Recent done"))
                        .children(done),
                )
            })
            .into_any_element()
    }

    /// Frees a claim: your own at once, someone else's after asking. A refusal comes back as
    /// a notification.
    fn claim_release(
        &mut self,
        swarm: String,
        context: String,
        claim: ClaimView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if claim.mine {
            return self.claim_release_now(swarm, context, claim.key, claim.author, window, cx);
        }
        let title = format!("Release {}?", claim.key);
        let body = format!(
            "Release {}? {}'s work on it will be told to stop.",
            claim.key, claim.person
        );
        self.swarms_confirm(
            title,
            body,
            "Release",
            true,
            move |ws, window, cx| {
                ws.claim_release_now(
                    swarm.clone(),
                    context.clone(),
                    claim.key.clone(),
                    claim.author.clone(),
                    window,
                    cx,
                )
            },
            window,
            cx,
        );
    }

    fn claim_release_now(
        &mut self,
        swarm: String,
        context: String,
        key: String,
        author: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (reply, answer) = oneshot::channel();
        self.swarms_command(Command::ClaimRelease {
            swarm,
            context,
            key,
            author,
            reply,
        });
        cx.spawn_in(window, async move |this, cx| {
            let problem = match answer.await {
                Ok(Ok(())) => return,
                Ok(Err(e)) => e,
                Err(_) => "The swarms stopped before the release was made.".to_string(),
            };
            let _ = this.update_in(cx, |_, window, cx| {
                window.push_notification(Notification::error(problem).title("Claims"), cx)
            });
        })
        .detach();
    }
}

/// Your own claims, and anyone's for an owner.
fn can_release(claim: &ClaimView, owner: bool) -> bool {
    claim.mine || owner
}

/// "Olive · olive-pc · terminal api · renewed 12 s ago". `now` is in seconds.
fn claim_line(claim: &ClaimView, now: u64) -> String {
    let secs = now.saturating_sub(claim.renewed_at / MICROS);
    let ago = match secs {
        0..60 => format!("{secs} s ago"),
        60..3600 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    };
    format!(
        "{} · {} · {} · renewed {ago}",
        claim.person,
        claim.machine,
        holder_word(&claim.holder)
    )
}

/// "terminal api" or "pipeline run INTH-9".
fn holder_word(h: &Holder) -> String {
    match h.kind {
        HolderKind::Session => format!("terminal {}", h.label),
        HolderKind::PipelineRun => format!("pipeline run {}", h.label),
    }
}

/// "30d", "12h" or "90m" as seconds; at least a minute.
fn parse_keep_done(typed: &str) -> Result<u64, String> {
    let say = || "Say how long, like 30d, 12h or 90m. The least is 1m.".to_string();
    let typed = typed.trim();
    let unit = match typed.chars().last() {
        Some('d') => 86_400,
        Some('h') => 3_600,
        Some('m') => 60,
        _ => return Err(say()),
    };
    let n: u64 = typed[..typed.len() - 1].parse().map_err(|_| say())?;
    match n.checked_mul(unit) {
        Some(secs) if secs >= LEAST_KEEP_DONE => Ok(secs),
        _ => Err(say()),
    }
}

/// 2_592_000 as "30 days": the biggest unit that divides it.
fn show_keep_done(secs: u64) -> String {
    let (n, unit) = if secs.is_multiple_of(86_400) {
        (secs / 86_400, "day")
    } else if secs.is_multiple_of(3_600) {
        (secs / 3_600, "hour")
    } else {
        (secs / 60, "minute")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// -1 and up: a limit, -1 for none, 0 to pause.
fn parse_limit(typed: &str) -> Result<i32, String> {
    typed
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|n| *n >= -1)
        .ok_or_else(|| "A number: 1 or more, -1 for no limit, 0 to pause.".into())
}

/// A whole number of 1 or more.
fn parse_count(typed: &str) -> Result<u32, String> {
    typed
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| "A whole number, 1 or more.".into())
}

/// "Renew every 60 s · lapse after 3 missed · 1 per machine · keep done 30 days".
fn settings_line(s: &ContextSettings) -> String {
    let per_machine = match s.max_per_machine {
        -1 => "no limit per machine".to_string(),
        0 => "paused".to_string(),
        n => format!("{n} per machine"),
    };
    format!(
        "Renew every {} s · lapse after {} missed · {per_machine} · keep done {}",
        s.renew_secs,
        s.lapse_after,
        show_keep_done(s.keep_done_secs)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydra_swarms::ClaimView;
    use hydra_swarms::claims::{ContextSettings, Holder, HolderKind};

    fn claim(mine: bool) -> ClaimView {
        ClaimView {
            key: "msg-1".into(),
            author: "author-1".into(),
            person: "Olive".into(),
            machine: "olive-pc".into(),
            // 100 s past the epoch, in microseconds.
            renewed_at: 100_000_000,
            holder: Holder {
                kind: HolderKind::Session,
                label: "api".into(),
            },
            mine,
        }
    }

    #[test]
    fn your_own_claims_and_owners_get_a_release_button() {
        assert!(can_release(&claim(true), false));
        assert!(can_release(&claim(true), true));
        assert!(can_release(&claim(false), true));
        assert!(!can_release(&claim(false), false));
    }

    #[test]
    fn a_claim_reads_who_where_what_and_when() {
        assert_eq!(
            claim_line(&claim(false), 112),
            "Olive · olive-pc · terminal api · renewed 12 s ago"
        );
        let run = Holder {
            kind: HolderKind::PipelineRun,
            label: "INTH-9".into(),
        };
        assert_eq!(holder_word(&run), "pipeline run INTH-9");
    }

    #[test]
    fn keep_done_reads_days_hours_and_minutes() {
        assert_eq!(parse_keep_done("30d"), Ok(2_592_000));
        assert_eq!(parse_keep_done("12h"), Ok(43_200));
        assert_eq!(parse_keep_done("90m"), Ok(5_400));
        assert_eq!(parse_keep_done("1m"), Ok(60));
        let say = "Say how long, like 30d, 12h or 90m. The least is 1m.";
        assert_eq!(parse_keep_done("0m"), Err(say.into()));
        assert_eq!(parse_keep_done("x"), Err(say.into()));
        assert_eq!(parse_keep_done("30"), Err(say.into()));
        assert_eq!(show_keep_done(2_592_000), "30 days");
        assert_eq!(show_keep_done(86_400), "1 day");
        assert_eq!(show_keep_done(43_200), "12 hours");
        assert_eq!(show_keep_done(5_400), "90 minutes");
        assert_eq!(show_keep_done(60), "1 minute");
        for secs in [2_592_000, 43_200, 5_400, 60] {
            let typed = match secs {
                2_592_000 => "30d",
                43_200 => "12h",
                5_400 => "90m",
                _ => "1m",
            };
            assert_eq!(parse_keep_done(typed), Ok(secs));
        }
    }

    #[test]
    fn the_limit_is_a_number_of_minus_one_or_more() {
        assert_eq!(parse_limit("3"), Ok(3));
        assert_eq!(parse_limit("-1"), Ok(-1));
        assert_eq!(parse_limit("0"), Ok(0));
        let say = "A number: 1 or more, -1 for no limit, 0 to pause.";
        assert_eq!(parse_limit("-2"), Err(say.into()));
        assert_eq!(parse_limit("a"), Err(say.into()));
    }

    #[test]
    fn settings_read_as_a_sentence() {
        let mut s = ContextSettings::default();
        assert_eq!(
            settings_line(&s),
            "Renew every 60 s · lapse after 3 missed · 1 per machine · keep done 30 days"
        );
        s.max_per_machine = -1;
        assert!(settings_line(&s).contains("· no limit per machine ·"));
        s.max_per_machine = 0;
        assert!(settings_line(&s).contains("· paused ·"));
    }
}
