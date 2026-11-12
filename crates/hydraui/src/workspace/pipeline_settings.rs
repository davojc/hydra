//! Pipeline settings (orchestrator spec §7.4; the design's View 2), in the Pipeline tab: the
//! orchestrator's name, the default bar, the stages (reorder, name, skill, bar, remove, add),
//! the limits, and a save bar. Saving rewrites the pipeline file (`pipeline::save`) and open
//! runs take the new bars at once; stage changes apply from the next ticket.
use crate::inbox::{Note, PushNote};
use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Subscription, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::select::{SelectEvent, SelectState};
use gpui_component::slider::{SliderEvent, SliderState};
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::pipeline::{self, Assignee, Edit, EditStage, LinearSettings, SlackSettings, validate};

mod view;

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
    prompt: Entity<TextareaState>,
    /// A Jev stage's question: shown, not edited here.
    jev: Option<JevDraft>,
    /// Keeps checking below its bar (core spec §62).
    keep: Option<pipeline::KeepChecking>,
    /// Its card is unfolded (core spec §63).
    open: bool,
    /// Its More options are unfolded.
    more: bool,
}

/// A Jev step being edited (email triage spec §4): its question, its categories as rows, the
/// file it reads and the model.
struct JevDraft {
    instructions: Entity<TextareaState>,
    /// Each category's row key, name and meaning, and whether it ends the run.
    options: Vec<(u64, Entity<InputState>, Entity<InputState>, bool)>,
    input: Entity<InputState>,
    model: Entity<InputState>,
}

impl JevDraft {
    fn edit(&self, cx: &App) -> crate::jev_stage::JevStage {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        crate::jev_stage::JevStage {
            input: text(&self.input),
            model: Some(text(&self.model)).filter(|m| !m.is_empty()),
            instructions: self.instructions.read(cx).value().trim().to_string(),
            options: self
                .options
                .iter()
                .map(|(_, k, v, _)| (text(k), text(v)))
                .filter(|(k, v)| !(k.is_empty() && v.is_empty()))
                .collect(),
            ends: self
                .options
                .iter()
                .filter(|(_, _, _, end)| *end)
                .map(|(_, k, _, _)| text(k))
                .filter(|k| !k.is_empty())
                .collect(),
        }
    }
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
    /// How steps write product questions; empty: as each sees fit.
    product_style: Entity<TextareaState>,
    /// Steps ask product before a change beyond the ticket (`[product] ask`).
    product_asks: bool,
    /// The card's accent; `None`: the default teal.
    color: Option<u32>,
    /// Its icon; `None`: the ◆ (core spec §64).
    icon: Option<String>,
    /// The colour and icon picker is open.
    look_open: bool,
    /// Tickets run at once, each with its own orchestrator.
    runs: u8,
    /// What a waiting stage's session does (core spec §60).
    waiting: pipeline::WaitingSettings,
    /// The Advanced part is unfolded.
    advanced: bool,
    /// The source card is unfolded, and its More options (core spec §63).
    source_open: bool,
    source_more: bool,
    /// **Add a stage** shows its two kinds to pick from.
    adding: bool,
    /// The page's scroll, so the setup checklist can take you to a part.
    scroll: gpui::ScrollHandle,
    bar: Entity<SliderState>,
    stages: Vec<DraftStage>,
    skills: Vec<String>,
    next_key: u64,
    linear: LinearDraft,
    slack: SlackDraft,
    schedule: ScheduleDraft,
    mail: MailDraft,
    /// The Trigger dropdown: None, Linear, Slack, Schedule or Mail.
    trigger: Entity<SelectState<Vec<&'static str>>>,
    _subs: Vec<Subscription>,
}

/// The Trigger dropdown's choices, in order.
const TRIGGERS: [&str; 5] = ["None", "Linear", "Slack", "Schedule", "Mail"];

/// The Mail section (email triage spec §3): off saves the pipeline without a `[mail]` table.
struct MailDraft {
    on: bool,
    /// When it looks: days and a time, e.g. `weekdays 09:00`.
    check: Entity<InputState>,
    /// Days each look covers.
    look_back: Entity<InputState>,
    /// A Gmail search narrowing it; empty: `in:inbox`.
    query: Entity<InputState>,
    /// The team swarm claims show in; empty: the labels alone.
    swarm: Entity<InputState>,
    env: Entity<InputState>,
}

