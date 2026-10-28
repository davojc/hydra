//! Settings › Swarms (spec: docs/superpowers/specs/2026-10-27-swarms-design.md §7): you and this
//! machine, each swarm's page, and making, joining and inviting. Everything is sent to the swarms
//! through `swarms_command` and `swarms_create_or_join`, which send nothing while another window
//! runs them.
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, Hsla, SharedString, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use hydra_swarms::invites::InviteCode;
use hydra_swarms::{Attention, Command, Invite, MemberView, Resolution, Role, SwarmView};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::Workspace;
use super::settings_tab::{Section, group, page_title};
use crate::action_log;
use crate::config;
use crate::swarms;

/// The longest name taken for you, this machine or a swarm, in characters.
const MAX_NAME_CHARS: usize = 200;
/// How much of an invite code the dialog shows; Copy invite copies all of it.
const SHOWN_CODE_CHARS: usize = 48;
/// What the page says while another window runs the swarms.
pub(super) const ELSEWHERE: &str = "Swarms run in another hydraui window";

/// Your name and this machine's name, as typed.
pub(super) struct IdentityFields {
    name: Entity<InputState>,
    machine: Entity<InputState>,
}

impl IdentityFields {
    /// Both names, checked.
    pub(super) fn read(&self, cx: &App) -> Result<(String, String), String> {
        let name = checked_name("Your name", &self.name.read(cx).value())?;
        let machine = checked_name("This machine's name", &self.machine.read(cx).value())?;
        Ok((name, machine))
    }

    pub(super) fn render(&self, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .child(labelled("YOUR NAME", Input::new(&self.name), cx))
            .child(labelled(
                "THIS MACHINE'S NAME",
                Input::new(&self.machine),
                cx,
            ))
            .child(div().text_sm().text_color(muted).child(format!(
                "Starts as the computer's name ({}). Pick something your teammates will recognise.",
                swarms::default_machine_name()
            )))
            .into_any_element()
    }
}

/// The invite dialog's state.
struct InviteState {
    role: Role,
    code: Option<String>,
    problem: Option<String>,
    asking: bool,
}

/// What the status dot beside a swarm shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dot {
    UpToDate,
    Attention,
    Offline,
}

/// A button under Needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fix {
    Compare,
    KeepTheirs,
    KeepMine,
    ShareAgain,
    DeleteIt,
    UseSwarmVersion,
}

impl Fix {
    fn label(self) -> &'static str {
        match self {
            Fix::Compare => "Compare",
            Fix::KeepTheirs => "Keep theirs",
            Fix::KeepMine => "Keep mine",
            Fix::ShareAgain => "Share it again",
            Fix::DeleteIt => "Delete it",
            Fix::UseSwarmVersion => "Use the swarm's version",
        }
    }
}

/// What an owner can do to another member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberAction {
    Make(Role),
    Remove,
}

impl MemberAction {
    fn label(self) -> &'static str {
        match self {
            MemberAction::Make(Role::Owner) => "Make owner",
            MemberAction::Make(Role::Writer) => "Make writer",
            MemberAction::Make(Role::Reader) => "Make reader",
            MemberAction::Remove => "Remove…",
        }
    }
}

