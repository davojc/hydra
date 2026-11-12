//! The launcher (spec §7.4): Ctrl+Shift+P opens a palette over environments, sign-in,
//! secrets and config, and Enter runs the entry's action.
use crate::inbox::{Note, PushNote};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Hsla, SharedString, Window, div, px, rgb};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, Theme, ThemeMode, WindowExt, h_flex, v_flex};
use hydra_core::envedit::ToolValues;

use super::{Workspace, env_item};
use crate::accounts::SIGN_IN_TOOLS;
use crate::config::{self, ThemeChoice};
use crate::hydra::Hydra;
use crate::jev::JevKey;
use crate::keys::Launcher;
use crate::launcher::{Action, entries};
use crate::palette::Palette;
use crate::pane_view::TermStyle;
use crate::setup::{ESSENTIALS, label as tool_label};

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
                Err(e) => {
                    window.push_note(Note::error(e.to_string()).title("Edit environment"), cx)
                }
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
            Action::ExportPipeline => self.choose_pipeline_to_export(window, cx),
            Action::ImportPipeline => self.import_pipeline(None, window, cx),
            Action::ConnectSlack => {
                let envs = self.hydra.env_names();
                self.choose_env_from(envs, "Connect Slack for", window, cx, |ws, env, w, cx| {
                    ws.connect_slack(env, w, cx)
                });
            }
            Action::ConnectGmail => {
                let envs = self.hydra.env_names();
                self.choose_env_from(envs, "Connect Gmail for", window, cx, |ws, env, w, cx| {
                    ws.connect_gmail(env, w, cx)
                });
            }
            Action::CheckGmail => {
                let envs = self.hydra.env_names();
                self.choose_env_from(envs, "Check Gmail in", window, cx, |ws, env, w, cx| {
                    ws.check_gmail(env, w, cx)
                });
            }
            Action::CheckMailNow => self.mail_tick(true, window, cx),
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
                        window.push_note(
                            Note::error(format!("Can't write jev.toml: {e}")).title("Jev"),
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
            Err(e) => window.push_note(Note::error(e).title("Sign in"), cx),
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
                                        window.push_note(
                                            Note::success(format!("stored {}", path.trim()))
                                                .title("Set secret"),
                                            cx,
                                        );
                                    }
                                    Err(e) => {
                                        window.push_note(Note::error(e).title("Set secret"), cx)
                                    }
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
                                        window.push_note(
                                            Note::success("Jev API key saved").title("Jev API key"),
                                            cx,
                                        );
                                    }
                                    Err(e) => {
                                        window.push_note(Note::error(e).title("Jev API key"), cx)
                                    }
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
                                    window.push_note(
                                        Note::success("Jev API key removed").title("Jev API key"),
                                        cx,
                                    );
                                }
                                Err(e) => window.push_note(Note::error(e).title("Jev API key"), cx),
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
            window.push_note(
                Note::info("There are no environments yet.").title("Environments"),
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
            window.push_note(
                Note::info(
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

    /// **Start pipeline** (core spec §67): a card per pipeline, in its colour with its icon,
    /// saying where its work comes from, and a last card to create one. The example is written
    /// when there is none.
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
        let mine = my_pipelines(&root);
        let workspace = cx.entity();
        let scroll = gpui::ScrollHandle::new();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title("Start a pipeline")
                .w(px(640.))
                .child(start_cards(&mine, &workspace, &env, &scroll, cx))
        });
    }

    /// **Create pipeline** (core spec §66): one chooser with three ways to start. Start fresh
    /// (blank, or a starter), copy one of yours, or import a file someone sent. Each goes on to
    /// name it (or pick the file), then its orchestrator starts (in `env`, else one picked)
    /// with its settings open.
    pub(super) fn new_pipeline(
        &mut self,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        let mine = my_pipelines(&root);
        let workspace = cx.entity();
        let scroll = gpui::ScrollHandle::new();
        let in_swarm = self.swarms.iter().any(|s| !s.removed);
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title("Create a pipeline")
                .w(px(600.))
                .child(create_choices(
                    &mine, &workspace, &env, &scroll, in_swarm, cx,
                ))
        });
    }

    /// **Share to a swarm** (core spec §69): picks one of your swarms, then writes pipeline
    /// `file` into its `pipelines` folder for your team.
    pub(super) fn share_to_swarm_dialog(
        &mut self,
        file: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let swarms: Vec<(String, std::path::PathBuf, bool, bool)> = self
            .swarms
            .iter()
            .filter(|s| !s.removed)
            .map(|s| {
                (
                    s.name.clone(),
                    s.folder.clone(),
                    s.my_role != hydra_swarms::Role::Reader,
                    crate::pipeline_share::syncs_toml(&s.types),
                )
            })
            .collect();
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let rows = swarms
                .iter()
                .enumerate()
                .map(|(i, (name, folder, writes, toml))| {
                    let (workspace, file, name, folder) = (
                        workspace.clone(),
                        file.clone(),
                        name.clone(),
                        folder.clone(),
                    );
                    let (writes, toml) = (*writes, *toml);
                    h_flex()
                        .id(SharedString::from(format!("share-swarm-{i}")))
                        .gap_3()
                        .items_center()
                        .px(px(14.))
                        .py(px(10.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgb(0x1f2524))
                        .when(writes, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.border_color(rgb(crate::orchestrator_theme::TEAL)))
                        })
                        .child(
                            gpui_component::Icon::new(gpui_kit_assets::IconName::Users)
                                .size(px(16.))
                                .text_color(rgb(crate::orchestrator_theme::TEAL)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(name.clone()))
                                .child(div().text_xs().text_color(muted).child(
                                    match (writes, toml) {
                                        (false, _) => {
                                            "You can only read this swarm, so you can't share \
                                             to it"
                                        }
                                        (true, true) => "Shared as a .toml file",
                                        (true, false) => {
                                            "Shared as a .md note: this swarm doesn't sync \
                                             .toml files"
                                        }
                                    },
                                )),
                        )
                        .when(writes, |d| {
                            d.on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let (file, name, folder) =
                                    (file.clone(), name.clone(), folder.clone());
                                workspace.update(cx, |ws, cx| {
                                    ws.share_to_swarm(&file, &name, &folder, toml, window, cx)
                                });
                            })
                        })
                });
            dialog.title("Share to a swarm").w(px(520.)).child(
                v_flex()
                    .gap_2()
                    .pb_2()
                    .child(div().text_xs().text_color(muted).child(
                        "Pick the swarm to share it with. It goes in that swarm's pipelines \
                         folder, without this machine's own settings.",
                    ))
                    .children(rows),
            )
        });
    }

    fn share_to_swarm(
        &mut self,
        file: &str,
        swarm: &str,
        folder: &std::path::Path,
        toml: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let note = match crate::pipeline_share::share(self.hydra.paths.root(), file, folder, toml) {
            Ok((path, replaced)) => {
                crate::action_log::write(format!(
                    "shared pipeline {file} to swarm {swarm}: {}",
                    path.display()
                ));
                Note::success(format!(
                    "{} {swarm}. Your team adds it with Create pipeline, then Import from a \
                     swarm.",
                    if replaced {
                        "Updated its copy in"
                    } else {
                        "Shared to"
                    }
                ))
                .title("Share to a swarm")
            }
            Err(e) => Note::error(e).title("Share to a swarm"),
        };
        window.push_note(note, cx);
    }

    /// **Import from a swarm** (core spec §69): the pipelines your swarms share; picking one
    /// adds it as a new pipeline that takes nothing until turned on, then opens its settings.
    fn import_from_swarm_dialog(
        &mut self,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found: Vec<(String, crate::pipeline_share::Shared)> = self
            .swarms
            .iter()
            .filter(|s| !s.removed)
            .flat_map(|s| {
                crate::pipeline_share::shared(&s.folder)
                    .into_iter()
                    .map(|p| (s.name.clone(), p))
                    .collect::<Vec<_>>()
            })
            .collect();
        let workspace = cx.entity();
        let scroll = gpui::ScrollHandle::new();
        window.open_dialog(cx, move |dialog, _, cx| {
            use crate::orchestrator_theme::{self as theme, color};
            let muted = cx.theme().muted_foreground;
            let body = if found.is_empty() {
                div()
                    .text_sm()
                    .text_color(muted)
                    .pb_2()
                    .child(
                        "Nothing is shared in your swarms yet. A teammate shares one with Share \
                         to a swarm, at the top of a pipeline's settings.",
                    )
                    .into_any_element()
            } else {
                scrolled(
                    v_flex()
                        .id("swarm-pipelines")
                        .gap_2()
                        .children(found.iter().enumerate().map(|(i, (swarm, p))| {
                            let accent = p.color.unwrap_or(theme::TEAL);
                            let (workspace, env, text) =
                                (workspace.clone(), env.clone(), p.text.clone());
                            h_flex()
                                .id(SharedString::from(format!("swarm-pipeline-{i}")))
                                .gap_3()
                                .items_center()
                                .px(px(14.))
                                .py(px(10.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(color(accent).opacity(0.3))
                                .hover(|d| d.border_color(color(accent)))
                                .cursor_pointer()
                                .child(crate::pipeline_icon::mark(p.icon.as_deref(), accent, 16.))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_sm().truncate().child(p.name.clone()))
                                        .child(div().text_xs().text_color(muted).truncate().child(
                                            format!(
                                                "{swarm} · {} · {}",
                                                p.source,
                                                crate::settings_words::stages(p.stages)
                                            ),
                                        )),
                                )
                                .child(div().text_xs().text_color(color(theme::TEAL)).child("Add"))
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let (env, text) = (env.clone(), text.clone());
                                    workspace.update(cx, |ws, cx| {
                                        ws.import_text(&text, "a swarm", env, window, cx)
                                    });
                                })
                        })),
                    &scroll,
                    360.,
                )
            };
            dialog.title("Import from a swarm").w(px(560.)).child(
                v_flex()
                    .gap_3()
                    .pb_2()
                    .child(div().text_xs().text_color(muted).child(
                        "Pipelines your swarms share. The one you add is yours, and it starts \
                         switched off for you to check.",
                    ))
                    .child(body),
            )
        });
    }

    /// Adds `text` (a pipeline from `from`, a file or a swarm) as a new pipeline, then opens it.
    fn import_text(
        &mut self,
        text: &str,
        from: &str,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        match crate::pipeline::import(&root, text) {
            Ok(name) => {
                crate::action_log::write(format!("imported pipeline {name} from {from}"));
                window.push_note(
                    Note::success("Added. Check its folders and skills, then turn its trigger on.")
                        .title(format!("Imported {name}")),
                    cx,
                );
                self.start_new_pipeline(name, env, window, cx);
            }
            Err(e) => window.push_note(Note::warning(e).title("Import pipeline"), cx),
        }
    }

    /// **Export pipeline…**: picks the pipeline, then where to save it.
    fn choose_pipeline_to_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.hydra.paths.root().to_path_buf();
        let names = crate::pipeline::names(&root);
        let labels = pipeline_labels(&root, &names);
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (names, workspace) = (names.clone(), workspace.clone());
            dialog.title("Export pipeline").child(
                Command::new(&state)
                    .items(labels.iter().map(|l| CommandItem::new().label(l.clone())))
                    .placeholder("Pipeline")
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        if let Some(name) = names.get(ix.row).cloned() {
                            workspace.update(cx, |ws, cx| ws.export_pipeline(name, window, cx));
                        }
                    }),
            )
        });
    }

    /// Saves pipeline `name` as a file to send someone (core spec §44): the Save panel starts
    /// in Downloads with `<name>.toml` filled in.
    pub(super) fn export_pipeline(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        let text = match crate::pipeline::export(&root, &name) {
            Ok(t) => t,
            Err(e) => {
                window.push_note(Note::error(e).title("Export pipeline"), cx);
                return;
            }
        };
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{name}.toml")));
        cx.spawn_in(window, async move |this, cx| {
            let note = match rx.await {
                Ok(Ok(Some(path))) => {
                    let path = if path.extension().is_some() {
                        path
                    } else {
                        path.with_extension("toml")
                    };
                    match std::fs::write(&path, text) {
                        Ok(()) => {
                            crate::action_log::write(format!(
                                "exported pipeline {name} to {}",
                                path.display()
                            ));
                            Note::success(format!(
                                "Saved to {}. Send it on: they add it with Create pipeline, then \
                                 Import a file.",
                                path.display()
                            ))
                        }
                        Err(e) => Note::error(format!("can't save {}: {e}", path.display())),
                    }
                }
                Ok(Err(e)) => Note::error(e.to_string()),
                _ => return,
            };
            let _ = this.update_in(cx, |_, window, cx| {
                window.push_note(note.title("Export pipeline"), cx)
            });
        })
        .detach();
    }

    /// **Import pipeline…** (core spec §44): picks a pipeline file, adds it as a new pipeline
    /// that takes nothing until turned on, then starts its orchestrator (in `env`, else one
    /// picked) with its settings open to check.
    pub(super) fn import_pipeline(
        &mut self,
        env: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_dialog_path(false, window, cx, move |ws, path, window, cx| {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    ws.import_text(&text, &path.display().to_string(), env.clone(), window, cx)
                }
                Err(e) => window.push_note(
                    Note::warning(format!("can't read {}: {e}", path.display()))
                        .title("Import pipeline"),
                    cx,
                ),
            }
        });
    }

    /// Names the new pipeline (copied from `from`, else blank) and writes its file.
    pub(super) fn name_pipeline(
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
            Some(f) if f == crate::pipeline::EMAIL_TRIAGE => "Takes its work from Gmail on \
                weekday mornings: Jev sorts each email, then Claude investigates and drafts a \
                reply for a person. Set the mailbox and categories in its settings."
                .to_string(),
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
                                from.as_deref().map_or("blank".into(), |f| {
                                    if f == crate::pipeline::EMAIL_TRIAGE {
                                        "email triage starter".into()
                                    } else {
                                        format!("from {f}")
                                    }
                                })
                            ));
                            let env = env.clone();
                            ws.update(cx, |ws, cx| ws.start_new_pipeline(name, env, window, cx));
                        }
                        Err(e) => window.push_note(Note::warning(e).title("New pipeline"), cx),
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
        // One orchestrator holds a pipeline's tickets; a second one, started by hand, is numbered.
        let name = if open > 0 {
            format!("Orchestrator {}", open + 1)
        } else {
            "Orchestrator".to_string()
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

    /// The pipelines open now, each by its first orchestrator: its environment and whether it
    /// is in Automatic (core spec §38).
    fn open_pipelines_now(&self, cx: &gpui::App) -> Vec<crate::open_pipelines::OpenPipeline> {
        let mut firsts: Vec<(crate::envs::PaneId, &String)> = self
            .orchestrators
            .iter()
            .filter(|(id, _)| self.panes.contains_key(id))
            .map(|(id, f)| (*id, f))
            .collect();
        firsts.sort();
        let mut open: Vec<crate::open_pipelines::OpenPipeline> = Vec::new();
        for (id, name) in firsts {
            if open.iter().any(|o| &o.name == name) {
                continue;
            }
            open.push(crate::open_pipelines::OpenPipeline {
                name: name.clone(),
                env: self.panes[&id].read(cx).env().to_string(),
                watch: self.linear_watch.get(&id).is_some_and(|w| w.on),
                collapsed: self.collapsed_pipelines.contains(name),
            });
        }
        open.sort();
        open
    }

    /// Saves the open pipelines when they changed, so a restart opens them again; never before
    /// launch has opened the saved ones (core spec §38).
    pub(super) fn remember_open_pipelines(&mut self, cx: &gpui::App) {
        // A closing window takes its terminals down: what was open stays saved.
        let closing = crate::mcp::server::get()
            .is_some_and(|sv| crate::mcp::server::lock(sv.registry()).closing);
        if self.open_pipelines.is_none() || closing {
            return;
        }
        let now = self.open_pipelines_now(cx);
        if self.open_pipelines.as_ref() == Some(&now) {
            return;
        }
        if let Err(e) = crate::open_pipelines::save(self.hydra.paths.root(), &now) {
            crate::action_log::write(format!("open pipelines: {e}"));
        }
        self.open_pipelines = Some(now);
    }

    /// At launch: each pipeline that was open when hydraui last ran opens again, Automatic or
    /// Manual as it was (core spec §38), and each pipeline with `[autostart] env` gets its
    /// orchestrator in that environment, once (the first piece of roadmap phase 3, "start what
    /// you need at launch"). `HYDRAUI_NO_AUTOSTART` turns both off, for test windows.
    pub(super) fn autostart_pipelines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::env::var_os("HYDRAUI_NO_AUTOSTART").is_some() {
            return;
        }
        let root = self.hydra.paths.root().to_path_buf();
        let envs = self.hydra.env_names();
        let names = crate::pipeline::names(&root);
        // What was open, then what starts itself; a pipeline once, as it was left.
        let saved = crate::open_pipelines::load(&root);
        self.collapsed_pipelines
            .extend(saved.iter().filter(|o| o.collapsed).map(|o| o.name.clone()));
        let mut starts: Vec<(String, String, Option<bool>)> = saved
            .into_iter()
            .filter(|o| names.contains(&o.name))
            .map(|o| (o.name, o.env, Some(o.watch)))
            .collect();
        for name in &names {
            if starts.iter().any(|(n, _, _)| n == name) {
                continue;
            }
            if let Some(env) = crate::pipeline::load(&root, name)
                .ok()
                .and_then(|p| p.autostart)
            {
                starts.push((name.clone(), env, None));
            }
        }
        // From here on, what's open is saved for the next launch.
        self.open_pipelines = Some(Vec::new());
        for (name, env, was_watching) in starts {
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
                let orch = self.open_orchestrator_slot(&env, &name, window, cx);
                if let Some(on) = was_watching {
                    self.linear_watch.entry(orch).or_default().on = on;
                }
                continue;
            };
            self.resume_run(idx, window, cx);
            // Its other open runs (several at once) carry on too, each in its orchestrator.
            while let Some(idx) = self
                .interrupted
                .iter()
                .position(|i| i.run.pipeline.file == name && i.env == env)
            {
                self.resume_run(idx, window, cx);
            }
            let watch = was_watching
                .unwrap_or_else(|| crate::pipeline::load(&root, &name).is_ok_and(|p| p.watches()));
            // The first orchestrator holds the trigger.
            let orch = self
                .orchestrators
                .iter()
                .filter(|(_, f)| **f == name)
                .map(|(id, _)| *id)
                .min();
            // Automatic or Manual as it was left; one that starts itself watches when it asks to.
            if let Some(orch) = orch.filter(|_| watch || was_watching.is_some()) {
                self.linear_watch.entry(orch).or_default().on = watch;
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
                window.push_note(Note::error(e).title("Delete environment"), cx);
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
                window.push_note(Note::success(text).title("Delete environment"), cx);
                // An environment with no terminals can still have a tab: close it.
                self.close_env_now(env, window, cx);
                self.reload_config(window, cx);
            }
            Err(e) => window.push_note(Note::error(e).title("Delete environment"), cx),
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
                    window.push_note(
                        Note::success(format!(
                            "New {env} terminals open in {}, and the folder is bound to {env}",
                            dir.display()
                        ))
                        .title("Set environment home"),
                        cx,
                    );
                    ws.reload_config(window, cx);
                }
                Err(e) => window.push_note(Note::error(e).title("Set environment home"), cx),
            }
        });
    }

    /// Names and creates an environment, then adds its tab. The name is checked as it
    /// is typed; Create stays off until it is free and valid.
    pub(super) fn new_env_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name, like work"));
        // The tools to add with it, all ticked: most people need each (spec §48).
        let picks: Vec<(&'static str, Rc<Cell<bool>>)> = ESSENTIALS
            .iter()
            .map(|t| (*t, Rc::new(Cell::new(true))))
            .collect();
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
            let mut tools = v_flex()
                .gap_1()
                .pt_1()
                .child(div().text_sm().child("Add these tools to it"));
            for (tool, on) in &picks {
                let flag = on.clone();
                tools = tools.child(
                    Checkbox::new(SharedString::from(format!("new-env-{tool}")))
                        .label(tool_label(tool))
                        .checked(on.get())
                        .on_click(move |v, window, _| {
                            flag.set(*v);
                            window.refresh();
                        }),
                );
            }
            tools = tools.child(div().text_xs().text_color(muted).whitespace_normal().child(
                "Each environment has its own logins, so you sign in to each tool here \
                 even if you already have elsewhere. Pipelines need Claude.",
            ));
            let chosen: Vec<&'static str> = picks
                .iter()
                .filter(|(_, on)| on.get())
                .map(|(t, _)| *t)
                .collect();
            dialog
                .title("New environment")
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&name))
                        .child(hint)
                        .child(tools),
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
                        .child(
                            Button::new("create")
                                .label("Create")
                                .primary()
                                .disabled(problem.is_some())
                                .on_click(move |_, window, cx| {
                                    let env = typed.trim().to_string();
                                    let chosen = chosen.clone();
                                    ws.update(cx, |ws, cx| ws.create_env(env, chosen, window, cx));
                                }),
                        ),
                )
        });
    }

    /// Creates `env` with `tools` turned on, then opens Claude's sign-in and Git's form when
    /// they were chosen; the sidebar's checklist has the rest (spec §48).
    fn create_env(
        &mut self,
        env: String,
        tools: Vec<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = self.hydra.create_env(&env) {
            window.push_note(Note::error(e).title("New environment"), cx);
            return;
        }
        window.close_dialog(cx);
        let mut problems = Vec::new();
        for tool in tools.iter().filter(|t| SIGN_IN_TOOLS.contains(t)) {
            if let Err(e) = self.hydra.add_tool(&env, tool, &ToolValues::default()) {
                problems.push(format!("{tool}: {e}"));
            }
        }
        self.add_env(&env, window, cx);
        if problems.is_empty() {
            window.push_note(
                Note::success(format!(
                    "Created {env}. The sidebar lists what's left to set up."
                ))
                .title("New environment"),
                cx,
            );
        } else {
            window.push_note(
                Note::error(format!(
                    "Created {env}, but couldn't add {}",
                    problems.join("; ")
                ))
                .title("New environment"),
                cx,
            );
        }
        if tools.contains(&"claude") && self.hydra.tools(&env).iter().any(|t| t == "claude") {
            self.sign_in(&env, "claude", window, cx);
        }
        // Git asks for a name and email, so it's added through its form, not ticked on silently.
        if tools.contains(&"git") {
            self.set_up_tool(env.clone(), "git".into(), window, cx);
        }
        self.recheck_accounts(&env, cx);
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
        crate::place_toasts(cx);
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
            window.push_note(Note::info("Settings reloaded").title("hydraui"), cx);
        }
        for p in problems {
            window.push_note(Note::warning(p).title("hydraui settings"), cx);
        }
        cx.notify();
    }
}

