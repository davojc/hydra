//! The start page (spec §7.12): recent layouts with a filter, New window, Open layout file….
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, Focusable, KeyDownEvent, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::start_page::{Row, rows};

pub(super) struct StartPage {
    input: Entity<InputState>,
    selected: usize,
    _sub: Subscription,
}

impl Workspace {
    pub(super) fn show_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter recent layouts"));
        let sub = cx.subscribe_in(
            &input,
            window,
            |ws, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    if let Some(p) = ws.start.as_mut() {
                        p.selected = 0;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => ws.choose_start_row(window, cx),
                _ => {}
            },
        );
        input.update(cx, |i, cx| i.focus(window, cx));
        self.start = Some(StartPage {
            input,
            selected: 0,
            _sub: sub,
        });
        cx.notify();
    }

    /// While the page shows and nothing (or only the window itself) has the keyboard, the
    /// filter takes it: a focus asked for at startup doesn't survive the window's first
    /// activation.
    pub(super) fn keep_start_focus(&self, window: &mut Window, cx: &mut App) {
        let Some(page) = &self.start else { return };
        let handle = page.input.read(cx).focus_handle(cx);
        let idle = match window.focused(cx) {
            None => true,
            Some(f) => f == self.focus,
        };
        if idle {
            window.focus(&handle, cx);
        }
    }

    fn start_rows(&self, cx: &App) -> Vec<Row> {
        let filter = self
            .start
            .as_ref()
            .map(|p| p.input.read(cx).value().to_string())
            .unwrap_or_default();
        rows(
            &hydraui_sets::read_recent(&self.hydra.paths),
            &filter,
            &self.hydra.user_home,
            crate::window_layout::now_secs(),
            &crate::claim::is_claimed,
        )
    }

    /// Enter: the selected row, or New window when nothing is listed.
    fn choose_start_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.start_rows(cx);
        let i = self.start.as_ref().map_or(0, |p| p.selected);
        match rows.get(i).cloned() {
            None => self.start_new_window(window, cx),
            Some(row) => self.open_start_row(row, window, cx),
        }
    }

    /// A row: its layout here, or its window brought forward; a missing file leaves the list.
    fn open_start_row(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        if !row.path.exists() {
            let _ = hydraui_sets::forget_recent(&self.hydra.paths, &row.path);
            window.push_notification(
                Notification::warning(format!("{} no longer exists", row.path.display()))
                    .title("Recent layouts"),
                cx,
            );
            cx.notify();
            return;
        }
        self.open_layout_from(row.path, true, window, cx);
    }

    /// New window (env-tabs design §3): untitled, with one environment's tab and terminal; the
    /// environment is the home folder's binding, else `default_env`, else the one picked.
    fn start_new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_start_page(window, cx);
        let home = self.hydra.user_home.clone();
        match self.hydra.adhoc_env(&home, self.ui.default_env.as_deref()) {
            Some(env) => self.add_env(&env, window, cx),
            None => self.pick_env(window, cx),
        }
    }

    fn start_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.start_rows(cx).len();
        let Some(page) = self.start.as_mut() else {
            return;
        };
        match ev.keystroke.key.as_str() {
            "down" if count > 0 => page.selected = (page.selected + 1).min(count - 1),
            "up" => page.selected = page.selected.saturating_sub(1),
            "escape" => page.input.update(cx, |i, cx| i.set_value("", window, cx)),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_start_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = &self.start else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let (muted, accent, border) = (theme.muted_foreground, theme.accent, theme.border);
        let rows = self.start_rows(cx);
        let empty = rows.is_empty();
        let filtered = !page.input.read(cx).value().is_empty();
        let list = v_flex()
            .gap_1()
            .children(rows.into_iter().enumerate().map(|(i, row)| {
                let chosen = row.clone();
                h_flex()
                    .id(("recent", i))
                    .px_3()
                    .py_2()
                    .gap_3()
                    .rounded_md()
                    .cursor_pointer()
                    .when(i == page.selected, |d| d.bg(accent))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.open_start_row(chosen.clone(), window, cx)
                    }))
                    .child(div().min_w(px(160.)).child(row.name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .child(row.folder),
                    )
                    .child(div().text_sm().text_color(muted).child(if row.elsewhere {
                        "open in another window".to_string()
                    } else {
                        row.ago
                    }))
            }))
            .when(empty, |l| {
                l.child(div().p_3().text_color(muted).child(if filtered {
                    "Nothing matches. Enter opens a new window."
                } else {
                    "No recent layouts. Enter opens a new window."
                }))
            });
        div()
            .size_full()
            .flex()
            .justify_center()
            .pt(px(64.))
            .capture_key_down(cx.listener(Self::start_key))
            .child(
                v_flex()
                    .w(px(560.))
                    .gap_3()
                    .child(div().text_xl().child("Recent layouts"))
                    .child(Input::new(&page.input))
                    .child(
                        div()
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .p_1()
                            .min_h(px(120.))
                            .child(list),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("start-new")
                                    .label("New window")
                                    .primary()
                                    .small()
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.start_new_window(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("start-open")
                                    .label("Open layout file…")
                                    .small()
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.open_layout_file(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