impl Workspace {
    /// The name fields, filled with what is saved, the machine's name defaulting to the host's.
    pub(super) fn identity_fields(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> IdentityFields {
        let name = self.ui.peer_name.clone().unwrap_or_default();
        let machine = self
            .ui
            .machine_name
            .clone()
            .unwrap_or_else(swarms::default_machine_name);
        IdentityFields {
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Your name, like Davo")
                    .default_value(name)
            }),
            machine: cx.new(|cx| InputState::new(window, cx).default_value(machine)),
        }
    }

    /// Fills the You page's fields again from what is saved, when it is showing.
    pub(super) fn swarms_refresh_you(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.as_ref().is_some_and(|t| t.you.is_some()) {
            let fields = self.identity_fields(window, cx);
            if let Some(tab) = self.settings.as_mut() {
                tab.you = Some(fields);
            }
        }
    }

    /// Whether making or joining a swarm has to ask for the names first.
    pub(super) fn swarms_need_identity(&self) -> bool {
        self.ui
            .peer_name
            .as_deref()
            .is_none_or(|n| n.trim().is_empty())
    }

    /// Saves the names to config.toml off the UI thread, tells the swarms, then runs `then`.
    fn swarms_set_identity(
        &mut self,
        name: String,
        machine: String,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = self.hydra.paths.clone();
        let (n, m) = (name.clone(), machine.clone());
        let saved = cx.background_spawn(async move {
            config::set(&paths, "peer_name", Some(n.as_str().into()))?;
            config::set(&paths, "machine_name", Some(m.as_str().into()))
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = saved.await;
            let _ = this.update_in(cx, |ws, window, cx| match saved {
                Ok(()) => {
                    ws.ui.peer_name = Some(name.clone());
                    ws.ui.machine_name = Some(machine.clone());
                    ws.swarms_command(Command::SetIdentity {
                        name,
                        machine_name: machine,
                    });
                    then(ws, window, cx);
                    cx.notify();
                }
                Err(e) => {
                    action_log::write(format!("swarms: couldn't save your name: {e}"));
                    window.push_notification(
                        Notification::error(format!("Couldn't save your name: {e}"))
                            .title("Swarms"),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// You and this machine's Save.
    fn swarms_save_you(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fields) = self.settings.as_ref().and_then(|t| t.you.as_ref()) else {
            return;
        };
        let (name, machine) = match fields.read(cx) {
            Ok(names) => names,
            Err(e) => {
                window.push_notification(Notification::warning(e).title("Swarms"), cx);
                return;
            }
        };
        self.swarms_set_identity(
            name,
            machine,
            |ws, window, cx| {
                // With the name set, the swarms can start here now, not at the next restart. A
                // search already waiting tries again at once.
                if let Some(wake) = ws.swarms_wake.take() {
                    let _ = wake.send(());
                }
                ws.swarms_start(window, cx);
                // The window running the swarms reads the names when it starts them.
                let saved = if ws.swarms_elsewhere {
                    "Saved. The hydraui window running your swarms picks this up when it restarts."
                } else {
                    "Saved."
                };
                window.push_notification(Notification::success(saved).title("Swarms"), cx);
            },
            window,
            cx,
        );
    }

    /// Makes or joins a swarm, saving the names first when they were asked for.
    pub(super) fn swarms_make_or_join(
        &mut self,
        identity: Option<(String, String)>,
        cmd: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match identity {
            Some((name, machine)) => self.swarms_set_identity(
                name,
                machine,
                move |ws, window, cx| ws.swarms_create_or_join(cmd, window, cx),
                window,
                cx,
            ),
            None => self.swarms_create_or_join(cmd, window, cx),
        }
    }

    /// Asks, then runs `then`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn swarms_confirm(
        &mut self,
        title: String,
        body: String,
        ok: &'static str,
        danger: bool,
        then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.entity();
        let then = Rc::new(then);
        window.open_dialog(cx, move |dialog, _, _| {
            let (ws, then) = (workspace.clone(), then.clone());
            dialog.title(title.clone()).child(body.clone()).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm")
                            .label(ok)
                            .when(danger, |b| b.danger())
                            .when(!danger, |b| b.primary())
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                ws.update(cx, |ws, cx| then(ws, window, cx));
                            }),
                    ),
            )
        });
    }

    /// + New swarm: a name and a folder, and the names first when they aren't set.
    pub(super) fn new_swarm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name, like team-skills"));
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("Folder on this machine"));
        let ident = self
            .swarms_need_identity()
            .then(|| self.identity_fields(window, cx));
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let ident = Rc::new(ident);
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let focus = match ident.as_ref() {
                Some(f) => &f.name,
                None => &name,
            };
            if first.replace(false) {
                focus.update(cx, |s, cx| s.focus(window, cx));
            }
            let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
            let shown = problem.read(cx).clone();
            let (n, f, p, i, ws) = (
                name.clone(),
                folder.clone(),
                problem.clone(),
                ident.clone(),
                workspace.clone(),
            );
            let body = v_flex()
                .gap_3()
                .when_some(ident.as_ref().as_ref(), |d, fields| {
                    d.child(fields.render(cx))
                })
                .when(ident.is_none(), |d| {
                    d.child(you_line(&workspace, "You'll be", cx))
                })
                .child(labelled("NAME", Input::new(&name), cx))
                .child(labelled(
                    "FOLDER ON THIS MACHINE",
                    folder_row(&workspace, &folder, "new-swarm"),
                    cx,
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("The files already in it that the swarm syncs are shared."),
                )
                .when_some(shown, |d, e| {
                    d.child(div().text_sm().text_color(danger).child(e))
                });
            dialog.title("New swarm").w(px(560.)).child(body).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("create").label("Create").primary().on_click(
                        move |_, window, cx| {
                            let checked = (|| {
                                let identity = match i.as_ref() {
                                    Some(fields) => Some(fields.read(cx)?),
                                    None => None,
                                };
                                let name = checked_name("The swarm's name", &n.read(cx).value())?;
                                let folder = chosen_folder(&f.read(cx).value())?;
                                Ok::<_, String>((identity, Command::Create { name, folder }))
                            })();
                            match checked {
                                Ok((identity, cmd)) => {
                                    window.close_dialog(cx);
                                    ws.update(cx, |ws, cx| {
                                        ws.swarms_make_or_join(identity, cmd, window, cx)
                                    });
                                }
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

    /// Join a swarm…: the invite and a folder, and the names first when they aren't set.
    pub(super) fn join_swarm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("hydra-swarm:…"));
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("Folder on this machine"));
        let ident = self
            .swarms_need_identity()
            .then(|| self.identity_fields(window, cx));
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let ident = Rc::new(ident);
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let focus = match ident.as_ref() {
                Some(f) => &f.name,
                None => &code,
            };
            if first.replace(false) {
                focus.update(cx, |s, cx| s.focus(window, cx));
            }
            let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
            let shown = problem.read(cx).clone();
            let (c, f, p, i, ws) = (
                code.clone(),
                folder.clone(),
                problem.clone(),
                ident.clone(),
                workspace.clone(),
            );
            let body = v_flex()
                .gap_3()
                .child(labelled(
                    "INVITE",
                    div().font_family("monospace").child(Input::new(&code)),
                    cx,
                ))
                .when_some(ident.as_ref().as_ref(), |d, fields| {
                    d.child(fields.render(cx))
                })
                .when(ident.is_none(), |d| {
                    d.child(you_line(&workspace, "You'll join as", cx))
                })
                .child(labelled(
                    "FOLDER ON THIS MACHINE",
                    folder_row(&workspace, &folder, "join-swarm"),
                    cx,
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("Pick an empty folder, or a new one."),
                )
                .when_some(shown, |d, e| {
                    d.child(div().text_sm().text_color(danger).child(e))
                });
            dialog.title("Join a swarm").w(px(560.)).child(body).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("join").label("Join").primary().on_click(
                        move |_, window, cx| {
                            let checked = (|| {
                                let code = checked_invite(&c.read(cx).value())?;
                                let identity = match i.as_ref() {
                                    Some(fields) => Some(fields.read(cx)?),
                                    None => None,
                                };
                                let folder = chosen_folder(&f.read(cx).value())?;
                                Ok::<_, String>((identity, Command::Join { code, folder }))
                            })();
                            match checked {
                                Ok((identity, cmd)) => {
                                    window.close_dialog(cx);
                                    ws.update(cx, |ws, cx| {
                                        ws.swarms_make_or_join(identity, cmd, window, cx)
                                    });
                                }
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

    /// Invite…: pick a role, make the code, copy it.
    fn invite_dialog(
        &mut self,
        swarm: String,
        swarm_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = cx.new(|_| InviteState {
            role: Role::Writer,
            code: None,
            problem: None,
            asking: false,
        });
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (muted, danger, border) = (
                cx.theme().muted_foreground,
                cx.theme().danger,
                cx.theme().border,
            );
            let shown = state.read(cx);
            let (role, code, problem, asking) = (
                shown.role,
                shown.code.clone(),
                shown.problem.clone(),
                shown.asking,
            );
            let fixed = code.is_some() || asking;
            let segments = h_flex().gap_1().children(
                [Role::Writer, Role::Reader, Role::Owner].map(|r| {
                    let s = state.clone();
                    Button::new(SharedString::from(format!("invite-role-{}", role_name(r))))
                        .label(capitalised(role_name(r)))
                        .small()
                        .when(r == role, |b| b.primary())
                        .disabled(fixed && r != role)
                        .on_click(move |_, _, cx| {
                            s.update(cx, |s, cx| {
                                s.role = r;
                                cx.notify();
                            })
                        })
                }),
            );
            let body = v_flex()
                .gap_3()
                .child(segments)
                .child(div().text_sm().text_color(muted).child(role_note(role)))
                .when_some(code.clone(), |d, code| {
                    d.child(
                        div()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_dashed()
                            .border_color(border)
                            .font_family("monospace")
                            .text_sm()
                            .child(shown_code(&code)),
                    )
                })
                .when_some(problem, |d, e| {
                    d.child(div().text_sm().text_color(danger).child(e))
                });
            let action = match code {
                Some(code) => Button::new("invite-copy")
                    .label("Copy invite")
                    .primary()
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()))
                    }),
                None => {
                    let (s, ws, swarm) = (state.clone(), workspace.clone(), swarm.clone());
                    Button::new("invite-make")
                        .label("Make invite")
                        .primary()
                        .loading(asking)
                        .on_click(move |_, _, cx| {
                            let (s, swarm) = (s.clone(), swarm.clone());
                            ws.update(cx, |ws, cx| ws.swarms_invite(swarm, s, cx));
                        })
                }
            };
            dialog
                .title(format!("Invite to {swarm_name}"))
                .w(px(560.))
                .child(body)
                .footer(
                    h_flex()
                        .gap_3()
                        .w_full()
                        .child(action)
                        .child(div().flex_1().text_sm().text_color(muted).child(
                            "Works once · expires in 7 days · your hydraui must be running when they join",
                        )),
                )
        });
    }

    /// Asks the swarms for an invite code for `state`'s role and shows it.
    fn swarms_invite(&mut self, swarm: String, state: Entity<InviteState>, cx: &mut Context<Self>) {
        let role = state.update(cx, |s, cx| {
            s.asking = true;
            s.problem = None;
            cx.notify();
            s.role
        });
        let (reply, answer) = oneshot::channel();
        let here = self.swarms_run.is_some();
        self.swarms_command(Command::Invite { swarm, role, reply });
        cx.spawn(async move |_, cx| {
            let answer = answer.await;
            state.update(cx, |s, cx| {
                s.asking = false;
                match answer {
                    Ok(Ok(code)) => s.code = Some(code),
                    Ok(Err(e)) => s.problem = Some(e),
                    Err(_) if !here => s.problem = Some(format!("{ELSEWHERE}: invite from there.")),
                    Err(_) => {
                        s.problem = Some("The swarms stopped before the invite was made.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks before an owner's action on another member.
    fn member_action(
        &mut self,
        swarm: &SwarmView,
        member: &MemberView,
        action: MemberAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (id, person, who) = (swarm.id.clone(), member.person.clone(), member.name.clone());
        let (title, body, ok, danger) = match action {
            MemberAction::Make(role) => (
                format!("Make {who} {} {}?", article(role), role_name(role)),
                role_note(role).to_string(),
                action.label(),
                false,
            ),
            MemberAction::Remove => (
                format!("Remove {who} from {}?", swarm.name),
                "Their machines stop syncing it. The files they already have stay with them."
                    .to_string(),
                "Remove",
                true,
            ),
        };
        self.swarms_confirm(
            title,
            body,
            ok,
            danger,
            move |ws, _, _| {
                let (swarm, person) = (id.clone(), person.clone());
                ws.swarms_command(match action {
                    MemberAction::Make(role) => Command::SetRole {
                        swarm,
                        person,
                        role,
                    },
                    MemberAction::Remove => Command::Remove { swarm, person },
                })
            },
            window,
            cx,
        );
    }

    /// Runs a button under Needs attention, asking first when it would throw a version away.
    fn attention_fix(
        &mut self,
        swarm: &SwarmView,
        item: &Attention,
        fix: Fix,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if fix == Fix::Compare {
            // Shown, not opened: a file from a teammate could be a script.
            if let Attention::Conflict { path, copy } = item {
                for p in [path, copy] {
                    if let Err(e) =
                        crate::file_ops::reveal_in_explorer(&in_folder(&swarm.folder, p.as_str()))
                    {
                        window.push_notification(Notification::error(e).title("Swarms"), cx);
                        break;
                    }
                }
            }
            return;
        }
        let Some(action) = resolution(fix, item) else {
            return;
        };
        let id = swarm.id.clone();
        let send = move |ws: &mut Self, _: &mut Window, _: &mut Context<Self>| {
            ws.swarms_command(Command::Resolve {
                swarm: id.clone(),
                action: action.clone(),
            })
        };
        match fix_warning(fix, item) {
            Some((title, body, ok)) => self.swarms_confirm(title, body, ok, true, send, window, cx),
            None => send(self, window, cx),
        }
    }

    /// The status line on the swarm pages while another window runs the swarms.
    fn swarms_status_line(&self, cx: &App) -> Option<AnyElement> {
        self.swarms_elsewhere.then(|| {
            div()
                .text_sm()
                .text_color(cx.theme().warning)
                .child(ELSEWHERE)
                .into_any_element()
        })
    }

    /// The colour of `swarm`'s dot in the sidebar.
    pub(super) fn swarm_nav_dot(&self, swarm: &SwarmView, cx: &App) -> Hsla {
        dot_colour(swarm_dot(swarm), cx)
    }

    pub(super) fn you_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(fields) = self.settings.as_ref().and_then(|t| t.you.as_ref()) else {
            return div().into_any_element();
        };
        v_flex()
            .gap_5()
            .child(page_title(
                "You and this machine",
                "Every swarm you're in shows these. Changing them updates all your swarms.".into(),
                div(),
                cx,
            ))
            .when_some(self.swarms_status_line(cx), |d, s| d.child(s))
            .child(group(
                "You",
                None,
                vec![
                    div().max_w(px(360.)).child(fields.render(cx)).into_any_element(),
                    Button::new("swarms-you-save")
                        .label("Save")
                        .primary()
                        .small()
                        .on_click(cx.listener(|ws, _, window, cx| ws.swarms_save_you(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
            .child(div().text_sm().text_color(muted).child(
                "The first time you make or join a swarm, hydraui asks for these if they aren't set yet.",
            ))
            .child(self.your_machines(cx))
            .when_some(self.offered_here(cx), |d, offers| d.child(offers))
            .into_any_element()
    }

    pub(super) fn swarm_page(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(swarm) = self.swarms.iter().find(|s| s.id == id) else {
            return v_flex()
                .gap_2()
                .when_some(self.swarms_status_line(cx), |d, s| d.child(s))
                .when(!self.swarms_elsewhere, |d| {
                    d.child(
                        div()
                            .text_color(muted)
                            .child("This swarm is no longer here."),
                    )
                })
                .into_any_element();
        };
        let tz = TimeZone::system();
        let now = now_secs();
        let owner = swarm.my_role == Role::Owner && !swarm.removed;
        let mut page = v_flex()
            .gap_5()
            .child(self.swarm_title(swarm, cx))
            .when_some(self.swarms_status_line(cx), |d, s| d.child(s))
            .child(self.swarm_folder(swarm, &tz, now, cx));
        if !swarm.attention.is_empty() {
            page = page.child(self.swarm_attention(swarm, cx));
        }
        page = page
            .child(self.swarm_members(swarm, owner, &tz, now, cx))
            .child(self.swarm_types(swarm, owner, cx))
            .child(self.swarm_leave(swarm, cx));
        page.into_any_element()
    }

    fn swarm_title(&self, swarm: &SwarmView, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let pill = h_flex()
            .gap_2()
            .h(px(24.))
            .px_3()
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .text_xs()
            .child(dot(dot_colour(pill_dot(swarm), cx)))
            .child(status_text(swarm));
        h_flex()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(swarm.name.clone()),
            )
            .child(pill)
            .child(div().flex_1())
            .when(!swarm.removed, |d| {
                d.child(badge(
                    format!(
                        "You're {} {}",
                        article(swarm.my_role),
                        role_name(swarm.my_role)
                    ),
                    swarm.my_role,
                    cx,
                ))
            })
            .into_any_element()
    }

    fn swarm_folder(
        &self,
        swarm: &SwarmView,
        tz: &TimeZone,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (muted, fg) = (cx.theme().muted_foreground, cx.theme().foreground);
        let (id, folder) = (swarm.id.clone(), swarm.folder.clone());
        let last = match &swarm.last_change {
            Some((machine, at)) => h_flex()
                .gap_1()
                .text_sm()
                .text_color(muted)
                .child("Last change from")
                .child(div().text_color(fg).child(machine.clone()))
                .child(format!("at {}", when(*at, now, tz))),
            None => h_flex().text_sm().text_color(muted).child("No changes yet"),
        };
        v_flex()
            .gap_2()
            .child(section_label("FOLDER ON THIS MACHINE", cx))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .font_family("monospace")
                            .text_sm()
                            .child(folder.display().to_string()),
                    )
                    .child(Button::new("swarm-open").label("Open").small().on_click(
                        move |_, window, cx| {
                            if let Err(e) = crate::file_ops::reveal_in_explorer(&folder) {
                                window
                                    .push_notification(Notification::error(e).title("Swarms"), cx);
                            }
                        },
                    ))
                    .when(!swarm.removed, |d| {
                        d.child(
                            Button::new("swarm-change-folder")
                                .label("Change…")
                                .ghost()
                                .small()
                                .tooltip("An empty folder, or a new one")
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    let swarm = id.clone();
                                    ws.open_dialog_path(
                                        true,
                                        window,
                                        cx,
                                        move |ws, folder, _, _| {
                                            ws.swarms_command(Command::SetFolder { swarm, folder })
                                        },
                                    )
                                })),
                        )
                    }),
            )
            .child(last)
            .into_any_element()
    }

    fn swarm_attention(&self, swarm: &SwarmView, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        let rows = swarm.attention.iter().enumerate().map(|(i, item)| {
            let (title, path, note) = attention_text(item);
            let buttons = h_flex().gap_2().flex_none().children(
                fixes(item, swarm.my_role)
                    .into_iter()
                    .filter(|_| !swarm.removed)
                    .map(|fix| {
                        let (s, item) = (swarm.clone(), item.clone());
                        Button::new(SharedString::from(format!("attention-{i}-{fix:?}")))
                            .label(fix.label())
                            .small()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.attention_fix(&s, &item, fix, window, cx)
                            }))
                    }),
            );
            h_flex()
                .gap_3()
                .px_4()
                .py_2()
                .when(i > 0, |d| d.border_t_1().border_color(warning.opacity(0.2)))
                .child(dot(warning))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(title)
                                .child(div().font_family("monospace").child(path)),
                        )
                        .child(div().text_sm().text_color(muted).child(note)),
                )
                .child(buttons)
        });
        v_flex()
            .gap_2()
            .child(section_label("NEEDS ATTENTION", cx))
            .child(
                v_flex()
                    .rounded_lg()
                    .border_1()
                    .border_color(warning.opacity(0.4))
                    .bg(warning.opacity(0.06))
                    .children(rows),
            )
            .into_any_element()
    }

    fn swarm_members(
        &self,
        swarm: &SwarmView,
        owner: bool,
        tz: &TimeZone,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, ok) = (theme.muted_foreground, theme.success);
        let workspace = cx.entity();
        let mut rows: Vec<AnyElement> = swarm
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let online = m.machines.iter().any(|x| x.online || x.this_machine);
                let actions = member_actions(swarm, m);
                let menu = (!actions.is_empty()).then(|| {
                    let (ws, s, m) = (workspace.clone(), swarm.clone(), m.clone());
                    Button::new(SharedString::from(format!("member-{i}")))
                        .label("⋯")
                        .ghost()
                        .xsmall()
                        .dropdown_menu(move |mut menu, _, _| {
                            for action in actions.iter().copied() {
                                let (ws, s, m) = (ws.clone(), s.clone(), m.clone());
                                menu = menu.item(PopupMenuItem::new(action.label()).on_click(
                                    move |_, window, cx| {
                                        ws.update(cx, |ws, cx| {
                                            ws.member_action(&s, &m, action, window, cx)
                                        })
                                    },
                                ));
                            }
                            menu
                        })
                });
                h_flex()
                    .gap_3()
                    .child(dot(if online { ok } else { muted }))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(m.name.clone())
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(muted)
                                    .child(format!("· {}", member_detail(m, now))),
                            ),
                    )
                    .child(badge(role_name(m.role).to_string(), m.role, cx))
                    .when_some(menu, |d, b| d.child(b))
                    .into_any_element()
            })
            .collect();
        if owner {
            rows.extend(swarm.pending_invites.iter().map(|inv| {
                let (s, id) = (swarm.id.clone(), inv.id.clone());
                h_flex()
                    .gap_3()
                    .text_sm()
                    .text_color(muted)
                    .child(div().flex_1().child(invite_text(inv, tz)))
                    .child(
                        Button::new(SharedString::from(format!("invite-cancel-{}", inv.id)))
                            .label("Cancel invite")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |ws, _, _, _| {
                                ws.swarms_command(Command::CancelInvite {
                                    swarm: s.clone(),
                                    id: id.clone(),
                                })
                            })),
                    )
                    .into_any_element()
            }));
        }
        let invite = owner.then(|| {
            let (id, name) = (swarm.id.clone(), swarm.name.clone());
            Button::new("swarm-invite")
                .label("Invite…")
                .primary()
                .small()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.invite_dialog(id.clone(), name.clone(), window, cx)
                }))
                .into_any_element()
        });
        group("Members", invite, rows, cx)
    }

    fn swarm_types(&self, swarm: &SwarmView, owner: bool, cx: &mut Context<Self>) -> AnyElement {
        let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
        let chips = swarm.types.iter().map(|t| {
            let rest: Vec<String> = swarm.types.iter().filter(|x| *x != t).cloned().collect();
            let id = swarm.id.clone();
            h_flex()
                .gap_1()
                .h(px(24.))
                .px_2()
                .rounded_full()
                .border_1()
                .border_color(border)
                .font_family("monospace")
                .text_xs()
                .child(t.clone())
                .when(owner, |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("type-remove-{t}")))
                            .cursor_pointer()
                            .text_color(muted)
                            .child("×")
                            .on_click(cx.listener(move |ws, _, _, _| {
                                ws.swarms_command(Command::SetTypes {
                                    swarm: id.clone(),
                                    types: rest.clone(),
                                })
                            })),
                    )
                })
                .into_any_element()
        });
        let add = owner.then(|| {
            let (id, types) = (swarm.id.clone(), swarm.types.clone());
            Button::new("type-add")
                .label("+ Add")
                .ghost()
                .small()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    let (id, types) = (id.clone(), types.clone());
                    ws.text_dialog(
                        "Add a file type".into(),
                        String::new(),
                        "An extension, like .py",
                        move |ws, typed, _, _| {
                            let types = with_type(&types, &typed)?;
                            ws.swarms_command(Command::SetTypes {
                                swarm: id.clone(),
                                types,
                            });
                            Ok(())
                        },
                        window,
                        cx,
                    )
                }))
        });
        v_flex()
            .gap_2()
            .child(section_label("FILE TYPES THAT SYNC", cx))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(chips)
                    .when_some(add, |d, b| d.child(b)),
            )
            .child(div().text_sm().text_color(muted).child(
                "Other files stay on each machine and never leave it. Files over 10 MB don't sync.",
            ))
            .into_any_element()
    }

    fn swarm_leave(&self, swarm: &SwarmView, cx: &mut Context<Self>) -> AnyElement {
        let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
        // Leaving on one of your machines leaves the rest in, owners included.
        let stuck = last_owner(swarm) && swarm.my_other_machines == 0;
        let (id, name) = (swarm.id.clone(), swarm.name.clone());
        let note = leave_note(swarm);
        let body = note.clone();
        let leave = Button::new("swarm-leave")
            .label(leave_label(swarm))
            .danger()
            .small()
            .disabled(stuck)
            .when(stuck, |b| b.tooltip("Make someone else an owner first"))
            .on_click(cx.listener(move |ws, _, window, cx| {
                let id = id.clone();
                ws.swarms_confirm(
                    format!("Leave {name}?"),
                    body.clone(),
                    "Leave",
                    true,
                    move |ws, window, cx| {
                        ws.swarms_command(Command::Leave { swarm: id.clone() });
                        ws.open_settings(Section::You, window, cx);
                    },
                    window,
                    cx,
                )
            }));
        h_flex()
            .gap_3()
            .pt_4()
            .border_t_1()
            .border_color(border)
            .child(div().flex_1().text_sm().text_color(muted).child(note))
            .child(leave)
            .into_any_element()
    }
}

