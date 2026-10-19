//! The terminal list on the left (env-tabs design §2.2): **+ New**, then the selected
//! environment's terminals in creation order, each with its activity dot, its name and × on
//! hover; click shows one, right-click renames, restarts or closes it.
use std::cell::Cell;

use gpui::prelude::*;
use gpui::{Context, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::pane_view::label_for;

impl Workspace {
    /// The selected environment's terminals, the shown one highlighted in the environment's
    /// colour; a row's tooltip is the title its program set, when it set one.
    pub(super) fn render_terminal_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let mut list = v_flex()
            .id("terminal-list")
            .w(px(self.list_width))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .p_1()
            .gap_0p5()
            .border_r_1()
            .border_color(border);
        let Some(tab) = self.envs.active() else {
            return list;
        };
        let (env, tint) = (tab.env.clone(), self.tint(&tab.env));
        list = list.child(
            Button::new("list-new-terminal")
                .label("+ New")
                .ghost()
                .small()
                .tooltip("New terminal (Ctrl+T)")
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.new_terminal(&env, None, window, cx);
                })),
        );
        for &id in &tab.terminals {
            let Some(view) = self.panes.get(&id) else {
                continue;
            };
            let v = view.read(cx);
            let (label, program) = (v.label(), v.program_title().map(str::to_string));
            let dot = self.render_tab_dot(self.activity.get(&id).copied(), tint, cx);
            // Keyed by the terminal, so element state stays with it when another closes.
            let group = SharedString::from(format!("terminal-row-{id}"));
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("terminal-{id}")))
                    .group(group.clone())
                    .px_2()
                    .py_1()
                    .gap_2()
                    .rounded_md()
                    .when(tab.shown == Some(id), |d| d.bg(tint.opacity(0.25)))
                    .hover(|d| d.bg(tint.opacity(0.15)))
                    .on_click(
                        cx.listener(move |ws, _, window, cx| ws.show_terminal(id, window, cx)),
                    )
                    .when_some(program, |d, title| {
                        d.tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                        })
                    })
                    .context_menu({
                        let ws = cx.entity();
                        move |menu, _, _| {
                            let (rename_ws, restart_ws, close_ws) =
                                (ws.clone(), ws.clone(), ws.clone());
                            menu.item(PopupMenuItem::new("Rename…").on_click(
                                move |_, window, cx| {
                                    rename_ws.update(cx, |ws, cx| ws.rename_dialog(id, window, cx));
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Restart").on_click(move |_, window, cx| {
                                    restart_ws
                                        .update(cx, |ws, cx| ws.restart_terminal(id, window, cx));
                                }),
                            )
                            .item(
                                PopupMenuItem::new("Close").on_click(move |_, window, cx| {
                                    close_ws.update(cx, |ws, cx| ws.close_terminal(id, window, cx));
                                }),
                            )
                        }
                    })
                    .child(dot)
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(label))
                    .child(
                        div()
                            .id(SharedString::from(format!("terminal-close-{id}")))
                            .px_1()
                            .text_color(muted)
                            .invisible()
                            .group_hover(group, |d| d.visible())
                            .hover(|d| d.text_color(tint))
                            .child("×")
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                cx.stop_propagation();
                                ws.close_terminal(id, window, cx);
                            })),
                    ),
            );
        }
        list
    }

    /// Asks for terminal `id`'s name (env-tabs design §2.2); Enter confirms, and an empty name
    /// goes back to the folder's name.
    pub(super) fn rename_dialog(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.panes.get(&id) else {
            return;
        };
        let (current, folder) = {
            let v = view.read(cx);
            (v.name().map(str::to_string), label_for(None, v.cwd()))
        };
        let input = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder(folder.clone());
            match &current {
                Some(name) => input.default_value(name.clone()),
                None => input,
            }
        });
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let typed = input.read(cx).value().to_string();
            let (ok_ws, ok_name) = (workspace.clone(), typed.clone());
            let (button_ws, button_name) = (workspace.clone(), typed);
            dialog
                .title("Rename terminal")
                .on_ok(move |_, window, cx| {
                    let name = ok_name.clone();
                    ok_ws.update(cx, |ws, cx| ws.rename(id, name, window, cx));
                    false
                })
                .child(
                    v_flex().gap_2().child(Input::new(&input)).child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Empty: named after its folder, {folder}")),
                    ),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("ok").label("Rename").primary().on_click(
                            move |_, window, cx| {
                                let name = button_name.clone();
                                button_ws.update(cx, |ws, cx| ws.rename(id, name, window, cx));
                            },
                        )),
                )
        });
    }

    /// Names terminal `id` and saves the layout: names are the user's and are kept, unlike the
    /// titles programs set, which don't save.
    fn rename(&mut self, id: PaneId, name: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(view) = self.panes.get(&id) {
            view.update(cx, |v, cx| v.set_name(Some(name), cx));
        }
        self.autosave(window, cx);
        // Again on the next frame, after the closing dialog hands the focus back.
        if let Some(shown) = self.envs.active().and_then(|t| t.shown) {
            self.focus_soon(shown, window, cx);
        }
        cx.notify();
    }
}
