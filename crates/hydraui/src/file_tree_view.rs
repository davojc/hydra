//! The file tree panel (spec §7.3): Ctrl+B shows it, rooted at the focused pane's folder.
//! Clicks ask the workspace to insert a path, open a file, or change folder.
use std::collections::BTreeSet;
use std::path::PathBuf;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{ClickEvent, Context, EventEmitter, SharedString, Task, Window, div, px};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, v_flex};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::file_tree::{FileTree, Row};

/// What a click asks the workspace to do with the focused pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEvent {
    /// Insert the file's quoted path at the cursor.
    Insert(PathBuf),
    /// Show the file in the tab's file pane (spec §7.8).
    Show(PathBuf),
    /// Change the shell's folder (only offered at a prompt).
    Cd(PathBuf),
}

/// A click on a file: show it; with Ctrl, insert its path (spec §7.3).
pub fn file_click(ctrl: bool, path: PathBuf) -> TreeEvent {
    if ctrl {
        TreeEvent::Insert(path)
    } else {
        TreeEvent::Show(path)
    }
}

pub struct FileTreeView {
    tree: FileTree,
    can_cd: bool,
    watcher: Option<RecommendedWatcher>,
    /// The root and the open folders, each watched (not recursively).
    watched: BTreeSet<PathBuf>,
    _pump: Task<()>,
}

impl EventEmitter<TreeEvent> for FileTreeView {}

/// A watcher that reports the folder of each changed path; `None` if one can't be made (the
/// tree then just doesn't update by itself).
fn watcher(tx: UnboundedSender<PathBuf>) -> Option<RecommendedWatcher> {
    notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                if let Some(dir) = path.parent() {
                    let _ = tx.unbounded_send(dir.to_path_buf());
                }
            }
        }
    })
    .ok()
}

impl FileTreeView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<PathBuf>();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(dir) = rx.next().await {
                let refreshed = this.update(cx, |view, cx| {
                    view.tree.refresh(&dir);
                    cx.notify();
                });
                if refreshed.is_err() {
                    break;
                }
            }
        });
        let mut view = Self {
            tree: FileTree::new(root),
            can_cd: false,
            watcher: watcher(tx),
            watched: BTreeSet::new(),
            _pump: pump,
        };
        view.sync_watches();
        view
    }

    /// Follows the focused pane's folder.
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.tree.set_root(root) {
            self.sync_watches();
            cx.notify();
        }
    }

    /// Double-click `cd` works only while the focused pane is at its prompt (spec §7.3).
    pub fn set_can_cd(&mut self, can_cd: bool, cx: &mut Context<Self>) {
        if self.can_cd != can_cd {
            self.can_cd = can_cd;
            cx.notify();
        }
    }

    fn sync_watches(&mut self) {
        let want: BTreeSet<PathBuf> = std::iter::once(self.tree.root().to_path_buf())
            .chain(self.tree.open_dirs().cloned())
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

    fn click(&mut self, row: &Row, ev: &ClickEvent, cx: &mut Context<Self>) {
        let path = row.entry.path.clone();
        if row.entry.is_dir {
            // A double-click's first click already opened or closed it: the second undoes that.
            self.tree.toggle(&path);
            if ev.click_count() >= 2 && self.can_cd {
                cx.emit(TreeEvent::Cd(path));
            }
            self.sync_watches();
            cx.notify();
        } else {
            cx.emit(file_click(ev.modifiers().control, path));
        }
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let can_cd = self.can_cd;
        let root = self.tree.root().display().to_string();
        let rows = self.tree.rows();
        v_flex()
            .size_full()
            .text_sm()
            .child(div().px_2().py_1().text_color(muted).truncate().child(root))
            .child(
                v_flex()
                    .id("file-tree")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows.into_iter().enumerate().map(|(i, row)| {
                        let marker = match (row.entry.is_dir, row.expanded) {
                            (true, true) => "▾ ",
                            (true, false) => "▸ ",
                            _ => "  ",
                        };
                        let label = format!("{marker}{}", row.entry.name);
                        let tip: SharedString = if !row.entry.is_dir {
                            "Click: open · Ctrl+click: insert the path".into()
                        } else if can_cd {
                            "Double-click: cd here".into()
                        } else {
                            "Double-click cd works when the shell is at its prompt".into()
                        };
                        let indent = px(8. + 12. * row.depth as f32);
                        div()
                            .id(("row", i))
                            .pl(indent)
                            .pr_2()
                            .whitespace_nowrap()
                            .hover(move |d| d.bg(hover))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .on_click(cx.listener(move |view, ev: &ClickEvent, _, cx| {
                                view.click(&row, ev, cx)
                            }))
                            .child(label)
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_shows_a_file_and_ctrl_click_inserts_its_path() {
        let p = PathBuf::from("C:/x/a.md");
        assert_eq!(file_click(false, p.clone()), TreeEvent::Show(p.clone()));
        assert_eq!(file_click(true, p.clone()), TreeEvent::Insert(p));
    }
}