/// The Leave button: on one of your machines only, when you have others in the swarm.
fn leave_label(swarm: &SwarmView) -> &'static str {
    if swarm.my_other_machines > 0 {
        "Leave on this machine…"
    } else {
        "Leave swarm…"
    }
}

fn leave_note(swarm: &SwarmView) -> String {
    if swarm.my_other_machines > 0 {
        format!(
            "Your other machines stay in {}. This machine keeps its files but stops syncing them.",
            swarm.name
        )
    } else {
        "Leaving keeps the folder's files here, but stops syncing them.".into()
    }
}

/// "You'll join as Davo on Davo's desktop. Change", where Change opens You and this machine.
fn you_line(workspace: &Entity<Workspace>, lead: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let ui = &workspace.read(cx).ui;
    let name = ui.peer_name.clone().unwrap_or_default();
    let machine = ui
        .machine_name
        .clone()
        .unwrap_or_else(swarms::default_machine_name);
    let ws = workspace.clone();
    h_flex()
        .gap_1()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(lead.to_string())
        .child(div().text_color(theme.foreground).child(name))
        .child("on")
        .child(
            div()
                .text_color(theme.foreground)
                .child(format!("{machine}.")),
        )
        .child(
            div()
                .id("swarms-change-you")
                .cursor_pointer()
                .text_color(theme.link)
                .child("Change")
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    ws.update(cx, |ws, cx| ws.open_settings(Section::You, window, cx));
                }),
        )
        .into_any_element()
}

