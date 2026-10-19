//! The file tree panel (spec §7.3): Ctrl+B shows it, rooted at the focused pane's folder.
//! Clicks ask the workspace to insert a path, open a file, or change folder.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, EventEmitter, Hsla, SharedString, Task, Window, div, px,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, h_flex, v_flex};
use gpui_kit_assets::IconName;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::file_tree::{FileTree, Row};
use crate::git_status::{Base, Change, change_of, changed_folders};

/// What a click asks the workspace to do with the focused pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEvent {
    /// Insert the file's quoted path at the cursor.
    Insert(PathBuf),
    /// Show the file in the tab's file pane (spec §7.8).
    Show(PathBuf),
    /// Change the shell's folder (only offered at a prompt).
    Cd(PathBuf),
    /// Show how the file differs from git's last commit (spec §7.3).
    ShowChanges(PathBuf),
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
    /// The repository's root and its changed files, from the status bar's last read.
    git: Option<(PathBuf, BTreeMap<PathBuf, Change>)>,
    /// The folders holding those changes, worked out once per read.
    changed: BTreeSet<PathBuf>,
    /// Each renamed file's old path, for its diff.
    renamed_from: BTreeMap<PathBuf, PathBuf>,
    _pump: Task<()>,
}

/// `path` relative to `root`: git spells the root `C:/x` while the shell may say `c:\X`.
fn rel_to(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(rel.to_path_buf());
    }
    let spell = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase()
    };
    let (r, p) = (spell(root), spell(path));
    let inside = p == r || p.strip_prefix(&r).is_some_and(|rest| rest.starts_with('/'));
    inside.then(|| path.components().skip(root.components().count()).collect())
}

/// A row's git mark and colour: a file's change letter, or a dot on a folder holding changes.
fn git_mark(
    git: &Option<(PathBuf, BTreeMap<PathBuf, Change>)>,
    changed: &BTreeSet<PathBuf>,
    path: &Path,
    is_dir: bool,
) -> Option<(&'static str, Change)> {
    let (root, files) = git.as_ref()?;
    let rel = rel_to(root, path)?;
    let rel = rel.as_path();
    match change_of(files, rel) {
        Some(Change::Ignored) => Some(("", Change::Ignored)),
        Some(Change::Untracked) => Some(("?", Change::Untracked)),
        Some(c) if !is_dir => Some((
            match c {
                Change::Added => "A",
                Change::Deleted => "D",
                Change::Renamed => "R",
                _ => "M",
            },
            c,
        )),
        _ if is_dir && changed.contains(rel) => Some(("•", Change::Modified)),
        _ => None,
    }
}

/// A row's icon: an open or closed folder, or a file icon picked by extension.
fn row_icon(name: &str, is_dir: bool, expanded: bool) -> IconName {
    if is_dir {
        return if expanded {
            IconName::FolderOpen
        } else {
            IconName::Folder
        };
    }
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "json" | "jsonc" => IconName::FileBraces,
        "md" | "markdown" | "txt" | "log" => IconName::FileText,
        "yaml" | "yml" | "toml" | "ini" | "config" | "cfg" | "conf" | "env" | "xml" | "hydraui" => {
            IconName::FileCog
        }
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "cs" | "go" | "java" | "kt" | "c"
        | "h" | "cpp" | "hpp" | "rb" | "php" | "swift" | "html" | "css" | "scss" | "sql"
        | "lua" => IconName::FileCode,
        "ps1" | "psm1" | "sh" | "bash" | "bat" | "cmd" => IconName::FileTerminal,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp" | "bmp" => IconName::FileImage,
        "lock" => IconName::FileLock,
        _ => IconName::File,
    }
}

