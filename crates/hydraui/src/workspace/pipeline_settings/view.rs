//! Pipeline settings drawn (core spec §63): a checklist of what's left to set up, then the
//! pipeline as it runs, top to bottom: what starts a run, each step as a card that folds to a
//! line, and **Add a step**. Each card shows what most people change; the rest waits under
//! **More options**, and the rules every pipeline shares under **Advanced settings**. Every
//! timing is a dropdown, read back in a sentence (`crate::settings_words`).

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, FontWeight, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::select::Select;
use gpui_component::slider::Slider;
use gpui_component::{Disableable, Sizable, h_flex, v_flex};

use super::{DraftStage, SettingsDraft, color_swatches, section_tip, slider_value, text};
use crate::envs::PaneId;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::pipeline::{self, EditStage, Part};
use crate::runs::MAX_ATTEMPTS;
use crate::settings_words as words;
use crate::workspace::Workspace;

/// Where each part sits among the page's rows, for the checklist to scroll to: the title,
/// the checklist, the name, the source, the Steps heading, one row per step, Add a step,
/// then Advanced settings.
pub(super) fn row_of(part: Part, steps: usize) -> usize {
    match part {
        Part::Name => 2,
        Part::Source => 3,
        Part::Steps => 4,
        Part::Step(i) => 5 + i,
        Part::Advanced => 5 + steps + 1,
    }
}

/// A card's background and edge; `edge` overrides the edge (amber: something to fix).
fn card(id: SharedString, edge: Option<u32>) -> gpui::Stateful<gpui::Div> {
    v_flex()
        .id(id)
        .gap_2()
        .px(px(16.))
        .py(px(14.))
        .rounded(px(10.))
        .bg(color(0x131616))
        .border_1()
        .border_color(edge.map_or(color(0x1f2524), |e| color(e).opacity(0.6)))
}

/// A section's heading, with a line under it saying what the section is for.
fn heading(title: &str, about: &str) -> gpui::Div {
    v_flex()
        .gap(px(2.))
        .child(
            text(16., theme::TEXT)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(
            text(12.5, theme::TEXT_MUTED)
                .whitespace_normal()
                .child(about.to_string()),
        )
}

/// A label on the left, its control on the right.
fn field(label: &str, control: impl IntoElement) -> gpui::Div {
    h_flex()
        .gap_3()
        .items_center()
        .child(
            text(12.5, theme::TEXT_SECONDARY)
                .w(px(170.))
                .flex_none()
                .child(label.to_string()),
        )
        .child(control)
}

fn input_field(label: &str, input: &Entity<InputState>) -> gpui::Div {
    field(label, div().w(px(300.)).child(Input::new(input).small()))
}

/// A small muted tag, e.g. the kind of step.
fn tag(label: &str, hex: u32) -> gpui::Div {
    text(11., hex)
        .flex_none()
        .px(px(8.))
        .py(px(2.))
        .rounded(px(6.))
        .bg(color(0x1c2120))
        .child(label.to_string())
}

/// **▸ More options**, with what's set under it when folded.
fn fold(
    id: SharedString,
    open: bool,
    title: &str,
    folded: Option<String>,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(id)
        .flex_wrap()
        .gap_x_2()
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(color(theme::TEXT_SECONDARY))
        .hover(|d| d.text_color(color(theme::TEXT)))
        .child(format!("{} {title}", if open { "▾" } else { "▸" }))
        .when_some(folded.filter(|_| !open), |d, f| {
            d.child(text(12., theme::TEXT_MUTED).child(f))
        })
}

type Pick = Rc<dyn Fn(usize, &mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// A dropdown: `label` on the button, one ticked item per choice; picking one runs `pick`.
fn dropdown(
    id: SharedString,
    label: String,
    choices: Vec<(String, bool)>,
    ws: Entity<Workspace>,
    pick: Pick,
) -> impl IntoElement + use<> {
    Button::new(id)
        .label(label)
        .outline()
        .small()
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for (i, (label, on)) in choices.iter().enumerate() {
                let (ws, pick) = (ws.clone(), pick.clone());
                menu = menu.item(PopupMenuItem::new(label.clone()).checked(*on).on_click(
                    move |_, window, cx| ws.update(cx, |ws, cx| pick(i, ws, window, cx)),
                ));
            }
            menu
        })
}