/// Each pipeline file by its name and, when it differs, the name the views show.
pub(super) fn pipeline_labels(root: &std::path::Path, names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|n| match crate::pipeline::load(root, n) {
            Ok(p) if p.name != *n => format!("{n} · {}", p.name),
            _ => n.clone(),
        })
        .collect()
}

/// A pipeline of yours, as Start a pipeline and Create a pipeline show it.
#[derive(Clone)]
struct Mine {
    file: String,
    name: String,
    color: Option<u32>,
    icon: Option<String>,
    stages: usize,
    /// Where its work comes from, e.g. `Slack · #pull-requests`.
    source: String,
}

/// Every pipeline that loads, by file name.
fn my_pipelines(root: &std::path::Path) -> Vec<Mine> {
    crate::pipeline::names(root)
        .into_iter()
        .filter_map(|file| {
            let p = crate::pipeline::load(root, &file).ok()?;
            Some(Mine {
                source: crate::settings_words::source_short(&p),
                file,
                name: p.name,
                color: p.color,
                icon: p.icon,
                stages: p.stages.len(),
            })
        })
        .collect()
}

/// `content` scrolling within `max_h`, with a scrollbar always shown at its right edge so
/// it's plain there's more below (the dialogs' lists).
fn scrolled(
    content: gpui::Stateful<gpui::Div>,
    scroll: &gpui::ScrollHandle,
    max_h: f32,
) -> gpui::AnyElement {
    use gpui_component::scroll::{Scrollbar, ScrollbarMode};
    div()
        .relative()
        .child(
            content
                .max_h(px(max_h))
                .overflow_y_scroll()
                .track_scroll(scroll)
                // Room for the scrollbar, so it never sits on a card.
                .pr(px(14.)),
        )
        .child(
            div().absolute().inset_0().child(
                Scrollbar::vertical(scroll)
                    .id(content_id(max_h))
                    .mode(ScrollbarMode::Always),
            ),
        )
        .into_any_element()
}

