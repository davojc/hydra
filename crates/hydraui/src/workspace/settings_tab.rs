//! The Settings tab: hydraui's own settings, each environment's page and the credentials,
//! shown in the middle in place of a terminal until a terminal is picked or it is closed. An
//! environment's name in the status bar opens its page.
use std::cell::Cell;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Hsla, SharedString, Window, div, px, rgb};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::config::{self, ThemeChoice};
use crate::jev::JevKey;
use crate::launcher::Action;
use crate::settings::{ENV_COLOURS, SHELLS};

/// The page the Settings tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Section {
    General,
    Phone,
    /// Swarms › You and this machine.
    You,
    /// A swarm's page, by its namespace id.
    Swarm(String),
    Env(String),
    Credentials,
}

pub(super) struct SettingsTab {
    section: Section,
    /// You and this machine's name fields, while that page shows.
    pub(super) you: Option<super::swarms_page::IdentityFields>,
}

/// A sidebar line's click.
type OnClick = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;

/// What a variable shows agents before the user describes it.
const DESCRIBE_ME: &str = "[What it is for, and how to use it]";

impl Workspace {
    pub(super) fn settings_shown(&self) -> bool {
        self.settings.is_some()
    }

    /// Opens the Settings tab at `section`; an environment's page checks its accounts again.
    pub(super) fn open_settings(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Section::Env(env) = &section {
            self.recheck_accounts(env, cx);
        }
        let you = (section == Section::You).then(|| self.identity_fields(window, cx));
        self.settings = Some(SettingsTab { section, you });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Writes one config.toml setting, then applies the file as Reload config does.
    fn set_ui(
        &mut self,
        key: &str,
        value: Option<toml_edit::Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match config::set(&self.hydra.paths, key, value) {
            Ok(()) => self.apply_config(false, window, cx),
            Err(e) => window.push_notification(Notification::error(e).title("Settings"), cx),
        }
    }

    /// After an env.toml edit: says what failed, else refreshes what shows it.
    fn env_edited(
        &mut self,
        env: &str,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                let tint = self.hydra.tint(env);
                self.tints.insert(env.to_string(), tint);
                self.recheck_accounts(env, cx);
            }
            Err(e) => window.push_notification(Notification::error(e).title(env.to_string()), cx),
        }
        cx.notify();
    }

    /// A one-line box filled with `initial`; Save runs `save`, and the dialog stays open
    /// showing what's wrong until it succeeds.
    pub(super) fn text_dialog(
        &mut self,
        title: String,
        initial: String,
        placeholder: &'static str,
        save: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) -> Result<(), String>
        + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(initial)
        });
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let save = std::rc::Rc::new(save);
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            let danger = cx.theme().danger;
            let shown = problem.read(cx).clone();
            let (i, p, ws, save) = (
                input.clone(),
                problem.clone(),
                workspace.clone(),
                save.clone(),
            );
            dialog
                .title(title.clone())
                .w(px(640.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&input))
                        .when_some(shown, |d, e| {
                            d.child(div().text_sm().text_color(danger).child(e))
                        }),
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
                                let value = i.read(cx).value().trim().to_string();
                                match ws.update(cx, |ws, cx| save(ws, value, window, cx)) {
                                    Ok(()) => window.close_dialog(cx),
                                    Err(e) => p.update(cx, |p, cx| {
                                        *p = Some(e);
                                        cx.notify();
                                    }),
                                }
                            },
                        )),
                )
        });
    }

    /// What `env`'s agents get from `hydra_access`, in a dialog.
    fn agents_preview(&mut self, env: String, window: &mut Window, cx: &mut Context<Self>) {
        let text = match crate::agent_access::describe(&self.hydra.paths, &env) {
            Ok(doc) => serde_json::to_string_pretty(&doc).expect("json serialises"),
            Err(e) => format!("can't read {env}: {e}"),
        };
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            dialog
                .title(format!("What agents in {env} see"))
                .w(px(640.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("A Claude session there gets this from the hydra_access tool. Values stay in its terminal's variables."),
                        )
                        .child(
                            div()
                                .id("agents-preview")
                                .max_h(px(420.))
                                .overflow_y_scroll()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().secondary)
                                .font_family("monospace")
                                .text_xs()
                                .whitespace_normal()
                                .children(text.lines().map(|l| {
                                    div().child(SharedString::from(l.replace(' ', "\u{a0}")))
                                })),
                        ),
                )
        });
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = &self.settings else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let page = match &tab.section {
            Section::General => self.general_page(cx),
            Section::Phone => self.phone_page(cx),
            Section::You => self.you_page(cx),
            Section::Swarm(id) => self.swarm_page(id, cx),
            Section::Env(env) => self.env_page(env, cx),
            Section::Credentials => self.credentials_page(cx),
        };
        h_flex()
            .size_full()
            .items_start()
            .child(self.settings_nav(&tab.section, border, muted, cx))
            .child(
                div()
                    .id("settings-page")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .px_6()
                    .py_5()
                    .child(v_flex().max_w(px(880.)).gap_5().child(page)),
            )
            .into_any_element()
    }

    fn settings_nav(
        &self,
        current: &Section,
        border: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let accent = cx.theme().accent;
        let warning = cx.theme().warning;
        let item = |id: SharedString,
                    label: SharedString,
                    dot: Option<Hsla>,
                    count: usize,
                    to: Section| {
            let on = *current == to;
            h_flex()
                .id(id)
                .px_2()
                .py_1()
                .gap_2()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(on, |d| d.bg(accent))
                .hover(|d| d.bg(accent.opacity(0.6)))
                .when_some(dot, |d, c| {
                    d.child(div().size(px(8.)).flex_none().rounded_full().bg(c))
                })
                .child(div().min_w_0().truncate().child(label))
                .when(count > 0, |d| {
                    d.child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .text_xs()
                            .text_color(warning)
                            .child(count.to_string()),
                    )
                })
                .on_click(
                    cx.listener(move |ws, _, window, cx| ws.open_settings(to.clone(), window, cx)),
                )
        };
        let heading = |text: &'static str| {
            div()
                .pt_3()
                .px_2()
                .pb_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(text)
        };
        let envs: Vec<_> = self
            .hydra
            .env_names()
            .into_iter()
            .map(|e| {
                let tint: Hsla = rgb(self.hydra.tint(&e)).into();
                item(
                    SharedString::from(format!("settings-env-{e}")),
                    SharedString::from(e.clone()),
                    Some(tint),
                    0,
                    Section::Env(e),
                )
            })
            .collect();
        let swarms: Vec<_> = self
            .swarms
            .iter()
            .map(|s| {
                item(
                    SharedString::from(format!("settings-swarm-{}", s.id)),
                    SharedString::from(s.name.clone()),
                    Some(self.swarm_nav_dot(s, cx)),
                    s.attention.len(),
                    Section::Swarm(s.id.clone()),
                )
            })
            .collect();
        // A line under a heading that makes or adds something.
        let add = |id: &'static str, label: &'static str, click: OnClick| {
            h_flex()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .text_color(muted)
                .cursor_pointer()
                .hover(|d| d.bg(accent.opacity(0.6)))
                .child(label)
                .on_click(click)
        };
        v_flex()
            .id("settings-nav")
            .w(px(200.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .p_2()
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .justify_between()
                    .px_2()
                    .pb_1()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        Button::new("settings-close")
                            .label("Close")
                            .ghost()
                            .xsmall()
                            .on_click(
                                cx.listener(|ws, _, window, cx| ws.close_settings(window, cx)),
                            ),
                    ),
            )
            .child(heading("HYDRAUI"))
            .child(item(
                "settings-general".into(),
                "General".into(),
                None,
                0,
                Section::General,
            ))
            .child(item(
                "settings-phone".into(),
                "Phone".into(),
                None,
                0,
                Section::Phone,
            ))
            .child(heading("SWARMS"))
            .child(item(
                "settings-you".into(),
                "You and this machine".into(),
                None,
                0,
                Section::You,
            ))
            .children(swarms)
            .child(add(
                "settings-new-swarm",
                "+ New swarm",
                Box::new(cx.listener(|ws, _, window, cx| ws.new_swarm_dialog(window, cx))),
            ))
            .child(add(
                "settings-join-swarm",
                "Join a swarm…",
                Box::new(cx.listener(|ws, _, window, cx| ws.join_swarm_dialog(window, cx))),
            ))
            .child(heading("ENVIRONMENTS"))
            .children(envs)
            .child(add(
                "settings-new-env",
                "+ New environment",
                Box::new(cx.listener(|ws, _, window, cx| ws.new_env_dialog(window, cx))),
            ))
            .child(heading("SECURITY"))
            .child(item(
                "settings-credentials".into(),
                "Credentials".into(),
                None,
                0,
                Section::Credentials,
            ))
    }

    fn general_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let ui = self.ui.clone();
        let theme_buttons = h_flex()
            .gap_1()
            .child(
                Button::new("theme-dark")
                    .label("Dark")
                    .small()
                    .when(ui.theme == ThemeChoice::Dark, |b| b.primary())
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.set_ui("theme", Some("dark".into()), window, cx)
                    })),
            )
            .child(
                Button::new("theme-light")
                    .label("Light")
                    .small()
                    .when(ui.theme == ThemeChoice::Light, |b| b.primary())
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.set_ui("theme", Some("light".into()), window, cx)
                    })),
            );
        let size = ui.font_size;
        let font_size = h_flex()
            .gap_2()
            .child(
                Button::new("font-smaller")
                    .label("−")
                    .small()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.set_ui(
                            "font_size",
                            Some(f64::from((size - 1.0).max(6.0)).into()),
                            window,
                            cx,
                        )
                    })),
            )
            .child(div().min_w(px(32.)).child(format!("{size}")))
            .child(
                Button::new("font-bigger")
                    .label("+")
                    .small()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.set_ui(
                            "font_size",
                            Some(f64::from((size + 1.0).min(72.0)).into()),
                            window,
                            cx,
                        )
                    })),
            );
        let shells = h_flex()
            .gap_1()
            .children(SHELLS.iter().map(|s| {
                Button::new(SharedString::from(format!("shell-{s}")))
                    .label(*s)
                    .small()
                    .when(ui.default_shell.as_deref() == Some(*s), |b| b.primary())
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.set_ui("default_shell", Some((*s).into()), window, cx)
                    }))
            }))
            .child(
                Button::new("shell-default")
                    .label("hydra's default")
                    .small()
                    .when(ui.default_shell.is_none(), |b| b.primary())
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.set_ui("default_shell", None, window, cx)
                    })),
            );
        let default_env = h_flex()
            .gap_2()
            .child(match &ui.default_env {
                Some(e) => div().child(e.clone()),
                None => div().text_color(muted).child("none: asked each time"),
            })
            .child(
                Button::new("default-env")
                    .label("Change…")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.choose_env("Default environment", window, cx, |ws, env, w, cx| {
                            ws.set_ui("default_env", Some(env.into()), w, cx)
                        })
                    })),
            )
            .when(ui.default_env.is_some(), |d| {
                d.child(
                    Button::new("default-env-clear")
                        .label("Clear")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|ws, _, window, cx| {
                            ws.set_ui("default_env", None, window, cx)
                        })),
                )
            });
        let text_setting =
            |key: &'static str, label: &'static str, value: Option<String>, hint: &'static str| {
                let shown = value.clone().unwrap_or_default();
                h_flex()
                    .gap_2()
                    .child(match &value {
                        Some(v) => div().font_family("monospace").text_sm().child(v.clone()),
                        None => div().text_color(muted).child(hint),
                    })
                    .child(
                        Button::new(SharedString::from(format!("edit-{key}")))
                            .label("Change…")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.text_dialog(
                                    label.to_string(),
                                    shown.clone(),
                                    hint,
                                    move |ws, v, window, cx| {
                                        let value = (!v.is_empty()).then(|| v.into());
                                        config::set(&ws.hydra.paths, key, value)?;
                                        ws.apply_config(false, window, cx);
                                        Ok(())
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    )
            };
        let editor_row = text_setting(
            "editor",
            "Editor for files from the tree",
            ui.editor.clone(),
            "the system's",
        )
        .into_any_element();
        let font_row = text_setting(
            "font",
            "Terminal font",
            Some(ui.font.clone()),
            config::DEFAULT_FONT,
        )
        .into_any_element();
        let jev = matches!(JevKey::app().get(), Ok(Some(_)));
        v_flex()
            .gap_5()
            .child(page_title(
                "General",
                format!(
                    "Saved to {} as you change it.",
                    config::config_file(&self.hydra.paths).display()
                ),
                h_flex()
                    .gap_2()
                    .child(action_button("open-config", "Edit config.toml", cx, |ws, w, cx| {
                        let path = config::config_file(&ws.hydra.paths);
                        if !path.exists() {
                            let _ = config::set(&ws.hydra.paths, "theme", Some(match ws.ui.theme {
                                ThemeChoice::Dark => "dark".into(),
                                ThemeChoice::Light => "light".into(),
                            }));
                        }
                        ws.show_file(path, w, cx)
                    }))
                    .child(action_button("reload-config", "Reload config", cx, |ws, w, cx| {
                        ws.run(Action::ReloadConfig, w, cx)
                    })),
                cx,
            ))
            .child(group(
                "Terminals",
                None,
                vec![
                    setting("Default environment", default_env.into_any_element(), cx),
                    setting("Default shell", shells.into_any_element(), cx),
                    setting("Editor", editor_row, cx),
                ],
                cx,
            ))
            .child(group(
                "Appearance",
                None,
                vec![
                    setting("Theme", theme_buttons.into_any_element(), cx),
                    setting("Terminal font", font_row, cx),
                    setting("Font size", font_size.into_any_element(), cx),
                    setting(
                        "Colours",
                        div()
                            .text_color(muted)
                            .child("[colors] in config.toml: foreground, background, cursor, selection and the 16-colour palette.")
                            .into_any_element(),
                        cx,
                    ),
                ],
                cx,
            ))
            .child(group(
                "Keyboard",
                Some(action_button("shortcuts", "Keyboard shortcuts", cx, |ws, w, cx| {
                    ws.shortcuts(w, cx)
                })),
                vec![setting(
                    "Keys",
                    div()
                        .text_color(muted)
                        .child(format!(
                            "{} changed under [keys] in config.toml; key changes apply when hydraui next starts.",
                            ui.keys.len()
                        ))
                        .into_any_element(),
                    cx,
                )],
                cx,
            ))
            .child(group(
                "Auto-pickup",
                None,
                vec![
                    setting(
                        "Jev API key",
                        h_flex()
                            .gap_2()
                            .child(if jev {
                                div().child("set")
                            } else {
                                div().text_color(muted).child("not set: finished sessions only")
                            })
                            .child(action_button("jev-manage", "Credentials…", cx, |ws, w, cx| {
                                ws.open_settings(Section::Credentials, w, cx)
                            }))
                            .into_any_element(),
                        cx,
                    ),
                    setting(
                        "Jev questions",
                        action_button("jev-questions", "Edit jev.toml", cx, |ws, w, cx| {
                            ws.run(Action::EditJevQuestions, w, cx)
                        })
                        .into_any_element(),
                        cx,
                    ),
                ],
                cx,
            ))
            .child(group(
                "About",
                None,
                vec![
                    setting(
                        "Version",
                        h_flex()
                            .gap_2()
                            .child(crate::version::VERSION)
                            .child(action_button("updates", "Check for updates", cx, |ws, w, cx| {
                                ws.check_for_updates(w, cx)
                            }))
                            .into_any_element(),
                        cx,
                    ),
                    setting(
                        "Files",
                        h_flex()
                            .gap_2()
                            .child(action_button("open-log", "Open hydraui log", cx, |ws, w, cx| {
                                ws.run(Action::OpenLog, w, cx)
                            }))
                            .child(action_button("open-folder", "Open config folder", cx, |ws, w, cx| {
                                ws.run(Action::OpenConfigFolder, w, cx)
                            }))
                            .into_any_element(),
                        cx,
                    ),
                ],
                cx,
            ))
            .into_any_element()
    }

    fn env_page(&self, env: &str, cx: &mut Context<Self>) -> AnyElement {
        let (muted, warn) = (cx.theme().muted_foreground, cx.theme().warning);
        if !self.hydra.env_exists(env) {
            return div()
                .text_color(muted)
                .child(format!("{env} no longer exists."))
                .into_any_element();
        }
        let (label, colour, home) = self.hydra.env_basics(env);
        let file = hydra_core::name::EnvName::parse(env)
            .map(|n| self.hydra.paths.env_file(&n).display().to_string())
            .unwrap_or_default();
        let e = env.to_string();
        let label_row = {
            let (e, shown) = (e.clone(), label.clone().unwrap_or_default());
            h_flex()
                .gap_2()
                .child(match &label {
                    Some(l) => div().child(l.clone()),
                    None => div().text_color(muted).child("none: its name"),
                })
                .child(
                    Button::new("env-label")
                        .label("Change…")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            let e = e.clone();
                            ws.text_dialog(
                                format!("Label for {e}"),
                                shown.clone(),
                                "shown instead of its name",
                                move |ws, v, window, cx| {
                                    let value = (!v.is_empty()).then_some(v.as_str());
                                    let r = ws.hydra.set_env_basic(&e, "label", value);
                                    let failed = r.clone().err();
                                    ws.env_edited(&e, r, window, cx);
                                    failed.map_or(Ok(()), Err)
                                },
                                window,
                                cx,
                            )
                        })),
                )
        };
        let swatches = h_flex()
            .gap_2()
            .children(ENV_COLOURS.iter().map(|c| {
                let chosen = colour.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(c));
                let tint: Hsla = rgb(u32::from_str_radix(&c[1..], 16).unwrap_or(0x808080)).into();
                let e = e.clone();
                div()
                    .id(SharedString::from(format!("env-colour-{c}")))
                    .size(px(22.))
                    .rounded_md()
                    .bg(tint)
                    .border_2()
                    .border_color(if chosen { cx.theme().foreground } else { tint })
                    .cursor_pointer()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        let r = ws.hydra.set_env_basic(&e, "color", Some(c));
                        ws.env_edited(&e, r, window, cx)
                    }))
            }))
            .when_some(colour.clone(), |d, c| {
                d.child(div().text_sm().text_color(muted).child(c))
            });
        let home_row = h_flex()
            .gap_2()
            .child(match &home {
                Some(h) => div().font_family("monospace").text_sm().child(h.clone()),
                None => div().text_color(muted).child("not set: your home folder"),
            })
            .child(action_button("env-home", "Set…", cx, {
                let e = e.clone();
                move |ws, w, cx| ws.set_env_home(e.clone(), w, cx)
            }));
        let remote_row = {
            let e = e.clone();
            Switch::new(SharedString::from(format!("env-remote-control-{env}")))
                .checked(self.hydra.env_remote_control(env))
                .small()
                .label("Claude sessions start with Remote Control")
                .on_click(cx.listener(move |ws, on: &bool, window, cx| {
                    let r = ws.hydra.set_env_remote_control(&e, *on);
                    ws.env_edited(&e, r, window, cx)
                }))
        };
        let vars = self.hydra.env_variables(env);
        let var_rows: Vec<AnyElement> = if vars.is_empty() {
            vec![
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("None. Add them under [env] in env.toml.")
                    .into_any_element(),
            ]
        } else {
            vars.into_iter()
                .map(|(k, v)| {
                    let (shown, note) = crate::settings::shown_value(&k, &v);
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(
                            div()
                                .w(px(260.))
                                .flex_none()
                                .truncate()
                                .font_family("monospace")
                                .child(k),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("monospace")
                                .text_color(muted)
                                .child(shown),
                        )
                        .when_some(note, |d, n| {
                            d.child(div().flex_none().text_xs().text_color(warn).child(n))
                        })
                        .into_any_element()
                })
                .collect()
        };
        let ws = cx.entity();
        v_flex()
            .gap_5()
            .child(page_title(
                env,
                file,
                h_flex()
                    .gap_2()
                    .child(action_button("env-edit", "Edit env.toml", cx, {
                        let e = e.clone();
                        move |ws, w, cx| ws.run(Action::EditEnv(e.clone()), w, cx)
                    }))
                    .child(action_button("env-terminal", "New terminal", cx, {
                        let e = e.clone();
                        move |ws, w, cx| {
                            ws.settings = None;
                            ws.new_terminal(&e, None, w, cx);
                        }
                    })),
                cx,
            ))
            .child(group(
                "Environment",
                Some(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(format!(
                            "Terminals already open in {env} see changes once reopened."
                        ))
                        .into_any_element(),
                ),
                vec![
                    setting("Label", label_row.into_any_element(), cx),
                    setting("Colour", swatches.into_any_element(), cx),
                    setting("Home", home_row.into_any_element(), cx),
                    setting("Remote Control", remote_row.into_any_element(), cx),
                ],
                cx,
            ))
            .child(group(
                "Accounts",
                None,
                vec![super::accounts::accounts_list(self, &ws, env, cx).into_any_element()],
                cx,
            ))
            .child(group(
                "Variables",
                Some(action_button(
                    "env-credentials",
                    "Credentials…",
                    cx,
                    |ws, w, cx| ws.open_settings(Section::Credentials, w, cx),
                )),
                var_rows,
                cx,
            ))
            .child(group(
                "Manage",
                None,
                vec![setting(
                    "Delete",
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(muted)
                                .child("Removes its folder and secrets; asks first."),
                        )
                        .child(
                            Button::new("env-delete")
                                .label(format!("Delete {env}…"))
                                .danger()
                                .small()
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.delete_env_dialog(e.clone(), window, cx)
                                })),
                        )
                        .into_any_element(),
                    cx,
                )],
                cx,
            ))
            .into_any_element()
    }

    fn credentials_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, ok, bad, accent) = (
            theme.muted_foreground,
            theme.success,
            theme.danger,
            theme.accent,
        );
        let jev = matches!(JevKey::app().get(), Ok(Some(_)));
        let app_group = group(
            "hydraui",
            Some(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Used by hydraui itself, never put in a terminal")
                    .into_any_element(),
            ),
            vec![setting(
                "Jev API key",
                h_flex()
                    .gap_2()
                    .child(if jev {
                        div().text_color(ok).child("✓ stored")
                    } else {
                        div().text_color(muted).child("not set")
                    })
                    .child(div().flex_1().min_w_0().text_sm().text_color(muted).child(
                        "Auto-pickup asks Jev whether a Claude session is free. For agents, add jev to an environment instead.",
                    ))
                    .child(action_button(
                        "jev-set",
                        if jev { "Replace…" } else { "Set…" },
                        cx,
                        |ws, w, cx| ws.run(Action::SetJevKey, w, cx),
                    ))
                    .when(jev, |d| {
                        d.child(action_button("jev-remove", "Remove", cx, |ws, w, cx| {
                            ws.run(Action::RemoveJevKey, w, cx)
                        }))
                    })
                    .into_any_element(),
                cx,
            )],
            cx,
        );
        let mut page = v_flex()
            .gap_5()
            .child(page_title(
                "Credentials",
                "Kept in the system's credential store. Values are never shown, written to a file or given to agents through MCP.".into(),
                action_button("secret-new", "Add secret…", cx, |ws, w, cx| {
                    ws.run(Action::NewSecret, w, cx)
                }),
                cx,
            ))
            .child(app_group);
        for env in self.hydra.env_names() {
            let tint: Hsla = rgb(self.hydra.tint(&env)).into();
            let creds = self.hydra.credentials(&env);
            let rows: Vec<AnyElement> = if creds.is_empty() {
                vec![
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("No secrets.")
                        .into_any_element(),
                ]
            } else {
                creds
                    .into_iter()
                    .map(|c| {
                        let id = format!("{}-{}", c.secret, c.var.clone().unwrap_or_default());
                        let secret = c.secret.clone();
                        let top = h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(180.))
                                    .flex_none()
                                    .truncate()
                                    .font_family("monospace")
                                    .text_sm()
                                    .child(c.secret.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(c.used_by.clone()),
                            )
                            .child(if c.stored {
                                div().text_sm().text_color(ok).child("✓ stored")
                            } else {
                                div().text_sm().text_color(bad).child("✗ not stored")
                            })
                            .child(
                                Button::new(SharedString::from(format!("set-{id}")))
                                    .label(if c.stored { "Replace…" } else { "Set…" })
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.run(Action::SetSecret(secret.clone()), window, cx)
                                    })),
                            );
                        let agents = c.var.clone().map(|var| {
                            let told = c.agents.clone();
                            let (e1, v1) = (env.clone(), var.clone());
                            let switch = Switch::new(SharedString::from(format!("agents-{id}")))
                                .checked(told.is_some())
                                .small()
                                .label("Agents can see")
                                .on_click(cx.listener(move |ws, on: &bool, window, cx| {
                                    let about = crate::agent_access::default_about(&v1)
                                        .unwrap_or(DESCRIBE_ME);
                                    let r = ws.hydra.set_agent(&e1, &v1, on.then_some(about));
                                    ws.env_edited(&e1, r, window, cx)
                                }));
                            let (e2, v2, shown) = (env.clone(), var.clone(), told.clone().unwrap_or_default());
                            v_flex()
                                .gap_1()
                                .when(told.is_some(), |d| d.p_2().rounded_md().bg(accent.opacity(0.35)))
                                .child(
                                    h_flex()
                                        .gap_3()
                                        .child(switch)
                                        .child(div().text_sm().text_color(muted).child(if told.is_some() {
                                            format!("Claude sessions in {env} find {var} with hydra_access")
                                        } else {
                                            format!("{var} is in {env}'s terminals, but agents aren't told about it")
                                        })),
                                )
                                .when_some(told, |d, about| {
                                    d.child(
                                        h_flex()
                                            .gap_2()
                                            .items_start()
                                            .child(div().flex_1().min_w_0().text_sm().child(about))
                                            .child(
                                                Button::new(SharedString::from(format!("describe-{id}")))
                                                    .label("Describe…")
                                                    .ghost()
                                                    .small()
                                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                                        let (e, v) = (e2.clone(), v2.clone());
                                                        ws.text_dialog(
                                                            format!("What {v} is for, for agents"),
                                                            shown.clone(),
                                                            "What it is for, and how to use it",
                                                            move |ws, text, window, cx| {
                                                                if text.is_empty() {
                                                                    return Err("Say what it is for, or turn Agents can see off".into());
                                                                }
                                                                let r = ws.hydra.set_agent(&e, &v, Some(&text));
                                                                let failed = r.clone().err();
                                                                ws.env_edited(&e, r, window, cx);
                                                                failed.map_or(Ok(()), Err)
                                                            },
                                                            window,
                                                            cx,
                                                        )
                                                    })),
                                            ),
                                    )
                                })
                        });
                        v_flex()
                            .gap_2()
                            .child(top)
                            .when_some(agents, |d, a| d.child(a))
                            .into_any_element()
                    })
                    .collect()
            };
            let e = env.clone();
            page = page.child(group_with_dot(
                &env,
                tint,
                Some(action_button(
                    SharedString::from(format!("agents-preview-{env}")),
                    "What agents see",
                    cx,
                    move |ws, w, cx| ws.agents_preview(e.clone(), w, cx),
                )),
                rows,
                cx,
            ));
        }
        page.into_any_element()
    }
}