/// A dropdown of numbers, each said by `say`.
fn numbers(
    id: String,
    values: Vec<u32>,
    chosen: u32,
    say: fn(u32) -> String,
    ws: Entity<Workspace>,
    set: impl Fn(u32, &mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> impl IntoElement {
    let choices = values.iter().map(|v| (say(*v), *v == chosen)).collect();
    dropdown(
        id.into(),
        say(chosen),
        choices,
        ws,
        Rc::new(move |i, ws, window, cx| set(values[i], ws, window, cx)),
    )
}

/// A dropdown of numbers kept in a text field (a source's timings): picking one writes it.
fn numbers_in(
    id: String,
    input: &Entity<InputState>,
    options: &[u32],
    say: fn(u32) -> String,
    ws: Entity<Workspace>,
    cx: &App,
) -> impl IntoElement + use<> {
    let current: u32 = input.read(cx).value().trim().parse().unwrap_or(0);
    let values = if current == 0 {
        options.to_vec()
    } else {
        words::with_current(options, current)
    };
    let choices = values.iter().map(|v| (say(*v), *v == current)).collect();
    let input = input.clone();
    dropdown(
        id.into(),
        if current == 0 {
            "Choose…".into()
        } else {
            say(current)
        },
        choices,
        ws,
        Rc::new(move |i, _, window, cx| {
            input.update(cx, |s, cx| s.set_value(values[i].to_string(), window, cx));
            cx.notify();
        }),
    )
}

impl Workspace {
    /// The setup checklist's **Go to**: unfold `part` and scroll to it.
    fn settings_go_to(&mut self, orch: PaneId, part: Part, cx: &mut Context<Self>) {
        let Some(d) = self.pipeline_settings.get_mut(&orch) else {
            return;
        };
        match part {
            Part::Source => d.source_open = true,
            Part::Step(i) => {
                if let Some(s) = d.stages.get_mut(i) {
                    s.open = true;
                }
            }
            Part::Advanced => d.advanced = true,
            Part::Name | Part::Steps => {}
        }
        d.scroll.scroll_to_top_of_item(row_of(part, d.stages.len()));
        cx.notify();
    }

    fn with_stage(
        &mut self,
        orch: PaneId,
        key: u64,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut DraftStage),
    ) {
        self.with_draft(orch, cx, |d| {
            if let Some(s) = d.stages.iter_mut().find(|s| s.key == key) {
                f(s);
            }
        })
    }

    /// The Settings view for orchestrator `orch`.
    pub(in crate::workspace) fn render_pipeline_settings(
        &self,
        orch: PaneId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = self.pipeline_settings.get(&orch) else {
            return v_flex().into_any_element();
        };
        let edit = draft.edit(cx);
        let dirty = edit != draft.saved;
        let problems = pipeline::problems(&edit);
        let router_on = self.router_state(orch, cx).is_some_and(|(_, _, ok)| ok);

        let mut page = v_flex()
            .id(SharedString::from(format!("settings-{orch}")))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&draft.scroll)
            .px(px(36.))
            .py(px(28.))
            .gap(px(24.))
            // 0. Title.
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        text(20., theme::TEXT)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Pipeline settings"),
                    )
                    .child({
                        let file = draft.file.clone();
                        Button::new(SharedString::from(format!("export-{orch}")))
                            .label("Export…")
                            .ghost()
                            .tooltip(
                                "Save this pipeline as a file to send someone. Saved settings \
                                 only; it holds no secrets.",
                            )
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.export_pipeline(file.clone(), window, cx)
                            }))
                    }),
            )
            // 1. What's left to set up.
            .child(checklist(orch, &problems, cx))
            // 2. Its name and colour.
            .child(
                v_flex()
                    .gap(px(10.))
                    .child(heading(
                        "Name",
                        "What it's called in the sidebar, and the colour and icon that tell it apart.",
                    ))
                    .child(div().max_w(px(480.)).child(Input::new(&draft.name)))
                    .child(color_swatches(orch, draft.color, cx))
                    .child(icon_picker(orch, draft.icon.as_deref(), draft.color, cx)),
            )
            // 3. What starts a run.
            .child(
                v_flex()
                    .gap(px(10.))
                    .child(heading(
                        "Trigger",
                        "What starts a run: a ticket, a pull request, an email, a time of day, \
                         or you.",
                    ))
                    .child(self.render_source_card(orch, draft, &edit, &problems, cx)),
            )
            // 4. The steps' heading.
            .child(
                v_flex()
                    .gap(px(10.))
                    .child(heading(
                        "Stages",
                        "What happens to each run, top to bottom. A stage moves on when it's sure \
                         enough of its work; otherwise it asks you.",
                    ))
                    .children(self.render_router_note(orch, cx)),
            );
        // 5… One card per step.
        let count = draft.stages.len();
        for (i, s) in draft.stages.iter().enumerate() {
            let Some(es) = edit.stages.get(i) else {
                continue;
            };
            let own: Vec<&'static str> = problems
                .iter()
                .filter(|(p, _)| *p == Part::Step(i))
                .map(|(_, w)| *w)
                .collect();
            page = page.child(step_card(
                orch, i, count, s, es, edit.bar, router_on, &own, cx,
            ));
        }
        page = page
            .child(add_step(orch, draft.adding, cx))
            .child(self.render_advanced(orch, draft, &edit, cx));

        let save_bar =
            if dirty {
                let (message, hex) = match problems.first() {
                    Some((part, why)) => (format!("{}: {why}", part.label()), theme::RED),
                    None => (
                        format!(
                            "Unsaved changes · {count} stage{}",
                            if count == 1 { "" } else { "s" }
                        ),
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
                            .disabled(!problems.is_empty())
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
            .child(page)
            .child(save_bar)
            .into_any_element()
    }

    /// **Starts when**: what starts a run, in a sentence; unfolded, the source and its fields.
    fn render_source_card(
        &self,
        orch: PaneId,
        draft: &SettingsDraft,
        edit: &pipeline::Edit,
        problems: &[(Part, &'static str)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (head, detail) = words::source(edit);
        let wrong = problems.iter().any(|(p, _)| *p == Part::Source);
        let open = draft.source_open;
        let mut c = card(
            SharedString::from(format!("settings-source-{orch}")),
            wrong.then_some(theme::AMBER),
        )
        .child(
            h_flex()
                .gap(px(10.))
                .items_center()
                .child(tag("Starts when", theme::TEAL))
                .child(
                    text(14., theme::TEXT)
                        .flex_1()
                        .font_weight(FontWeight::MEDIUM)
                        .child(head),
                )
                .child(if wrong {
                    text(13., theme::AMBER).child("Needs setting up")
                } else {
                    text(13., theme::TEAL).child("✓")
                })
                .child(
                    Button::new(SharedString::from(format!("source-edit-{orch}")))
                        .label(if open { "Done" } else { "Change" })
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.with_draft(orch, cx, |d| d.source_open = !d.source_open)
                        })),
                ),
        );
        if !open {
            return c
                .child(text(12.5, theme::TEXT_MUTED).child(detail))
                .into_any_element();
        }
        c = c
            .child(
                field(
                    "Where work comes from",
                    div()
                        .w(px(300.))
                        .child(Select::new(&draft.trigger).small().menu_width(px(300.))),
                )
                .mt_1(),
            )
            .child(self.render_source_fields(orch, draft, cx));
        for (_, why) in problems.iter().filter(|(p, _)| *p == Part::Source) {
            c = c.child(text(12., theme::AMBER).pl(px(182.)).child(*why));
        }
        c.into_any_element()
    }

    /// The chosen source's fields: what most people set, then **More options**.
    fn render_source_fields(
        &self,
        orch: PaneId,
        draft: &SettingsDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ws = cx.entity();
        let more = draft.source_more;
        let mut main = v_flex().gap_2();
        let mut extra = v_flex().gap_2();
        let mut folded = Vec::new();
        if draft.linear.on {
            let l = &draft.linear;
            let pair = |on: bool,
                        a: &'static str,
                        b: &'static str,
                        id: &str,
                        set: fn(&mut SettingsDraft, bool)| {
                dropdown(
                    SharedString::from(format!("{id}-{orch}")),
                    if on { b } else { a }.to_string(),
                    vec![(a.to_string(), !on), (b.to_string(), on)],
                    ws.clone(),
                    Rc::new(move |i, ws, _, cx| ws.with_draft(orch, cx, |d| set(d, i == 1))),
                )
            };
            main = main
                .child(input_field("Team", &l.team))
                .child(input_field("Pick tickets in", &l.pick))
                .child(input_field("With one of the labels", &l.labels));
            extra = extra
                .child(field(
                    "Which tickets",
                    pair(
                        l.no_sla,
                        "With an SLA",
                        "All, by priority",
                        "linear-sla",
                        |d, v| d.linear.no_sla = v,
                    ),
                ))
                .child(field(
                    "Whose tickets",
                    pair(l.any, "Unassigned", "Anyone's", "linear-any", |d, v| {
                        d.linear.any = v
                    }),
                ))
                .child(input_field("Then move it to", &l.claim))
                .child(input_field("When done, move it to", &l.passed))
                .child(field(
                    "Check every",
                    numbers_in(
                        format!("linear-every-{orch}"),
                        &l.every,
                        &words::CHECK_EVERY,
                        words::minutes,
                        ws.clone(),
                        cx,
                    ),
                ));
            folded.push("states, timing");
        }
        if draft.slack.on {
            let sl = &draft.slack;
            main = main
                .child(input_field("Channel name", &sl.label))
                .child(input_field("Channel ID", &sl.channel));
            extra = extra
                .child(field(
                    "Check every",
                    numbers_in(
                        format!("slack-every-{orch}"),
                        &sl.every,
                        &words::CHECK_EVERY,
                        words::minutes,
                        ws.clone(),
                        cx,
                    ),
                ))
                .child(field(
                    "Look back",
                    numbers_in(
                        format!("slack-back-{orch}"),
                        &sl.lookback,
                        &words::LOOK_BACK_HOURS,
                        words::hours,
                        ws.clone(),
                        cx,
                    ),
                ))
                .child(input_field("Mark taken with", &sl.reaction))
                .child(input_field("Mark done with", &sl.done_reaction));
            folded.push("timing, reactions");
        }
        if draft.schedule.on {
            let sc = &draft.schedule;
            let every = sc.every.read(cx).value().to_string();
            let reads = match crate::schedule::parse(&every) {
                Ok(w) => (
                    format!(
                        "{}, while hydraui is open; once when it next opens that day if it \
                         wasn't. Each run is named like {}.",
                        w.text(),
                        crate::schedule::ticket(&draft.file, "2026-11-16")
                    ),
                    theme::TEXT_SECONDARY,
                ),
                Err(_) if every.trim().is_empty() => (
                    "Days and a time: mon 18:00, weekdays 09:00, daily 08:30, mon,thu 17:00"
                        .to_string(),
                    theme::TEXT_MUTED,
                ),
                Err(e) => (e.replace("[schedule] ", ""), theme::AMBER),
            };
            main = main.child(input_field("Runs", &sc.every)).child(
                text(12., reads.1)
                    .pl(px(182.))
                    .whitespace_normal()
                    .child(reads.0),
            );
            extra = extra.child(input_field("In environment", &sc.env));
            folded.push("environment");
        }
        if draft.mail.on {
            let m = &draft.mail;
            main = main.child(input_field("Check", &m.check)).child(field(
                "Look back",
                numbers_in(
                    format!("mail-back-{orch}"),
                    &m.look_back,
                    &words::LOOK_BACK_DAYS,
                    words::days,
                    ws.clone(),
                    cx,
                ),
            ));
            let mut missing = Vec::new();
            if self.ui.gmail_client().is_none() {
                missing.push(
                    "set gmail_client_id and gmail_client_secret in config.toml, then Connect \
                     Gmail",
                );
            }
            if self
                .ui
                .peer_name
                .as_deref()
                .is_none_or(|n| n.trim().is_empty())
            {
                missing.push("set peer_name in config.toml, so claims say who has each email");
            }
            if !missing.is_empty() {
                main = main.child(
                    text(12., theme::AMBER)
                        .pl(px(182.))
                        .whitespace_normal()
                        .child(format!("Before it can read mail: {}.", missing.join("; "))),
                );
            }
            extra = extra
                .child(input_field("Gmail search", &m.query))
                .child(input_field("Team swarm", &m.swarm))
                .child(input_field("In environment", &m.env));
            folded.push("search, swarm, environment");
        }
        if folded.is_empty() {
            return main
                .child(
                    text(12.5, theme::TEXT_MUTED).child(
                        "You start each run yourself, from the pipeline's card in the sidebar.",
                    ),
                )
                .into_any_element();
        }
        main.child(
            fold(
                SharedString::from(format!("source-more-{orch}")),
                more,
                "More options",
                Some(folded.join(", ")),
            )
            .on_click(cx.listener(move |ws, _, _, cx| {
                ws.with_draft(orch, cx, |d| d.source_more = !d.source_more)
            })),
        )
        .when(more, |d| d.child(extra.pl(px(14.))))
        .into_any_element()
    }

    /// **Advanced settings**, folded: what a waiting step does, tickets at once, the default
    /// pass mark, the branch, product questions.
    fn render_advanced(
        &self,
        orch: PaneId,
        draft: &SettingsDraft,
        edit: &pipeline::Edit,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = draft.advanced;
        let toggle = fold(
            SharedString::from(format!("settings-advanced-{orch}")),
            open,
            "Advanced settings",
            Some(
                "While a stage waits · Tickets at once · Default pass mark · Branch · Product \
                 questions"
                    .into(),
            ),
        )
        .on_click(
            cx.listener(move |ws, _, _, cx| ws.with_draft(orch, cx, |d| d.advanced = !d.advanced)),
        );
        let mut body = v_flex().gap(px(22.)).child(toggle);
        if !open {
            return body.into_any_element();
        }
        let ws = cx.entity();
        let floor = server::get().map_or(crate::runs::MIN_WEEK_LEFT, |sv| {
            server::lock(sv.registry()).min_week_left
        });
        let runs = draft.runs;
        let tickets = dropdown(
            SharedString::from(format!("pipeline-runs-{orch}")),
            runs_label(runs),
            (1..=pipeline::MAX_RUNS_AT_ONCE)
                .map(|n| (runs_label(n), n == runs))
                .collect(),
            ws.clone(),
            Rc::new(move |i, ws, _, cx| ws.with_draft(orch, cx, |d| d.runs = i as u8 + 1)),
        );
        body = body.child(
            v_flex()
                .gap(px(22.))
                .pl(px(14.))
                .border_l_2()
                .border_color(color(theme::BORDER_SUBTLE))
                .child(waiting(orch, draft.waiting, ws.clone()))
                .child(
                    v_flex()
                        .gap_2()
                        .child(section_tip(
                            "Tickets at once",
                            "One at a time: one ticket after another. Up to N: the \
                             orchestrator runs up to N tickets side by side, each with its own \
                             sessions under it.",
                        ))
                        .child(field("Run", tickets)),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(section_tip(
                            "Default pass mark",
                            "How sure a stage must be of its work to move on. A stage can set its \
                             own under More options.",
                        ))
                        .child(
                            h_flex()
                                .max_w(px(480.))
                                .gap_3()
                                .child(div().flex_1().child(Slider::new(&draft.bar)))
                                .child(text(13., theme::TEXT).child(format!("{}%", edit.bar))),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(section_tip(
                            "Branch name",
                            "The branch every stage must use, e.g. {ticket}. Empty: any branch.",
                        ))
                        .child(div().max_w(px(480.)).child(Input::new(&draft.branch))),
                )
                .child(self.render_product_settings(orch, draft, cx))
                .child(text(12., theme::TEXT_MUTED).child(format!(
                    "A stage that fails is retried up to {MAX_ATTEMPTS} times. Stages pause below \
                     {floor}% of the Claude week."
                ))),
        );
        body.into_any_element()
    }
}

/// The icon row (core spec §64): the ◆ and each of `pipeline_icon::ICONS`, in the pipeline's
/// colour, the chosen one ringed and named on hover.
fn icon_picker(
    orch: PaneId,
    chosen: Option<&str>,
    accent: Option<u32>,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let accent = accent.unwrap_or(theme::TEAL);
    let choices = std::iter::once((None, "Diamond (the default)")).chain(
        crate::pipeline_icon::ICONS
            .iter()
            .map(|(n, l)| (Some(*n), *l)),
    );
    h_flex()
        .flex_wrap()
        .gap_1()
        .children(choices.map(|(icon, label)| {
            let on = icon == chosen;
            div()
                .id(SharedString::from(format!(
                    "pipeline-icon-{orch}-{}",
                    icon.unwrap_or("diamond")
                )))
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .border_1()
                .border_color(if on {
                    color(theme::TEXT)
                } else {
                    color(theme::BORDER_SUBTLE)
                })
                .when(on, |d| d.bg(color(accent).opacity(0.12)))
                .hover(|d| d.border_color(color(accent)))
                .cursor_pointer()
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                })
                .child(crate::pipeline_icon::mark(icon, accent, 15.))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    ws.with_draft(orch, cx, |d| d.icon = icon.map(str::to_string))
                }))
        }))
        .into_any_element()
}