/// A folder box with Browse…, which fills it from GPUI's folder prompt.
pub(super) fn folder_row(
    workspace: &Entity<Workspace>,
    folder: &Entity<InputState>,
    id: &'static str,
) -> AnyElement {
    let (ws, f) = (workspace.clone(), folder.clone());
    h_flex()
        .gap_2()
        .child(
            div()
                .flex_1()
                .font_family("monospace")
                .child(Input::new(folder)),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-browse")))
                .label("Browse…")
                .on_click(move |_, window, cx| {
                    let f = f.clone();
                    ws.update(cx, |ws, cx| {
                        ws.open_dialog_path(true, window, cx, move |_, dir, window, cx| {
                            f.update(cx, |s, cx| {
                                s.set_value(dir.display().to_string(), window, cx)
                            })
                        })
                    })
                }),
        )
        .into_any_element()
}

pub(super) fn labelled(label: &'static str, control: impl IntoElement, cx: &App) -> AnyElement {
    v_flex()
        .gap_1()
        .child(section_label(label, cx))
        .child(control)
        .into_any_element()
}

fn section_label(label: &'static str, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

pub(super) fn dot(colour: Hsla) -> AnyElement {
    div()
        .size(px(8.))
        .flex_none()
        .rounded_full()
        .bg(colour)
        .into_any_element()
}

/// A role's badge: owners in violet, writers in teal, readers plain.
pub(super) fn badge(text: String, role: Role, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let colour: Hsla = match role {
        Role::Owner => rgb(0xa78bfa).into(),
        Role::Writer => rgb(0x2dd4bf).into(),
        Role::Reader => muted,
    };
    div()
        .flex_none()
        .px_2()
        .rounded_full()
        .border_1()
        .border_color(colour.opacity(0.7))
        .text_xs()
        .text_color(colour)
        .child(text)
        .into_any_element()
}

fn dot_colour(dot: Dot, cx: &App) -> Hsla {
    match dot {
        Dot::UpToDate => cx.theme().success,
        Dot::Attention => cx.theme().warning,
        Dot::Offline => cx.theme().muted_foreground,
    }
}

/// The sidebar's dot: grey when removed or no other machine is online, amber with something
/// needing attention, else green.
fn swarm_dot(s: &SwarmView) -> Dot {
    if s.removed {
        Dot::Offline
    } else if !s.attention.is_empty() {
        Dot::Attention
    } else if s.machines > 1 && s.online <= 1 {
        Dot::Offline
    } else {
        Dot::UpToDate
    }
}

/// The status pill's dot, which follows the pill's words, not the attention list.
fn pill_dot(s: &SwarmView) -> Dot {
    if s.removed {
        Dot::Offline
    } else if s.syncing {
        Dot::Attention
    } else {
        Dot::UpToDate
    }
}

/// The status pill's words.
fn status_text(s: &SwarmView) -> String {
    if s.removed {
        "Removed from this swarm".into()
    } else if s.syncing {
        "Syncing…".into()
    } else {
        let noun = if s.machines == 1 {
            "machine"
        } else {
            "machines"
        };
        format!("Up to date · {} of {} {noun} online", s.online, s.machines)
    }
}

pub(super) fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Writer => "writer",
        Role::Reader => "reader",
    }
}

