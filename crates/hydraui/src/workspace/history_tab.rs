//! History (core spec §50): one timeline of what every pipeline did, for catching up after time
//! away. Opened by the History button beside Projects; read from the run logs by
//! `crate::run_feed`.

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Selectable, Sizable, h_flex, v_flex};

use super::Workspace;
use crate::orchestrator_theme::{self as theme, color};
use crate::run_feed::{self, Item, Tone};

/// How far back the timeline goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    /// Since History was last opened, at that log time.
    SinceLastLook(String),
    Hours(u64),
    Today,
}

impl Span {
    fn label(&self) -> String {
        match self {
            Span::SinceLastLook(_) => "Since you last looked".into(),
            Span::Hours(h) => format!("Last {h} hours"),
            Span::Today => "Today".into(),
        }
    }

    /// The log time it starts at, given the log time now.
    fn start(&self, now: &str) -> String {
        match self {
            Span::SinceLastLook(at) => at.clone(),
            Span::Hours(h) => run_feed::before(now, h * 3_600).unwrap_or_default(),
            Span::Today => format!("{} 00:00:00.000", now.split(' ').next().unwrap_or(now)),
        }
    }
}

pub struct HistoryTab {
    pub span: Span,
    /// When History was opened before this time, if ever.
    pub last_look: Option<String>,
    pub items: Vec<Item>,
    /// The log time the items were read at.
    pub read_at: String,
    pub loading: bool,
}

impl Workspace {
    pub(super) fn history_shown(&self) -> bool {
        self.history.is_some()
    }