fn runs_label(n: u8) -> String {
    if n == 1 {
        "One ticket at a time".into()
    } else {
        format!("Up to {n} tickets")
    }
}

/// The checklist: each thing left to set up, with **Go to**; or a quiet line when none is.
fn checklist(
    orch: PaneId,
    problems: &[(Part, &'static str)],
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if problems.is_empty() {
        return h_flex()
            .gap_2()
            .items_center()
            .child(div().size(px(8.)).rounded_full().bg(color(theme::TEAL)))
            .child(text(12.5, theme::TEXT_SECONDARY).child("Set up and ready to run"))
            .into_any_element();
    }
    let n = problems.len();
    v_flex()
        .gap_1()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(color(theme::AMBER).opacity(0.5))
        .bg(color(theme::AMBER).opacity(0.06))
        .child(
            text(13., theme::AMBER)
                .font_weight(FontWeight::MEDIUM)
                .child(if n == 1 {
                    "1 thing to finish before you can save".to_string()
                } else {
                    format!("{n} things to finish before you can save")
                }),
        )
        .children(problems.iter().enumerate().map(|(k, (part, why))| {
            let part = *part;
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    text(12.5, theme::TEXT_SECONDARY)
                        .w(px(150.))
                        .flex_none()
                        .child(part.label()),
                )
                .child(text(12.5, theme::TEXT).flex_1().child(*why))
                .child(
                    Button::new(SharedString::from(format!("todo-{orch}-{k}")))
                        .label("Go to")
                        .ghost()
                        .xsmall()
                        .on_click(
                            cx.listener(move |ws, _, _, cx| ws.settings_go_to(orch, part, cx)),
                        ),
                )
        }))
        .into_any_element()
}

/// One step's card: folded, its name and what it's told in a line; unfolded, its
/// instructions, and its options under **More options**.
#[allow(clippy::too_many_arguments)]
fn step_card(
    orch: PaneId,
    i: usize,
    count: usize,
    s: &DraftStage,
    es: &EditStage,
    default_bar: u8,
    router_on: bool,
    problems: &[&'static str],
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let key = s.key;
    let ws = cx.entity();
    let jev = s.jev.is_some();
    let number = div()
        .size(px(24.))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(color(theme::BORDER_STRONG))
        .child(text(11.5, theme::TEXT_SECONDARY).child(format!("{}", i + 1)));
    let title: AnyElement = if s.open {
        div()
            .flex_1()
            .min_w(px(120.))
            .child(Input::new(&s.title).small())
            .into_any_element()
    } else {
        div()
            .id(SharedString::from(format!("stage-title-{orch}-{key}")))
            .flex_1()
            .cursor_pointer()
            .child(
                text(14., theme::TEXT)
                    .font_weight(FontWeight::MEDIUM)
                    .child(if es.title.trim().is_empty() {
                        "Untitled stage".to_string()
                    } else {
                        es.title.clone()
                    }),
            )
            .on_click(
                cx.listener(move |ws, _, _, cx| ws.with_stage(orch, key, cx, |s| s.open = true)),
            )
            .into_any_element()
    };
    let menu = {
        let ws = ws.clone();
        Button::new(SharedString::from(format!("stage-menu-{orch}-{key}")))
            .label("⋯")
            .ghost()
            .xsmall()
            .tooltip("Move or remove this stage")
            .dropdown_menu(move |menu, _, _| {
                let (up, down, gone) = (ws.clone(), ws.clone(), ws.clone());
                menu.item(PopupMenuItem::new("Move up").disabled(i == 0).on_click(
                    move |_, _, cx| {
                        up.update(cx, |ws, cx| {
                            ws.with_draft(orch, cx, |d| d.stages.swap(i, i - 1))
                        })
                    },
                ))
                .item(
                    PopupMenuItem::new("Move down")
                        .disabled(i + 1 == count)
                        .on_click(move |_, _, cx| {
                            down.update(cx, |ws, cx| {
                                ws.with_draft(orch, cx, |d| d.stages.swap(i, i + 1))
                            })
                        }),
                )
                .item(
                    PopupMenuItem::new("Remove stage")
                        .disabled(count == 1)
                        .on_click(move |_, _, cx| {
                            gone.update(cx, |ws, cx| {
                                ws.with_draft(orch, cx, |d| d.stages.retain(|s| s.key != key))
                            })
                        }),
                )
            })
    };
    let header = h_flex()
        .gap(px(10.))
        .items_center()
        .child(number)
        .child(title)
        .child(tag(
            if jev { "Sort (Jev)" } else { "Claude" },
            theme::TEXT_SECONDARY,
        ))
        .when(!problems.is_empty(), |h| {
            h.child(text(13., theme::AMBER).child("Needs setting up"))
        })
        .child(menu)
        .child(
            Button::new(SharedString::from(format!("stage-fold-{orch}-{key}")))
                .label(if s.open { "▾" } else { "▸" })
                .ghost()
                .xsmall()
                .tooltip(if s.open {
                    "Fold this stage"
                } else {
                    "Open this stage"
                })
                .on_click(cx.listener(move |ws, _, _, cx| {
                    ws.with_stage(orch, key, cx, |s| s.open = !s.open)
                })),
        );
    let mut c = card(
        SharedString::from(format!("settings-stage-{orch}-{key}")),
        (!problems.is_empty()).then_some(theme::AMBER),
    )
    .child(header);
    let indent = |d: gpui::Div| d.pl(px(34.));
    if !s.open {
        return c
            .child(indent(
                v_flex()
                    .gap_1()
                    .child(text(12.5, theme::TEXT_SECONDARY).child(words::instructions(es)))
                    .child(text(12., theme::TEXT_MUTED).child(words::options(
                        es,
                        default_bar,
                        router_on,
                    ))),
            ))
            .into_any_element();
    }
    let label = |t: &'static str| text(12., theme::TEXT_SECONDARY).child(t);
    let mut body = v_flex().gap_2();
    let bar = slider_value(&s.bar, cx);
    let sure = h_flex()
        .gap_3()
        .items_center()
        .child(div().w(px(160.)).child(Slider::new(&s.bar)))
        .child(
            text(
                12.5,
                if s.own {
                    theme::AMBER
                } else {
                    theme::TEXT_SECONDARY
                },
            )
            .child(format!("{bar}%")),
        )
        .when(s.own, |d| {
            d.child(
                Button::new(SharedString::from(format!("reset-{key}")))
                    .label("Use the default")
                    .ghost()
                    .xsmall()
                    .on_click(
                        cx.listener(move |ws, _, window, cx| ws.reset_bar(orch, key, window, cx)),
                    ),
            )
        });
    let mut extra = v_flex()
        .gap_2()
        .p(px(12.))
        .rounded(px(8.))
        .bg(color(0x0f1212));
    match &s.jev {
        None => {
            let inserts = [
                ("{ticket}", "The ticket"),
                ("{pr}", "The pull request link"),
                ("{handoff}", "What the stage before handed over"),
            ];
            body = body
                .child(label("What should this stage do?"))
                .child(Textarea::new(&s.prompt))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .items_center()
                        .child(text(11.5, theme::TEXT_MUTED).mr_1().child("Insert"))
                        .children(inserts.into_iter().map(|(token, means)| {
                            let prompt = s.prompt.clone();
                            Button::new(SharedString::from(format!("insert-{orch}-{key}-{token}")))
                                .label(token)
                                .ghost()
                                .xsmall()
                                .tooltip(means)
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    prompt.update(cx, |p, cx| {
                                        p.insert(token, window, cx);
                                        p.focus(window, cx);
                                    });
                                    cx.notify();
                                }))
                        }))
                        .child(
                            text(11.5, theme::TEXT_MUTED)
                                .ml_2()
                                .child("Empty: the default instructions for the stage's name."),
                        ),
                );
            let keep = s.keep;
            let unsure = dropdown(
                SharedString::from(format!("keep-{orch}-{key}")),
                if keep.is_some() {
                    "Keep checking"
                } else {
                    "Stop and ask me"
                }
                .into(),
                vec![
                    ("Stop and ask me".into(), keep.is_none()),
                    ("Keep checking".into(), keep.is_some()),
                ],
                ws.clone(),
                Rc::new(move |i, ws, _, cx| {
                    ws.with_stage(orch, key, cx, |s| {
                        s.keep = (i == 1).then(|| {
                            s.keep.unwrap_or(pipeline::KeepChecking {
                                every: 10,
                                hours: 8,
                            })
                        })
                    })
                }),
            );
            extra = extra.child(field("If it isn't sure enough", unsure));
            if let Some(k) = keep {
                extra = extra
                    .child(field(
                        "Check again every",
                        numbers(
                            format!("keep-every-{orch}-{key}"),
                            words::with_current(&words::KEEP_EVERY, k.every),
                            k.every,
                            words::minutes,
                            ws.clone(),
                            move |v, ws, _, cx| {
                                ws.with_stage(orch, key, cx, |s| {
                                    s.keep = Some(pipeline::KeepChecking { every: v, ..k })
                                })
                            },
                        ),
                    ))
                    .child(field(
                        "For up to",
                        numbers(
                            format!("keep-for-{orch}-{key}"),
                            words::with_current(&words::KEEP_FOR, k.hours),
                            k.hours,
                            words::hours,
                            ws.clone(),
                            move |v, ws, _, cx| {
                                ws.with_stage(orch, key, cx, |s| {
                                    s.keep = Some(pipeline::KeepChecking { hours: v, ..k })
                                })
                            },
                        ),
                    ));
            }
            extra = extra
                .child(field("Sure enough at", sure))
                .child(field(
                    "Skill",
                    Button::new(SharedString::from(format!("skill-{key}")))
                        .label(crate::setup::stage_skill_label(
                            s.skill.as_deref(),
                            router_on,
                        ))
                        .tooltip(if router_on {
                            "The skill this stage must use before it reports. The team skills \
                             router adds the skills its changes need."
                        } else {
                            "The skill this stage must use before it reports"
                        })
                        .dropdown_caret(true)
                        .outline()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.pick_skill(orch, key, window, cx)
                        })),
                ))
                .child(field(
                    "Works in",
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(SharedString::from(format!("folder-{key}")))
                                .label(match &s.folder {
                                    Some(f) => std::path::Path::new(f)
                                        .file_name()
                                        .map_or(f.clone(), |n| n.to_string_lossy().into_owned()),
                                    None => "The orchestrator's folder".into(),
                                })
                                .dropdown_caret(true)
                                .outline()
                                .small()
                                .tooltip("The folder this stage's session starts in")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.pick_folder(orch, key, window, cx)
                                })),
                        )
                        .when(s.folder.is_some(), |d| {
                            d.child(
                                Button::new(SharedString::from(format!("folder-clear-{key}")))
                                    .label("Use the orchestrator's")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |ws, _, _, cx| {
                                        ws.with_stage(orch, key, cx, |s| s.folder = None)
                                    })),
                            )
                        }),
                ))
                .child(
                    text(12., theme::TEXT_MUTED)
                        .whitespace_normal()
                        .child(words::unsure(keep, bar)),
                );
        }
        Some(j) => {
            body = body
                .child(label("What should Jev decide?"))
                .child(Textarea::new(&j.instructions))
                .child(label(
                    "Categories: a name, what it means, and what the run does then. End the run \
                     for a category this pipeline doesn't deal with: its run stops quietly, and \
                     an email goes back to the inbox unclaimed.",
                ))
                .children(j.options.iter().map(|(row, name, means, end)| {
                    let (row, end) = (*row, *end);
                    let then = dropdown(
                        SharedString::from(format!("jev-opt-then-{key}-{row}")),
                        if end { "End the run" } else { "Go on" }.into(),
                        vec![("Go on".into(), !end), ("End the run".into(), end)],
                        ws.clone(),
                        Rc::new(move |i, ws, _, cx| {
                            ws.with_stage(orch, key, cx, |s| {
                                if let Some(o) = s
                                    .jev
                                    .as_mut()
                                    .and_then(|j| j.options.iter_mut().find(|o| o.0 == row))
                                {
                                    o.3 = i == 1;
                                }
                            })
                        }),
                    );
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .w(px(170.))
                                .flex_none()
                                .child(Input::new(name).small()),
                        )
                        .child(div().flex_1().min_w_0().child(Input::new(means).small()))
                        .child(text(11.5, theme::TEXT_MUTED).flex_none().child("Then"))
                        .child(div().w(px(130.)).flex_none().child(then))
                        .child(
                            Button::new(SharedString::from(format!("jev-opt-x-{key}-{row}")))
                                .label("×")
                                .ghost()
                                .xsmall()
                                .tooltip("Remove this category")
                                .on_click(cx.listener(move |ws, _, _, cx| {
                                    ws.with_stage(orch, key, cx, |s| {
                                        if let Some(j) = s.jev.as_mut() {
                                            j.options.retain(|o| o.0 != row);
                                        }
                                    })
                                })),
                        )
                }))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            Button::new(SharedString::from(format!("jev-opt-add-{key}")))
                                .label("+ Category")
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.add_jev_option(orch, key, window, cx)
                                })),
                        )
                        .child(text(11.5, theme::TEXT_MUTED).child(
                            "\"None of these\" is always offered, so Jev can say it can't tell.",
                        )),
                );
            extra = extra
                .child(field("Sure enough at", sure))
                .child(input_field("Reads the file", &j.input))
                .child(input_field("Model", &j.model))
                .child(text(12., theme::TEXT_MUTED).whitespace_normal().child(format!(
                    "Jev's confidence is the stage's score: below {bar}% a person decides. The \
                     next stage reads the category in its brief."
                )));
        }
    }
    for why in problems {
        body = body.child(text(12., theme::AMBER).child(*why));
    }
    body = body
        .child(
            fold(
                SharedString::from(format!("stage-more-{orch}-{key}")),
                s.more,
                "More options",
                Some(words::options(es, default_bar, router_on)),
            )
            .mt_1()
            .on_click(
                cx.listener(move |ws, _, _, cx| ws.with_stage(orch, key, cx, |s| s.more = !s.more)),
            ),
        )
        .when(s.more, |d| d.child(extra));
    c = c.child(indent(body));
    c.into_any_element()
}

