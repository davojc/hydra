//! The file tree's right-click operations: new file or folder, rename and delete (to the
//! Recycle Bin), with open file tabs kept in step.
use std::cell::Cell;
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{Context, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::file_ops;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NameFor {
    File,
    Folder,
    Rename,
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Workspace {
    /// Whether a file at or inside `path` has unsaved edits in a file pane.
    fn unsaved_under(&self, path: &Path, cx: &Context<Self>) -> bool {
        self.files.values().any(|f| f.read(cx).unsaved_under(path))
    }

    /// Asks for a name: a new file or folder in `at`, or `at`'s new name.
    pub(super) fn name_dialog(
        &mut self,
        what: NameFor,
        at: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = match what {
            NameFor::File => "New file",
            NameFor::Folder => "New folder",
            NameFor::Rename => "Rename",
        };
        if what == NameFor::Rename && self.unsaved_under(&at, cx) {
            window.push_notification(
                Notification::warning(format!(
                    "{} has unsaved edits. Save them first.",
                    file_name(&at)
                ))
                .title(title),
                cx,
            );
            return;
        }
        let (dir, current) = match what {
            NameFor::Rename => (
                at.parent().map(Path::to_path_buf).unwrap_or_default(),
                Some(file_name(&at)),
            ),
            _ => (at.clone(), None),
        };
        let input = cx.new(|cx| {
            let input = InputState::new(window, cx);
            match &current {
                Some(name) => input.default_value(name.clone()),
                None if what == NameFor::File => input.placeholder("notes.md"),
                None => input.placeholder("Folder name"),
            }
        });
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let typed = input.read(cx).value().to_string();
            let problem = file_ops::name_problem(&dir, &typed, current.as_deref());
            let hint = match &problem {
                Some(p) if !p.is_empty() => div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(p.clone()),
                _ => div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("In {}", dir.display())),
            };
            let (ws, at) = (workspace.clone(), at.clone());
            // Enter confirms the dialog: do what the button does, when the name is fine. The
            // dialog stays open until that succeeds, so an error can still be read.
            let (enter_ws, enter_at, enter_name, ok) =
                (ws.clone(), at.clone(), typed.clone(), problem.is_none());
            dialog
                .title(title)
                .on_ok(move |_, window, cx| {
                    if ok {
                        let (at, name) = (enter_at.clone(), enter_name.clone());
                        enter_ws.update(cx, |ws, cx| ws.apply_name(what, at, name, window, cx));
                    }
                    false
                })
                .child(v_flex().gap_2().child(Input::new(&input)).child(hint))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("ok")
                                .label(if what == NameFor::Rename {
                                    "Rename"
                                } else {
                                    "Create"
                                })
                                .primary()
                                .disabled(problem.is_some())
                                .on_click(move |_, window, cx| {
                                    let (at, name) = (at.clone(), typed.clone());
                                    ws.update(cx, |ws, cx| {
                                        ws.apply_name(what, at, name, window, cx)
                                    });
                                }),
                        ),
                )
        });
    }

    fn apply_name(
        &mut self,
        what: NameFor,
        at: PathBuf,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let done = match what {
            NameFor::File => file_ops::create_file(&at, &name),
            NameFor::Folder => file_ops::create_dir(&at, &name),
            NameFor::Rename => file_ops::rename(&at, &name),
        };
        let path = match done {
            Ok(path) => path,
            Err(e) => {
                window.push_notification(Notification::error(e), cx);
                return;
            }
        };
        window.close_dialog(cx);
        if let Some(dir) = path.parent()
            && let Some(tree) = &self.tree
        {
            tree.update(cx, |t, cx| t.show_in(dir, cx));
        }
        match what {
            NameFor::File => self.show_file(path, window, cx),
            NameFor::Folder => {}
            NameFor::Rename => {
                for f in self.files.values() {
                    f.update(cx, |f, cx| f.renamed(&at, &path, cx));
                }
            }
        }
    }

    /// Asks, then moves `path` to the Recycle Bin and closes its file tabs.
    pub(super) fn delete_dialog(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = file_name(&path);
        if self.unsaved_under(&path, cx) {
            window.push_notification(
                Notification::warning(format!("{name} has unsaved edits. Save them first."))
                    .title("Delete"),
                cx,
            );
            return;
        }
        let what = if path.is_dir() {
            "The folder and everything in it go to the Recycle Bin."
        } else {
            "It goes to the Recycle Bin."
        };
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, path) = (workspace.clone(), path.clone());
            dialog
                .title(format!("Delete {name}?"))
                .child(div().child(what))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("delete").label("Delete").danger().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let path = path.clone();
                                ws.update(cx, |ws, cx| ws.delete_now(path, window, cx));
                            },
                        )),
                )
        });
    }

    fn delete_now(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = file_ops::to_recycle_bin(&path) {
            window.push_notification(Notification::error(e).title("Delete"), cx);
            return;
        }
        for f in self.files.values().cloned().collect::<Vec<_>>() {
            f.update(cx, |f, cx| f.close_under(&path, cx));
        }
        if let Some(dir) = path.parent()
            && let Some(tree) = &self.tree
        {
            tree.update(cx, |t, cx| t.show_in(dir, cx));
        }
    }
}