    /// The History button, right of Projects.
    pub(super) fn history_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.history.is_some();
        h_flex()
            .id("open-history")
            .flex_none()
            .gap_2()
            .items_center()
            .h(px(26.))
            .px_3()
            .mr_1()
            .rounded_md()
            .border_1()
            .text_sm()
            .cursor_pointer()
            .border_color(color(if open {
                theme::TEAL
            } else {
                theme::BORDER_STRONG
            }))
            .bg(color(if open {
                theme::TEAL_TINT
            } else {
                theme::RAISED
            }))
            .text_color(color(if open { theme::TEAL } else { theme::TEXT }))
            .hover(|s| s.border_color(color(theme::TEAL)))
            .child("History")
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("What every pipeline did while you were away")
                    .build(window, cx)
            })
            .on_click(cx.listener(|ws, _, window, cx| {
                if ws.history_shown() {
                    ws.close_history(window, cx)
                } else {
                    ws.open_history(window, cx)
                }
            }))
            .into_any_element()
    }

    /// Opens History at "since you last looked" (the last 2 hours the first time), and
    /// remembers now as the last look, in config.toml as `history_seen`.
    pub(super) fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.projects = None;
        let now = crate::action_log::now();
        let last_look = self.ui.history_seen.clone();
        self.history = Some(HistoryTab {
            span: match &last_look {
                Some(at) => Span::SinceLastLook(at.clone()),
                None => Span::Hours(2),
            },
            last_look,
            items: Vec::new(),
            read_at: now.clone(),
            loading: true,
        });
        self.ui.history_seen = Some(now.clone());
        let _ = crate::config::set(&self.hydra.paths, "history_seen", Some(now.into()));
        self.reload_history(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn set_history_span(&mut self, span: Span, cx: &mut Context<Self>) {
        if let Some(h) = &mut self.history {
            h.span = span;
        }
        self.reload_history(cx);
    }

    /// Reads the run logs again in the background for the span shown.
    fn reload_history(&mut self, cx: &mut Context<Self>) {
        let Some(h) = &mut self.history else {
            return;
        };
        let now = crate::action_log::now();
        let start = h.span.start(&now);
        // Logs last written before the span can't hold anything in it.
        let back = crate::run_history::ago(&start, &now).unwrap_or_default();
        let oldest = std::time::SystemTime::now()
            .checked_sub(back + std::time::Duration::from_secs(60))
            .unwrap_or(std::time::UNIX_EPOCH);
        h.loading = true;
        let root = self.hydra.paths.root().to_path_buf();
        cx.spawn(async move |this, cx| {
            let items = cx
                .background_spawn(async move { run_feed::read(&root, &start, oldest) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(h) = &mut ws.history {
                    h.items = items;
                    h.read_at = now;
                    h.loading = false;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_history_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(h) = &self.history else {
            return div().into_any_element();
        };
        let mut spans = Vec::new();
        if let Some(at) = &h.last_look {
            spans.push(Span::SinceLastLook(at.clone()));
        }
        spans.extend([Span::Hours(2), Span::Hours(8), Span::Today, Span::Hours(48)]);
        let chips = h_flex()
            .gap_1p5()
            .flex_wrap()
            .children(spans.into_iter().enumerate().map(|(i, span)| {
                let on = span == h.span;
                Button::new(SharedString::from(format!("history-span-{i}")))
                    .label(span.label())
                    .small()
                    .outline()
                    .selected(on)
                    .on_click(
                        cx.listener(move |ws, _, _, cx| ws.set_history_span(span.clone(), cx)),
                    )
            }));
        let since = match &h.span {
            Span::SinceLastLook(at) => format!(
                "Since {} {}",
                run_feed::day_heading(at, &h.read_at),
                run_feed::clock(at)
            ),
            other => other.label(),
        };
        let attention = h.items.iter().filter(|i| i.tone == Tone::Attention).count();
        let finished = h.items.iter().filter(|i| i.text == "Done").count();
        let failed = h.items.iter().filter(|i| i.tone == Tone::Bad).count();
        let summary = if h.loading {
            "Reading the run logs…".to_string()
        } else if h.items.is_empty() {
            "Nothing happened in this time.".to_string()
        } else {
            let tickets = {
                let mut t: Vec<&str> = h.items.iter().map(|i| i.run.as_str()).collect();
                t.sort_unstable();
                t.dedup();
                t.len()
            };
            format!(
                "{tickets} ticket{} active · {finished} finished · {failed} failed or stopped · {attention} asked for a person",
                if tickets == 1 { "" } else { "s" }
            )
        };
        let mut list = v_flex().gap_0p5();
        let mut day = String::new();
        for (n, item) in h.items.iter().enumerate() {
            let heading = run_feed::day_heading(&item.at, &h.read_at);
            if heading != day {
                list = list.child(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_xs()
                        .text_color(color(theme::TEXT_MUTED))
                        .child(heading.clone()),
                );
                day = heading;
            }
            list = list.child(self.history_row(n, item, cx));
        }
        let header = h_flex()
            .justify_between()
            .items_center()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xl()
                            .text_color(color(theme::TEXT))
                            .child("History"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(format!("{since}: {summary}")),
                    ),
            )
            .child(
                Button::new("history-refresh")
                    .label("Refresh")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|ws, _, _, cx| ws.reload_history(cx))),
            );
        div()
            .id("history-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_5()
            .child(
                v_flex()
                    .max_w(px(980.))
                    .gap_3()
                    .child(header)
                    .child(chips)
                    .child(list),
            )
            .into_any_element()
    }

    /// One line: the time, a dot in its tone, the ticket and what happened, its pipeline. A click
    /// opens the ticket or pull request.
    fn history_row(&self, n: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let tone = match item.tone {
            Tone::Good => theme::GREEN,
            Tone::Bad => theme::RED,
            Tone::Attention => theme::AMBER,
            Tone::Neutral => theme::TEXT_MUTED,
        };
        let ticket = crate::pipeline_card::ticket_title(&item.ticket, item.link.as_deref());
        let title = item.title.clone();
        let link = item.link.clone();
        h_flex()
            .id(SharedString::from(format!("history-row-{n}")))
            .gap_3()
            .px_2()
            .py_1()
            .items_center()
            .rounded_md()
            .hover(|d| d.bg(color(theme::RAISED)))
            .when(link.is_some(), |d| d.cursor_pointer())
            .when(!title.is_empty(), |d| {
                d.tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                })
            })
            .child(
                div()
                    .w(px(44.))
                    .flex_none()
                    .text_xs()
                    .text_color(color(theme::TEXT_MUTED))
                    .child(run_feed::clock(&item.at).to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(tone))
                    .child("●"),
            )
            .child(
                div()
                    .w(px(170.))
                    .flex_none()
                    .truncate()
                    .text_sm()
                    .text_color(color(theme::TEXT))
                    .child(ticket),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(color(if item.tone == Tone::Neutral {
                        theme::TEXT_SECONDARY
                    } else {
                        tone
                    }))
                    .child(item.text.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(180.))
                    .truncate()
                    .text_xs()
                    .text_color(color(theme::TEXT_MUTED))
                    .child(item.pipeline.clone()),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(url) = &link {
                    cx.open_url(url);
                }
            }))
            .into_any_element()
    }
}
