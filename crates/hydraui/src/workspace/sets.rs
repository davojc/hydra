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

use hydraui_sets::file::{SetFile, TabFile};
use hydraui_sets::{
    Context as SetContext, LoadedSet, Session, SetRef, SetSpec, file_modified, load_set,
    load_set_file, read_session, resolve_loose, sets_dir, write_session,
};
use hydraui_term::PromptState;

use super::{DEFAULT_TREE_WIDTH, Workspace, tree_width_at};
use crate::keys::FileTree;
use crate::layout::{SavedPane, TabLayout, tab_file, tab_layout};
use crate::layout_plan::{open_plan, session_entry};
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
        self.save_session(window, cx);
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
        if let Some(set) = self.loaded.remove(name) {
            for tab in set.tabs {
                for id in self.tabs.close_tab(tab) {
                    self.drop_pane(id);
                }
            }
        }
        self.changed_sets.remove(name);
        self.save_session(window, cx);
        if !reload || !self.load_set(name, window, cx) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// A tab closed by hand: the set whose last tab it was is unloaded (spec §5.4).
    pub(super) fn tab_gone(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.loaded.tab_closed(tab) {
            self.changed_sets.remove(&set);
            self.save_session(window, cx);
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
    #[allow(dead_code)] // used from Task 4 of the layout-files plan
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

    pub(super) fn save_session(&self, window: &mut Window, cx: &mut App) {
        let session = self.current_layout(cx);
        if let Err(e) = write_session(&self.hydra.paths, &session) {
            window.push_notification(
                Notification::warning(format!("can't save what is open: {e}")).title("hydraui"),
                cx,
            );
        }
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
        match load_set_file(&self.set_context(), path, &self.loaded.names()) {
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

    /// Reopens what was open at last exit (spec §5.4). Returns whether any tab opened.
    pub(super) fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let any = self.open_layout(read_session(&self.hydra.paths), window, cx);
        if any {
            self.save_session(window, cx);
        }
        any
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
        for set in open_plan(&layout.sets, &self.loaded.names(), &files) {
            let tabs = match set {
                SetRef::Named(name) => {
                    self.load_set(&name, window, cx);
                    self.loaded.get(&name).map(|s| s.tabs.clone())
                }
                SetRef::File(path) => {
                    self.open_group_file(&path, window, cx);
                    self.loaded.by_file(&path).map(|s| s.tabs.clone())
                }
            };
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
        }
        if opened.is_empty() {
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
        h_flex()
            .gap_1()
            .px_1()
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
