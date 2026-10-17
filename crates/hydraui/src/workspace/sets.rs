//! Sets in the window (spec §5.4): loading a set opens its tabs, unloading closes them
//! (asking first when commands are running), a changed file offers a reload; and what was
//! open (sets, Loose tabs, the file tree, the active tab) is saved for the next start.
use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{App, Context, Hsla, SharedString, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};
use hydraui_sets::{
    Context as SetContext, LoadedSet, Session, SetSpec, file_modified, load_set, read_session,
    resolve_loose, write_session,
};
use hydraui_term::PromptState;

use super::{DEFAULT_TREE_WIDTH, Workspace, tree_width_at};
use crate::keys::FileTree;
use crate::layout::{SavedPane, tab_file, tab_layout};
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
            let (tree, panes) = tab_layout(tab, &mut || {
                *next += 1;
                *next
            });
            let focused = panes[0].0;
            for (id, request) in panes {
                self.create_pane(id, request, window, cx);
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

    pub(super) fn save_session(&self, window: &mut Window, cx: &mut App) {
        let pane = |id| {
            self.panes.get(&id).map(|v| {
                let v = v.read(cx);
                SavedPane {
                    env: v.env().to_string(),
                    cwd: v.cwd().clone(),
                    shell: v.request.shell,
                }
            })
        };
        let loose = self
            .tabs
            .iter()
            .filter(|t| t.set.is_none())
            .filter_map(|t| tab_file(&t.tree, &pane))
            .collect();
        let active = self.tabs.active_id();
        let session = Session {
            sets: self.loaded.names(),
            active: self.tabs.iter().position(|t| Some(t.id) == active),
            tree: self.tree.is_some(),
            tree_width: (self.tree.is_some() && self.tree_width != DEFAULT_TREE_WIDTH)
                .then_some(self.tree_width),
            loose,
        };
        if let Err(e) = write_session(&self.hydra.paths, &session) {
            window.push_notification(
                Notification::warning(format!("can't save what is open: {e}")).title("hydraui"),
                cx,
            );
        }
    }

    /// Notes the sets whose file changed or went away since they were loaded.
    pub(super) fn check_sets(&mut self, cx: &mut Context<Self>) {
        let changed: HashSet<String> = self.loaded.changed(&self.hydra.paths).into_iter().collect();
        if changed != self.changed_sets {
            self.changed_sets = changed;
            cx.notify();
        }
    }

    /// Reopens what was open at last exit: the sets, then the Loose tabs, then the file tree
    /// and the active tab. A set or tab that fails is skipped with a toast (and so dropped
    /// from the session). Returns whether any tab opened.
    pub(super) fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let session = read_session(&self.hydra.paths);
        let mut any = false;
        for name in &session.sets {
            any |= self.load_set(name, window, cx);
        }
        for tab in &session.loose {
            let spec = match resolve_loose(&self.set_context(), tab) {
                Ok(spec) => spec,
                Err(problems) => {
                    window.push_notification(
                        Notification::warning(problems.join(
                            "
",
                        ))
                        .title("Can't reopen a tab"),
                        cx,
                    );
                    continue;
                }
            };
            let next = &mut self.next_pane;
            let (tree, panes) = tab_layout(&spec, &mut || {
                *next += 1;
                *next
            });
            let focused = panes[0].0;
            for (id, request) in panes {
                self.create_pane(id, request, window, cx);
            }
            self.tabs.add_tree(None, None, tree, focused);
            any = true;
        }
        if !any {
            return false;
        }
        if session.tree && self.tree.is_none() {
            let width = f32::from(window.viewport_size().width);
            self.tree_width =
                tree_width_at(session.tree_width.unwrap_or(DEFAULT_TREE_WIDTH), width);
            self.toggle_tree(&FileTree, window, cx);
        }
        let saved = session
            .active
            .and_then(|i| self.tabs.iter().nth(i).map(|t| t.id));
        if let Some(id) = saved {
            self.tabs.activate(id);
        }
        // After the sets' own focus requests, so the active tab keeps the keyboard.
        if let Some(focused) = self.tabs.active().map(|t| t.focused) {
            self.focus_soon(focused, window, cx);
        }
        self.save_session(window, cx);
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
