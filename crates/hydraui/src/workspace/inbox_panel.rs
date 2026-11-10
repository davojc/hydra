//! The bell (core spec §57): every notice hydraui gives, kept in `crate::inbox`, in a panel
//! that drops down from the tab strip instead of toasts over the work.

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, deferred, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Sizable, h_flex, v_flex};

use super::Workspace;
use crate::inbox::{Inbox, Kind};
use crate::orchestrator_theme::{self as theme, color};

/// The panel while it is open: how many notices were unread when it opened, shown as new.
#[derive(Debug, Clone, Copy, Default)]
pub struct InboxPanel {
    pub new: usize,
}

fn kind_color(kind: Kind) -> u32 {
    match kind {
        Kind::Info => theme::TEXT_MUTED,
        Kind::Success => theme::GREEN,
        Kind::Warning => theme::AMBER,
        Kind::Error => theme::RED,
    }
}

impl Workspace {
    /// The bell, right of History: its unread count, amber while one is a warning or error.
    pub(super) fn inbox_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let (unread, urgent) = cx
            .try_global::<Inbox>()
            .map_or((0, false), |i| (i.unread(), i.urgent()));
        let open = self.inbox.is_some();
        let tone = if urgent {
            theme::AMBER
        } else if open {
            theme::TEAL
        } else {
            theme::TEXT
        };
        h_flex()
            .id("open-inbox")
            .flex_none()
            .gap_1p5()
            .items_center()
            .h(px(26.))
            .px_2p5()
            .mr_1()
            .rounded_md()
            .border_1()
            .text_sm()
            .cursor_pointer()
            .border_color(color(if open || urgent {
                tone
            } else {
                theme::BORDER_STRONG
            }))
            .bg(color(if open {
                theme::TEAL_TINT
            } else if urgent {
                theme::AMBER_TINT
            } else {
                theme::RAISED
            }))
            .text_color(color(tone))
            .hover(|s| s.border_color(color(theme::TEAL)))
            .child("🔔")
            .when(unread > 0, |d| d.child(unread.to_string()))
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("Notifications").build(window, cx)
            })
            .on_click(cx.listener(|ws, _, _, cx| ws.toggle_inbox(cx)))
            .into_any_element()
    }

    /// Opens the panel, marking everything read (what was unread shows as new while it's open),
    /// or closes it.
    pub(super) fn toggle_inbox(&mut self, cx: &mut Context<Self>) {
        if self.inbox.take().is_none() {
            let inbox = cx.default_global::<Inbox>();
            let new = inbox.unread();
            inbox.mark_all_read();
            self.inbox = Some(InboxPanel { new });
        }
        cx.notify();
    }

    fn clear_inbox(&mut self, cx: &mut Context<Self>) {
        cx.default_global::<Inbox>().entries.clear();
        if let Some(p) = &mut self.inbox {
            p.new = 0;
        }
        cx.notify();
    }

    /// The panel under the bell, over everything, with a backdrop that closes it.
    pub(super) fn render_inbox_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.inbox?;
        let entries = cx
            .try_global::<Inbox>()
            .map(|i| i.entries.clone())
            .unwrap_or_default();
        let today = crate::action_log::now();
        let rows = entries.into_iter().enumerate().map(|(n, e)| {
            let new = n < panel.new;
            let click = e.note.on_click.clone();
            let clickable = click.is_some();
            let when = format!(
                "{} {}",
                crate::run_feed::day_heading(&e.at, &today),
                crate::run_feed::clock(&e.at)
            );
            h_flex()
                .id(SharedString::from(format!("inbox-row-{n}")))
                .gap_2p5()
                .items_start()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(color(theme::BORDER_SUBTLE))
                .when(new, |d| d.bg(color(theme::RAISED)))
                .when(clickable, |d| {
                    d.cursor_pointer().hover(|d| d.bg(color(theme::TEAL_TINT)))
                })
                .child(
                    div()
                        .pt(px(3.))
                        .flex_none()
                        .text_xs()
                        .text_color(color(kind_color(e.note.kind)))
                        .child("●"),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_between()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .text_color(color(theme::TEXT))
                                        .child(e.note.title.clone().unwrap_or_default()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(color(theme::TEXT_MUTED))
                                        .child(when),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .line_clamp(4)
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(e.note.message.clone()),
                        ),
                )
                .on_click(cx.listener(move |ws, ev, window, cx| {
                    if let Some(f) = &click {
                        ws.inbox = None;
                        cx.notify();
                        f(ev, window, cx);
                    }
                }))
        });
        let empty = panel_empty(
            cx.try_global::<Inbox>()
                .is_none_or(|i| i.entries.is_empty()),
        );
        let header = h_flex()
            .justify_between()
            .items_center()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(color(theme::BORDER))
            .child(
                div()
                    .text_sm()
                    .text_color(color(theme::TEXT))
                    .child("Notifications"),
            )
            .child(
                Button::new("inbox-clear")
                    .label("Clear")
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(|ws, _, _, cx| ws.clear_inbox(cx))),
            );
        let list = v_flex()
            .id("inbox-panel")
            .absolute()
            .top(px(crate::ENV_STRIP_HEIGHT + 34. + 4.))
            .right(px(12.))
            .w(px(420.))
            .max_h(px(560.))
            .rounded_lg()
            .border_1()
            .border_color(color(theme::BORDER_STRONG))
            .bg(color(theme::TOAST))
            .shadow_lg()
            .occlude()
            .child(header)
            .child(
                v_flex()
                    .id("inbox-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .children(empty),
            );
        let backdrop = div()
            .id("inbox-backdrop")
            .absolute()
            .inset_0()
            .on_click(cx.listener(|ws, _, _, cx| {
                ws.inbox = None;
                cx.notify();
            }));
        Some(
            deferred(div().absolute().inset_0().child(backdrop).child(list))
                .with_priority(1)
                .into_any_element(),
        )
    }
}

fn panel_empty(empty: bool) -> Option<impl IntoElement> {
    empty.then(|| {
        div()
            .px_3()
            .py_4()
            .text_sm()
            .text_color(color(theme::TEXT_MUTED))
            .child("No notifications")
    })
}