fn content_id(max_h: f32) -> SharedString {
    SharedString::from(format!("dialog-scrollbar-{max_h}"))
}

/// Start a pipeline's cards: one per pipeline, then **Create a pipeline**.
fn start_cards(
    mine: &[Mine],
    workspace: &gpui::Entity<Workspace>,
    env: &str,
    scroll: &gpui::ScrollHandle,
    cx: &gpui::App,
) -> gpui::AnyElement {
    use crate::orchestrator_theme::{self as theme, color};
    let muted = cx.theme().muted_foreground;
    let card = |id: SharedString| {
        v_flex()
            .id(id)
            .w(px(184.))
            .gap(px(6.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .cursor_pointer()
    };
    let cards = mine.iter().map(|m| {
        let accent = m.color.unwrap_or(theme::TEAL);
        let (workspace, env, file) = (workspace.clone(), env.to_string(), m.file.clone());
        card(SharedString::from(format!("start-{}", m.file)))
            .border_color(color(accent).opacity(0.3))
            .bg(color(0x131616))
            .hover(|d| {
                d.border_color(color(accent))
                    .bg(color(accent).opacity(0.06))
            })
            .child(
                div()
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .bg(color(accent).opacity(0.14))
                    .child(crate::pipeline_icon::mark(m.icon.as_deref(), accent, 16.)),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .truncate()
                    .child(m.name.clone()),
            )
            .child(
                v_flex()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(m.source.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(crate::settings_words::stages(m.stages)),
                    ),
            )
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let env = env.clone();
                workspace.update(cx, |ws, cx| ws.open_orchestrator(&env, &file, window, cx));
            })
            .into_any_element()
    });
    let create = {
        let (workspace, env) = (workspace.clone(), env.to_string());
        card("start-create".into())
            .items_center()
            .justify_center()
            .border_dashed()
            .border_color(color(theme::BORDER_STRONG))
            .hover(|d| d.border_color(color(theme::TEAL)))
            .child(div().text_color(color(theme::TEAL)).text_xl().child("+"))
            .child(div().text_sm().child("Create a pipeline"))
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let env = env.clone();
                workspace.update(cx, |ws, cx| ws.new_pipeline(Some(env), window, cx));
            })
            .into_any_element()
    };
    v_flex()
        .gap_3()
        .pb_2()
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("Pick the pipeline to run, or create a new one."),
        )
        .child(scrolled(
            h_flex()
                .id("start-cards")
                .flex_wrap()
                .items_start()
                .gap(px(10.))
                .children(cards)
                .child(create),
            scroll,
            400.,
        ))
        .into_any_element()
}

