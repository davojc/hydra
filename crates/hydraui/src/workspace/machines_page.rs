//! You and this machine's part for your other machines (spec §3, §6): the machines linked to
//! you, linking another, the swarms offered to this one, and removing one.
use std::cell::Cell;
use std::rc::Rc;

use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, Entity, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use hydra_swarms::mine::LinkCode;
use hydra_swarms::wire::Offer;
use hydra_swarms::{Command, DeviceView, Invite};
use jiff::tz::TimeZone;

use super::Workspace;
use super::settings_tab::group;
use super::swarms_page::{
    ELSEWHERE, badge, chosen_folder, day, dot, folder_row, labelled, role_name,
};

/// The Add another machine dialog's state.
struct LinkState {
    code: Option<String>,
    problem: Option<String>,
    asking: bool,
}

impl Workspace {
    /// Your machines: each with whether it's online and Remove…, the link codes waiting to be
    /// used, and Add another machine of mine….
    pub(super) fn your_machines(&self, cx: &mut Context<Self>) -> AnyElement {
        let (ok, muted) = (cx.theme().success, cx.theme().muted_foreground);
        let tz = TimeZone::system();
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(m) = &self.swarm_machines {
            for (i, d) in m.devices.iter().enumerate() {
                let remove = (!d.this_machine).then(|| {
                    let (id, name) = (d.id.clone(), d.name.clone());
                    Button::new(SharedString::from(format!("machine-remove-{i}")))
                        .label("Remove…")
                        .small()
                        .danger()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.remove_machine(id.clone(), name.clone(), window, cx)
                        }))
                });
                rows.push(
                    h_flex()
                        .gap_3()
                        .child(dot(if d.online { ok } else { muted }))
                        .child(
                            div().child(d.name.clone()).child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(format!("· {}", device_detail(d))),
                            ),
                        )
                        .child(div().flex_1())
                        .when_some(remove, |r, b| r.child(b))
                        .into_any_element(),
                );
            }
            for link in &m.links {
                let id = link.id.clone();
                rows.push(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .text_color(muted)
                        .child(div().flex_1().child(link_text(link, &tz)))
                        .child(
                            Button::new(SharedString::from(format!("link-cancel-{id}")))
                                .label("Cancel")
                                .small()
                                .ghost()
                                .on_click(cx.listener(move |ws, _, _, _| {
                                    ws.swarms_command(Command::CancelLink { id: id.clone() })
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        let add = Button::new("machine-add")
            .label("Add another machine of mine…")
            .small()
            .on_click(cx.listener(|ws, _, window, cx| ws.add_machine_dialog(window, cx)));
        let link = self.may_link_here().then(|| {
            Button::new("machine-link")
                .label("Link this machine to one of yours…")
                .small()
                .ghost()
                .on_click(cx.listener(|ws, _, window, cx| ws.link_machine_dialog(window, cx)))
        });
        rows.push(
            h_flex()
                .gap_2()
                .child(add)
                .when_some(link, |d, b| d.child(b))
                .into_any_element(),
        );
        group("Your machines", None, rows, cx)
    }

    /// Whether this machine may be linked to another of yours: in no swarm, and linked to none.
    pub(super) fn may_link_here(&self) -> bool {
        self.swarms.is_empty()
            && self
                .swarm_machines
                .as_ref()
                .is_none_or(|m| m.devices.len() <= 1)
    }

    /// The swarms offered to this machine, each with Choose folder… and Not on this machine.
    pub(super) fn offered_here(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.swarm_machines.as_ref()?;
        if m.offers.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let rows = m
            .offers
            .iter()
            .enumerate()
            .map(|(i, offer)| {
                let from = m
                    .devices
                    .iter()
                    .find(|d| d.id == offer.from)
                    .map_or("another machine of yours", |d| d.name.as_str());
                let (take, skip) = (offer.clone(), offer.ns.clone());
                h_flex()
                    .gap_3()
                    .child(offer.name.clone())
                    .child(badge(role_name(offer.role).to_string(), offer.role, cx))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("from {from}")),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(SharedString::from(format!("offer-take-{i}")))
                            .label("Choose folder…")
                            .small()
                            .primary()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.accept_offer_dialog(take.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("offer-skip-{i}")))
                            .label("Not on this machine")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |ws, _, _, _| {
                                ws.swarms_command(Command::DeclineOffer {
                                    swarm: skip.clone(),
                                })
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        Some(group("Offered to this machine", None, rows, cx))
    }

    fn remove_machine(
        &mut self,
        device: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.swarms_confirm(
            format!("Remove {name}?"),
            "It's shut out of the swarms your other machines are in too, and they get a new key \
             for you. A swarm only it is in keeps it until an owner there removes it. It keeps \
             the files it already has. This can't be undone: it can't be linked to you again."
                .into(),
            "Remove",
            true,
            move |ws, _, _| {
                ws.swarms_command(Command::RemoveMachine {
                    device: device.clone(),
                })
            },
            window,
            cx,
        );
    }

    /// Add another machine of mine…: makes a link code to copy, with the names first when they
    /// aren't set (the swarms start for it if they aren't running yet).
    pub(super) fn add_machine_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.new(|_| LinkState {
            code: None,
            problem: None,
            asking: false,
        });
        let ident = Rc::new(
            self.swarms_need_identity()
                .then(|| self.identity_fields(window, cx)),
        );
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (muted, danger, border) = (
                cx.theme().muted_foreground,
                cx.theme().danger,
                cx.theme().border,
            );
            let shown = state.read(cx);
            let (code, problem, asking) = (shown.code.clone(), shown.problem.clone(), shown.asking);
            let body = v_flex()
                .gap_3()
                .child(div().text_sm().child(
                    "On your other machine, open Settings › Swarms › Link this machine… and paste \
                     this code. It becomes yours: your name, and the swarms you're in are offered \
                     to it.",
                ))
                .when(code.is_none(), |d| {
                    d.when_some(ident.as_ref().as_ref(), |d, fields| {
                        d.child(fields.render(cx))
                    })
                })
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
                            .child(code),
                    )
                })
                .when_some(problem, |d, e| {
                    d.child(div().text_sm().text_color(danger).child(e))
                });
            let action = match code {
                Some(code) => Button::new("link-copy")
                    .label("Copy code")
                    .primary()
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()))
                    }),
                None => {
                    let (s, ws, i) = (state.clone(), workspace.clone(), ident.clone());
                    Button::new("link-make")
                        .label("Make code")
                        .primary()
                        .loading(asking)
                        .on_click(move |_, window, cx| {
                            let identity = match i.as_ref() {
                                Some(fields) => match fields.read(cx) {
                                    Ok(names) => Some(names),
                                    Err(e) => {
                                        s.update(cx, |s, cx| {
                                            s.problem = Some(e);
                                            cx.notify();
                                        });
                                        return;
                                    }
                                },
                                None => None,
                            };
                            let s = s.clone();
                            ws.update(cx, |ws, cx| ws.make_link_code(identity, s, window, cx));
                        })
                }
            };
            dialog
                .title("Add another machine of mine")
                .w(px(560.))
                .child(body)
                .footer(h_flex().gap_3().w_full().child(action).child(
                    div().flex_1().text_sm().text_color(muted).child(
                        "Works once · expires in 7 days · this hydraui must be running when \
                             your other machine uses it",
                    ),
                ))
        });
    }

    /// Asks the swarms for a link code and shows it.
    fn make_link_code(
        &mut self,
        identity: Option<(String, String)>,
        state: Entity<LinkState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        state.update(cx, |s, cx| {
            s.asking = true;
            s.problem = None;
            cx.notify();
        });
        let (reply, answer) = oneshot::channel();
        let elsewhere = self.swarms_elsewhere;
        self.swarms_make_or_join(identity, Command::LinkCode { reply }, window, cx);
        cx.spawn(async move |_, cx| {
            let answer = answer.await;
            state.update(cx, |s, cx| {
                s.asking = false;
                match answer {
                    Ok(Ok(code)) => s.code = Some(code),
                    Ok(Err(e)) => s.problem = Some(e),
                    Err(_) if elsewhere => {
                        s.problem = Some(format!("{ELSEWHERE}: add your machine from there."))
                    }
                    Err(_) => {
                        s.problem = Some("The swarms stopped before the code was made.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Link this machine to one of yours…: the code, and the names first when they aren't set.
    pub(super) fn link_machine_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("hydra-link:…"));
        let ident = Rc::new(
            self.swarms_need_identity()
                .then(|| self.identity_fields(window, cx)),
        );
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                code.update(cx, |s, cx| s.focus(window, cx));
            }
            let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
            let shown = problem.read(cx).clone();
            let (c, p, i, ws) = (
                code.clone(),
                problem.clone(),
                ident.clone(),
                workspace.clone(),
            );
            let body = v_flex()
                .gap_3()
                .child(labelled(
                    "LINK CODE",
                    div().font_family("monospace").child(Input::new(&code)),
                    cx,
                ))
                .when_some(ident.as_ref().as_ref(), |d, fields| {
                    d.child(fields.render(cx))
                })
                .child(div().text_sm().text_color(muted).child(
                    "This machine takes on your name from your other machine, and the swarms \
                     you're in are offered to it. It has to be in no swarm yet.",
                ))
                .when_some(shown, |d, e| {
                    d.child(div().text_sm().text_color(danger).child(e))
                });
            dialog
                .title("Link this machine to one of yours")
                .w(px(560.))
                .child(body)
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("link").label("Link").primary().on_click(
                            move |_, window, cx| {
                                let checked = (|| {
                                    let code = checked_link(&c.read(cx).value())?;
                                    let identity = match i.as_ref() {
                                        Some(fields) => Some(fields.read(cx)?),
                                        None => None,
                                    };
                                    Ok::<_, String>((identity, Command::Link { code }))
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

    /// Choose folder…: where on this machine the offered swarm goes.
    fn accept_offer_dialog(&mut self, offer: Offer, window: &mut Window, cx: &mut Context<Self>) {
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("Folder on this machine"));
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
            let shown = problem.read(cx).clone();
            let (f, p, ws, ns) = (
                folder.clone(),
                problem.clone(),
                workspace.clone(),
                offer.ns.clone(),
            );
            let body = v_flex()
                .gap_3()
                .child(labelled(
                    "FOLDER ON THIS MACHINE",
                    folder_row(&workspace, &folder, "offer-folder"),
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
            dialog
                .title(format!("Add {} to this machine", offer.name))
                .w(px(560.))
                .child(body)
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("offer-join").label("Join").primary().on_click(
                            move |_, window, cx| match chosen_folder(&f.read(cx).value()) {
                                Ok(folder) => {
                                    window.close_dialog(cx);
                                    let swarm = ns.clone();
                                    ws.update(cx, |ws, _| {
                                        ws.swarms_command(Command::AcceptOffer { swarm, folder })
                                    });
                                }
                                Err(e) => p.update(cx, |p, cx| {
                                    *p = Some(e);
                                    cx.notify();
                                }),
                            },
                        )),
                )
        });
    }
}

/// A pasted link code, checked.
fn checked_link(typed: &str) -> Result<String, String> {
    let code = typed.trim();
    if code.is_empty() {
        return Err("Paste the link code from your other machine.".into());
    }
    LinkCode::decode(code)
        .map(|_| code.to_string())
        .ok_or_else(|| "That isn't a link code. It starts with hydra-link:".into())
}

/// Where one of your machines is: here, online or offline.
fn device_detail(d: &DeviceView) -> &'static str {
    if d.this_machine {
        "this machine"
    } else if d.online {
        "online"
    } else {
        "offline"
    }
}

/// A link code waiting to be used, as an invite is shown.
fn link_text(link: &Invite<()>, tz: &TimeZone) -> String {
    format!(
        "Link code made {}, {}, expires {}",
        day(link.created, tz),
        if link.used { "used" } else { "unused" },
        day(link.expires, tz)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, online: bool, this_machine: bool) -> DeviceView {
        DeviceView {
            id: format!("id-{name}"),
            name: name.into(),
            online,
            this_machine,
        }
    }

    #[test]
    fn a_link_code_is_checked_before_it_is_sent() {
        let host = hydra_swarms::ids::PersonKey::generate().id().0.to_string();
        let code = hydra_swarms::mine::LinkCode {
            host,
            relay: None,
            token: "ab".repeat(32),
        }
        .encode();
        assert_eq!(checked_link(&format!("  {code}\n")), Ok(code.clone()));
        let invite = code.replacen("hydra-link:", "hydra-swarm:", 1);
        let not_a_link = Err("That isn't a link code. It starts with hydra-link:".to_string());
        assert_eq!(checked_link(&invite), not_a_link);
        assert_eq!(checked_link("hydra-link:junk"), not_a_link);
        assert_eq!(
            checked_link("   "),
            Err("Paste the link code from your other machine.".to_string())
        );
    }

    #[test]
    fn a_machine_says_where_it_is() {
        assert_eq!(device_detail(&device("desk", true, true)), "this machine");
        assert_eq!(device_detail(&device("laptop", true, false)), "online");
        assert_eq!(device_detail(&device("laptop", false, false)), "offline");
    }

    #[test]
    fn a_link_code_reads_like_an_invite() {
        let tz = jiff::tz::TimeZone::UTC;
        let link = Invite {
            id: "x".into(),
            role: (),
            created: 1_761_523_200,
            expires: 1_761_523_200 + 7 * 24 * 3600,
            used: false,
        };
        assert_eq!(
            link_text(&link, &tz),
            "Link code made 27 Oct, unused, expires 3 Nov"
        );
    }
}