fn article(role: Role) -> &'static str {
    match role {
        Role::Owner => "an",
        Role::Writer | Role::Reader => "a",
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// What a role may do, as the invite dialog and the role confirmations say it.
fn role_note(role: Role) -> &'static str {
    match role {
        Role::Owner => "Owners can also invite people, change roles and choose which files sync.",
        Role::Writer => "Writers add, change and delete files.",
        Role::Reader => "Readers get every change but can't send any.",
    }
}

/// The buttons beside an item under Needs attention. Readers can't settle a conflict, which
/// would write to the swarm, so they get none for one.
fn fixes(item: &Attention, role: Role) -> Vec<Fix> {
    match item {
        Attention::Conflict { .. } if role == Role::Reader => Vec::new(),
        Attention::Conflict { .. } => vec![Fix::Compare, Fix::KeepTheirs, Fix::KeepMine],
        Attention::EditedAfterDelete { .. } => vec![Fix::ShareAgain, Fix::DeleteIt],
        Attention::NoLongerShared { .. } if role == Role::Reader => vec![Fix::DeleteIt],
        Attention::NoLongerShared { .. } => vec![Fix::ShareAgain, Fix::DeleteIt],
        Attention::ReaderEdit { .. } => vec![Fix::UseSwarmVersion],
        Attention::TooBig { .. } => Vec::new(),
    }
}

/// What a button sends; nothing for Compare, which only shows the files.
fn resolution(fix: Fix, item: &Attention) -> Option<Resolution> {
    let pair = |path: &hydra_swarms::paths::RelPath, copy: &hydra_swarms::paths::RelPath| {
        (path.as_str().to_string(), copy.as_str().to_string())
    };
    match (fix, item) {
        (Fix::KeepTheirs, Attention::Conflict { path, copy }) => {
            let (path, copy) = pair(path, copy);
            Some(Resolution::KeepTheirs { path, copy })
        }
        (Fix::KeepMine, Attention::Conflict { path, copy }) => {
            let (path, copy) = pair(path, copy);
            Some(Resolution::KeepMine { path, copy })
        }
        (
            Fix::ShareAgain,
            Attention::EditedAfterDelete { path } | Attention::NoLongerShared { path },
        ) => Some(Resolution::ShareAgain(path.as_str().into())),
        (
            Fix::DeleteIt,
            Attention::EditedAfterDelete { path } | Attention::NoLongerShared { path },
        ) => Some(Resolution::DeleteHeld(path.as_str().into())),
        (Fix::UseSwarmVersion, Attention::ReaderEdit { path }) => {
            Some(Resolution::UseSwarmVersion(path.as_str().into()))
        }
        _ => None,
    }
}

