//! The file pane (spec §7.8): file tabs, Raw (an editor; Ctrl+S saves) and Rendered (Markdown),
//! Open in editor, the unsaved-edits prompt, and changes on disk.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, SharedString,
    Subscription, Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Editor, EditorState, InputEvent};
use gpui_component::notification::Notification;
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::file_doc::{self, DiskChange, FileDoc};
use crate::keys::SaveFile;
use crate::open_files::{after_close, find};

#[derive(Debug, Clone, PartialEq)]
pub enum FilePaneEvent {
    Focused,
    /// The last file tab closed: the pane goes away.
    Empty,
    OpenInEditor(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Raw,
    Rendered,
}

enum Body {
    Text {
        editor: Entity<EditorState>,
        /// What was last loaded or saved: unsaved edits are a difference from it.
        saved: FileDoc,
        view: View,
        _sub: Subscription,
    },
    CantShow(String),
}

struct FileTab {
    path: PathBuf,
    body: Body,
    dirty: bool,
    changed_on_disk: bool,
    deleted: bool,
}

pub struct FilePane {
    tabs: Vec<FileTab>,
    active: usize,
    focus: FocusHandle,
    watcher: Option<RecommendedWatcher>,
    watched: BTreeSet<PathBuf>,
    _pump: Task<()>,
}

impl EventEmitter<FilePaneEvent> for FilePane {}

impl Focusable for FilePane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Reports each changed path; `None` if a watcher can't be made (files then don't reload).
fn watcher(tx: UnboundedSender<PathBuf>) -> Option<RecommendedWatcher> {
    notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                let _ = tx.unbounded_send(path);
            }
        }
    })
    .ok()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl FilePane {
    pub fn new(
        files: &[PathBuf],
        active: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, mut rx) = unbounded::<PathBuf>();
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Some(path) = rx.next().await {
                if this
                    .update_in(cx, |pane, window, cx| pane.disk_changed(&path, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut pane = Self {
            tabs: Vec::new(),
            active: 0,
            focus: cx.focus_handle(),
            watcher: watcher(tx),
            watched: BTreeSet::new(),
            _pump: pump,
        };
        for f in files {
            pane.push(f.clone(), window, cx);
        }
        pane.active = active.min(pane.tabs.len().saturating_sub(1));
        pane.sync_watches();
        pane
    }

    fn push(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let body = match file_doc::load(&path) {
            Ok(doc) => Self::text_body(&path, doc, window, cx),
            Err(e) => Body::CantShow(e),
        };
        self.tabs.push(FileTab {
            path,
            body,
            dirty: false,
            changed_on_disk: false,
            deleted: false,
        });
    }

    fn text_body(path: &Path, doc: FileDoc, window: &mut Window, cx: &mut Context<Self>) -> Body {
        let lang = file_doc::language(path);
        let text = doc.text.clone();
        let editor = cx.new(|cx| {
            let mut e = EditorState::new(window, cx).language(lang);
            e.set_value(text, window, cx);
            e
        });
        let key = path.to_path_buf();
        let sub = cx.subscribe(&editor, move |pane, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                pane.edited(&key, cx);
            }
        });
        let view = if file_doc::is_markdown(path) {
            View::Rendered
        } else {
            View::Raw
        };
        Body::Text {
            editor,
            saved: doc,
            view,
            _sub: sub,
        }
    }

    /// Recomputes a file's unsaved state after an edit.
    fn edited(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.path == path)
            && let Body::Text { editor, saved, .. } = &t.body
        {
            t.dirty = editor.read(cx).value().as_ref() != saved.text;
            cx.notify();
        }
    }

    /// Shows `path`: its file tab if it's open, else a new one.
    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.active = match find(&paths, &path) {
            Some(i) => i,
            None => {
                self.push(path, window, cx);
                self.tabs.len() - 1
            }
        };
        self.sync_watches();
        cx.notify();
    }

    /// The open files and the active one, for the session.
    pub fn saved(&self) -> (Vec<PathBuf>, usize) {
        (
            self.tabs.iter().map(|t| t.path.clone()).collect(),
            self.active,
        )
    }

    /// The files with unsaved edits.
    pub fn unsaved(&self) -> Vec<PathBuf> {
        self.tabs
            .iter()
            .filter(|t| t.dirty)
            .map(|t| t.path.clone())
            .collect()
    }

    fn save_tab(&mut self, i: usize, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(t) = self.tabs.get_mut(i) else {
            return Ok(());
        };
        let Body::Text { editor, saved, .. } = &mut t.body else {
            return Ok(());
        };
        let doc = FileDoc {
            text: editor.read(cx).value().to_string(),
            crlf: saved.crlf,
            bom: saved.bom,
        };
        file_doc::save(&t.path, &doc)?;
        *saved = doc;
        t.dirty = false;
        t.changed_on_disk = false;
        t.deleted = false;
        cx.notify();
        Ok(())
    }

    /// Saves every file with unsaved edits; stops at the first that fails.
    pub fn save_all(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        for i in 0..self.tabs.len() {
            if self.tabs[i].dirty {
                self.save_tab(i, cx)?;
            }
        }
        Ok(())
    }

    fn save_active(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.save_tab(self.active, cx) {
            window.push_notification(Notification::error(e).title("Save"), cx);
        }
    }

    /// Closes a file tab, asking first when it has unsaved edits.
    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(i) else {
            return;
        };
        if !t.dirty {
            self.remove_tab(i, cx);
            return;
        }
        let name = file_name(&t.path);
        let pane = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (save, discard) = (pane.clone(), pane.clone());
            dialog.title(format!("{name} has unsaved edits")).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("discard")
                            .label("Discard")
                            .on_click(move |_, w, cx| {
                                w.close_dialog(cx);
                                discard.update(cx, |p, cx| p.remove_tab(i, cx));
                            }),
                    )
                    .child(Button::new("save").label("Save").primary().on_click(
                        move |_, w, cx| {
                            w.close_dialog(cx);
                            save.update(cx, |p, cx| match p.save_tab(i, cx) {
                                Ok(()) => p.remove_tab(i, cx),
                                Err(e) => {
                                    w.push_notification(Notification::error(e).title("Save"), cx)
                                }
                            });
                        },
                    )),
            )
        });
    }

    fn remove_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        match after_close(self.tabs.len(), self.active, i) {
            None => {
                self.tabs.clear();
                cx.emit(FilePaneEvent::Empty);
            }
            Some(next) => {
                self.tabs.remove(i);
                self.active = next;
            }
        }
        self.sync_watches();
        cx.notify();
    }

    /// Watches the folders of the open files (not recursively).
    fn sync_watches(&mut self) {
        let want: BTreeSet<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|t| t.path.parent().map(Path::to_path_buf))
            .collect();
        if let Some(w) = self.watcher.as_mut() {
            for gone in self.watched.difference(&want) {
                let _ = w.unwatch(gone);
            }
            for new in want.difference(&self.watched) {
                let _ = w.watch(new, RecursiveMode::NonRecursive);
            }
        }
        self.watched = want;
    }

    fn disk_changed(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.path == path) else {
            return;
        };
        let Body::Text { editor, saved, .. } = &mut t.body else {
            return;
        };
        let now = path.is_file().then(|| file_doc::load(path));
        match file_doc::on_disk(saved, t.dirty, now) {
            DiskChange::Nothing => {}
            DiskChange::Reload(doc) => {
                let text = doc.text.clone();
                editor.update(cx, |e, cx| e.set_value(text, window, cx));
                *saved = doc;
                t.deleted = false;
            }
            DiskChange::Changed => t.changed_on_disk = true,
            DiskChange::Deleted => t.deleted = true,
        }
        cx.notify();
    }

    /// Replaces the active file's edits with what is on disk.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        if let (Body::Text { editor, saved, .. }, Ok(doc)) = (&mut t.body, file_doc::load(&t.path))
        {
            let text = doc.text.clone();
            editor.update(cx, |e, cx| e.set_value(text, window, cx));
            *saved = doc;
            t.dirty = false;
            t.changed_on_disk = false;
        }
        cx.notify();
    }

    fn toggle_view(&mut self, cx: &mut Context<Self>) {
        if let Some(FileTab {
            body: Body::Text { view, .. },
            ..
        }) = self.tabs.get_mut(self.active)
        {
            *view = if *view == View::Raw {
                View::Rendered
            } else {
                View::Raw
            };
        }
        cx.notify();
    }
}

impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, bar, accent, warning) = (
            theme.muted_foreground,
            theme.border,
            theme.tab_bar,
            theme.accent,
            theme.warning,
        );
        let mut strip = h_flex()
            .h(px(30.))
            .gap_1()
            .px_1()
            .bg(bar)
            .border_b_1()
            .border_color(border);
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!("{}{}", file_name(&t.path), if t.dirty { " ●" } else { "" });
            strip = strip.child(
                h_flex()
                    .id(("file-tab", i))
                    .px_2()
                    .gap_1()
                    .rounded_sm()
                    .when(i == self.active, |d| d.bg(accent))
                    .child(div().text_sm().child(label))
                    .child(
                        div()
                            .id(("file-close", i))
                            .text_xs()
                            .text_color(muted)
                            .child("×")
                            .on_click(cx.listener(move |p, _, w, cx| {
                                cx.stop_propagation();
                                p.close_tab(i, w, cx);
                            })),
                    )
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.active = i;
                        cx.notify();
                    })),
            );
        }
        let root = v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("FilePane")
            .on_action(cx.listener(Self::save_active))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(FilePaneEvent::Focused)),
            )
            .child(strip);
        let Some(t) = self.tabs.get(self.active) else {
            return root;
        };
        let path = t.path.clone();
        let mut header = h_flex()
            .h(px(28.))
            .px_2()
            .gap_2()
            .justify_end()
            .border_b_1()
            .border_color(border);
        if let Body::Text { view, .. } = &t.body
            && file_doc::is_markdown(&t.path)
        {
            let other = if *view == View::Raw {
                "Rendered"
            } else {
                "Raw"
            };
            header = header.child(
                Button::new("view-mode")
                    .label(other)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(|p, _, _, cx| p.toggle_view(cx))),
            );
        }
        let open = path.clone();
        header = header.child(
            Button::new("open-in-editor")
                .label("Open in editor")
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(FilePaneEvent::OpenInEditor(open.clone()))
                })),
        );
        let banner =
            if t.deleted {
                Some(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(warning)
                        .child("Deleted on disk. Save writes it back.")
                        .into_any_element(),
                )
            } else if t.changed_on_disk {
                Some(
                    h_flex()
                        .px_2()
                        .gap_2()
                        .text_sm()
                        .text_color(warning)
                        .child("Changed on disk.")
                        .child(
                            Button::new("reload")
                                .label("Reload")
                                .xsmall()
                                .on_click(cx.listener(|p, _, w, cx| p.reload(w, cx))),
                        )
                        .child(Button::new("keep").label("Keep mine").xsmall().on_click(
                            cx.listener(|p, _, _, cx| {
                                if let Some(t) = p.tabs.get_mut(p.active) {
                                    t.changed_on_disk = false;
                                }
                                cx.notify();
                            }),
                        ))
                        .into_any_element(),
                )
            } else {
                None
            };
        let body = match &t.body {
            Body::CantShow(why) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child(format!("Can't show this file: {why}"))
                .into_any_element(),
            Body::Text {
                editor,
                view: View::Raw,
                ..
            } => Editor::new(editor)
                .bordered(false)
                .h(gpui::relative(1.))
                .into_any_element(),
            Body::Text {
                editor,
                view: View::Rendered,
                ..
            } => {
                let md = file_doc::defang_remote_images(editor.read(cx).value().as_ref());
                div()
                    .size_full()
                    .p_3()
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("md-{}", path.display())),
                            md,
                        )
                        .selectable(true)
                        .scrollable(true),
                    )
                    .into_any_element()
            }
        };
        root.child(header)
            .children(banner)
            .child(div().flex_1().min_h_0().child(body))
    }
}
