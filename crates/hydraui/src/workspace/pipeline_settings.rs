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
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{Disableable, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::pipeline::{self, Assignee, Edit, EditStage, LinearSettings, SlackSettings, validate};
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
    /// The branch rule, e.g. `{ticket}`; empty: none.
    branch: Entity<InputState>,
    /// The Slack channel for product questions; empty: stages ask the user in hydraui.
    product: Entity<InputState>,
    /// The card's accent; `None`: the default teal.
    color: Option<u32>,
    /// Tickets run at once, each with its own orchestrator.
    runs: u8,
    /// The Advanced part is unfolded.
    advanced: bool,
    bar: Entity<SliderState>,
    stages: Vec<DraftStage>,
    skills: Vec<String>,
    next_key: u64,
    linear: LinearDraft,
    slack: SlackDraft,
    /// The Trigger dropdown: None, Linear or Slack.
    trigger: Entity<SelectState<Vec<&'static str>>>,
    _subs: Vec<Subscription>,
}

/// The Trigger dropdown's choices, in order.
const TRIGGERS: [&str; 3] = ["None", "Linear", "Slack"];

/// The Slack section: off saves the pipeline without a `[slack]` table. Linear and Slack can't
/// both be on.
struct SlackDraft {
    on: bool,
    /// Kept as the file has it (`watch`).
    watch: bool,
    channel: Entity<InputState>,
    label: Entity<InputState>,
    every: Entity<InputState>,
    lookback: Entity<InputState>,
    reaction: Entity<InputState>,
}

impl SlackDraft {
    fn edit(&self, cx: &App) -> Option<SlackSettings> {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        self.on.then(|| SlackSettings {
            channel: text(&self.channel),
            label: text(&self.label),
            reaction: text(&self.reaction).trim_matches(':').to_string(),
            // Not a number: 0, which validation names.
            every_minutes: text(&self.every).parse().unwrap_or(0),
            lookback_hours: text(&self.lookback).parse().unwrap_or(0),
            watch: self.watch,
        })
    }
}

/// The Linear section (phase 2 spec §2, §5): off saves the pipeline without a `[linear]` table.
struct LinearDraft {
    on: bool,
    /// Kept as the file has it (`watch`).
    watch: bool,
    any: bool,
    team: Entity<InputState>,
    pick: Entity<InputState>,
    claim: Entity<InputState>,
    passed: Entity<InputState>,
    every: Entity<InputState>,
}

