//! The launcher (spec §7.4): Ctrl+Shift+P opens a palette over sets, environments, sign-in,
//! secrets and config, and Enter runs the entry's action.
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Window, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{Theme, ThemeMode, WindowExt, h_flex, v_flex};
use hydraui_sets::{list_sets, load_set};

use super::Workspace;
use crate::config::{self, ThemeChoice};
use crate::hydra::Hydra;
use crate::keys::Launcher;
use crate::launcher::{Action, SetState, entries};
use crate::palette::Palette;
use crate::pane_view::TermStyle;

impl Workspace {
    pub(super) fn open_launcher(
        &mut self,
        _: &Launcher,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ctx = self.set_context();
        let sets: Vec<(String, SetState)> = list_sets(&self.hydra.paths)
            .unwrap_or_default()
            .into_iter()
            .map(|name| {
                let state = if self.loaded.get(&name).is_some() {
                    SetState::Loaded
                } else {
                    match load_set(&ctx, &name) {
                        Ok(_) => SetState::Unloaded,
                        Err(e) => SetState::Invalid(e.to_string()),
                    }
                };
                (name, state)
            })
            .collect();
        let envs = self.hydra.env_names();
        let sign_ins: Vec<(String, String)> = envs
            .iter()
            .flat_map(|e| {
                self.hydra
                    .sign_ins(e)
                    .into_iter()
                    .map(move |t| (e.clone(), t))
            })
            .collect();
        let secrets: Vec<String> = envs
            .iter()
            .flat_map(|e| self.hydra.missing_secrets(e))
            .collect();
        let list = Rc::new(entries(&sets, &envs, &sign_ins, &secrets));
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (chosen, ws) = (list.clone(), workspace.clone());
            dialog.title("hydraui").child(
                Command::new(&state)
                    .items(
                        list.iter()
                            .map(|e| CommandItem::new().label(e.label.clone())),
                    )
                    .placeholder("Set, environment, sign in, secret…")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(entry) = chosen.get(ix.row) {
                            let action = entry.action.clone();
                            window.close_dialog(cx);
                            ws.update(cx, |ws, cx| ws.run(action, window, cx));
                        }
                    }),
            )
        });
    }

    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::LoadSet(name) => {
                self.load_set(&name, window, cx);
            }
            Action::UnloadSet(name) => self.unload_set(&name, false, window, cx),
            Action::OpenEnv(env) => {
                let cwd = self.focused_cwd(cx);
                self.open_tab(env, cwd, window, cx);
            }
            Action::SignIn { env, tool } => {
                let cwd = self.focused_cwd(cx);
                let shell = self.hydra.shell(self.ui.default_shell.as_deref());
                match self.hydra.sign_in_request(&env, &tool, cwd, shell) {
                    Ok(request) => self.open_request(request, window, cx),
                    Err(e) => window.push_notification(Notification::error(e).title("Sign in"), cx),
                }
            }
            Action::SetSecret(path) => self.secret_dialog(Some(path), window, cx),
            Action::NewSecret => self.secret_dialog(None, window, cx),
            Action::OpenConfigFolder => {
                let dir = self.hydra.paths.root().join("hydraui");
                let _ = std::fs::create_dir_all(&dir);
                cx.open_with_system(&dir);
            }
            Action::ReloadConfig => self.reload_config(window, cx),
            Action::Problem(message) => {
                window.push_notification(Notification::error(message).title("hydraui"), cx)
            }
        }
    }

    /// Asks for a secret's value in a masked box (and its `env/key` when not given) and stores
    /// it through hydra's store.
    fn secret_dialog(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let fixed = path.is_some();
        let name = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder("env/key, like work/linear");
            match path {
                Some(p) => input.default_value(p),
                None => input,
            }
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Value")
                .masked(true)
        });
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                let target = if fixed { &value } else { &name };
                target.update(cx, |s, cx| s.focus(window, cx));
            }
            let (n, v, ws) = (name.clone(), value.clone(), workspace.clone());
            dialog
                .title("Set secret")
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&name))
                        .child(Input::new(&value).mask_toggle()),
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
                        .child(Button::new("save").label("Save").primary().on_click(
                            move |_, window, cx| {
                                let path = n.read(cx).value().to_string();
                                let secret = v.read(cx).value().to_string();
                                match ws.read(cx).hydra.set_secret(&path, &secret) {
                                    Ok(()) => {
                                        window.close_dialog(cx);
                                        window.push_notification(
                                            Notification::success(format!(
                                                "stored {}",
                                                path.trim()
                                            ))
                                            .title("Set secret"),
                                            cx,
                                        );
                                    }
                                    Err(e) => window.push_notification(
                                        Notification::error(e).title("Set secret"),
                                        cx,
                                    ),
                                }
                            },
                        )),
                )
        });
    }

    /// Reads hydraui's and hydra's config.toml again: font, colours, theme, default
    /// environment and shell, and hydra's bindings apply now (new and open panes); key
    /// changes apply at the next start, since rebinding would also clear gpui-component's
    /// own keys.
    fn reload_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (ui, mut problems) = config::load(&self.hydra.paths);
        let (palette, colour_problems) = Palette::from_config(&ui.colors);
        problems.extend(colour_problems);
        if ui.keys != self.ui.keys {
            problems.push("key changes apply when hydraui next starts".into());
        }
        match Hydra::from_env() {
            Ok((hydra, problem)) => {
                problems.extend(problem);
                self.hydra = Arc::new(hydra);
            }
            Err(e) => problems.push(e),
        }
        let mode = match ui.theme {
            ThemeChoice::Dark => ThemeMode::Dark,
            ThemeChoice::Light => ThemeMode::Light,
        };
        Theme::change(mode, None, cx);
        self.style = Arc::new(TermStyle {
            font: ui.font.clone().into(),
            font_size: px(ui.font_size),
            palette,
        });
        let style = self.style.clone();
        for view in self.panes.values() {
            view.update(cx, |v, cx| v.set_style(style.clone(), cx));
        }
        let envs: HashSet<String> = self.tints.keys().cloned().collect();
        for env in envs {
            let tint = self.hydra.tint(&env);
            self.tints.insert(env, tint);
        }
        self.ui = ui;
        window.push_notification(Notification::info("Settings reloaded").title("hydraui"), cx);
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        cx.notify();
    }
}