impl MailDraft {
    fn edit(&self, cx: &App) -> Option<pipeline::MailSettings> {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        self.on.then(|| pipeline::MailSettings {
            check: text(&self.check),
            // Not a number: 0, which the save bar names.
            look_back_days: text(&self.look_back).parse().unwrap_or(0),
            query: Some(text(&self.query))
                .filter(|q| !q.is_empty())
                .unwrap_or_else(|| "in:inbox".to_string()),
            env: Some(text(&self.env)).filter(|e| !e.is_empty()),
            swarm: Some(text(&self.swarm)).filter(|e| !e.is_empty()),
        })
    }
}

/// The Schedule section (core spec §58): off saves the pipeline without a `[schedule]` table.
struct ScheduleDraft {
    on: bool,
    /// Days and a time, e.g. `mon 18:00`.
    every: Entity<InputState>,
    /// The orchestrator's environment; empty: `[autostart]`'s, else the default.
    env: Entity<InputState>,
}

impl ScheduleDraft {
    fn edit(&self, cx: &App) -> Option<pipeline::ScheduleSettings> {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        self.on.then(|| pipeline::ScheduleSettings {
            every: text(&self.every),
            env: Some(text(&self.env)).filter(|e| !e.is_empty()),
        })
    }
}

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
    done_reaction: Entity<InputState>,
}

