//! Pipeline settings (orchestrator spec §7.4; the design's View 2), in the Pipeline tab: the
//! orchestrator's name, the default bar, the stages (reorder, name, skill, bar, remove, add),
//! the limits, and a save bar. Saving rewrites the pipeline file (`pipeline::save`) and open
//! runs take the new bars at once; stage changes apply from the next ticket.
use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, SharedString, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{Disableable, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::pipeline::{self, Edit, EditStage, validate};
use crate::runs::MAX_ATTEMPTS;

/// The width of a stage row's up/down arrows, so the prompt can line up under the title.
const ARROWS_W: f32 = 44.;

struct DraftStage {
    /// Keys its row's elements, stable while rows move.
    key: u64,
    id: Option<String>,
    title: Entity<InputState>,
    skill: Option<String>,
    /// The folder its session starts in, as written (`~/…`).
    folder: Option<String>,
    bar: Entity<SliderState>,
    /// The bar is the stage's own, not the pipeline's.
    own: bool,
    /// What its session is asked, before hydraui's note.
    prompt: Entity<InputState>,
}

/// The settings being edited for one orchestrator's pipeline file.
pub(super) struct SettingsDraft {
    file: String,
    saved: Edit,
    name: Entity<InputState>,
    bar: Entity<SliderState>,
    stages: Vec<DraftStage>,
    skills: Vec<String>,
    next_key: u64,
    _subs: Vec<Subscription>,
}

fn slider(value: u8, cx: &mut App) -> Entity<SliderState> {
    cx.new(|_| {
        SliderState::new()
            .min(50.)
            .max(100.)
            .step(1.)
            .default_value(f32::from(value))
    })
}

fn slider_value(s: &Entity<SliderState>, cx: &App) -> u8 {
    s.read(cx).value().end().round().clamp(0., 100.) as u8
}

fn text(size: f32, hex: u32) -> gpui::Div {
    div().text_size(px(size)).text_color(color(hex))
}

impl SettingsDraft {
    fn edit(&self, cx: &App) -> Edit {
        Edit {
            name: self.name.read(cx).value().to_string(),
            // Not edited here: the pattern is the same for every ticket, so it stays as the file has it.
            feature_env: self.saved.feature_env.clone(),
            bar: slider_value(&self.bar, cx),
            stages: self
                .stages
                .iter()
                .map(|s| EditStage {
                    id: s.id.clone(),
                    title: s.title.read(cx).value().to_string(),
                    skill: s.skill.clone(),
                    folder: s.folder.clone(),
                    bar: s.own.then(|| slider_value(&s.bar, cx)),
                    prompt: s.prompt.read(cx).value().to_string(),
                })
                .collect(),
        }
    }
}

impl Workspace {
    /// Pipeline settings for orchestrator `orch`'s pipeline file, read afresh.
    pub(super) fn open_pipeline_settings(
        &mut self,
        orch: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((file, env, cwd)) = self.settings_target(orch, cx) else {
            window.push_notification(
                Notification::info("Start a run first: settings edit its pipeline.")
                    .title("Pipeline settings"),
                cx,
            );
            return;
        };
        let root = self.hydra.paths.root().to_path_buf();
        let saved = match pipeline::load(&root, &file) {
            Ok(p) => Edit::from(&p),
            Err(e) => {
                window.push_notification(Notification::error(e).title("Pipeline settings"), cx);
                return;
            }
        };
        let claude = hydra_core::name::EnvName::parse(&env)
            .ok()
            .map(|n| self.hydra.paths.state_dir(&n).join("claude"));
        // What the stages' sessions could use: from the orchestrator's project and each stage's
        // own folder, plus the environment's, synced and plugin skills.
        let mut folders = vec![crate::tasks_project::project_root(&cwd)];
        folders.extend(saved.stages.iter().filter_map(|s| {
            s.folder
                .as_deref()
                .map(|f| hydra_core::paths::expand_tilde(f, &self.hydra.user_home))
        }));
        let mut skills = crate::skills_list::available(claude.as_deref(), &folders);
        for s in saved.stages.iter().filter_map(|s| s.skill.clone()) {
            if !skills.contains(&s) {
                skills.push(s);
            }
        }
        skills.sort();
        let draft = self.new_draft(orch, file, saved, skills, window, cx);
        self.pipeline_settings.insert(orch, draft);
        self.show_pipeline_view(orch, super::pipeline_tab::TabView::Settings, window, cx);
    }

    /// The pipeline file, environment and folder of `orch`'s current run.
    fn settings_target(
        &self,
        orch: PaneId,
        cx: &App,
    ) -> Option<(String, String, std::path::PathBuf)> {
        let view = self.panes.get(&orch)?.read(cx);
        let token = view.mcp_token()?.to_string();
        let sv = server::get()?;
        // Its current run's pipeline, else the one it was started for (no ticket yet).
        let file = server::lock(sv.registry())
            .runs
            .current(&token)
            .map(|r| r.pipeline.file.clone())
            .or_else(|| self.orchestrators.get(&orch).cloned())?;
        (!file.is_empty()).then(|| (file, view.env().to_string(), view.cwd().clone()))
    }

    fn new_draft(
        &mut self,
        orch: PaneId,
        file: String,
        saved: Edit,
        skills: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SettingsDraft {
        let name = cx.new(|cx| InputState::new(window, cx).default_value(saved.name.clone()));
        let bar = slider(saved.bar, cx);
        let mut subs = vec![
            cx.subscribe_in(&name, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&bar, window, move |ws, _, _: &SliderEvent, window, cx| {
                ws.default_bar_moved(orch, window, cx)
            }),
        ];
        let mut draft = SettingsDraft {
            file,
            saved: saved.clone(),
            name,
            bar,
            stages: Vec::new(),
            skills,
            next_key: 0,
            _subs: Vec::new(),
        };
        for s in &saved.stages {
            let stage = self.new_stage(orch, &mut draft, s, saved.bar, window, cx, &mut subs);
            draft.stages.push(stage);
        }
        draft._subs = subs;
        draft
    }

    #[allow(clippy::too_many_arguments)]
    fn new_stage(
        &mut self,
        orch: PaneId,
        draft: &mut SettingsDraft,
        s: &EditStage,
        default: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
        subs: &mut Vec<Subscription>,
    ) -> DraftStage {
        let key = draft.next_key;
        draft.next_key += 1;
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Stage name")
                .default_value(s.title.clone())
        });
        let bar = slider(s.bar.unwrap_or(default), cx);
        subs.push(cx.subscribe_in(&title, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
        let prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("e.g. replicate {ticket}")
                .default_value(s.prompt.clone())
        });
        subs.push(cx.subscribe_in(&prompt, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
        subs.push(
            cx.subscribe_in(&bar, window, move |ws, _, _: &SliderEvent, _, cx| {
                if let Some(stage) = ws
                    .pipeline_settings
                    .get_mut(&orch)
                    .and_then(|d| d.stages.iter_mut().find(|s| s.key == key))
                {
                    stage.own = true;
                }
                cx.notify();
            }),
        );
        DraftStage {
            key,
            id: s.id.clone(),
            title,
            skill: s.skill.clone(),
            folder: s.folder.clone(),
            bar,
            own: s.bar.is_some(),
            prompt,
        }
    }

    /// The default bar moved: the stages that follow it move with it.
    fn default_bar_moved(&mut self, orch: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.pipeline_settings.get(&orch) else {
            return;
        };
        let value = f32::from(slider_value(&draft.bar, cx));
        let following: Vec<Entity<SliderState>> = draft
            .stages
            .iter()
            .filter(|s| !s.own)
            .map(|s| s.bar.clone())
            .collect();
        for s in following {
            s.update(cx, |s, cx| s.set_value(value, window, cx));
        }
        cx.notify();
    }

    fn with_draft(
        &mut self,
        orch: PaneId,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut SettingsDraft),
    ) {
        if let Some(draft) = self.pipeline_settings.get_mut(&orch) {
            f(draft);
        }
        cx.notify();
    }

    fn add_stage(&mut self, orch: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut draft) = self.pipeline_settings.remove(&orch) else {
            return;
        };
        let default = slider_value(&draft.bar, cx);
        let skill = draft.skills.first().cloned();
        let mut subs = Vec::new();
        let stage = self.new_stage(
            orch,
            &mut draft,
            &EditStage {
                id: None,
                title: "New stage".into(),
                skill,
                folder: None,
                bar: None,
                prompt: String::new(),
            },
            default,
            window,
            cx,
            &mut subs,
        );
        draft.stages.push(stage);
        draft._subs.extend(subs);
        self.pipeline_settings.insert(orch, draft);
        cx.notify();
    }

    fn reset_bar(&mut self, orch: PaneId, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.pipeline_settings.get_mut(&orch) else {
            return;
        };
        let default = f32::from(slider_value(&draft.bar, cx));
        if let Some(stage) = draft.stages.iter_mut().find(|s| s.key == key) {
            stage.own = false;
            let bar = stage.bar.clone();
            bar.update(cx, |s, cx| s.set_value(default, window, cx));
        }
        cx.notify();
    }

    fn pick_skill(&mut self, orch: PaneId, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skills) = self.pipeline_settings.get(&orch).map(|d| d.skills.clone()) else {
            return;
        };
        let mut choices: Vec<Option<String>> = vec![None];
        choices.extend(skills.into_iter().map(Some));
        let state = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity();
        let first = Cell::new(true);
        let choices = Rc::new(choices);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let (picked, workspace) = (choices.clone(), workspace.clone());
            dialog.title("Skill").child(
                Command::new(&state)
                    .items(choices.iter().map(|c| {
                        CommandItem::new().label(c.clone().unwrap_or_else(|| "No skill".into()))
                    }))
                    .placeholder("Skill")
                    .on_confirm(move |ix, window, cx| {
                        if let Some(skill) = picked.get(ix.row).cloned() {
                            window.close_dialog(cx);
                            workspace.update(cx, |ws, cx| {
                                ws.with_draft(orch, cx, |d| {
                                    if let Some(s) = d.stages.iter_mut().find(|s| s.key == key) {
                                        s.skill = skill;
                                    }
                                })
                            });
                        }
                    }),
            )
        });
    }

    /// Picks the folder stage `key`'s session starts in, written with `~` for the home folder.
    fn pick_folder(&mut self, orch: PaneId, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let home = self.hydra.user_home.clone();
        self.open_dialog_path(true, window, cx, move |ws, path, _, cx| {
            let folder = match path.strip_prefix(&home) {
                Ok(rest) => format!("~/{}", rest.to_string_lossy().replace('\\', "/")),
                Err(_) => path.to_string_lossy().into_owned(),
            };
            // The new folder's own skills join the picker.
            let found = crate::skills_list::installed(None, &path);
            ws.with_draft(orch, cx, |d| {
                if let Some(s) = d.stages.iter_mut().find(|s| s.key == key) {
                    s.folder = Some(folder);
                }
                for skill in found {
                    if !d.skills.contains(&skill) {
                        d.skills.push(skill);
                    }
                }
                d.skills.sort();
            });
        });
    }

    fn discard_settings(&mut self, orch: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.pipeline_settings.remove(&orch) else {
            return;
        };
        let fresh = self.new_draft(orch, draft.file, draft.saved, draft.skills, window, cx);
        self.pipeline_settings.insert(orch, fresh);
        cx.notify();
    }

    fn save_settings(&mut self, orch: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.pipeline_settings.get(&orch) else {
            return;
        };
        let edit = draft.edit(cx);
        if let Err(why) = validate(&edit) {
            window.push_notification(Notification::warning(why).title("Pipeline settings"), cx);
            return;
        }
        let (file, skills) = (draft.file.clone(), draft.skills.clone());
        let root = self.hydra.paths.root().to_path_buf();
        let saved = pipeline::save(&root, &file, &edit)
            .map_err(|e| e.to_string())
            .and_then(|()| pipeline::load(&root, &file));
        match saved {
            Ok(p) => {
                // Open runs take the new bars at once (spec §2, §5.1).
                if let Some(sv) = server::get() {
                    server::lock(sv.registry())
                        .runs
                        .refresh_from_file(&file, &p);
                }
                crate::action_log::write(format!("pipeline {file}: settings saved"));
                let fresh = self.new_draft(orch, file, Edit::from(&p), skills, window, cx);
                self.pipeline_settings.insert(orch, fresh);
                window.push_notification(
                    Notification::success("A stage's settings apply when it next starts.")
                        .title("Pipeline saved"),
                    cx,
                );
            }
            Err(e) => {
                window.push_notification(Notification::error(e).title("Pipeline settings"), cx)
            }
        }
        cx.notify();
    }

    /// The Settings view for orchestrator `orch`.
    pub(super) fn render_pipeline_settings(
        &self,
        orch: PaneId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = self.pipeline_settings.get(&orch) else {
            return v_flex().into_any_element();
        };
        let edit = draft.edit(cx);
        let dirty = edit != draft.saved;
        let invalid = validate(&edit).err();
        let default = edit.bar;
        let floor = server::get().map_or(crate::runs::MIN_WEEK_LEFT, |sv| {
            server::lock(sv.registry()).min_week_left
        });
        let count = draft.stages.len();
        let section = |title: &str| text(11., theme::TEXT_MUTED).child(title.to_uppercase());
        let mut stages = v_flex().gap_2();
        for (i, s) in draft.stages.iter().enumerate() {
            let key = s.key;
            let value = slider_value(&s.bar, cx);
            stages = stages.child(
                v_flex()
                    .id(SharedString::from(format!("settings-stage-{orch}-{key}")))
                    .gap_2()
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(10.))
                    .bg(color(0x131616))
                    .border_1()
                    .border_color(color(0x1f2524))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .child(
                                div()
                                    .size(px(24.))
                                    .flex_none()
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .border_1()
                                    .border_color(color(theme::BORDER_STRONG))
                                    .child(
                                        text(11.5, theme::TEXT_SECONDARY)
                                            .child(format!("{}", i + 1)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .w(px(ARROWS_W))
                                    .child(
                                        Button::new(SharedString::from(format!("up-{key}")))
                                            .label("↑")
                                            .ghost()
                                            .xsmall()
                                            .disabled(i == 0)
                                            .on_click(cx.listener(move |ws, _, _, cx| {
                                                ws.with_draft(orch, cx, |d| d.stages.swap(i, i - 1))
                                            })),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!("down-{key}")))
                                            .label("↓")
                                            .ghost()
                                            .xsmall()
                                            .disabled(i + 1 == count)
                                            .on_click(cx.listener(move |ws, _, _, cx| {
                                                ws.with_draft(orch, cx, |d| d.stages.swap(i, i + 1))
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(120.))
                                    .child(Input::new(&s.title).small()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("skill-{key}")))
                                    .label(s.skill.clone().unwrap_or_else(|| "No skill".into()))
                                    .dropdown_caret(true)
                                    .outline()
                                    .small()
                                    .w(px(120.))
                                    .flex_none()
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.pick_skill(orch, key, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("folder-{key}")))
                                    .label(match &s.folder {
                                        Some(f) => format!(
                                            "in {}",
                                            std::path::Path::new(f)
                                                .file_name()
                                                .map_or(f.clone(), |n| n
                                                    .to_string_lossy()
                                                    .into_owned())
                                        ),
                                        None => "Orchestrator's folder".into(),
                                    })
                                    .dropdown_caret(true)
                                    .outline()
                                    .small()
                                    .w(px(165.))
                                    .flex_none()
                                    .tooltip("The folder this stage's session starts in")
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.pick_folder(orch, key, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(24.))
                                    .flex_none()
                                    .when(s.folder.is_some(), |slot| {
                                        slot.child(
                                            Button::new(SharedString::from(format!(
                                                "folder-clear-{key}"
                                            )))
                                            .label("↺")
                                            .ghost()
                                            .xsmall()
                                            .tooltip("Start in the orchestrator's folder")
                                            .on_click(cx.listener(move |ws, _, _, cx| {
                                                ws.with_draft(orch, cx, |d| {
                                                    if let Some(s) =
                                                        d.stages.iter_mut().find(|s| s.key == key)
                                                    {
                                                        s.folder = None;
                                                    }
                                                })
                                            })),
                                        )
                                    }),
                            )
                            .child(div().w(px(120.)).flex_none().child(Slider::new(&s.bar)))
                            .child(
                                text(
                                    12.5,
                                    if s.own {
                                        theme::AMBER
                                    } else {
                                        theme::TEXT_SECONDARY
                                    },
                                )
                                .w(px(36.))
                                .flex_none()
                                .child(format!("{value}%")),
                            )
                            .child(div().w(px(44.)).flex_none().when(s.own, |d| {
                                d.child(
                                    Button::new(SharedString::from(format!("reset-{key}")))
                                        .label("Reset")
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |ws, _, window, cx| {
                                            ws.reset_bar(orch, key, window, cx)
                                        })),
                                )
                            }))
                            .child(
                                Button::new(SharedString::from(format!("remove-{key}")))
                                    .label("×")
                                    .ghost()
                                    .xsmall()
                                    .disabled(count == 1)
                                    .on_click(cx.listener(move |ws, _, _, cx| {
                                        ws.with_draft(orch, cx, |d| {
                                            d.stages.retain(|s| s.key != key);
                                        })
                                    })),
                            ),
                    )
                    // What the stage's session is asked; hydraui's note follows it.
                    .child(
                        // Lined up under the title: past the number, the arrows and their gaps.
                        div().pl(px(24. + 10. + ARROWS_W + 10.)).child(
                            Input::new(&s.prompt)
                                .small()
                                .prefix(text(11.5, theme::TEXT_MUTED).pr(px(6.)).child("Prompt")),
                        ),
                    ),
            );
        }
        let save_bar =
            if dirty {
                let (message, hex) = match invalid {
                    Some(why) => (why.to_string(), theme::RED),
                    None => (
                        format!("Unsaved changes · {count} stages"),
                        theme::TEXT_SECONDARY,
                    ),
                };
                h_flex()
                    .px(px(28.))
                    .py(px(12.))
                    .gap_3()
                    .border_t_1()
                    .border_color(color(theme::BORDER_SUBTLE))
                    .bg(color(theme::SURFACE))
                    .child(div().size(px(8.)).rounded_full().bg(color(theme::AMBER)))
                    .child(text(12.5, hex).flex_1().child(message))
                    .child(
                        Button::new("settings-discard")
                            .label("Discard")
                            .ghost()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.discard_settings(orch, window, cx)
                            })),
                    )
                    .child(
                        Button::new("settings-save")
                            .label("Save changes")
                            .primary()
                            .disabled(invalid.is_some())
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.save_settings(orch, window, cx)
                            })),
                    )
            } else {
                h_flex()
                .px(px(28.))
                .py(px(12.))
                .child(text(12., theme::TEXT_MUTED).child(
                "Adding, removing or reordering stages applies from the next ticket; a stage's own \
                 settings apply when it next starts.",
            ))
            };
        let limit = |label: &str, value: String| {
            v_flex()
                .flex_1()
                .px(px(14.))
                .py(px(12.))
                .gap_1()
                .rounded(px(10.))
                .bg(color(theme::SURFACE))
                .border_1()
                .border_color(color(theme::BORDER_SUBTLE))
                .child(text(12., theme::TEXT_MUTED).child(label.to_string()))
                .child(text(16., theme::TEXT).child(value))
        };
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .child(
                div()
                    .id(SharedString::from(format!("settings-{orch}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .px(px(36.))
                            .py(px(28.))
                            .gap(px(28.))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(
                                        text(20., theme::TEXT)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Pipeline settings"),
                                    )
                                    .child(
                                        Button::new("settings-back")
                                            .label("← Back to pipeline")
                                            .ghost()
                                            .small()
                                            .on_click(cx.listener(move |ws, _, window, cx| {
                                                ws.show_pipeline_view(
                                                    orch,
                                                    super::pipeline_tab::TabView::Pipeline,
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Orchestrator name"))
                                    .child(div().max_w(px(480.)).child(Input::new(&draft.name)))
                                    .child(text(12., theme::TEXT_MUTED).child(
                                        "Shown in the sidebar, notifications and Slack messages",
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Default confidence bar"))
                                    .child(
                                        h_flex()
                                            .max_w(px(480.))
                                            .gap_3()
                                            .px(px(14.))
                                            .py(px(12.))
                                            .rounded(px(10.))
                                            .bg(color(theme::SURFACE))
                                            .border_1()
                                            .border_color(color(theme::BORDER_SUBTLE))
                                            .child(div().flex_1().child(Slider::new(&draft.bar)))
                                            .child(
                                                text(13., theme::TEXT).child(format!("{default}%")),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section(&format!(
                                        "Stages · run top to bottom · {count} stages"
                                    )))
                                    .child(text(12., theme::TEXT_MUTED).child(
                                        "Prompts can use {ticket}, {ticket_lower}, {feature_env} \
                                         and {handoff}. After each one, hydraui adds a short note: \
                                         where the stage sits, its skill, and to finish with \
                                         stage_report.",
                                    ))
                                    .child(stages)
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("settings-add-{orch}")))
                                            .px(px(14.))
                                            .py(px(10.))
                                            .rounded(px(10.))
                                            .border_1()
                                            .border_dashed()
                                            .border_color(color(0x2f7d6f))
                                            .bg(color(0x0f2622))
                                            .hover(|d| d.bg(color(0x133630)))
                                            .cursor_pointer()
                                            .text_center()
                                            .text_color(color(theme::TEAL))
                                            .text_size(px(13.5))
                                            .child("+ Add stage")
                                            .on_click(cx.listener(move |ws, _, window, cx| {
                                                ws.add_stage(orch, window, cx)
                                            })),
                                    )
                                    .child(
                                        h_flex()
                                            .px(px(14.))
                                            .py(px(10.))
                                            .gap_3()
                                            .rounded(px(10.))
                                            .border_1()
                                            .border_color(color(theme::BORDER_SUBTLE))
                                            .child(
                                                text(13., theme::TEXT_SECONDARY)
                                                    .child("Done · PR left for a person to merge"),
                                            )
                                            .child(div().flex_1())
                                            .child(
                                                text(11.5, theme::TEXT_MUTED)
                                                    .child("Always last · can't be removed"),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex().gap_2().child(section("Limits")).child(
                                    h_flex()
                                        .gap_3()
                                        .max_w(px(640.))
                                        .child(limit(
                                            "Retries before escalating",
                                            format!("{MAX_ATTEMPTS}"),
                                        ))
                                        .child(limit(
                                            "Claude allowance floor",
                                            format!("{floor}%"),
                                        )),
                                ),
                            ),
                    ),
            )
            .child(save_bar)
            .into_any_element()
    }
}