/// What a choice does once picked.
#[derive(Clone)]
enum Start {
    /// Name a new pipeline made from this (`None`: blank).
    Name(Option<String>),
    Import,
    /// Import from one of your swarms.
    Swarm,
}

/// Create a pipeline's three sections: start fresh, copy one of yours, import a file.
fn create_choices(
    mine: &[Mine],
    workspace: &gpui::Entity<Workspace>,
    env: &Option<String>,
    scroll: &gpui::ScrollHandle,
    in_swarm: bool,
    cx: &gpui::App,
) -> gpui::AnyElement {
    use crate::orchestrator_theme::{self as theme, color};
    let muted = cx.theme().muted_foreground;
    let pick = |start: Start| {
        let (workspace, env) = (workspace.clone(), env.clone());
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
            window.close_dialog(cx);
            let env = env.clone();
            workspace.update(cx, |ws, cx| match start.clone() {
                Start::Name(from) => ws.name_pipeline(from, env, window, cx),
                Start::Import => ws.import_pipeline(env, window, cx),
                Start::Swarm => ws.import_from_swarm_dialog(env, window, cx),
            });
        }
    };
    let heading = |title: &'static str, about: &'static str| {
        v_flex()
            .gap(px(2.))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().text_xs().text_color(muted).child(about))
    };
    let tile = |id: &'static str,
                icon: gpui_kit_assets::IconName,
                title: &'static str,
                about: &'static str| {
        h_flex()
            .id(id)
            .flex_1()
            .flex_basis(px(0.))
            .min_w_0()
            .gap_3()
            .items_start()
            .px(px(14.))
            .py(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(color(0x1f2524))
            .bg(color(0x131616))
            .hover(|d| d.border_color(color(theme::TEAL)))
            .cursor_pointer()
            .child(
                gpui_component::Icon::new(icon)
                    .size(px(18.))
                    .text_color(color(theme::TEAL)),
            )
            .child(
                // The rest of the tile's width: without it the text has none and wraps per letter.
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(about),
                    ),
            )
    };
    let fresh = v_flex()
        .gap_2()
        .child(heading(
            "Start fresh",
            "Build it yourself, or begin from a ready-made starter.",
        ))
        .child(
            h_flex()
                .gap_2()
                .items_stretch()
                .child(
                    tile(
                        "create-blank",
                        gpui_kit_assets::IconName::FilePlus,
                        "Blank",
                        "One stage to fill in yourself.",
                    )
                    .on_click(pick(Start::Name(None))),
                )
                .child(
                    tile(
                        "create-email-triage",
                        gpui_kit_assets::IconName::Mail,
                        "Email triage",
                        "Gmail, Jev sorts each email, Claude drafts a reply.",
                    )
                    .on_click(pick(Start::Name(Some(
                        crate::pipeline::EMAIL_TRIAGE.to_string(),
                    )))),
                ),
        );
    let copy = (!mine.is_empty()).then(|| {
        v_flex()
            .gap_2()
            .child(heading(
                "Copy one of yours",
                "The copy starts switched off, so it never takes the original's work.",
            ))
            .child(scrolled(
                v_flex()
                    .id("create-copies")
                    .rounded(px(10.))
                    .border_1()
                    .border_color(color(0x1f2524))
                    .children(mine.iter().map(|m| {
                        let accent = m.color.unwrap_or(theme::TEAL);
                        h_flex()
                            .id(SharedString::from(format!("create-copy-{}", m.file)))
                            .gap_3()
                            .items_center()
                            .px(px(14.))
                            .py(px(9.))
                            .border_b_1()
                            .border_color(color(0x1a1f1e))
                            .hover(|d| d.bg(color(0x161b1a)))
                            .cursor_pointer()
                            .child(crate::pipeline_icon::mark(m.icon.as_deref(), accent, 15.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(m.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(crate::settings_words::stages(m.stages)),
                            )
                            .child(div().text_xs().text_color(color(theme::TEAL)).child("Copy"))
                            .on_click(pick(Start::Name(Some(m.file.clone()))))
                    })),
                scroll,
                220.,
            ))
    });
    let import = v_flex()
        .gap_2()
        .child(heading(
            "Import one someone shared",
            "Added switched off, for you to check before it takes any work.",
        ))
        .child(
            h_flex()
                .gap_2()
                .items_stretch()
                .child(
                    tile(
                        "create-import",
                        gpui_kit_assets::IconName::FileUp,
                        "From a file",
                        "A .toml they saved with Export to share.",
                    )
                    .on_click(pick(Start::Import)),
                )
                .when(in_swarm, |row| {
                    row.child(
                        tile(
                            "create-swarm",
                            gpui_kit_assets::IconName::Users,
                            "From a swarm",
                            "One your team shared with Share to a swarm.",
                        )
                        .on_click(pick(Start::Swarm)),
                    )
                }),
        );
    v_flex()
        .gap(px(20.))
        .pb_2()
        .child(fresh)
        .children(copy)
        .child(import)
        .into_any_element()
}