impl SlackDraft {
    fn edit(&self, cx: &App) -> Option<SlackSettings> {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        self.on.then(|| SlackSettings {
            channel: text(&self.channel),
            label: text(&self.label),
            reaction: text(&self.reaction).trim_matches(':').to_string(),
            done_reaction: text(&self.done_reaction).trim_matches(':').to_string(),
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
    /// Tickets without an SLA too (`sla_only` off).
    no_sla: bool,
    team: Entity<InputState>,
    pick: Entity<InputState>,
    claim: Entity<InputState>,
    passed: Entity<InputState>,
    every: Entity<InputState>,
    /// Comma-separated; empty: any label.
    labels: Entity<InputState>,
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
            sla_only: !self.no_sla,
            labels: pipeline::clean_labels(
                self.labels.read(cx).value().split(',').map(str::to_string),
            ),
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
            schedule: self.schedule.edit(cx),
            mail: self.mail.edit(cx),
            autostart: self.saved.autostart.clone(),
            product_channel: Some(self.product.read(cx).value().trim().to_string())
                .filter(|c| !c.is_empty()),
            product_style: Some(self.product_style.read(cx).value().trim().to_string())
                .filter(|c| !c.is_empty()),
            product_asks: self.product_asks,
            color: self.color,
            icon: self.icon.clone(),
            runs_at_once: self.runs,
            waiting: self.waiting,
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
                    jev: s.jev.as_ref().map(|j| j.edit(cx)),
                    keep: s.keep,
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
            window.push_note(
                Note::info("Start a run first: settings edit its pipeline.")
                    .title("Pipeline settings"),
                cx,
            );
            return;
        };
        let root = self.hydra.paths.root().to_path_buf();
        let saved = match pipeline::load(&root, &file) {
            Ok(p) => Edit::from(&p),
            Err(e) => {
                window.push_note(Note::error(e).title("Pipeline settings"), cx);
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
        let product_style = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Or say how, e.g. one line on why it matters, then the options")
                .default_value(saved.product_style.clone().unwrap_or_default())
        });
        let bar = slider(saved.bar, cx);
        let mut subs = vec![
            cx.subscribe_in(&name, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&branch, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&product, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&product_style, window, |_, _, _: &InputEvent, _, cx| {
                cx.notify()
            }),
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
            no_sla: l.as_ref().is_some_and(|l| !l.sla_only),
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
            labels: input(
                field(|l| l.labels.join(", "), ""),
                "Any label; or e.g. Bug, Frontend",
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
            done_reaction: input(
                sfield(|s| s.done_reaction.clone(), ""),
                "white_check_mark (✅); empty for none",
            ),
        };
        let sc = saved.schedule.clone();
        let schedule = ScheduleDraft {
            on: sc.is_some(),
            every: input(
                sc.as_ref().map_or(String::new(), |s| s.every.clone()),
                "mon 18:00 · weekdays 09:00 · daily 08:30",
            ),
            env: input(
                sc.as_ref().and_then(|s| s.env.clone()).unwrap_or_default(),
                "Environment; empty: the default",
            ),
        };
        let ml = saved.mail.clone();
        let mail = MailDraft {
            on: ml.is_some(),
            check: input(
                ml.as_ref()
                    .map_or("weekdays 09:00".to_string(), |m| m.check.clone()),
                "weekdays 09:00 · mon 09:00 · daily 08:30",
            ),
            look_back: input(ml.as_ref().map_or(7, |m| m.look_back_days).to_string(), "7"),
            query: input(
                ml.as_ref()
                    .map(|m| m.query.clone())
                    .filter(|q| q != "in:inbox")
                    .unwrap_or_default(),
                "Gmail search; empty: in:inbox",
            ),
            swarm: input(
                ml.as_ref()
                    .and_then(|m| m.swarm.clone())
                    .unwrap_or_default(),
                "Team swarm for claims; empty: Gmail labels alone",
            ),
            env: input(
                ml.as_ref().and_then(|m| m.env.clone()).unwrap_or_default(),
                "Environment; empty: the default",
            ),
        };
        let chosen = if saved.mail.is_some() {
            4
        } else if saved.schedule.is_some() {
            3
        } else if saved.slack.is_some() {
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
                    d.schedule.on = choice == "Schedule";
                    d.mail.on = choice == "Mail";
                })
            },
        ));
        let mut draft = SettingsDraft {
            file,
            saved: saved.clone(),
            name,
            branch,
            product,
            product_style,
            product_asks: saved.product_asks,
            color: saved.color,
            icon: saved.icon.clone(),
            look_open: false,
            runs: saved.runs_at_once,
            waiting: saved.waiting,
            advanced: false,
            source_open: false,
            source_more: false,
            adding: false,
            scroll: gpui::ScrollHandle::new(),
            bar,
            stages: Vec::new(),
            skills,
            next_key: 0,
            linear,
            slack,
            schedule,
            mail,
            trigger,
            _subs: Vec::new(),
        };
        for s in &saved.stages {
            let stage = self.new_stage(orch, &mut draft, s, saved.bar, window, cx, &mut subs);
            draft.stages.push(stage);
        }
        draft._subs = subs;
        // Unfold what needs doing; a one-step pipeline shows its step.
        let edit = draft.edit(cx);
        for (part, _) in pipeline::problems(&edit) {
            match part {
                pipeline::Part::Source => draft.source_open = true,
                pipeline::Part::Step(i) => {
                    if let Some(s) = draft.stages.get_mut(i) {
                        s.open = true;
                    }
                }
                pipeline::Part::Advanced => draft.advanced = true,
                pipeline::Part::Name | pipeline::Part::Steps => {}
            }
        }
        if let [only] = draft.stages.as_mut_slice() {
            only.open = true;
        }
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
                .placeholder("Step name")
                .default_value(s.title.clone())
        });
        let bar = slider(s.bar.unwrap_or(default), cx);
        subs.push(cx.subscribe_in(&title, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
        // A prompt is read whole: the box grows with it (a long one scrolls past 14 lines).
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 14)
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
        let jev = s.jev.as_ref().map(|j| {
            let instructions = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, 8)
                    .placeholder(
                        "e.g. A customer email to our integrations team. What are they asking for?",
                    )
                    .default_value(j.instructions.clone())
            });
            subs.push(
                cx.subscribe_in(&instructions, window, |_, _, _: &InputEvent, _, cx| {
                    cx.notify()
                }),
            );
            let mut line = |value: String, placeholder: &'static str| {
                let i = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                });
                subs.push(cx.subscribe_in(&i, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
                i
            };
            let mut options = Vec::new();
            // `none` is always offered: its row shows only when it says something else.
            for (k, v) in j.options.iter().filter(|(k, v)| {
                !(k.as_str() == crate::jev::NONE && v.as_str() == crate::jev_stage::NONE_TEXT)
            }) {
                let row_key = draft.next_key;
                draft.next_key += 1;
                options.push((
                    row_key,
                    line(k.clone(), "how_to"),
                    line(v.clone(), "Asks how to do something with the platform"),
                    j.ends.contains(k),
                ));
            }
            JevDraft {
                instructions,
                options,
                input: line(j.input.clone(), crate::jev_stage::DEFAULT_INPUT),
                model: line(
                    j.model.clone().unwrap_or_default(),
                    "jev-latest: pin a version",
                ),
            }
        });
        DraftStage {
            key,
            id: s.id.clone(),
            title,
            skill: s.skill.clone(),
            folder: s.folder.clone(),
            bar,
            own: s.bar.is_some(),
            prompt,
            jev,
            keep: s.keep,
            open: false,
            more: false,
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

    /// **Add a step**: a Claude step, or a Jev sort step (`jev`), last and unfolded to fill in.
    fn add_stage(&mut self, orch: PaneId, jev: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut draft) = self.pipeline_settings.remove(&orch) else {
            return;
        };
        let default = slider_value(&draft.bar, cx);
        let skill = draft.skills.first().cloned();
        let mut subs = Vec::new();
        let stage = self.new_stage(
            orch,
            &mut draft,
            &if jev {
                EditStage {
                    id: None,
                    title: "Classify".into(),
                    skill: None,
                    folder: None,
                    bar: Some(70),
                    prompt: String::new(),
                    jev: Some(crate::jev_stage::JevStage {
                        input: crate::jev_stage::DEFAULT_INPUT.into(),
                        model: None,
                        instructions: String::new(),
                        options: [("".to_string(), "".to_string())].into(),
                        ends: Default::default(),
                    }),
                    keep: None,
                }
            } else {
                EditStage {
                    id: None,
                    title: "New step".into(),
                    skill,
                    folder: None,
                    bar: None,
                    prompt: String::new(),
                    jev: None,
                    keep: None,
                }
            },
            default,
            window,
            cx,
            &mut subs,
        );
        draft.stages.push(stage);
        if let Some(s) = draft.stages.last_mut() {
            s.open = true;
        }
        draft.scroll.scroll_to_top_of_item(view::row_of(
            pipeline::Part::Step(draft.stages.len() - 1),
            draft.stages.len(),
        ));
        draft._subs.extend(subs);
        self.pipeline_settings.insert(orch, draft);
        cx.notify();
    }

    /// **+ Category** on Jev step `key`: an empty row to name and describe.
    fn add_jev_option(
        &mut self,
        orch: PaneId,
        key: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.pipeline_settings.get_mut(&orch) else {
            return;
        };
        let row_key = draft.next_key;
        draft.next_key += 1;
        let mut subs = Vec::new();
        let mut line = |placeholder: &'static str| {
            let i = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            subs.push(cx.subscribe_in(&i, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
            i
        };
        let (name, means) = (line("category_name"), line("What it means, for Jev"));
        let Some(draft) = self.pipeline_settings.get_mut(&orch) else {
            return;
        };
        if let Some(j) = draft
            .stages
            .iter_mut()
            .find(|s| s.key == key)
            .and_then(|s| s.jev.as_mut())
        {
            j.options.push((row_key, name, means, false));
        }
        draft._subs.extend(subs);
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
        let router_on = self.router_state(orch, cx).is_some_and(|(_, _, ok)| ok);
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
                        CommandItem::new()
                            .label(crate::setup::stage_skill_label(c.as_deref(), router_on))
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
            window.push_note(Note::warning(why).title("Pipeline settings"), cx);
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
                // A stage added after where a run is joins it (core spec §56).
                if let Some(sv) = server::get() {
                    let added = server::lock(sv.registry())
                        .runs
                        .refresh_from_file(&file, &p);
                    crate::run_log::record_added(&root, &added);
                }
                crate::action_log::write(format!("pipeline {file}: settings saved"));
                let fresh = self.new_draft(orch, file, Edit::from(&p), skills, window, cx);
                self.pipeline_settings.insert(orch, fresh);
                window.push_note(
                    Note::success("A stage's settings apply when it next starts.")
                        .title("Pipeline saved"),
                    cx,
                );
            }
            Err(e) => window.push_note(Note::error(e).title("Pipeline settings"), cx),
        }
        cx.notify();
    }

    /// The team skills router (core spec §55) in orchestrator `orch`'s environment: its label,
    /// its state, and whether it's on; none when config lists no router.
    fn router_state(
        &self,
        orch: PaneId,
        cx: &Context<Self>,
    ) -> Option<(String, crate::setup::PluginState, bool)> {
        let env = self.panes.get(&orch)?.read(cx).env().to_string();
        let i = self
            .ui
            .setup_plugins
            .iter()
            .position(crate::setup::is_router)?;
        let (_, label, state) = self
            .plugin_states(&env)
            .into_iter()
            .find(|(n, ..)| *n == i)?;
        Some((label, state, state == crate::setup::PluginState::On))
    }

    /// Above the steps: whether the team skills router guides them, in words (core spec §55).
    fn render_router_note(&self, orch: PaneId, cx: &Context<Self>) -> Option<AnyElement> {
        let (label, state, _) = self.router_state(orch, cx)?;
        let env = self.panes.get(&orch)?.read(cx).env().to_string();
        let (note, ok) = crate::setup::router_note(&label, state, &env);
        let tone = if ok { theme::TEAL } else { theme::AMBER };
        Some(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_start()
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(color(tone).opacity(0.5))
                .child(
                    text(12., tone)
                        .flex_none()
                        .child(if ok { "●" } else { "○" }),
                )
                .child(
                    text(12.5, theme::TEXT_SECONDARY)
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .child(note),
                )
                .into_any_element(),
        )
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
        .flex_wrap()
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

impl Workspace {
    /// **Product questions**: where a step asks product, and how it writes the question (one of
    /// `pipeline::PRODUCT_STYLES`, or the person's own words).
    fn render_product_settings(
        &self,
        orch: PaneId,
        draft: &SettingsDraft,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let style = draft.product_style.read(cx).value().trim().to_string();
        let presets = h_flex()
            .gap_1()
            .children(
                pipeline::PRODUCT_STYLES
                    .iter()
                    .enumerate()
                    .map(|(i, (label, text))| {
                        let b =
                            Button::new(SharedString::from(format!("product-style-{orch}-{i}")))
                                .label(*label)
                                .small()
                                .tooltip(*text)
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    let Some(input) = ws
                                        .pipeline_settings
                                        .get(&orch)
                                        .map(|d| d.product_style.clone())
                                    else {
                                        return;
                                    };
                                    input.update(cx, |s, cx| s.set_value(*text, window, cx));
                                    cx.notify();
                                }));
                        if style == *text {
                            b.primary()
                        } else {
                            b.ghost()
                        }
                    }),
            );
        let asks = draft.product_asks;
        let ask = |on: bool, label: &'static str| {
            let b = Button::new(SharedString::from(format!("product-asks-{on}-{orch}")))
                .label(label)
                .xsmall()
                .on_click(cx.listener(move |ws, _, _, cx| {
                    ws.with_draft(orch, cx, |d| d.product_asks = on)
                }));
            if asks == on { b.primary() } else { b.ghost() }
        };
        let rule = if asks {
            "Every step is told: a change beyond the ticket that alters what users see or do \
             (a feature, copy or a flow) is product's call, so it asks before making it and the \
             ticket waits for the answer."
        } else {
            "Steps aren't told to ask product and can't: they decide within their own prompt. \
             Right for reviews, where the PR's author owns the change."
        };
        let mut body = v_flex()
            .gap_2()
            .child(section_tip(
                "Product questions",
                "Whether steps ask product before a change beyond the ticket, how they write the \
                 question, and the Slack channel it goes to (empty: they ask you here).",
            ))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        text(12.5, theme::TEXT_SECONDARY)
                            .w(px(170.))
                            .flex_none()
                            .child("Steps ask product"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(ask(true, "On"))
                            .child(ask(false, "Off")),
                    ),
            )
            .child(
                div()
                    .max_w(px(640.))
                    .child(text(12., theme::TEXT_MUTED).child(rule)),
            );
        if !asks {
            return body.into_any_element();
        }
        body = body
            .child(presets)
            .child(
                div()
                    .max_w(px(640.))
                    .child(Textarea::new(&draft.product_style)),
            )
            .child(
                div().max_w(px(480.)).child(
                    Input::new(&draft.product)
                        .prefix(text(11.5, theme::TEXT_MUTED).pr(px(6.)).child("Channel")),
                ),
            );
        body.into_any_element()
    }
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
