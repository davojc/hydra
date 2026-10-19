//! The window's layout file (spec §7.12, env-tabs design §3): opening a version 2 layout into
//! environment tabs and terminals, saving the window back to it on every change, Save layout
//! as…, and asking before stopping running commands.
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{App, Context, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex, v_flex};
use hydraui_sets::layout::{
    EnvFile, FilesFile, LAYOUT_VERSION, Layout, LayoutError, TerminalFile, TreeFile, plan_layout,
    read_layout, write_layout,
};
use hydraui_sets::{Context as LayoutContext, PaneSpec};
use hydraui_term::PromptState;

use super::{DEFAULT_TREE_WIDTH, Workspace, clamp_tree_width};
use crate::envs::PaneId;
use crate::hydra::PaneRequest;
use crate::keys::FileTree;

/// What to ask before stopping `what` when some of its terminals are running a command (OSC
/// 133); `running` names each terminal. `None`: nothing runs, so don't ask.
pub fn running_question(what: &str, running: &[String]) -> Option<String> {
    if running.is_empty() {
        return None;
    }
    let list: Vec<String> = running.iter().map(|r| format!("  {r}")).collect();
    Some(format!(
        "{what} has commands running:\n{}\nThey will be stopped.",
        list.join("\n")
    ))
}

/// What a terminal planned from a layout runs: its folder, shell and hand-written command, and
/// a warning when its folder is bound to another environment (spec §5.2).
fn pane_request(spec: &PaneSpec) -> PaneRequest {
    let mut request = PaneRequest::new(spec.env.to_string(), spec.cwd.clone(), spec.shell);
    request.command = spec.command.clone();
    request.note = spec
        .bound_elsewhere
        .as_ref()
        .map(|e| format!("this folder is bound to {e}"));
    request
}