/// A small button that runs `f` on the workspace.
fn action_button(
    id: impl Into<SharedString>,
    label: &'static str,
    cx: &mut Context<Workspace>,
    f: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> AnyElement {
    Button::new(id.into())
        .label(label)
        .small()
        .on_click(cx.listener(move |ws, _, window, cx| f(ws, window, cx)))
        .into_any_element()
}

/// A page's heading: its title and a line under it, with buttons on the right.
pub(super) fn page_title(
    title: &str,
    sub: String,
    buttons: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .justify_between()
        .items_start()
        .gap_4()
        .child(
            v_flex()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(div().text_sm().text_color(muted).child(sub)),
        )
        .child(buttons)
        .into_any_element()
}

/// A bordered group of rows under a heading, with something on the heading's right.
pub(super) fn group(
    title: &str,
    right: Option<AnyElement>,
    rows: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    group_inner(div().child(title.to_string()), right, rows, cx)
}

fn group_with_dot(
    title: &str,
    dot: Hsla,
    right: Option<AnyElement>,
    rows: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let heading = h_flex()
        .gap_2()
        .child(div().size(px(8.)).rounded_full().bg(dot))
        .child(title.to_string());
    group_inner(heading, right, rows, cx)
}

fn group_inner(
    heading: impl IntoElement,
    right: Option<AnyElement>,
    rows: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let border = cx.theme().border;
    v_flex()
        .border_1()
        .border_color(border)
        .rounded_lg()
        .child(
            h_flex()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(border)
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(heading))
                .when_some(right, |d, r| d.child(r)),
        )
        .children(rows.into_iter().enumerate().map(|(i, row)| {
            div()
                .px_4()
                .py_2()
                .when(i > 0, |d| d.border_t_1().border_color(border.opacity(0.5)))
                .child(row)
        }))
        .into_any_element()
}

/// One setting: its name on the left, its control on the right.
fn setting(label: &'static str, control: AnyElement, cx: &App) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            div()
                .w(px(160.))
                .flex_none()
                .text_color(cx.theme().foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(control))
        .into_any_element()
}
