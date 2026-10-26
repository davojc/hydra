//! The launcher (spec §7.4): Ctrl+Shift+P opens a palette over environments, sign-in,
//! secrets and config, and Enter runs the entry's action.
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Hsla, Window, div, px, rgb};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, Theme, ThemeMode, WindowExt, h_flex, v_flex};

use super::{Workspace, env_item};
use crate::config::{self, ThemeChoice};
use crate::hydra::Hydra;
use crate::jev::JevKey;
use crate::keys::Launcher;
use crate::launcher::{Action, entries};
use crate::palette::Palette;
use crate::pane_view::TermStyle;

impl Workspace {
    pub(super) fn open_launcher(
        &mut self,
        _: &Launcher,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let list = Rc::new(entries(
            &envs,
            &sign_ins,
            &secrets,
            matches!(JevKey::app().get(), Ok(Some(_))),
        ));
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
                    .placeholder("Environment, sign in, secret…")
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

    pub(super) fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::OpenEnv(env) => self.add_env(&env, window, cx),
            Action::EditEnv(env) => match hydra_core::name::EnvName::parse(&env) {
                Ok(name) => self.show_file(self.hydra.paths.env_file(&name), window, cx),
                Err(e) => window.push_notification(
                    Notification::error(e.to_string()).title("Edit environment"),
                    cx,
                ),
            },
            Action::SignIn { env, tool } => self.sign_in(&env, &tool, window, cx),
            Action::SetSecret(path) => self.secret_dialog(Some(path), window, cx),
            Action::NewSecret => self.secret_dialog(None, window, cx),
            Action::DeleteEnv => {
                self.choose_env("Delete which environment?", window, cx, |ws, env, w, cx| {
                    ws.delete_env_dialog(env, w, cx)
                })
            }
            Action::SetEnvHome => {
                self.choose_env("Set the home of…", window, cx, |ws, env, w, cx| {
                    ws.set_env_home(env, w, cx)
                })
            }
            Action::NewEnv => self.new_env_dialog(window, cx),
            Action::OpenFile => self.open_file_finder(window, cx),
            Action::SearchFiles => self.search_dialog(window, cx),
            Action::OpenLayout => self.open_layout_file(window, cx),
            Action::SaveLayout => self.save_layout_file(window, cx),
            Action::StartPage => {
                let _ = crate::window_layout::spawn_hydraui(None);
            }
            Action::StartOrchestrator => self.start_orchestrator(window, cx),
            Action::NewPipeline => self.new_pipeline(None, window, cx),
            Action::ConnectSlack => {
                let envs = self.hydra.env_names();
                self.choose_env_from(envs, "Connect Slack for", window, cx, |ws, env, w, cx| {
                    ws.connect_slack(env, w, cx)
                });
            }
            Action::EditPipelines => {
                let root = self.hydra.paths.root().to_path_buf();
                if let Err(e) = crate::pipeline::ensure_example(&root) {
                    crate::action_log::write(format!("example pipeline: {e}"));
                }
                let _ = std::fs::create_dir_all(crate::pipeline::folder(&root));
                cx.open_with_system(&crate::pipeline::folder(&root));
            }
            Action::OpenConfigFolder => {
                let dir = self.hydra.paths.root().join("hydraui");
                let _ = std::fs::create_dir_all(&dir);
                cx.open_with_system(&dir);
            }
            Action::OpenLog => {
                let path = crate::action_log::file_for(self.hydra.paths.root());
                if !path.exists() {
                    crate::action_log::write("log opened");
                }
                self.show_file(path, window, cx);
            }
            Action::ReloadConfig => self.reload_config(window, cx),
            Action::CheckForUpdates => self.check_for_updates(window, cx),
            Action::AllBoards => self.show_all_tasks(window, cx),
            Action::Shortcuts => self.shortcuts(window, cx),
            Action::Settings => {
                self.open_settings(super::settings_tab::Section::General, window, cx)
            }
            Action::SetJevKey => self.set_jev_key_dialog(window, cx),
            Action::RemoveJevKey => self.remove_jev_key_dialog(window, cx),
            Action::EditJevQuestions => {
                let path = crate::jev::settings_file(self.hydra.paths.root());
                if !path.exists() {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Err(e) = std::fs::write(&path, crate::jev::DEFAULT_SETTINGS_TEXT) {
                        window.push_notification(
                            Notification::error(format!("Can't write jev.toml: {e}")).title("Jev"),
                            cx,
                        );
                        return;
                    }
                }
                self.show_file(path, window, cx);
            }
        }
    }

    /// Signs in to `tool` in a new terminal of `env`, adding its tab when it isn't open
    /// (env-tabs design §5).
    pub(super) fn sign_in(
        &mut self,
        env: &str,
        tool: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.env_cwd(env, cx);
        match self.hydra.sign_in_request(env, tool, cwd, self.shell()) {
            Ok(request) => {
                self.open_terminal(env, request, window, cx);
            }
            Err(e) => window.push_notification(Notification::error(e).title("Sign in"), cx),
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

    /// Asks for the Jev API key in a masked box and stores it in the credential store.
    fn set_jev_key_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Jev API key")
                .masked(true)
        });
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                key.update(cx, |s, cx| s.focus(window, cx));
            }
            let k = key.clone();
            dialog
                .title("Jev API key")
                .child(Input::new(&key).mask_toggle())
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
                                let value = k.read(cx).value().to_string();
                                match JevKey::app().set(&value) {
                                    Ok(()) => {
                                        window.close_dialog(cx);
                                        window.push_notification(
                                            Notification::success("Jev API key saved")
                                                .title("Jev API key"),
                                            cx,
                                        );
                                    }
                                    Err(e) => window.push_notification(
                                        Notification::error(e).title("Jev API key"),
                                        cx,
                                    ),
                                }
                            },
                        )),
                )
        });
    }

    /// Asks before forgetting the stored Jev API key.
    fn remove_jev_key_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Jev API key")
                .child("Remove the Jev API key?")
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("remove").label("Remove").danger().on_click(
                            |_, window, cx| match JevKey::app().remove() {
                                Ok(_) => {
                                    window.close_dialog(cx);
                                    window.push_notification(
                                        Notification::success("Jev API key removed")
                                            .title("Jev API key"),
                                        cx,
                                    );
                                }
                                Err(e) => window.push_notification(
                                    Notification::error(e).title("Jev API key"),
                                    cx,
                                ),
                            },
                        )),
                )
        });
    }

    /// Asks which environment (each with its colour, as on its tabs), then calls `then`.
    pub(super) fn choose_env(
        &mut self,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let envs = self.hydra.env_names();
        self.choose_env_from(envs, title, window, cx, then);
    }

    /// `choose_env` over `envs` only.
    fn choose_env_from(
        &mut self,
        envs: Vec<String>,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if envs.is_empty() {
            window.push_notification(
                Notification::info("There are no environments yet.").title("Environments"),
                cx,
            );
            return;
        }
        let swatches: Vec<(String, Hsla)> = envs
            .iter()
            .map(|e| (e.clone(), rgb(self.hydra.tint(e)).into()))
            .collect();
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let then = Rc::new(then);
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (choices, workspace, then) = (envs.clone(), workspace.clone(), then.clone());
            dialog.title(title).child(
                Command::new(&state)
                    .items(swatches.iter().map(|(e, c)| env_item(e, Some(*c))))
                    .placeholder("Environment")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(env) = choices.get(ix.row).cloned() {
                            window.close_dialog(cx);
                            workspace.update(cx, |ws, cx| then(ws, env, window, cx));
                        }
                    }),
            )
        });
    }

    /// **Start orchestrator…** (orchestrator spec §3.0): an environment with Claude (the MCP
    /// server is registered only there), then a pipeline, then a terminal titled `orchestrator`
    /// running Claude with the orchestrator's first prompt.
    fn start_orchestrator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let envs: Vec<String> = self
            .hydra
            .env_names()
            .into_iter()
            .filter(|e| self.hydra.sign_ins(e).iter().any(|t| t == "claude"))
            .collect();
        if envs.is_empty() {
            window.push_notification(
                Notification::info(
                    "No environment has Claude set up in hydra. Run `hydra add claude <env>`, \
                     then Reload config and sign in from a new terminal.",
                )
                .title("Start orchestrator"),
                cx,
            );
            return;
        }
        self.choose_env_from(
            envs,
            "Start orchestrator in",
            window,
            cx,
            |ws, env, window, cx| ws.choose_pipeline(env, window, cx),
        );
    }

    /// Picks the orchestrator's pipeline (the example is written when there is none); one
    /// pipeline is taken without asking.
    pub(super) fn choose_pipeline(
        &mut self,
        env: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        if let Err(e) = crate::pipeline::ensure_example(&root) {
            crate::action_log::write(format!("example pipeline: {e}"));
        }
        let names = crate::pipeline::names(&root);
        // Each file by its name and the name the views show, so they're told apart; then a
        // new one.
        let mut labels = pipeline_labels(&root, &names);
        labels.push("New pipeline…".into());
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (choices, workspace, env) = (names.clone(), workspace.clone(), env.clone());
            dialog.title("Pipeline").child(
                Command::new(&state)
                    .items(labels.iter().map(|l| CommandItem::new().label(l.clone())))
                    .placeholder("Pipeline")
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        let env = env.clone();
                        match choices.get(ix.row).cloned() {
                            Some(name) => workspace
                                .update(cx, |ws, cx| ws.open_orchestrator(&env, &name, window, cx)),
                            None => workspace
                                .update(cx, |ws, cx| ws.new_pipeline(Some(env), window, cx)),
                        }
                    }),
            )
        });
    }

    /// **New pipeline…**: start blank or from a copy of a pipeline, name it, then its
    /// orchestrator starts (in `env`, else one picked) with its settings open to fill in.
    pub(super) fn new_pipeline(
        &mut self,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        let names = crate::pipeline::names(&root);
        let mut labels = vec!["Blank: one stage to fill in".to_string()];
        labels.extend(
            pipeline_labels(&root, &names)
                .into_iter()
                .map(|l| format!("Copy of {l}")),
        );
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (names, workspace, env) = (names.clone(), workspace.clone(), env.clone());
            dialog.title("New pipeline: start from").child(
                Command::new(&state)
                    .items(labels.iter().map(|l| CommandItem::new().label(l.clone())))
                    .placeholder("Blank, or a pipeline to copy")
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        let from = ix.row.checked_sub(1).and_then(|i| names.get(i).cloned());
                        let env = env.clone();
                        workspace.update(cx, |ws, cx| ws.name_pipeline(from, env, window, cx));
                    }),
            )
        });
    }

    /// Names the new pipeline (copied from `from`, else blank) and writes its file.
    fn name_pipeline(
        &mut self,
        from: Option<String>,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. PRE bugs, or Release notes"));
        let ws = cx.entity();
        let first = Cell::new(true);
        let hint = match &from {
            Some(f) => format!(
                "A copy of {f}: its stages, bars and prompts. It won't watch Linear or start at \
                 launch until you turn those on in its settings."
            ),
            None => "One stage to rename and fill in; add more in its settings.".to_string(),
        };
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let submit = {
                let (ws, input, from, env) = (ws.clone(), input.clone(), from.clone(), env.clone());
                move |window: &mut Window, cx: &mut gpui::App| {
                    let title = input.read(cx).value().trim().to_string();
                    let root = ws.read(cx).hydra.paths.root().to_path_buf();
                    match crate::pipeline::create(&root, &title, from.as_deref()) {
                        Ok(name) => {
                            window.close_dialog(cx);
                            crate::action_log::write(format!(
                                "new pipeline {name} ({})",
                                from.as_deref()
                                    .map_or("blank".into(), |f| format!("from {f}"))
                            ));
                            let env = env.clone();
                            ws.update(cx, |ws, cx| ws.start_new_pipeline(name, env, window, cx));
                        }
                        Err(e) => window
                            .push_notification(Notification::warning(e).title("New pipeline"), cx),
                    }
                }
            };
            let on_ok = submit.clone();
            dialog
                .title("Name the pipeline")
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(
                    v_flex().gap_2().child(Input::new(&input)).child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(hint.clone()),
                    ),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("new-pipeline-cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("new-pipeline-go")
                                .label("Create and edit")
                                .primary()
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                )
        });
    }

    /// The new pipeline's orchestrator, in `env` or one picked, with its settings open.
    fn start_new_pipeline(
        &mut self,
        name: String,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open =
            move |ws: &mut Self, env: String, window: &mut Window, cx: &mut Context<Self>| {
                ws.open_orchestrator(&env, &name, window, cx);
                let orch = ws
                    .orchestrators
                    .iter()
                    .find(|(_, f)| **f == name)
                    .map(|(id, _)| *id);
                if let Some(orch) = orch {
                    ws.open_pipeline_settings(orch, window, cx);
                }
            };
        match env {
            Some(env) => open(self, env, window, cx),
            None => {
                let envs: Vec<String> = self
                    .hydra
                    .env_names()
                    .into_iter()
                    .filter(|e| self.hydra.sign_ins(e).iter().any(|t| t == "claude"))
                    .collect();
                self.choose_env_from(envs, "Start its orchestrator in", window, cx, open);
            }
        }
    }

    /// Starts `pipeline` in `env` with one orchestrator; a pipeline allowed several tickets at
    /// once gets more as its work needs them (`Workspace::slot_for_work`).
    fn open_orchestrator(
        &mut self,
        env: &str,
        pipeline: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_orchestrator_slot(env, pipeline, window, cx);
    }

    /// How many orchestrators run `pipeline` in `env`.
    fn open_slots(&self, env: &str, pipeline: &str, cx: &gpui::App) -> usize {
        self.orchestrators
            .iter()
            .filter(|(id, f)| {
                *f == pipeline && self.panes.get(id).is_some_and(|v| v.read(cx).env() == env)
            })
            .count()
    }

    /// One orchestrator for `pipeline`, its pane; the first in `env` holds its trigger.
    pub(super) fn open_orchestrator_slot(
        &mut self,
        env: &str,
        pipeline: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::envs::PaneId {
        let open = self.open_slots(env, pipeline, cx);
        let first = open == 0;
        let want = crate::pipeline::load(self.hydra.paths.root(), pipeline)
            .map_or(1, |p| usize::from(p.runs_at_once));
        // Several at once are numbered under the pipeline's card.
        let name = if want > 1 {
            format!("orchestrator {}", open + 1)
        } else {
            "orchestrator".to_string()
        };
        let shell = self.shell();
        let cwd = super::orchestrator::orchestrator_folder(
            &self.env_cwd(env, cx),
            &self.hydra.user_home,
            self.hydra.paths.root(),
            pipeline,
        );
        if let Err(e) = std::fs::create_dir_all(&cwd) {
            crate::action_log::write(format!("orchestrator folder {}: {e}", cwd.display()));
        }
        let mut request = crate::hydra::PaneRequest::new(env, cwd, shell);
        request.command = Some(crate::hydra::command_line(
            shell,
            &[
                "claude".to_string(),
                super::orchestrator::orchestrator_prompt(
                    pipeline,
                    crate::pipeline::load(self.hydra.paths.root(), pipeline)
                        .is_ok_and(|p| p.has_trigger()),
                ),
            ],
        ));
        let id = self.open_terminal(env, request, window, cx);
        // Nothing is handed to it until Claude has finished its first turn.
        self.starting_up.insert(id, std::time::Instant::now());
        if let Some(view) = self.panes.get(&id).cloned() {
            view.update(cx, |v, cx| v.set_name(Some(name), cx));
        }
        // Its card shows at once, so its settings can be set before the first ticket.
        self.orchestrators.insert(id, pipeline.to_string());
        // A pipeline that asks for it watches Linear from the start (`[linear] watch`).
        let watch = first
            && crate::pipeline::load(self.hydra.paths.root(), pipeline).is_ok_and(|p| p.watches());
        if watch {
            let w = self.linear_watch.entry(id).or_default();
            w.on = true;
        }
        crate::action_log::write(format!(
            "started an orchestrator for {pipeline} in {env}{}",
            if watch { ", in Automatic" } else { "" }
        ));
        id
    }

    /// At launch: each pipeline with `[autostart] env` gets its orchestrator in that
    /// environment, once (the first piece of roadmap phase 3, "start what you need at
    /// launch"). `HYDRAUI_NO_AUTOSTART` turns it off, for test windows.
    pub(super) fn autostart_pipelines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::env::var_os("HYDRAUI_NO_AUTOSTART").is_some() {
            return;
        }
        let root = self.hydra.paths.root().to_path_buf();
        let envs = self.hydra.env_names();
        for name in crate::pipeline::names(&root) {
            let Some(env) = crate::pipeline::load(&root, &name)
                .ok()
                .and_then(|p| p.autostart)
            else {
                continue;
            };
            if !envs.contains(&env) {
                crate::action_log::write(format!(
                    "autostart: pipeline {name} names environment {env}, which doesn't exist"
                ));
                continue;
            }
            if self.open_slots(&env, &name, cx) > 0 {
                continue;
            }
            // Its window shows the pipelines, not the start page.
            if self.is_start_page() {
                self.leave_start_page(window, cx);
            }
            // A run of it that hydraui closed on carries on, rather than a second orchestrator
            // starting beside its Interrupted card (orchestrator §13).
            let left = self
                .interrupted
                .iter()
                .position(|i| i.run.pipeline.file == name && i.env == env);
            let Some(idx) = left else {
                self.open_orchestrator_slot(&env, &name, window, cx);
                continue;
            };
            self.resume_run(idx, window, cx);
            let watch = crate::pipeline::load(&root, &name).is_ok_and(|p| p.watches());
            let orch = self
                .orchestrators
                .iter()
                .find(|(_, f)| **f == name)
                .map(|(id, _)| *id);
            if let (true, Some(orch)) = (watch, orch) {
                let w = self.linear_watch.entry(orch).or_default();
                w.on = true;
            }
            crate::action_log::write(format!(
                "autostart: {name} resumed the run hydraui closed on{}",
                if watch { ", in Automatic" } else { "" }
            ));
        }
    }

    /// Deletes `env` after a confirmation, as `hydra env rm` does (spec §7.4); its saved
    /// logins go too only when ticked. Refused while hydraui has its terminals open.
    pub(super) fn delete_env_dialog(
        &mut self,
        env: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .panes
            .values()
            .filter(|v| v.read(cx).env() == env)
            .count();
        if open > 0 {
            if let Err(e) = self.hydra.delete_env(&env, open, false) {
                window.push_notification(Notification::error(e).title("Delete environment"), cx);
            }
            return;
        }
        // Say exactly what goes (spec §7.4).
        let folder = self
            .hydra
            .env_folder(&env)
            .map(|f| f.display().to_string())
            .unwrap_or_default();
        let mut secrets = self.hydra.env_secrets(&env);
        secrets.sort();
        let secrets = if secrets.is_empty() {
            "Secrets: none".to_string()
        } else {
            format!("Secrets: {}", secrets.join(", "))
        };
        let logins = Rc::new(Cell::new(false));
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let (ws, env, ticked, flag) =
                (workspace.clone(), env.clone(), logins.get(), logins.clone());
            dialog
                .title("Delete environment")
                .child(
                    v_flex()
                        .gap_2()
                        .child(format!("Delete {env}? These go:"))
                        .child(
                            v_flex()
                                .pl_2()
                                .text_sm()
                                .text_color(muted)
                                .child(format!("Folder: {folder}"))
                                .child(secrets.clone()),
                        )
                        .child(
                            Checkbox::new("delete-logins")
                                .label("Also delete its saved logins")
                                .checked(ticked)
                                .on_click(move |on, window, _| {
                                    flag.set(*on);
                                    window.refresh();
                                }),
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
                        .child(Button::new("delete").label("Delete").danger().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                ws.update(cx, |ws, cx| ws.finish_delete(&env, ticked, window, cx));
                            },
                        )),
                )
        });
    }

    fn finish_delete(
        &mut self,
        env: &str,
        logins: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.hydra.delete_env(env, 0, logins) {
            Ok(report) => {
                let mut text = format!("Deleted {env}");
                if !report.secrets.is_empty() {
                    text.push_str(&format!(" and its secrets: {}", report.secrets.join(", ")));
                }
                if report.state_deleted {
                    text.push_str("; its saved logins too");
                }
                if !report.bindings.is_empty() {
                    text.push_str(&format!(
                        ". Still bound in config.toml: {}",
                        report.bindings.join(", ")
                    ));
                }
                window
                    .push_notification(Notification::success(text).title("Delete environment"), cx);
                // An environment with no terminals can still have a tab: close it.
                self.close_env_now(env, window, cx);
                self.reload_config(window, cx);
            }
            Err(e) => {
                window.push_notification(Notification::error(e).title("Delete environment"), cx)
            }
        }
    }

    /// Asks for a folder, then makes it `env`'s home and binds it to `env`.
    pub(super) fn set_env_home(
        &mut self,
        env: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_dialog_path(true, window, cx, move |ws, dir, window, cx| {
            match ws.hydra.set_env_home(&env, &dir) {
                Ok(()) => {
                    window.push_notification(
                        Notification::success(format!(
                            "New {env} terminals open in {}, and the folder is bound to {env}",
                            dir.display()
                        ))
                        .title("Set environment home"),
                        cx,
                    );
                    ws.reload_config(window, cx);
                }
                Err(e) => window
                    .push_notification(Notification::error(e).title("Set environment home"), cx),
            }
        });
    }

    /// Names and creates an environment, then adds its tab. The name is checked as it
    /// is typed; Create stays off until it is free and valid.
    pub(super) fn new_env_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name, like work"));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                name.update(cx, |s, cx| s.focus(window, cx));
            }
            let typed = name.read(cx).value().to_string();
            let problem = workspace.read(cx).hydra.new_env_problem(&typed);
            let muted = cx.theme().muted_foreground;
            let danger = cx.theme().danger;
            let hint = match &problem {
                Some(p) if !p.is_empty() => div().text_sm().text_color(danger).child(p.clone()),
                _ => div()
                    .text_sm()
                    .text_color(muted)
                    .child("Lowercase letters, digits and '-'."),
            };
            let ws = workspace.clone();
            dialog
                .title("New environment")
                .child(v_flex().gap_2().child(Input::new(&name)).child(hint))
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
                            Button::new("create")
                                .label("Create")
                                .primary()
                                .disabled(problem.is_some())
                                .on_click(move |_, window, cx| {
                                    let env = typed.trim().to_string();
                                    ws.update(cx, |ws, cx| ws.create_env(env, window, cx));
                                }),
                        ),
                )
        });
    }

    fn create_env(&mut self, env: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.hydra.create_env(&env) {
            window.push_notification(Notification::error(e).title("New environment"), cx);
            return;
        }
        window.close_dialog(cx);
        window.push_notification(
            Notification::success(format!(
                "Created {env}. Click its name in the status bar to add accounts."
            ))
            .title("New environment"),
            cx,
        );
        self.add_env(&env, window, cx);
    }

    /// Reads hydraui's and hydra's config.toml again: font, colours, theme, default
    /// environment and shell, and hydra's bindings apply now (new and open panes); key
    /// changes apply at the next start, since rebinding would also clear gpui-component's
    /// own keys.
    fn reload_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_config(true, window, cx);
    }

    /// Applies config.toml as Reload config does; `announce` says so in a toast. Problems are
    /// always shown.
    pub(super) fn apply_config(
        &mut self,
        announce: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            font: crate::config::terminal_font(&ui.font, &cx.text_system().all_font_names()).into(),
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
        if let Some(server) = crate::mcp::server::get() {
            crate::mcp::server::lock(server.registry()).min_week_left = ui.min_week_left;
        }
        self.ui = ui;
        if announce {
            window.push_notification(Notification::info("Settings reloaded").title("hydraui"), cx);
        }
        for p in problems {
            window.push_notification(Notification::warning(p).title("hydraui settings"), cx);
        }
        cx.notify();
    }
}

/// Each pipeline file by its name and, when it differs, the name the views show.
fn pipeline_labels(root: &std::path::Path, names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|n| match crate::pipeline::load(root, n) {
            Ok(p) if p.name != *n => format!("{n} · {}", p.name),
            _ => n.clone(),
        })
        .collect()
}