impl Workspace {
    fn layout_context(&self) -> LayoutContext<'_> {
        LayoutContext {
            paths: &self.hydra.paths,
            user_home: &self.hydra.user_home,
            bindings: &self.hydra.global.bindings,
            default_shell: self.shell(),
        }
    }

    /// The names of the terminals among `panes` that are running a command.
    pub(super) fn running(&self, panes: &[PaneId], cx: &App) -> Vec<String> {
        panes
            .iter()
            .filter_map(|id| self.panes.get(id))
            .map(|v| v.read(cx))
            .filter(|v| v.prompt() == Some(PromptState::CommandRunning))
            .map(|v| v.label())
            .collect()
    }

    /// Runs `then` now when nothing in `running` runs; else asks "`verb` `what`?" first, naming
    /// the running terminals, and runs it on confirm.
    pub(super) fn ask_running(
        &mut self,
        verb: &'static str,
        what: &str,
        running: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(question) = running_question(what, running) else {
            then(self, window, cx);
            return;
        };
        let workspace = cx.entity();
        let what = what.to_string();
        let then = std::rc::Rc::new(then);
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, then) = (workspace.clone(), then.clone());
            dialog
                .title(format!("{verb} {what}?"))
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
                                ws.update(cx, |ws, cx| then(ws, window, cx));
                            },
                        )),
                )
        });
    }

    /// The window as a version 2 layout (env-tabs design §3): each environment in tab order
    /// with its terminals (name, current folder, shell, and a hand-written command), the shown
    /// one, its file pane, the selected environment and the tree.
    pub(super) fn current_layout(&self, cx: &App) -> Layout {
        let envs = self
            .envs
            .iter()
            .map(|tab| {
                let terminals = tab
                    .terminals
                    .iter()
                    .filter_map(|id| {
                        let v = self.panes.get(id)?.read(cx);
                        Some(TerminalFile {
                            title: v.name().map(str::to_string),
                            cwd: v.cwd().to_string_lossy().into_owned(),
                            shell: Some(v.request.shell.as_str().into()),
                            command: self.commands.get(id).cloned(),
                        })
                    })
                    .collect();
                let files = tab
                    .files
                    .and_then(|id| self.files.get(&id))
                    .map(|f| f.read(cx).saved())
                    .filter(|(files, _)| !files.is_empty())
                    .map(|(files, active)| FilesFile {
                        files: files
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect(),
                        active: Some(active),
                    });
                EnvFile {
                    name: tab.env.clone(),
                    active: tab
                        .shown
                        .and_then(|s| tab.terminals.iter().position(|&t| t == s)),
                    files,
                    terminals,
                }
            })
            .collect();
        Layout {
            version: LAYOUT_VERSION,
            active_env: self.envs.active().map(|t| t.env.clone()),
            // A dragged width outlives closing the tree.
            tree: (self.tree.is_some() || self.tree_width != DEFAULT_TREE_WIDTH).then(|| {
                TreeFile {
                    open: self.tree.is_some(),
                    width: (self.tree_width != DEFAULT_TREE_WIDTH).then_some(self.tree_width),
                }
            }),
            envs,
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

    /// Hides the start page, for the environments about to open.
    pub(super) fn leave_start_page(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.start = None;
        cx.notify();
    }

    /// Opens a layout file (spec §7.12): in this window when it is untitled with no
    /// environments, else in a new hydraui (layouts are never merged); a file another window
    /// has brings that window forward instead. Returns whether this window opened it.
    pub(super) fn open_layout_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.open_layout_from(path, false, window, cx)
    }

    /// `open_layout_path`; `recent`: chosen from the start page's recent list, which forgets a
    /// file that isn't a layout this hydraui opens (env-tabs design §3).
    pub(super) fn open_layout_from(
        &mut self,
        path: PathBuf,
        recent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        if !crate::window_layout::opens_here(self.layout.is_some(), !self.envs.is_empty()) {
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
        // An older hydraui's layout is refused, never converted; the file stays as it is.
        let layout = match read_layout(&path) {
            Ok(layout) => layout,
            Err(e) => {
                if recent && matches!(e, LayoutError::Old(_) | LayoutError::NotLayout(_)) {
                    let _ = hydraui_sets::forget_recent(&paths, &path);
                    cx.notify();
                }
                window
                    .push_notification(Notification::error(e.to_string()).title("Open layout"), cx);
                return false;
            }
        };
        match crate::claim::take(&running, &path, crate::claim::hwnd_of(window)) {
            crate::claim::Taken::Mine(c) => {
                let _ = hydraui_sets::touch_recent(&paths, &path, crate::window_layout::now_secs());
                self.claim = Some(c);
                self.layout = Some(path);
            }
            crate::claim::Taken::Elsewhere(holder) => {
                self.elsewhere(holder, window);
                return false;
            }
        }
        self.leave_start_page(window, cx);
        self.unopened = Layout::default();
        self.opening = true;
        let any = self.open_layout(&layout, window, cx);
        self.opening = false;
        // Nothing opened (a deleted environment, a missing folder): let the file go untouched
        // rather than keep an empty window that would save over it.
        if let Some(file) = self.layout.clone()
            && crate::window_layout::let_go_after_open(any, &layout)
        {
            self.claim = None;
            self.layout = None;
            self.unopened = Layout::default();
            window.push_notification(
                Notification::warning(format!(
                    "Nothing in {} could be opened; the file is unchanged",
                    file.display()
                ))
                .title("Open layout"),
                cx,
            );
            if self.envs.is_empty() {
                self.show_start_page(window, cx);
            }
            return false;
        }
        cx.notify();
        true
    }

    /// Opens a layout's environments in this window (env-tabs design §3): each one's tab, its
    /// terminals with their names and commands, the shown one and its file pane; then the
    /// tree and the selected environment. What can't open is said in a toast and kept in
    /// `unopened`. Returns whether any environment opened.
    fn open_layout(
        &mut self,
        layout: &Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let plan = plan_layout(&self.layout_context(), layout);
        for problem in &plan.problems {
            window.push_notification(
                Notification::warning(problem.clone()).title("Can't reopen"),
                cx,
            );
        }
        self.unopened = plan.unopened;
        if plan.envs.is_empty() {
            return false;
        }
        for env in plan.envs {
            self.add_env_tab(&env.name);
            let mut ids = Vec::new();
            for t in env.terminals {
                let id = self.new_pane(pane_request(&t.spec), window, cx);
                if let Some(view) = self.panes.get(&id).cloned()
                    && t.title.is_some()
                {
                    view.update(cx, |v, cx| v.set_name(t.title, cx));
                }
                if let Some(command) = t.spec.command {
                    self.commands.insert(id, command);
                }
                self.envs.add_terminal(&env.name, id);
                ids.push(id);
            }
            if let Some(&id) = env.shown.and_then(|i| ids.get(i)) {
                self.envs.show(id);
            }
            if let Some(f) = env.files {
                self.next_pane += 1;
                let id = self.next_pane;
                self.create_files_pane(id, &f.files, f.active, window, cx);
                self.envs.set_files(&env.name, Some(id));
            }
        }
        if let Some(tree) = plan.tree
            && self.tree.is_none()
        {
            let width = f32::from(window.viewport_size().width);
            self.tree_width = clamp_tree_width(tree.width.unwrap_or(DEFAULT_TREE_WIDTH), width);
            if tree.open {
                self.toggle_tree(&FileTree, window, cx);
            }
        }
        if let Some(env) = plan.active_env {
            self.envs.activate(&env);
        }
        // After the terminals' own focus requests, so the selected one keeps the keyboard.
        if let Some(id) = self.envs.active().and_then(|t| t.shown) {
            self.focus_soon(id, window, cx);
        }
        self.sync_tree(cx);
        self.refresh_status(cx);
        self.refresh_git(cx);
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
    /// The dialog knows the file type, so a name typed without one gets its extension.
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
        let (tx, rx) = futures::channel::oneshot::channel();
        let owner = crate::claim::hwnd_of(window);
        std::thread::spawn(move || {
            let _ = tx.send(crate::save_dialog::prompt(&dir, &name, owner));
        });
        cx.spawn_in(window, async move |this, cx| match rx.await {
            Ok(Ok(Some(path))) => {
                let _ = this.update_in(cx, |ws, window, cx| then(ws, path, window, cx));
            }
            Ok(Err(e)) => {
                let _ = this.update_in(cx, |_, window, cx| {
                    window.push_notification(Notification::error(e).title("Save"), cx)
                });
            }
            _ => {}
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_question_asks_only_when_something_runs_and_names_it() {
        assert_eq!(running_question("client-a", &[]), None);
        let q = running_question(
            "client-a",
            &["api: npm run dev".to_string(), "api: work".to_string()],
        )
        .unwrap();
        assert!(q.contains("client-a"), "{q}");
        assert!(q.contains("  api: npm run dev\n  api: work"), "{q}");
        assert!(q.contains("stopped"), "{q}");
    }
}