/// The confirmation a button asks for when it throws a version away: its title, body and button.
fn fix_warning(fix: Fix, item: &Attention) -> Option<(String, String, &'static str)> {
    match (fix, item) {
        (Fix::KeepTheirs, Attention::Conflict { path, copy }) => Some((
            format!("Keep {} as it is?", path.as_str()),
            format!(
                "The other version, {}, is deleted on every machine in the swarm.",
                file_name(copy.as_str())
            ),
            "Keep this version",
        )),
        (Fix::KeepMine, Attention::Conflict { path, copy }) => Some((
            format!("Use {} for {}?", file_name(copy.as_str()), path.as_str()),
            format!(
                "The version in {} now is replaced, and {} is deleted, on every machine in the swarm.",
                file_name(path.as_str()),
                file_name(copy.as_str())
            ),
            "Use that version",
        )),
        (Fix::DeleteIt, Attention::EditedAfterDelete { path }) => Some((
            format!("Delete {}?", path.as_str()),
            "Your edit is the only copy and can't be recovered.".into(),
            "Delete",
        )),
        (Fix::DeleteIt, Attention::NoLongerShared { path }) => Some((
            format!("Delete {}?", path.as_str()),
            "The copy on this machine may be the only one, and can't be recovered.".into(),
            "Delete",
        )),
        (Fix::UseSwarmVersion, Attention::ReaderEdit { path }) => Some((
            format!("Replace your {}?", path.as_str()),
            "Your change is on this machine only and can't be recovered.".into(),
            "Use the swarm's version",
        )),
        _ => None,
    }
}

/// An item's heading, its path, and the line under it.
fn attention_text(item: &Attention) -> (&'static str, String, String) {
    match item {
        Attention::Conflict { path, copy } => (
            "Conflict in",
            path.as_str().into(),
            format!(
                "Two machines changed it. The newer change kept the name; the other is beside it as {}.",
                file_name(copy.as_str())
            ),
        ),
        Attention::NoLongerShared { path } => (
            "No longer in the swarm:",
            path.as_str().into(),
            "Whoever wrote it was taken out of the swarm; your copy is on this machine only."
                .into(),
        ),
        Attention::EditedAfterDelete { path } => (
            "Edited after a delete:",
            path.as_str().into(),
            "Someone deleted it; your later edit is kept on this machine only.".into(),
        ),
        Attention::ReaderEdit { path } => (
            "Your change isn't shared:",
            path.as_str().into(),
            "You're a reader, so this file stopped syncing on this machine.".into(),
        ),
        Attention::TooBig { path } => (
            "Too big to sync:",
            path.as_str().into(),
            "Files over 10 MB stay on this machine.".into(),
        ),
    }
}

/// The last part of a swarm path.
fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// A swarm path (with `/`) under the swarm's folder.
fn in_folder(folder: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(folder.to_path_buf(), |p, part| p.join(part))
}

/// What an owner can do to `member`: nothing to themselves, nor in a swarm they were removed
/// from; otherwise the two other roles and Remove.
fn member_actions(swarm: &SwarmView, member: &MemberView) -> Vec<MemberAction> {
    let me = member.machines.iter().any(|m| m.this_machine);
    // An owner who isn't junior to you can only be demoted or removed by someone senior.
    let senior = member.role == Role::Owner && !member.junior;
    if swarm.my_role != Role::Owner || swarm.removed || me || senior {
        return Vec::new();
    }
    [Role::Owner, Role::Writer, Role::Reader]
        .into_iter()
        .filter(|r| *r != member.role)
        .map(MemberAction::Make)
        .chain([MemberAction::Remove])
        .collect()
}

/// Whether you are this swarm's only owner, who can't leave until there's another.
fn last_owner(s: &SwarmView) -> bool {
    !s.removed
        && s.my_role == Role::Owner
        && s.members.iter().filter(|m| m.role == Role::Owner).count() <= 1
}