fn change_color(change: Change, cx: &App) -> Hsla {
    let theme = cx.theme();
    match change {
        Change::Modified | Change::Renamed => theme.warning,
        Change::Added | Change::Untracked => theme.success,
        Change::Deleted => theme.danger,
        Change::Ignored => theme.muted_foreground,
    }
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
            git: None,
            changed: BTreeSet::new(),
            renamed_from: BTreeMap::new(),
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

    /// Marks git changes: `root` is the repository's root, `files` relative to it.
    pub fn set_git(
        &mut self,
        root: Option<PathBuf>,
        files: BTreeMap<PathBuf, Change>,
        renamed_from: BTreeMap<PathBuf, PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.renamed_from = renamed_from;
        let git = root.map(|r| (r, files));
        if self.git != git {
            self.changed = git
                .as_ref()
                .map(|(_, f)| changed_folders(f))
                .unwrap_or_default();
            self.git = git;
            cx.notify();
        }
    }

    /// For a diff of `path`: the repository's root, the path relative to it, and what to
    /// compare it with.
    pub fn git_place(&self, path: &Path) -> Option<(PathBuf, PathBuf, Base)> {
        let (root, files) = self.git.as_ref()?;
        let rel = rel_to(root, path)?;
        let base = match (change_of(files, &rel), self.renamed_from.get(&rel)) {
            (Some(Change::Untracked), _) => Base::Nothing,
            (_, Some(old)) => Base::Renamed(old.clone()),
            _ => Base::Head,
        };
        Some((root.clone(), rel, base))
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
        let me = cx.entity();
        v_flex()
            .size_full()
            .text_sm()
            .child(div().px_2().py_1().text_color(muted).truncate().child(root))
            .child(
                v_flex()
                    .id("file-tree")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(
                        rows.into_iter()
                            .enumerate()
                            .map(|(i, row)| {
                                let marker = match (row.entry.is_dir, row.expanded) {
                                    (true, true) => "▾",
                                    (true, false) => "▸",
                                    _ => "",
                                };
                                let label = row.entry.name.clone();
                                let icon =
                                    row_icon(&row.entry.name, row.entry.is_dir, row.expanded);
                                let tip: SharedString = if !row.entry.is_dir {
                                    "Click: open · Ctrl+click: insert the path".into()
                                } else if can_cd {
                                    "Double-click: cd here".into()
                                } else {
                                    "Double-click cd works when the shell is at its prompt".into()
                                };
                                let indent = px(8. + 12. * row.depth as f32);
                                let mark = git_mark(
                                    &self.git,
                                    &self.changed,
                                    &row.entry.path,
                                    row.entry.is_dir,
                                );
                                let color = mark.map(|(_, c)| change_color(c, cx));
                                let changed_file = !row.entry.is_dir
                                    && mark.is_some_and(|(_, c)| c != Change::Ignored);
                                let path = row.entry.path.clone();
                                let el = h_flex()
                                    .id(("row", i))
                                    .pl(indent)
                                    .pr_2()
                                    .gap_2()
                                    .whitespace_nowrap()
                                    .hover(move |d| d.bg(hover))
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(tip.clone()).build(window, cx)
                                    })
                                    .on_click(cx.listener(move |view, ev: &ClickEvent, _, cx| {
                                        view.click(&row, ev, cx)
                                    }))
                                    .child(div().w(px(8.)).flex_none().child(marker))
                                    .child(
                                        Icon::new(icon).size(px(14.)).flex_none().text_color(muted),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .when_some(color, |d, c| d.text_color(c))
                                            .child(label),
                                    )
                                    .when_some(mark, |d, (m, _)| {
                                        d.child(
                                            div()
                                                .flex_none()
                                                .when_some(color, |d, c| d.text_color(c))
                                                .child(m),
                                        )
                                    });
                                if changed_file {
                                    let me = me.clone();
                                    el.context_menu(move |menu, _, _| {
                                        let (me, path) = (me.clone(), path.clone());
                                        menu.item(PopupMenuItem::new("Show changes").on_click(
                                            move |_, _, cx| {
                                                me.update(cx, |_, cx| {
                                                    cx.emit(TreeEvent::ShowChanges(path.clone()))
                                                })
                                            },
                                        ))
                                    })
                                    .into_any_element()
                                } else {
                                    el.into_any_element()
                                }
                            })
                            .collect::<Vec<AnyElement>>(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_get_an_icon_for_what_they_hold() {
        use IconName::*;
        assert_eq!(row_icon("src", true, false), Folder);
        assert_eq!(row_icon("src", true, true), FolderOpen);
        assert_eq!(row_icon("a.json", false, false), FileBraces);
        assert_eq!(row_icon("README.MD", false, false), FileText);
        assert_eq!(row_icon("ci.yml", false, false), FileCog);
        assert_eq!(row_icon("Cargo.toml", false, false), FileCog);
        assert_eq!(row_icon("main.rs", false, false), FileCode);
        assert_eq!(row_icon("build.ps1", false, false), FileTerminal);
        assert_eq!(row_icon("logo.png", false, false), FileImage);
        assert_eq!(row_icon("Cargo.lock", false, false), FileLock);
        assert_eq!(row_icon("LICENSE", false, false), File);
        assert_eq!(row_icon(".gitignore", false, false), File);
    }

    #[test]
    fn every_row_icon_is_bundled() {
        use gpui::AssetSource;
        let names = [
            "a.json", "a.md", "a.yml", "a.rs", "a.ps1", "a.png", "a.lock", "a",
        ];
        let icons = names
            .iter()
            .map(|n| row_icon(n, false, false))
            .chain([row_icon("d", true, false), row_icon("d", true, true)]);
        for icon in icons {
            let path = icon.path();
            assert!(
                crate::assets::AppAssets.load(&path).unwrap().is_some(),
                "{path} isn't bundled"
            );
        }
    }

    #[test]
    fn click_shows_a_file_and_ctrl_click_inserts_its_path() {
        let p = PathBuf::from("C:/x/a.md");
        assert_eq!(file_click(false, p.clone()), TreeEvent::Show(p.clone()));
        assert_eq!(file_click(true, p.clone()), TreeEvent::Insert(p));
    }

    #[test]
    fn paths_are_found_under_the_repository_however_windows_spells_them() {
        let rel = |root: &str, p: &str| rel_to(Path::new(root), Path::new(p));
        assert_eq!(
            rel("C:/w/repo", r"C:\w\repo\src\A.rs"),
            Some(PathBuf::from("src/A.rs"))
        );
        assert_eq!(
            rel("C:/w/repo", r"c:\W\Repo\src\A.rs"),
            Some(PathBuf::from("src/A.rs"))
        );
        assert_eq!(rel("C:/w/repo", "C:/w/repo"), Some(PathBuf::new()));
        assert_eq!(rel("C:/w/repo", "C:/w/repository/a"), None);
        assert_eq!(rel("C:/w/repo", "D:/w/repo/a"), None);
    }
}