/// **Add a stage**: a dashed button; clicked, it becomes two tiles to pick the kind of stage,
/// each with a line on what it's for, and **Cancel**.
fn add_step(orch: PaneId, adding: bool, cx: &mut Context<Workspace>) -> AnyElement {
    if !adding {
        return div()
            .id(SharedString::from(format!("settings-add-{orch}")))
            .px(px(14.))
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_dashed()
            .border_color(color(theme::BORDER_STRONG))
            .hover(|d| {
                d.border_color(color(theme::TEAL))
                    .text_color(color(theme::TEAL))
            })
            .cursor_pointer()
            .text_center()
            .text_size(px(13.5))
            .text_color(color(theme::TEXT_SECONDARY))
            .child("+ Add a stage")
            .on_click(cx.listener(move |ws, _, _, cx| ws.with_draft(orch, cx, |d| d.adding = true)))
            .into_any_element();
    }
    let tile = |id: &str, title: &str, about: &str, jev: bool| {
        v_flex()
            .id(SharedString::from(format!("settings-add-{id}-{orch}")))
            // Equal halves that shrink with the window: their text wraps, not the row.
            .flex_1()
            .flex_basis(px(0.))
            .min_w_0()
            .gap_1()
            .px(px(16.))
            .py(px(14.))
            .rounded(px(10.))
            .border_1()
            .border_color(color(0x1f2524))
            .bg(color(0x131616))
            .hover(|d| d.border_color(color(theme::TEAL)))
            .cursor_pointer()
            .child(
                text(14., theme::TEXT)
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_string()),
            )
            .child(
                text(12.5, theme::TEXT_MUTED)
                    .whitespace_normal()
                    .child(about.to_string()),
            )
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.with_draft(orch, cx, |d| d.adding = false);
                ws.add_stage(orch, jev, window, cx)
            }))
    };
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(text(12.5, theme::TEXT_SECONDARY).child("What kind of stage?"))
                .child(
                    Button::new(SharedString::from(format!("settings-add-cancel-{orch}")))
                        .label("Cancel")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.with_draft(orch, cx, |d| d.adding = false)
                        })),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_stretch()
                .child(tile(
                    "claude",
                    "Claude stage",
                    "Does the work: reviews a pull request, fixes a ticket, drafts a reply.",
                    false,
                ))
                .child(tile(
                    "jev",
                    "Sort with Jev",
                    "Picks one category, like what an email is about, so the next stage knows.",
                    true,
                )),
        )
        .into_any_element()
}

