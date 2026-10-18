//! Sets in the window (spec §5.4): loading a set opens its tabs, unloading closes them
//! (asking first when commands are running), a changed file offers a reload; and what was
//! open (sets, Loose tabs, the file tree, the active tab) is saved for the next start.
use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{App, Context, Hsla, SharedString, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};
use std::path::{Path, PathBuf};

use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use hydraui_sets::file::{SetFile, TabFile};
use hydraui_sets::{
    Context as SetContext, LoadedSet, Opened, Session, SetRef, SetSpec, file_modified, list_sets,
    load_set, load_set_file, resolve_loose, sets_dir, write_layout, write_set_file,
};
use hydraui_term::PromptState;

use super::{DEFAULT_TREE_WIDTH, Workspace, tree_width_at};
use crate::keys::FileTree;
use crate::layout::{SavedPane, TabLayout, tab_file, tab_layout};
use crate::layout_plan::{open_plan, reload_from, session_entry, taken_names};
use crate::tabs::Tab;
use crate::tabs::TabId;

/// What to ask before unloading `set` when some of its panes are running a command (OSC 133);
/// `running` names each as `tab: pane`. `None`: nothing runs, so don't ask.
pub fn unload_question(set: &str, running: &[String]) -> Option<String> {
    if running.is_empty() {
        return None;
    }
    let list: Vec<String> = running.iter().map(|r| format!("  {r}")).collect();
    Some(format!(
        "{set} has commands running:\n{}\nThey will be stopped.",
        list.join("\n")
    ))
}