/// A member's machines, with which is this one and when an offline one was last seen.
fn member_detail(member: &MemberView, now: u64) -> String {
    let seen = |at: Option<u64>| match at {
        Some(at) => format!(
            "last seen {}",
            crate::start_page::ago(now.saturating_sub(at))
        ),
        None => "offline".into(),
    };
    if let [only] = member.machines.as_slice()
        && !only.online
        && !only.this_machine
    {
        return format!("{} · {}", only.name, seen(only.last_seen));
    }
    member
        .machines
        .iter()
        .map(|m| {
            if m.this_machine {
                format!("{} (this machine)", m.name)
            } else if m.online {
                m.name.clone()
            } else {
                format!("{} ({})", m.name, seen(m.last_seen))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "Invite for a reader, made 26 Oct, unused, expires 2 Nov".
fn invite_text(inv: &Invite, tz: &TimeZone) -> String {
    format!(
        "Invite for {} {}, made {}, {}, expires {}",
        article(inv.role),
        role_name(inv.role),
        day(inv.created, tz),
        if inv.used { "used" } else { "unused" },
        day(inv.expires, tz)
    )
}

/// The start of an invite code, for showing.
fn shown_code(code: &str) -> String {
    if code.chars().count() <= SHOWN_CODE_CHARS {
        code.to_string()
    } else {
        let start: String = code.chars().take(SHOWN_CODE_CHARS).collect();
        format!("{start}…")
    }
}

/// A name for you, this machine or a swarm: not blank, and at most 200 characters.
fn checked_name(what: &str, typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        Err(format!("{what} can't be empty."))
    } else if name.chars().count() > MAX_NAME_CHARS {
        Err(format!(
            "{what} can be at most {MAX_NAME_CHARS} characters."
        ))
    } else {
        Ok(name.to_string())
    }
}

/// A pasted invite, checked to be one.
fn checked_invite(typed: &str) -> Result<String, String> {
    let code = typed.trim();
    if code.is_empty() {
        return Err("Paste the invite you were sent.".into());
    }
    InviteCode::decode(code)
        .map(|_| code.to_string())
        .ok_or_else(|| "That isn't a swarm invite. Paste all of the hydra-swarm: code.".into())
}

pub(super) fn chosen_folder(typed: &str) -> Result<PathBuf, String> {
    let folder = typed.trim();
    if folder.is_empty() {
        Err("Pick a folder on this machine.".into())
    } else {
        Ok(PathBuf::from(folder))
    }
}

/// `types` with the extension typed added, as `.py`.
fn with_type(types: &[String], typed: &str) -> Result<Vec<String>, String> {
    let ext = typed.trim().trim_start_matches('.').to_ascii_lowercase();
    if ext.is_empty() {
        return Err("Type an extension, like .py".into());
    }
    if !ext
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("An extension is letters and digits, like .py".into());
    }
    let ext = format!(".{ext}");
    if types.iter().any(|t| t.eq_ignore_ascii_case(&ext)) {
        return Err(format!("{ext} already syncs."));
    }
    let mut types = types.to_vec();
    types.push(ext);
    Ok(types)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn zoned(secs: u64, tz: &TimeZone) -> Option<jiff::Zoned> {
    let at = Timestamp::from_second(i64::try_from(secs).ok()?).ok()?;
    Some(at.to_zoned(tz.clone()))
}

/// "26 Oct".
pub(super) fn day(secs: u64, tz: &TimeZone) -> String {
    zoned(secs, tz).map_or_else(String::new, |z| z.strftime("%-d %b").to_string())
}

/// "13:02" today, else "26 Oct 13:02".
fn when(secs: u64, now: u64, tz: &TimeZone) -> String {
    let (Some(at), Some(today)) = (zoned(secs, tz), zoned(now, tz)) else {
        return String::new();
    };
    if at.date() == today.date() {
        at.strftime("%H:%M").to_string()
    } else {
        at.strftime("%-d %b %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydra_swarms::MachineView;
    use hydra_swarms::paths::safe_rel_path;

    fn rel(s: &str) -> hydra_swarms::paths::RelPath {
        safe_rel_path(s).unwrap()
    }

    fn machine(name: &str, online: bool, last_seen: Option<u64>, here: bool) -> MachineView {
        MachineView {
            name: name.into(),
            online,
            last_seen,
            this_machine: here,
        }
    }

    fn member(name: &str, role: Role, machines: Vec<MachineView>) -> MemberView {
        MemberView {
            person: format!("{name}-id"),
            name: name.into(),
            role,
            junior: false,
            machines,
        }
    }

    fn swarm(my_role: Role, members: Vec<MemberView>) -> SwarmView {
        SwarmView {
            id: "ns".into(),
            name: "team-skills".into(),
            folder: PathBuf::from("E:/Shared/team-skills"),
            my_role,
            members,
            types: vec![".md".into()],
            pending_invites: Vec::new(),
            attention: Vec::new(),
            online: 3,
            machines: 5,
            syncing: false,
            last_change: None,
            removed: false,
            my_other_machines: 0,
        }
    }

    fn me(role: Role) -> MemberView {
        member(
            "Davo",
            role,
            vec![machine("Davo's desktop", true, None, true)],
        )
    }

    #[test]
    fn leaving_says_whether_your_other_machines_stay() {
        let mut s = swarm(Role::Writer, vec![me(Role::Writer)]);
        assert_eq!(leave_label(&s), "Leave swarm…");
        s.my_other_machines = 1;
        assert_eq!(leave_label(&s), "Leave on this machine…");
    }

    #[test]
    fn the_status_pill_says_how_the_swarm_is() {
        let mut s = swarm(Role::Writer, vec![me(Role::Writer)]);
        assert_eq!(status_text(&s), "Up to date · 3 of 5 machines online");
        s.online = 1;
        s.machines = 1;
        assert_eq!(status_text(&s), "Up to date · 1 of 1 machine online");
        s.syncing = true;
        assert_eq!(status_text(&s), "Syncing…");
        s.removed = true;
        assert_eq!(status_text(&s), "Removed from this swarm");
    }

    #[test]
    fn the_sidebar_dot_is_amber_with_attention_and_grey_offline_or_removed() {
        let mut s = swarm(Role::Writer, vec![me(Role::Writer)]);
        assert_eq!(swarm_dot(&s), Dot::UpToDate);
        s.attention.push(Attention::TooBig { path: rel("a.md") });
        assert_eq!(swarm_dot(&s), Dot::Attention);
        s.attention.clear();
        s.online = 1;
        assert_eq!(swarm_dot(&s), Dot::Offline, "no other machine online");
        s.machines = 1;
        assert_eq!(swarm_dot(&s), Dot::UpToDate, "alone in it");
        s.attention.push(Attention::TooBig { path: rel("a.md") });
        s.removed = true;
        assert_eq!(swarm_dot(&s), Dot::Offline);
    }

    #[test]
    fn each_attention_item_gets_its_buttons() {
        let conflict = Attention::Conflict {
            path: rel("skills/SKILL.md"),
            copy: rel("skills/SKILL (conflict, Bob's PC, 27 Oct 13.05).md"),
        };
        assert_eq!(
            fixes(&conflict, Role::Writer),
            vec![Fix::Compare, Fix::KeepTheirs, Fix::KeepMine]
        );
        assert_eq!(fixes(&conflict, Role::Owner).len(), 3);
        assert!(fixes(&conflict, Role::Reader).is_empty(), "not for readers");
        let deleted = Attention::EditedAfterDelete {
            path: rel("notes/old.md"),
        };
        assert_eq!(
            fixes(&deleted, Role::Writer),
            vec![Fix::ShareAgain, Fix::DeleteIt]
        );
        let held = Attention::ReaderEdit { path: rel("a.md") };
        assert_eq!(fixes(&held, Role::Reader), vec![Fix::UseSwarmVersion]);
        let orphan = Attention::NoLongerShared {
            path: rel("new.md"),
        };
        assert_eq!(
            fixes(&orphan, Role::Writer),
            vec![Fix::ShareAgain, Fix::DeleteIt]
        );
        assert_eq!(fixes(&orphan, Role::Reader), vec![Fix::DeleteIt]);
        assert_eq!(
            resolution(Fix::ShareAgain, &orphan),
            Some(Resolution::ShareAgain("new.md".into()))
        );
        assert!(fix_warning(Fix::DeleteIt, &orphan).is_some());
        let big = Attention::TooBig {
            path: rel("big.md"),
        };
        assert!(fixes(&big, Role::Owner).is_empty());

        assert_eq!(resolution(Fix::Compare, &conflict), None);
        assert_eq!(
            resolution(Fix::KeepTheirs, &conflict),
            Some(Resolution::KeepTheirs {
                path: "skills/SKILL.md".into(),
                copy: "skills/SKILL (conflict, Bob's PC, 27 Oct 13.05).md".into()
            })
        );
        assert!(matches!(
            resolution(Fix::KeepMine, &conflict),
            Some(Resolution::KeepMine { .. })
        ));
        assert_eq!(
            resolution(Fix::ShareAgain, &deleted),
            Some(Resolution::ShareAgain("notes/old.md".into()))
        );
        assert_eq!(
            resolution(Fix::DeleteIt, &deleted),
            Some(Resolution::DeleteHeld("notes/old.md".into()))
        );
        assert_eq!(
            resolution(Fix::UseSwarmVersion, &held),
            Some(Resolution::UseSwarmVersion("a.md".into()))
        );
        assert_eq!(resolution(Fix::KeepMine, &held), None);
        let (_, _, note) = attention_text(&conflict);
        assert!(note.ends_with("as SKILL (conflict, Bob's PC, 27 Oct 13.05).md."));
    }

    #[test]
    fn buttons_that_throw_a_version_away_ask_first() {
        let conflict = Attention::Conflict {
            path: rel("skills/SKILL.md"),
            copy: rel("skills/SKILL (conflict, Bob's PC, 27 Oct 13.05).md"),
        };
        let deleted = Attention::EditedAfterDelete {
            path: rel("notes/old.md"),
        };
        let held = Attention::ReaderEdit { path: rel("a.md") };
        let (title, body, ok) = fix_warning(Fix::DeleteIt, &deleted).unwrap();
        assert_eq!(title, "Delete notes/old.md?");
        assert_eq!(body, "Your edit is the only copy and can't be recovered.");
        assert_eq!(ok, "Delete");
        let (title, body, _) = fix_warning(Fix::KeepTheirs, &conflict).unwrap();
        assert_eq!(title, "Keep skills/SKILL.md as it is?");
        assert!(body.contains("SKILL (conflict, Bob's PC, 27 Oct 13.05).md, is deleted"));
        let (title, _, _) = fix_warning(Fix::KeepMine, &conflict).unwrap();
        assert_eq!(
            title,
            "Use SKILL (conflict, Bob's PC, 27 Oct 13.05).md for skills/SKILL.md?"
        );
        assert!(fix_warning(Fix::UseSwarmVersion, &held).is_some());
        // Nothing is lost by these.
        assert!(fix_warning(Fix::Compare, &conflict).is_none());
        assert!(fix_warning(Fix::ShareAgain, &deleted).is_none());
    }

    #[test]
    fn compare_shows_files_under_the_folder() {
        let folder = Path::new("root");
        assert_eq!(
            in_folder(folder, "skills/review/SKILL.md"),
            Path::new("root")
                .join("skills")
                .join("review")
                .join("SKILL.md")
        );
    }

    #[test]
    fn owners_can_change_or_remove_other_members_only() {
        let alice = member(
            "Alice",
            Role::Writer,
            vec![machine("Alice's laptop", true, None, false)],
        );
        let s = swarm(Role::Owner, vec![me(Role::Owner), alice.clone()]);
        assert_eq!(
            member_actions(&s, &alice),
            vec![
                MemberAction::Make(Role::Owner),
                MemberAction::Make(Role::Reader),
                MemberAction::Remove
            ]
        );
        assert!(
            member_actions(&s, &me(Role::Owner)).is_empty(),
            "not yourself"
        );
        let writer = swarm(Role::Writer, vec![me(Role::Writer), alice.clone()]);
        assert!(member_actions(&writer, &alice).is_empty());
        let reader = swarm(Role::Reader, vec![me(Role::Reader), alice.clone()]);
        assert!(member_actions(&reader, &alice).is_empty());
        let removed = SwarmView {
            removed: true,
            ..s.clone()
        };
        assert!(member_actions(&removed, &alice).is_empty());
        let bob = MemberView {
            junior: true,
            ..member("Bob", Role::Owner, vec![])
        };
        assert_eq!(
            member_actions(&s, &bob),
            vec![
                MemberAction::Make(Role::Writer),
                MemberAction::Make(Role::Reader),
                MemberAction::Remove
            ]
        );
        // An owner senior to you (or promoted alongside you, earlier) is beyond your reach.
        let carol = member("Carol", Role::Owner, vec![]);
        assert!(member_actions(&s, &carol).is_empty());
        assert_eq!(MemberAction::Remove.label(), "Remove…");
        assert_eq!(MemberAction::Make(Role::Owner).label(), "Make owner");
    }

    #[test]
    fn the_last_owner_cannot_leave() {
        let alice = member("Alice", Role::Writer, vec![]);
        let s = swarm(Role::Owner, vec![me(Role::Owner), alice]);
        assert!(last_owner(&s));
        let two = swarm(
            Role::Owner,
            vec![me(Role::Owner), member("Bob", Role::Owner, vec![])],
        );
        assert!(!last_owner(&two));
        assert!(!last_owner(&swarm(Role::Writer, vec![me(Role::Writer)])));
        let removed = SwarmView { removed: true, ..s };
        assert!(!last_owner(&removed), "a removed swarm can always be left");
    }

    #[test]
    fn names_are_not_blank_and_not_too_long() {
        assert_eq!(checked_name("Your name", "  Davo "), Ok("Davo".into()));
        assert_eq!(
            checked_name("Your name", "   "),
            Err("Your name can't be empty.".into())
        );
        assert!(checked_name("Your name", "").is_err());
        let long = "é".repeat(200);
        assert_eq!(checked_name("x", &long), Ok(long.clone()));
        assert_eq!(
            checked_name("The swarm's name", &format!("{long}a")),
            Err("The swarm's name can be at most 200 characters.".into())
        );
    }

    #[test]
    fn a_pasted_invite_must_be_one() {
        assert!(checked_invite("").is_err());
        assert!(checked_invite("hello").is_err());
        assert!(checked_invite("hydra-swarm:not-base64!").is_err());
        let code = InviteCode {
            host: iroh_like_id(),
            relay: None,
            token: "a".repeat(64),
        }
        .encode();
        assert_eq!(checked_invite(&format!("  {code}\n")), Ok(code));
    }

    /// An endpoint id, as an invite names its host: the public key for the secret key of all 7s.
    fn iroh_like_id() -> String {
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c".into()
    }

    #[test]
    fn file_types_are_added_as_extensions() {
        let md = vec![".md".to_string()];
        assert_eq!(with_type(&md, "py"), Ok(vec![".md".into(), ".py".into()]));
        assert_eq!(
            with_type(&md, " .PY "),
            Ok(vec![".md".into(), ".py".into()])
        );
        assert_eq!(with_type(&md, ".MD"), Err(".md already syncs.".into()));
        assert!(with_type(&md, "").is_err());
        assert!(with_type(&md, ".").is_err());
        assert!(with_type(&md, "a b").is_err());
        assert!(with_type(&md, "a/b").is_err());
    }

    #[test]
    fn members_show_their_machines_and_when_they_were_seen() {
        let now = 10 * 86400;
        let carol = member(
            "Carol",
            Role::Writer,
            vec![machine("Carol's Mac", false, Some(now - 86400), false)],
        );
        assert_eq!(
            member_detail(&carol, now),
            "Carol's Mac · last seen 1 day ago"
        );
        let davo = member(
            "Davo",
            Role::Owner,
            vec![
                machine("Davo's desktop", true, None, true),
                machine("Davo's laptop", false, Some(now - 3 * 86400), false),
                machine("Old box", false, None, false),
            ],
        );
        assert_eq!(
            member_detail(&davo, now),
            "Davo's desktop (this machine), Davo's laptop (last seen 3 days ago), Old box (offline)"
        );
        let alice = member(
            "Alice",
            Role::Writer,
            vec![machine("Alice's laptop", true, None, false)],
        );
        assert_eq!(member_detail(&alice, now), "Alice's laptop");
    }

    #[test]
    fn times_read_as_the_mockup_has_them() {
        let tz = TimeZone::UTC;
        // 27 Oct 2026 13:02 UTC.
        let at = 1_793_106_120;
        assert_eq!(when(at, at + 60, &tz), "13:02");
        assert_eq!(when(at, at + 2 * 86400, &tz), "27 Oct 13:02");
        assert_eq!(day(at, &tz), "27 Oct");
        let inv = Invite {
            id: "x".into(),
            role: Role::Reader,
            created: at - 86400,
            expires: at + 6 * 86400,
            used: false,
        };
        assert_eq!(
            invite_text(&inv, &tz),
            "Invite for a reader, made 26 Oct, unused, expires 2 Nov"
        );
    }

    #[test]
    fn invite_codes_are_shortened_for_showing_only() {
        assert_eq!(shown_code("hydra-swarm:abc"), "hydra-swarm:abc");
        let long = format!("hydra-swarm:{}", "x".repeat(100));
        let shown = shown_code(&long);
        assert!(shown.ends_with('…'));
        assert_eq!(shown.chars().count(), SHOWN_CODE_CHARS + 1);
        assert!(long.starts_with(shown.trim_end_matches('…')));
    }

    #[test]
    fn roles_read_as_words() {
        assert_eq!(capitalised(role_name(Role::Writer)), "Writer");
        assert_eq!(
            format!("You're {} {}", article(Role::Owner), role_name(Role::Owner)),
            "You're an owner"
        );
        assert_eq!(file_name("a/b/c.md"), "c.md");
        assert_eq!(file_name("c.md"), "c.md");
    }
}