/// **While a step waits** (core spec §60), as dropdowns read back in a sentence.
fn waiting(orch: PaneId, w: pipeline::WaitingSettings, ws: Entity<Workspace>) -> AnyElement {
    fn every(m: u32) -> String {
        if m == 0 {
            "Never".into()
        } else {
            words::minutes(m)
        }
    }
    fn until(h: u32) -> String {
        if h == 0 {
            "Until someone acts".into()
        } else {
            words::hours(h)
        }
    }
    let set = |f: fn(&mut pipeline::WaitingSettings, u32)| {
        move |v: u32, ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
            ws.with_draft(orch, cx, |d| f(&mut d.waiting, v))
        }
    };
    let mut section = v_flex()
        .gap_2()
        .child(section_tip(
            "While a stage waits",
            "The same for every pipeline. A stage waits on you, on a pull request's author, or \
             on product. Its session stays open; on this timetable hydraui tells it to look \
             again, and if something changed it reports again.",
        ))
        .child(field(
            "Look again every",
            numbers(
                format!("waiting-every-{orch}"),
                words::with_current(&words::LOOK_EVERY, w.look_every),
                w.look_every,
                every,
                ws.clone(),
                set(|w, v| w.look_every = v),
            ),
        ));
    if w.look_every > 0 {
        section = section
            .child(field(
                "Keep looking",
                numbers(
                    format!("waiting-for-{orch}"),
                    words::with_current(&words::LOOK_FOR, w.look_for),
                    w.look_for,
                    until,
                    ws.clone(),
                    set(|w, v| w.look_for = v),
                ),
            ))
            .child(field(
                "While it waits on you",
                numbers(
                    format!("waiting-you-{orch}"),
                    vec![1, 0],
                    u32::from(w.needs_you),
                    |v| {
                        if v == 1 {
                            "Look again".into()
                        } else {
                            "Don't look".into()
                        }
                    },
                    ws.clone(),
                    set(|w, v| w.needs_you = v == 1),
                ),
            ));
        if w.look_for > 0 {
            section = section.child(field(
                "When it's done looking",
                numbers(
                    format!("waiting-then-{orch}"),
                    vec![0, 1],
                    u32::from(w.then == pipeline::WaitThen::Close),
                    |v| {
                        if v == 1 {
                            "Close its session".into()
                        } else {
                            "Stop looking".into()
                        }
                    },
                    ws,
                    set(|w, v| {
                        w.then = if v == 1 {
                            pipeline::WaitThen::Close
                        } else {
                            pipeline::WaitThen::Stop
                        }
                    }),
                ),
            ));
        }
    }
    section
        .child(text(12.5, theme::TEXT).whitespace_normal().child(w.text()))
        .when(w.look_every > 0 && w.needs_you, |d| {
            d.child(
                text(11.5, theme::TEXT_MUTED)
                    .whitespace_normal()
                    .child(format!(
                        "Each look, a session waiting on you is told: \"{}\"",
                        crate::waiting::needs_you_prompt("<what it needs from you>")
                    )),
            )
        })
        .child(text(11.5, theme::TEXT_MUTED).whitespace_normal().child(
            "A run about a pull request also ends by itself when that pull request is merged or \
             closed.",
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checklist_scrolls_to_each_parts_row() {
        assert_eq!(row_of(Part::Name, 3), 2);
        assert_eq!(row_of(Part::Source, 3), 3);
        assert_eq!(row_of(Part::Steps, 3), 4);
        assert_eq!(row_of(Part::Step(0), 3), 5);
        assert_eq!(row_of(Part::Step(2), 3), 7);
        // After the last step comes Add a step, then Advanced settings.
        assert_eq!(row_of(Part::Advanced, 3), 9);
    }
}