impl Workspace {
    pub(super) fn set_context(&self) -> SetContext<'_> {
        SetContext {
            paths: &self.hydra.paths,
            user_home: &self.hydra.user_home,
            bindings: &self.hydra.global.bindings,
            default_shell: self.hydra.shell(self.ui.default_shell.as_deref()),
        }
    }

    /// Opens a set, or focuses its first tab when it is already open. A set that doesn't
    /// validate opens nothing and says why. Returns whether the set is open now.
    pub(super) fn load_set(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(first) = self.loaded.get(name).and_then(|s| s.tabs.first().copied()) {
            self.tabs.activate(first);
            self.focus_active(window, cx);
            cx.notify();
            return true;
        }
        let spec = match load_set(&self.set_context(), name) {
            Ok(spec) => spec,
            Err(e) => {
                window.push_notification(
                    Notification::error(e.to_string()).title(format!("Set {name}")),
                    cx,
                );
                return false;
            }
        };
        self.open_set(spec, window, cx);
        true
    }

    fn open_set(&mut self, spec: SetSpec, window: &mut Window, cx: &mut Context<Self>) {
        let mut tabs = Vec::new();
        for tab in &spec.tabs {
            let next = &mut self.next_pane;
            let TabLayout {
                tree,
                terminals: panes,
                files,
            } = tab_layout(tab, &mut || {
                *next += 1;
                *next
            });
            let focused = panes[0].0;
            for (id, request) in panes {
                self.create_pane(id, request, window, cx);
            }
            if let Some((id, f)) = files {
                self.create_files_pane(id, &f.files, f.active, window, cx);
            }
            tabs.push(self.tabs.add_tree(
                Some(spec.name.clone()),
                Some(tab.title.clone()),
                tree,
                focused,
            ));
        }
        self.loaded.insert(LoadedSet {
            name: spec.name.clone(),
            file: spec.file.clone(),
            tabs: tabs.clone(),
            modified: file_modified(&spec.file),
        });
        self.changed_sets.remove(&spec.name);
        if let Some(first) = tabs.first() {
            self.tabs.activate(*first);
        }
        if let Some(focused) = self.tabs.active().map(|t| t.focused) {
            self.focus_soon(focused, window, cx);
        }
        self.autosave(window, cx);
        cx.notify();
    }

    /// The panes of these tabs that are running a command, as `tab: pane`.
    fn running(&self, tabs: &[TabId], cx: &App) -> Vec<String> {
        let mut out = Vec::new();
        for tab in tabs.iter().filter_map(|t| self.tabs.get(*t)) {
            for id in tab.tree.panes() {
                let Some(view) = self.panes.get(&id) else {
                    continue;
                };
                let v = view.read(cx);
                if v.prompt() == Some(PromptState::CommandRunning) {
                    let tab_name = tab.title.clone().unwrap_or_else(|| v.label());
                    out.push(format!("{tab_name}: {}", v.label()));
                }
            }
        }
        out
    }

    /// Closes a set's tabs, asking first when commands are running; `reload` opens it again
    /// from its file afterwards (spec §5.4: reload is unload + load).
    pub(super) fn unload_set(
        &mut self,
        name: &str,
        reload: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(set) = self.loaded.get(name) else {
            return;
        };
        let panes: Vec<_> = set
            .tabs
            .iter()
            .filter_map(|t| self.tabs.get(*t))
            .flat_map(|t| t.tree.panes())
            .collect();
        let name = name.to_string();
        self.confirm_unsaved(panes, window, cx, move |ws, w, cx| {
            ws.unload_set_asking(&name, reload, w, cx)
        });
    }

    /// `unload_set` once unsaved edits are dealt with: asks when commands are running.
    fn unload_set_asking(
        &mut self,
        name: &str,
        reload: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(set) = self.loaded.get(name) else {
            return;
        };
        let running = self.running(&set.tabs, cx);
        let Some(question) = unload_question(name, &running) else {
            self.close_set(name, reload, window, cx);
            return;
        };
        let workspace = cx.entity();
        let name = name.to_string();
        let verb = if reload { "Reload" } else { "Unload" };
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, set) = (workspace.clone(), name.clone());
            dialog
                .title(format!("{verb} {name}?"))
                .child(v_flex().children(question.lines().map(|l| div().child(l.to_string()))))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("confirm").label(verb).danger().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                ws.update(cx, |ws, cx| ws.close_set(&set, reload, window, cx));
                            },
                        )),
                )
        });
    }

    fn close_set(&mut self, name: &str, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut file = None;
        if let Some(set) = self.loaded.remove(name) {
            file = Some(set.file);
            for tab in set.tabs {
                for id in self.tabs.close_tab(tab) {
                    self.drop_pane(id);
                }
            }
        }
        self.changed_sets.remove(name);
        self.autosave(window, cx);
        // A group reloads from its own file (spec §5.4); a sets-folder set by name.
        let reopened = reload
            && match file.map(|f| reload_from(name, &f, &sets_dir(&self.hydra.paths))) {
                Some(SetRef::File(path)) => self.open_group_file(&path, window, cx),
                _ => self.load_set(name, window, cx),
            };
        if reopened {
            self.autosave(window, cx);
        }
        if !reopened {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// A tab closed by hand: the set whose last tab it was is unloaded (spec §5.4).
    pub(super) fn tab_gone(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.loaded.tab_closed(tab) {
            self.changed_sets.remove(&set);
            self.autosave(window, cx);
        }
    }

    /// A tab as the set file's tab format: each pane's env, current folder and shell, file
    /// panes, the tab's title, and (set tabs only) startup commands (spec §7.9).
    pub(super) fn snapshot_tab(&self, tab: &Tab, cx: &App) -> Option<TabFile> {
        let set_tab = tab.set.is_some();
        let pane = |id| {
            if let Some(f) = self.files.get(&id) {
                let (files, active) = f.read(cx).saved();
                return Some(SavedPane::Files { files, active });
            }
            self.panes.get(&id).map(|v| {
                let v = v.read(cx);
                SavedPane::Terminal {
                    env: v.env().to_string(),
                    cwd: v.cwd().clone(),
                    shell: v.request.shell,
                    // A Loose tab's command is a sign-in's: not worth running again.
                    command: if set_tab {
                        v.request.command.clone()
                    } else {
                        None
                    },
                }
            })
        };
        tab_file(&tab.tree, tab.title.as_deref(), &pane)
    }

    /// The window as a layout (session.toml's format, spec §5.4).
    pub(super) fn current_layout(&self, cx: &App) -> Session {
        let dir = sets_dir(&self.hydra.paths);
        let loose = self
            .tabs
            .iter()
            .filter(|t| t.set.is_none())
            .filter_map(|t| self.snapshot_tab(t, cx))
            .collect();
        let active = self.tabs.active_id();
        Session {
            sets: self
                .loaded
                .all()
                .iter()
                .map(|s| session_entry(&s.name, &s.file, &dir))
                .collect(),
            active: self.tabs.iter().position(|t| Some(t.id) == active),
            tree: self.tree.is_some(),
            tree_width: (self.tree.is_some() && self.tree_width != DEFAULT_TREE_WIDTH)
                .then_some(self.tree_width),
            loose,
        }
    }

    /// These tabs as a group file (a set file; every pane carries its own env and folder).
    pub(super) fn group_file_of(&self, tabs: &[TabId], cx: &App) -> SetFile {
        SetFile {
            tabs: tabs
                .iter()
                .filter_map(|t| self.tabs.get(*t))
                .filter_map(|t| self.snapshot_tab(t, cx))
                .collect(),
            ..Default::default()
        }
    }

    /// Writes the window's layout to its file (spec §7.12); an untitled window writes nothing.
    /// Not while a layout is opening: the file already holds it, and a half-open window would
    /// write over it. Entries that didn't open are kept in what is written.
    pub(super) fn autosave(&self, window: &mut Window, cx: &mut App) {
        let Some(path) = &self.layout else { return };
        if self.opening {
            return;
        }
        let layout = crate::window_layout::with_unopened(self.current_layout(cx), &self.unopened);
        if let Err(e) = write_layout(path, &layout) {
            window.push_notification(
                Notification::warning(format!("can't save the layout to {}: {e}", path.display()))
                    .title("hydraui"),
                cx,
            );
        }
    }

    /// The window is showing the start page (spec §7.12).
    pub(super) fn is_start_page(&self) -> bool {
        self.start.is_some()
    }

    /// Hides the start page, for the tabs about to open.
    pub(super) fn leave_start_page(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.start = None;
        cx.notify();
    }

    /// Opens a layout file (spec §7.12): in this window when it is untitled with no tabs, else
    /// in a new hydraui (layouts are never merged); a file another window has brings that
    /// window forward instead. Returns whether this window opened it.
    pub(super) fn open_layout_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        if !crate::window_layout::opens_here(self.layout.is_some(), !self.tabs.is_empty()) {
            let mine = self
                .layout
                .as_deref()
                .is_some_and(|m| hydraui_sets::same_path(m, &path));
            if !mine && let Err(e) = crate::window_layout::spawn_hydraui(Some(&path)) {
                window.push_notification(
                    Notification::error(format!("can't start another hydraui: {e}"))
                        .title("Open layout"),
                    cx,
                );
            }
            return false;
        }
        let running = crate::claim::running_dir(&self.hydra.paths);
        if crate::claim::is_claimed(&path) {
            self.elsewhere(crate::claim::holder_window(&running, &path), window);
            return false;
        }
        let paths = self.hydra.paths.clone();
        let now = crate::window_layout::now_secs();
        let (layout, backing) = match hydraui_sets::open_layout_file(&path) {
            Err(e) => {
                window.push_notification(Notification::error(e).title("Open layout"), cx);
                return false;
            }
            Ok(Opened::Current(l)) => (l, Some(path)),
            Ok(Opened::Converted { layout, from, to }) => {
                window.push_notification(
                    Notification::info(format!("Converted {} → {}", from.display(), to.display()))
                        .title("Open layout"),
                    cx,
                );
                let _ = hydraui_sets::rename_recent(&paths, &from, &to, now);
                (layout, Some(to))
            }
            Ok(Opened::Untitled { layout, exists }) => {
                window.push_notification(
                    Notification::warning(format!(
                        "{} already exists, so this opens untitled: Save layout as… to keep it",
                        exists.display()
                    ))
                    .title("Open layout"),
                    cx,
                );
                (layout, None)
            }
        };
        if let Some(file) = backing {
            match crate::claim::take(&running, &file, crate::claim::hwnd_of(window)) {
                crate::claim::Taken::Mine(c) => {
                    let _ = hydraui_sets::touch_recent(&paths, &file, now);
                    self.claim = Some(c);
                    self.layout = Some(file);
                }
                crate::claim::Taken::Elsewhere(holder) => {
                    self.elsewhere(holder, window);
                    return false;
                }
            }
        }
        self.leave_start_page(window, cx);
        self.unopened = Session::default();
        self.opening = true;
        let any = self.open_layout(layout.clone(), window, cx);
        self.opening = false;
        // Nothing opened (a missing folder, a renamed environment): let the file go untouched
        // rather than keep an empty window that would save over it.
        if let Some(file) = self.layout.clone()
            && crate::window_layout::let_go_after_open(any, &layout)
        {
            self.claim = None;
            self.layout = None;
            self.unopened = Session::default();
            window.push_notification(
                Notification::warning(format!(
                    "Nothing in {} could be opened; the file is unchanged",
                    file.display()
                ))
                .title("Open layout"),
                cx,
            );
            if self.tabs.is_empty() {
                self.show_start_page(window, cx);
            }
            return false;
        }
        cx.notify();
        true
    }

    /// Another window has the file: bring it forward; a start page that only existed to open
    /// it closes.
    fn elsewhere(&mut self, holder: Option<isize>, window: &mut Window) {
        if let Some(h) = holder {
            crate::claim::bring_forward(h);
        }
        if self.is_start_page() {
            window.remove_window();
        }
    }

    /// Writes the window to `path` and backs the window with it from then on (Save layout
    /// as…, spec §7.12). A file another window has is refused; a failed write keeps the old
    /// file.
    pub(super) fn save_layout_to(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        if self
            .layout
            .as_deref()
            .is_some_and(|p| hydraui_sets::same_path(p, &path))
        {
            self.autosave(window, cx);
            return true;
        }
        let running = crate::claim::running_dir(&self.hydra.paths);
        let claim = match crate::claim::take(&running, &path, crate::claim::hwnd_of(window)) {
            crate::claim::Taken::Mine(c) => c,
            crate::claim::Taken::Elsewhere(_) => {
                window.push_notification(
                    Notification::error(format!(
                        "{} is open in another hydraui window",
                        path.display()
                    ))
                    .title("Save layout"),
                    cx,
                );
                return false;
            }
        };
        let layout = crate::window_layout::with_unopened(self.current_layout(cx), &self.unopened);
        let result = write_layout(&path, &layout);
        let ok = result.is_ok();
        self.saved("the layout", &path, result, window, cx);
        if !ok {
            return false; // the new claim drops here; the old file stays
        }
        let _ =
            hydraui_sets::touch_recent(&self.hydra.paths, &path, crate::window_layout::now_secs());
        self.claim = Some(claim); // the old claim drops: the old file is let go
        self.layout = Some(path);
        cx.notify();
        true
    }

    /// Asks where to save (the Save As dialog), then calls `then` with the path. Cancel does
    /// nothing. Debug builds take `HYDRAUI_TEST_SAVE_PATH` instead, for smoke tests.
    fn save_dialog(
        &mut self,
        dir: PathBuf,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("HYDRAUI_TEST_SAVE_PATH") {
            then(self, PathBuf::from(p), window, cx);
            return;
        }
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update_in(cx, |ws, window, cx| then(ws, path, window, cx));
            }
        })
        .detach();
    }

    /// Asks which file (or folder) to open (the Open dialog), then calls `then` with it.
    /// Cancel does nothing. Debug builds take `HYDRAUI_TEST_OPEN_PATH` instead, for smoke
    /// tests.
    pub(super) fn open_dialog_path(
        &mut self,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("HYDRAUI_TEST_OPEN_PATH") {
            then(self, PathBuf::from(p), window, cx);
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |ws, window, cx| then(ws, path, window, cx));
            }
        })
        .detach();
    }

    /// Says how a save went.
    fn saved(
        &self,
        what: &str,
        path: &Path,
        result: std::io::Result<()>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let note = match result {
            Ok(()) => Notification::success(format!("Saved {what} to {}", path.display())),
            Err(e) => Notification::error(format!("can't save {}: {e}", path.display())),
        };
        window.push_notification(note.title("Save"), cx);
    }

    /// Save layout as…: starts in the current file's folder, else the layouts folder.
    pub(super) fn save_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (dir, name) = match &self.layout {
            Some(p) => (
                p.parent().map(Path::to_path_buf).unwrap_or_default(),
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            None => {
                let dir = hydraui_sets::layouts_dir(&self.hydra.paths);
                let _ = std::fs::create_dir_all(&dir);
                (dir, "layout.hydraui".into())
            }
        };
        self.save_dialog(dir, name, window, cx, |ws, path, window, cx| {
            ws.save_layout_to(path, window, cx);
        });
    }

    pub(super) fn open_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog_path(false, window, cx, |ws, path, window, cx| {
            ws.open_layout_path(path, window, cx);
        });
    }

    pub(super) fn open_group_file_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog_path(false, window, cx, |ws, path, window, cx| {
            if ws.open_group_file(&path, window, cx) {
                ws.autosave(window, cx);
            }
        });
    }

    /// Writes `tabs` to a group file chosen in the Save As dialog, starting at `dir/name`.
    fn save_tabs_as_group(
        &mut self,
        tabs: Vec<TabId>,
        dir: PathBuf,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_dialog(dir, name, window, cx, move |ws, path, window, cx| {
            let result = write_set_file(&path, &ws.group_file_of(&tabs, cx));
            if result.is_ok() {
                // Saving a group over its own file isn't a change to offer a reload for.
                ws.loaded.saved_to(&path);
            }
            ws.saved("the group", &path, result, window, cx);
        });
    }

    pub(super) fn save_loose_as_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let loose: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|t| t.set.is_none())
            .map(|t| t.id)
            .collect();
        if loose.is_empty() {
            window.push_notification(
                Notification::info("No Loose tabs to save.").title("Save"),
                cx,
            );
            return;
        }
        let dir = self.hydra.user_home.clone();
        self.save_tabs_as_group(loose, dir, "group.toml".into(), window, cx);
    }

    /// Right-click a tab: Save tab as group… (spec §7.9).
    pub(super) fn save_tab_as_group(
        &mut self,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .tabs
            .get(tab)
            .and_then(|t| {
                t.title
                    .clone()
                    .or_else(|| self.tab_terminal(t).map(|v| v.read(cx).label()))
            })
            .unwrap_or_else(|| "tab".into());
        let dir = self.hydra.user_home.clone();
        self.save_tabs_as_group(vec![tab], dir, format!("{name}.toml"), window, cx);
    }

    /// Right-click a group header: Save group…, starting on the group's own file.
    fn save_group(&mut self, set: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.loaded.get(set) else {
            return;
        };
        let dir = s
            .file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.hydra.user_home.clone());
        let name = s
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{set}.toml"));
        let tabs = s.tabs.clone();
        self.save_tabs_as_group(tabs, dir, name, window, cx);
    }

    /// Opens a group file as a set (spec §5.4), or focuses it when it is open already. Says
    /// what's wrong when it can't. Returns whether the group is open now.
    pub(super) fn open_group_file(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(first) = self
            .loaded
            .by_file(path)
            .and_then(|s| s.tabs.first().copied())
        {
            self.tabs.activate(first);
            self.focus_active(window, cx);
            cx.notify();
            return true;
        }
        let folder = list_sets(&self.hydra.paths).unwrap_or_default();
        let taken = taken_names(&self.loaded.names(), &folder);
        match load_set_file(&self.set_context(), path, &taken) {
            Ok(spec) => {
                self.open_set(spec, window, cx);
                true
            }
            Err(e) => {
                window
                    .push_notification(Notification::error(e.to_string()).title("Open group"), cx);
                false
            }
        }
    }

    /// Notes the sets whose file changed or went away since they were loaded.
    pub(super) fn check_sets(&mut self, cx: &mut Context<Self>) {
        let changed: HashSet<String> = self.loaded.changed().into_iter().collect();
        if changed != self.changed_sets {
            self.changed_sets = changed;
            cx.notify();
        }
    }

    /// Adds a layout to the window (spec §7.9): its sets that aren't loaded yet, its Loose tabs
    /// and their file panes, the tree if it was open, and its active tab. A set or tab that
    /// fails is skipped with a toast. Returns whether any tab opened.
    pub(super) fn open_layout(
        &mut self,
        layout: Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let files: Vec<PathBuf> = self.loaded.all().iter().map(|s| s.file.clone()).collect();
        let mut opened: Vec<TabId> = Vec::new();
        let mut any = false;
        for (set, loaded) in open_plan(&layout.sets, &self.loaded.names(), &files) {
            // A set that fails to open stays in the window's layout (`unopened`), so the file
            // keeps it for when it opens again.
            let tabs = match set {
                SetRef::Named(name) => {
                    if !loaded {
                        let ok = self.load_set(&name, window, cx);
                        if !ok {
                            self.unopened.sets.push(name.clone());
                        }
                        any |= ok;
                    }
                    self.loaded.get(&name).map(|s| s.tabs.clone())
                }
                SetRef::File(path) => {
                    if !loaded {
                        let ok = self.open_group_file(&path, window, cx);
                        if !ok {
                            self.unopened.sets.push(path.display().to_string());
                        }
                        any |= ok;
                    }
                    self.loaded.by_file(&path).map(|s| s.tabs.clone())
                }
            };
            // Tabs of sets open already count too, so the active tab's position lines up.
            opened.extend(tabs.unwrap_or_default());
        }
        for tab in &layout.loose {
            let spec = match resolve_loose(&self.set_context(), tab) {
                Ok(spec) => spec,
                Err(problems) => {
                    window.push_notification(
                        Notification::warning(problems.join("\n")).title("Can't reopen a tab"),
                        cx,
                    );
                    self.unopened.loose.push(tab.clone());
                    continue;
                }
            };
            let next = &mut self.next_pane;
            let TabLayout {
                tree,
                terminals: panes,
                files,
            } = tab_layout(&spec, &mut || {
                *next += 1;
                *next
            });
            let focused = panes[0].0;
            for (id, request) in panes {
                self.create_pane(id, request, window, cx);
            }
            if let Some((id, f)) = files {
                self.create_files_pane(id, &f.files, f.active, window, cx);
            }
            opened.push(self.tabs.add_tree(None, None, tree, focused));
            any = true;
        }
        if !any {
            return false;
        }
        if layout.tree && self.tree.is_none() {
            let width = f32::from(window.viewport_size().width);
            self.tree_width = tree_width_at(layout.tree_width.unwrap_or(DEFAULT_TREE_WIDTH), width);
            self.toggle_tree(&FileTree, window, cx);
        }
        // The layout's active tab, by its place among the tabs it opened.
        if let Some(id) = layout.active.and_then(|i| opened.get(i).copied()) {
            self.tabs.activate(id);
        }
        // After the sets' own focus requests, so the active tab keeps the keyboard.
        if let Some(focused) = self.tabs.active().map(|t| t.focused) {
            self.focus_soon(focused, window, cx);
        }
        cx.notify();
        true
    }

    /// A set's group label in the tab strip: collapse toggle, "changed - reload?", reload,
    /// unload (spec §7.1, §5.4).
    pub(super) fn render_set_header(
        &self,
        set: &str,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let collapsed = self.collapsed.contains(set);
        let changed = self.changed_sets.contains(set);
        let id = |what: &str| SharedString::from(format!("{what}-{set}"));
        let toggle = set.to_string();
        let changed_name = set.to_string();
        let reload_name = set.to_string();
        let unload_name = set.to_string();
        let ws = cx.entity();
        let menu_set = set.to_string();
        h_flex()
            .id(id("header"))
            .gap_1()
            .px_1()
            .context_menu(move |menu, _, _| {
                let (save_ws, unload_ws) = (ws.clone(), ws.clone());
                let (save_set, unload_set) = (menu_set.clone(), menu_set.clone());
                menu.item(
                    PopupMenuItem::new("Save group…").on_click(move |_, window, cx| {
                        save_ws.update(cx, |ws, cx| ws.save_group(&save_set, window, cx));
                    }),
                )
                .item(
                    PopupMenuItem::new("Unload group").on_click(move |_, window, cx| {
                        unload_ws
                            .update(cx, |ws, cx| ws.unload_set(&unload_set, false, window, cx));
                    }),
                )
            })
            .child(
                div()
                    .id(id("set"))
                    .px_1()
                    .text_sm()
                    .text_color(muted)
                    .child(format!("{} {set}", if collapsed { "▸" } else { "▾" }))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if !ws.collapsed.remove(&toggle) {
                            ws.collapsed.insert(toggle.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(changed, |d| {
                d.child(
                    Button::new(id("changed"))
                        .label("changed - reload?")
                        .warning()
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.unload_set(&changed_name, true, window, cx)
                        })),
                )
            })
            .child(
                Button::new(id("reload"))
                    .label("reload")
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.unload_set(&reload_name, true, window, cx)
                    })),
            )
            .child(
                Button::new(id("unload"))
                    .label("unload")
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.unload_set(&unload_name, false, window, cx)
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloading_asks_only_when_something_runs_and_names_it() {
        assert_eq!(unload_question("client-a", &[]), None);
        let q = unload_question(
            "client-a",
            &["api: npm run dev".to_string(), "api: work".to_string()],
        )
        .unwrap();
        assert!(q.contains("client-a"), "{q}");
        assert!(q.contains("  api: npm run dev\n  api: work"), "{q}");
        assert!(q.contains("stopped"), "{q}");
    }
}
