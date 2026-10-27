//! Ticket history (orchestrator spec §7.5; the design's View 3), in the Pipeline tab: every
//! run from the run logs, the live one pinned on top, filtered by outcome; a row expands to
//! its stages' scores and the note.
use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px};
use gpui_component::h_flex;
use gpui_component::v_flex;

use super::Workspace;
use super::pipeline_tab::TabView;
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::run_history::{Outcome, read, took_text};

/// The history's filter chips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum HistoryFilter {
    #[default]
    All,
    Done,
    Stopped,
}

fn text(size: f32, hex: u32) -> gpui::Div {
    div().text_size(px(size)).text_color(color(hex))
}

impl Workspace {
    /// The Ticket history view for orchestrator `orch`.
    pub(super) fn render_history(&self, orch: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.pipeline_tabs.get(&orch).cloned().unwrap_or_default();
        let live: Vec<String> = server::get()
            .map(|sv| {
                server::lock(sv.registry())
                    .runs
                    .all()
                    .filter(|r| r.state.open())
                    .map(|r| r.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let entries = read(self.hydra.paths.root(), &live);
        let shown: Vec<_> = entries
            .iter()
            .filter(|e| match tab.history_filter {
                HistoryFilter::All => true,
                HistoryFilter::Done => e.outcome == Outcome::Done,
                HistoryFilter::Stopped => e.outcome == Outcome::Stopped,
            })
            .collect();
        let chip = |label: &'static str, filter: HistoryFilter| {
            let on = tab.history_filter == filter;
            div()
                .id(SharedString::from(format!("history-filter-{orch}-{label}")))
                .px(px(10.))
                .py(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(color(if on {
                    theme::TEAL_BORDER
                } else {
                    theme::BORDER
                }))
                .when(on, |d| d.bg(color(theme::TEAL_TINT)))
                .cursor_pointer()
                .child(
                    text(
                        12.5,
                        if on {
                            theme::TEAL
                        } else {
                            theme::TEXT_SECONDARY
                        },
                    )
                    .child(label),
                )
                .on_click(cx.listener(move |ws, _, _, cx| {
                    if let Some(t) = ws.pipeline_tabs.get_mut(&orch) {
                        t.history_filter = filter;
                    }
                    cx.notify();
                }))
        };
        let header = |label: &str| text(11., theme::TEXT_MUTED).child(label.to_uppercase());
        let columns = |a, b, c, d, e, f| {
            h_flex()
                .gap_3()
                .child(div().w(px(80.)).flex_none().child(a))
                .child(div().flex_1().min_w(px(120.)).child(b))
                .child(div().w(px(100.)).flex_none().child(c))
                .child(div().flex_1().min_w(px(140.)).child(d))
                .child(div().w(px(64.)).flex_none().child(e))
                .child(div().w(px(60.)).flex_none().child(f))
        };
        let mut rows = v_flex().child(div().px(px(14.)).pb_2().child(columns(
            header("Ticket"),
            header("Title"),
            header("Outcome"),
            header("Detail"),
            header("Time"),
            header("SLA"),
        )));
        if shown.is_empty() {
            rows = rows.child(
                div()
                    .px(px(14.))
                    .py(px(20.))
                    .border_t_1()
                    .border_color(color(theme::BORDER_SUBTLE))
                    .child(text(12.5, theme::TEXT_MUTED).child("No tickets yet.")),
            );
        }
        for e in shown {
            let (fg, bg) = match e.outcome {
                Outcome::Done => (theme::GREEN, theme::GREEN_TINT),
                Outcome::Stopped => (theme::RED, theme::RED_TINT),
                Outcome::InProgress => (theme::TEAL, theme::TEAL_TINT),
            };
            let open = tab.open_row.as_deref() == Some(e.run.as_str());
            let (run, live_row) = (e.run.clone(), e.outcome == Outcome::InProgress);
            let pill = div()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .bg(color(bg))
                .text_size(px(11.5))
                .text_color(color(fg))
                .child(e.outcome.word());
            let mut row = v_flex()
                .id(SharedString::from(format!("history-row-{orch}-{}", e.run)))
                .px(px(14.))
                .py(px(13.))
                .gap_2()
                .border_t_1()
                .border_color(color(theme::BORDER_SUBTLE))
                .hover(|d| d.bg(color(0x141717)))
                .cursor_pointer()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    if live_row {
                        ws.show_pipeline_view(orch, TabView::Pipeline, window, cx);
                        return;
                    }
                    if let Some(t) = ws.pipeline_tabs.get_mut(&orch) {
                        t.open_row =
                            (t.open_row.as_deref() != Some(run.as_str())).then(|| run.clone());
                    }
                    cx.notify();
                }))
                .child(columns(
                    text(12.5, theme::TEXT_SECONDARY).child(e.ticket.clone()),
                    text(13., theme::TEXT).truncate().child(e.title.clone()),
                    h_flex().child(pill),
                    text(12., theme::TEXT_SECONDARY)
                        .truncate()
                        .child(e.detail.clone()),
                    text(12., theme::TEXT_SECONDARY).child(took_text(e.took)),
                    text(12., theme::TEXT_MUTED).child("–"),
                ));
            if open {
                let chips = e.stages.iter().map(|(stage, score)| {
                    h_flex()
                        .px(px(8.))
                        .py(px(3.))
                        .gap_1()
                        .rounded(px(5.))
                        .bg(color(theme::RAISED))
                        .child(text(11.5, theme::TEXT_SECONDARY).child(stage.clone()))
                        .child(
                            text(11.5, theme::TEXT)
                                .child(score.map_or("–".to_string(), |s| format!("{s}%"))),
                        )
                });
                row = row
                    .child(h_flex().pl(px(92.)).gap_2().flex_wrap().children(chips))
                    .children(e.note.clone().map(|n| {
                        text(12., theme::TEXT_SECONDARY)
                            .pl(px(92.))
                            .child(format!("Note: {n}"))
                    }));
            }
            rows = rows.child(row);
        }
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .child(
                div()
                    .id(SharedString::from(format!("history-{orch}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .px(px(36.))
                            .py(px(28.))
                            .gap(px(20.))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        text(20., theme::TEXT)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Ticket history"),
                                    )
                                    .child(div().flex_1())
                                    .child(chip("All", HistoryFilter::All))
                                    .child(chip("Done", HistoryFilter::Done))
                                    .child(chip("Stopped", HistoryFilter::Stopped)),
                            )
                            .child(rows),
                    ),
            )
            .into_any_element()
    }
}