impl LinearDraft {
    fn edit(&self, cx: &App) -> Option<LinearSettings> {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        self.on.then(|| LinearSettings {
            team: text(&self.team),
            pick: text(&self.pick),
            assignee: if self.any {
                Assignee::Any
            } else {
                Assignee::Unassigned
            },
            claim: text(&self.claim),
            passed: text(&self.passed),
            // Not a number: 0, which validation names.
            every_minutes: text(&self.every).parse().unwrap_or(0),
            watch: self.watch,
        })
    }
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
            branch: Some(self.branch.read(cx).value().trim().to_string()).filter(|b| !b.is_empty()),
            linear: self.linear.edit(cx),
            slack: self.slack.edit(cx),
            autostart: self.saved.autostart.clone(),
            product_channel: Some(self.product.read(cx).value().trim().to_string())
                .filter(|c| !c.is_empty()),
            color: self.color,
            runs_at_once: self.runs,
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
    pub(super) fn settings_target(
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
        let branch = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("{ticket}")
                .default_value(saved.branch.clone().unwrap_or_default())
        });
        let product = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("#product")
                .default_value(saved.product_channel.clone().unwrap_or_default())
        });
        let bar = slider(saved.bar, cx);
        let mut subs = vec![
            cx.subscribe_in(&name, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&branch, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&product, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&bar, window, move |ws, _, _: &SliderEvent, window, cx| {
                ws.default_bar_moved(orch, window, cx)
            }),
        ];
        let l = saved.linear.clone();
        let mut input = |value: String, placeholder: &'static str| {
            let i = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            });
            subs.push(cx.subscribe_in(&i, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
            i
        };
        let field = |f: fn(&LinearSettings) -> String, default: &str| {
            l.as_ref().map_or(default.to_string(), f)
        };
        let linear = LinearDraft {
            on: l.is_some(),
            watch: l.as_ref().is_some_and(|l| l.watch),
            any: l.as_ref().is_some_and(|l| l.assignee == Assignee::Any),
            team: input(field(|l| l.team.clone(), ""), "INTH"),
            pick: input(
                field(|l| l.pick.clone(), pipeline::DEFAULT_PICK),
                pipeline::DEFAULT_PICK,
            ),
            claim: input(
                field(|l| l.claim.clone(), pipeline::DEFAULT_CLAIM),
                pipeline::DEFAULT_CLAIM,
            ),
            passed: input(
                field(|l| l.passed.clone(), pipeline::DEFAULT_PASSED),
                pipeline::DEFAULT_PASSED,
            ),
            every: input(
                field(
                    |l| l.every_minutes.to_string(),
                    &pipeline::DEFAULT_EVERY_MINUTES.to_string(),
                ),
                "5",
            ),
        };
        let sl = saved.slack.clone();
        let sfield = |f: fn(&SlackSettings) -> String, default: &str| {
            sl.as_ref().map_or(default.to_string(), f)
        };
        let slack = SlackDraft {
            on: sl.is_some(),
            watch: sl.as_ref().is_some_and(|s| s.watch),
            channel: input(
                sfield(|s| s.channel.clone(), ""),
                "In Slack: channel name → About → Channel ID",
            ),
            label: input(sfield(|s| s.label.clone(), ""), "#pull-requests"),
            every: input(
                sfield(
                    |s| s.every_minutes.to_string(),
                    &pipeline::DEFAULT_SLACK_EVERY_MINUTES.to_string(),
                ),
                "2",
            ),
            lookback: input(
                sfield(
                    |s| s.lookback_hours.to_string(),
                    &pipeline::DEFAULT_LOOKBACK_HOURS.to_string(),
                ),
                "24",
            ),
            reaction: input(
                sfield(|s| s.reaction.clone(), pipeline::DEFAULT_REACTION),
                "eyes (👀)",
            ),
        };
        let chosen = if saved.slack.is_some() {
            2
        } else if saved.linear.is_some() {
            1
        } else {
            0
        };
        let trigger = cx.new(|cx| {
            SelectState::new(
                TRIGGERS.to_vec(),
                Some(gpui_component::IndexPath::new(chosen)),
                window,
                cx,
            )
        });
        subs.push(cx.subscribe_in(
            &trigger,
            window,
            move |ws, _, e: &SelectEvent<Vec<&'static str>>, _, cx| {
                let SelectEvent::Confirm(Some(choice)) = e else {
                    return;
                };
                let choice = *choice;
                // One trigger: choosing one turns the other off.
                ws.with_draft(orch, cx, |d| {
                    d.linear.on = choice == "Linear";
                    d.slack.on = choice == "Slack";
                })
            },
        ));
        let mut draft = SettingsDraft {
            file,
            saved: saved.clone(),
            name,
            branch,
            product,
            color: saved.color,
            runs: saved.runs_at_once,
            advanced: false,
            bar,
            stages: Vec::new(),
            skills,
            next_key: 0,
            linear,
            slack,
            trigger,
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

    /// The Linear section (phase 2 spec §2): where the pipeline's tickets come from.
    fn render_linear_settings(
        &self,
        orch: PaneId,
        draft: &SettingsDraft,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let l = &draft.linear;
        let row = |label: &str, input: &Entity<InputState>| {
            h_flex()
                .gap_3()
                .child(
                    text(12.5, theme::TEXT_SECONDARY)
                        .w(px(170.))
                        .flex_none()
                        .child(label.to_string()),
                )
                .child(div().w(px(300.)).child(Input::new(input).small()))
        };
        let mut body = v_flex().gap_2();
        if l.on {
            let who = |any: bool, label: &'static str| {
                Button::new(SharedString::from(format!("linear-any-{any}-{orch}")))
                    .label(label)
                    .when(l.any == any, |b| b.primary())
                    .when(l.any != any, |b| b.ghost())
                    .xsmall()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        ws.with_draft(orch, cx, |d| d.linear.any = any)
                    }))
            };
            body = body
                .child(row("Team", &l.team))
                .child(row("Pick tickets in", &l.pick))
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            text(12.5, theme::TEXT_SECONDARY)
                                .w(px(170.))
                                .flex_none()
                                .child("Whose tickets"),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(who(false, "Unassigned"))
                                .child(who(true, "Any")),
                        ),
                )
                .child(row("Then move it to", &l.claim))
                .child(row("When done, move it to", &l.passed))
                .child(row("Check every (minutes)", &l.every));
        }
        body
    }

    /// The Slack trigger's options: pull requests posted in a channel.
    fn render_slack_settings(&self, draft: &SettingsDraft) -> impl IntoElement + use<> {
        let sl = &draft.slack;
        let row = |label: &str, input: &Entity<InputState>| {
            h_flex()
                .gap_3()
                .child(
                    text(12.5, theme::TEXT_SECONDARY)
                        .w(px(170.))
                        .flex_none()
                        .child(label.to_string()),
                )
                .child(div().w(px(300.)).child(Input::new(input).small()))
        };
        let mut body = v_flex().gap_2();
        if sl.on {
            body = body
                .child(row("Channel name", &sl.label))
                .child(row("Channel ID", &sl.channel))
                .child(row("Mark taken with", &sl.reaction))
                .child(row("Check every (minutes)", &sl.every))
                .child(row("Look back (hours)", &sl.lookback));
        }
        body
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
                            // 1. What it's called and how it looks in the sidebar.
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Name"))
                                    .child(div().max_w(px(480.)).child(Input::new(&draft.name)))
                                    .child(color_swatches(orch, draft.color, cx)),
                            )
                            // 2. Where its work comes from.
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section_tip(
                                        "Where work comes from",
                                        "Linear: picks tickets from a team. Slack: takes pull \
                                         requests posted in a channel. None: you give it work.",
                                    ))
                                    .child(div().w(px(220.)).child(
                                        Select::new(&draft.trigger).small().menu_width(px(220.)),
                                    ))
                                    .child(self.render_linear_settings(orch, draft, cx))
                                    .child(self.render_slack_settings(draft)),
                            )
                            // 3. What it does, step by step.
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section_tip(
                                        "Steps",
                                        "Each step is a Claude session, run top to bottom; the \
                                         next starts when one passes its bar. In a prompt, \
                                         {ticket} is the ticket, {pr} the pull request link, \
                                         {handoff} what the step before handed over.",
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
                                            .child("+ Add step")
                                            .on_click(cx.listener(move |ws, _, window, cx| {
                                                ws.add_stage(orch, window, cx)
                                            })),
                                    ),
                            )
                            // 4. Everything else, folded away.
                            .child(
                                div()
                                    .id(SharedString::from(format!("settings-advanced-{orch}")))
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .text_color(color(theme::TEXT_SECONDARY))
                                    .hover(|d| d.text_color(color(theme::TEXT)))
                                    .child(if draft.advanced {
                                        "▾ Advanced"
                                    } else {
                                        "▸ Advanced"
                                    })
                                    .on_click(cx.listener(move |ws, _, _, cx| {
                                        ws.with_draft(orch, cx, |d| d.advanced = !d.advanced)
                                    })),
                            )
                            .when(draft.advanced, |d| {
                                d.child(
                                    v_flex()
                                        .gap(px(20.))
                                        .pl(px(14.))
                                        .border_l_2()
                                        .border_color(color(theme::BORDER_SUBTLE))
                                        .child(
                                            v_flex()
                                                .gap_2()
                                                .child(section_tip(
                                                    "Tickets at once",
                                                    "One at a time: one orchestrator, one ticket \
                                                     after another. Up to N: when every \
                                                     orchestrator is busy and more work is \
                                                     waiting, another starts, up to N.",
                                                ))
                                                .child(runs_buttons(orch, draft.runs, cx)),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_2()
                                                .child(section_tip(
                                                    "Pass mark",
                                                    "How confident a step must be to move on. A \
                                                     step can set its own.",
                                                ))
                                                .child(
                                                    h_flex()
                                                        .max_w(px(480.))
                                                        .gap_3()
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .child(Slider::new(&draft.bar)),
                                                        )
                                                        .child(
                                                            text(13., theme::TEXT)
                                                                .child(format!("{default}%")),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_2()
                                                .child(section_tip(
                                                    "Branch name",
                                                    "The branch every step must use, e.g. \
                                                     {ticket}. Empty: any branch.",
                                                ))
                                                .child(
                                                    div()
                                                        .max_w(px(480.))
                                                        .child(Input::new(&draft.branch)),
                                                ),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_2()
                                                .child(section_tip(
                                                    "Product questions channel",
                                                    "Where a step asks product before changing \
                                                     what users see. Empty: it asks you here.",
                                                ))
                                                .child(
                                                    div()
                                                        .max_w(px(480.))
                                                        .child(Input::new(&draft.product)),
                                                ),
                                        )
                                        .child(text(12., theme::TEXT_MUTED).child(format!(
                                            "A step that fails is retried up to {MAX_ATTEMPTS} \
                                             times. Steps pause below {floor}% of the Claude week."
                                        ))),
                                )
                            }),
                    ),
            )
            .child(save_bar)
            .into_any_element()
    }
}

/// The Colour row: one swatch per `pipeline::COLORS`, the chosen one ringed.
fn color_swatches(
    orch: PaneId,
    chosen: Option<u32>,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let chosen = chosen.unwrap_or(theme::TEAL);
    h_flex()
        .gap_2()
        .children(pipeline::COLORS.iter().map(|(name, hex)| {
            let (name, hex) = (*name, *hex);
            let on = hex == chosen;
            div()
                .id(SharedString::from(format!(
                    "pipeline-color-{orch}-{hex:06x}"
                )))
                .size(px(22.))
                .rounded_full()
                .p(px(3.))
                .border_2()
                .border_color(if on {
                    color(theme::TEXT)
                } else {
                    gpui::transparent_black()
                })
                .cursor_pointer()
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(name).build(window, cx)
                })
                .child(div().size_full().rounded_full().bg(color(hex)))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    // Teal is the default: chosen, the file needs no colour.
                    let pick = (hex != theme::TEAL).then_some(hex);
                    ws.with_draft(orch, cx, |d| d.color = pick)
                }))
        }))
        .into_any_element()
}

/// The Runs at once row: 1 to `pipeline::MAX_RUNS_AT_ONCE`, the chosen one filled.
fn runs_buttons(orch: PaneId, chosen: u8, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    h_flex()
        .gap_1()
        .children((1..=pipeline::MAX_RUNS_AT_ONCE).map(|n| {
            let b = Button::new(SharedString::from(format!("pipeline-runs-{orch}-{n}")))
                .label(if n == 1 {
                    "One at a time".to_string()
                } else {
                    format!("Up to {n}")
                })
                .small()
                .on_click(cx.listener(move |ws, _, _, cx| ws.with_draft(orch, cx, |d| d.runs = n)));
            if n == chosen { b.primary() } else { b.ghost() }
        }))
        .into_any_element()
}

/// A section heading with its explanation on hover (ⓘ), so the page stays short.
fn section_tip(title: &str, tip: &'static str) -> gpui::AnyElement {
    h_flex()
        .gap_1()
        .items_center()
        .child(text(11., theme::TEXT_MUTED).child(title.to_uppercase()))
        .child(
            div()
                .id(SharedString::from(format!("tip-{title}")))
                .text_size(px(11.))
                .text_color(color(theme::TEXT_MUTED))
                .cursor(gpui::CursorStyle::Arrow)
                .child("ⓘ")
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
                }),
        )
        .into_any_element()
}
