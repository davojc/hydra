//! Settings › Phone: turn the phone link on, pair a phone with a QR code, unpair it.
use std::time::Duration;

use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, Hsla, Task, Window, div, px, rgb};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use hydra_link::pairing::PairingOffer;
use qrcode::{Color, QrCode};

use super::Workspace;
use super::settings_tab::{group, page_title};
use crate::action_log;
use crate::link::store;

/// How long a pairing code works (the host's offer lifetime).
const OFFER_SECS: u32 = 120;
/// Width of a QR module on screen, and the blank border round the code, in modules.
const MODULE: f32 = 5.0;
const QUIET: usize = 4;

/// The QR code for `text` as rows of cells, `true` where a module is dark; empty when `text`
/// is too long for a code.
pub(super) fn qr_cells(text: &str) -> Vec<Vec<bool>> {
    let Ok(code) = QrCode::new(text.as_bytes()) else {
        return Vec::new();
    };
    let side = code.width();
    code.to_colors()
        .chunks(side)
        .map(|row| row.iter().map(|c| *c == Color::Dark).collect())
        .collect()
}

/// `cells` with the quiet zone round it, as runs of equal modules per row.
fn runs(cells: &[Vec<bool>]) -> Vec<Vec<(bool, usize)>> {
    let side = cells.len() + 2 * QUIET;
    let blank = vec![false; side];
    let padded = |row: &[bool]| {
        let mut r = vec![false; QUIET];
        r.extend_from_slice(row);
        r.extend(vec![false; QUIET]);
        r
    };
    let rows = std::iter::repeat_n(blank.clone(), QUIET)
        .chain(cells.iter().map(|r| padded(r)))
        .chain(std::iter::repeat_n(blank, QUIET));
    rows.map(|row| {
        let mut out: Vec<(bool, usize)> = Vec::new();
        for dark in row {
            match out.last_mut() {
                Some((d, n)) if *d == dark => *n += 1,
                _ => out.push((dark, 1)),
            }
        }
        out
    })
    .collect()
}

/// A pairing code on show, counting down.
pub(super) struct ShownOffer {
    code: String,
    cells: Vec<Vec<bool>>,
    left: u32,
    _tick: Task<()>,
}

/// `m:ss`.
fn clock(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl Workspace {
    /// Turns the phone link on or off: saved to config.toml off the UI thread, and the link
    /// started or stopped here. Stopping first also ends a wait for a link another window holds.
    /// The link reads the setting back from config.toml when it starts, so it starts only once
    /// the setting is saved.
    fn phone_toggle(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.phone_access = on;
        self.phone_stop();
        let seek = self.phone_seek;
        let paths = self.hydra.paths.clone();
        let saved = cx.background_spawn(async move { store::set_enabled(&paths, on) });
        cx.spawn_in(window, async move |this, cx| {
            let saved = saved.await;
            let _ = this.update_in(cx, |ws, window, cx| {
                if let Err(e) = saved {
                    phone_unsaved("phone access", &e, window, cx);
                }
                // Not when the switch was flipped again meanwhile: that flip starts it.
                if ws.phone_seek == seek {
                    ws.phone_start(window, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Tells the user when a setting's write failed.
    fn phone_saved(
        &self,
        saved: Task<Result<(), String>>,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = saved.await {
                let _ = cx.update(|window, cx| phone_unsaved(what, &e, window, cx));
            }
        })
        .detach();
    }

    /// Asks the host for a fresh offer and shows it.
    fn phone_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = oneshot::channel();
        self.phone_offer_job(tx);
        self.phone_word = None;
        cx.spawn_in(window, async move |this, cx| {
            let Ok(offer) = rx.await else { return };
            let _ = this.update_in(cx, |ws, window, cx| ws.phone_show_offer(offer, window, cx));
        })
        .detach();
    }

    fn phone_show_offer(
        &mut self,
        offer: PairingOffer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let code = offer.encode();
        let tick = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(over) = this.update(cx, |ws, cx| {
                    let over = match &mut ws.phone_offer {
                        Some(shown) => {
                            shown.left = shown.left.saturating_sub(1);
                            shown.left == 0
                        }
                        None => true,
                    };
                    if over {
                        ws.phone_offer = None;
                    }
                    cx.notify();
                    over
                }) else {
                    return;
                };
                if over {
                    return;
                }
            }
        });
        self.phone_offer = Some(ShownOffer {
            cells: qr_cells(&code),
            code,
            left: OFFER_SECS,
            _tick: tick,
        });
        cx.notify();
    }

    /// Asks before forgetting the paired phone.
    fn phone_unpair_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let ws = workspace.clone();
            dialog
                .title("Unpair this phone?")
                .child("It will need to be paired again before it can connect.")
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
                            Button::new("unpair-confirm")
                                .label("Unpair")
                                .danger()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    ws.update(cx, |ws, cx| ws.phone_unpair(window, cx));
                                }),
                        ),
                )
        });
    }

    fn phone_unpair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.phone_unpair_job();
        self.ui.phone = None;
        self.phone_connected = false;
        self.phone_word = None;
        self.phone_offer = None;
        let paths = self.hydra.paths.clone();
        let saved = cx.background_spawn(async move { store::set_paired(&paths, None) });
        self.phone_saved(saved, "the unpaired phone", window, cx);
        cx.notify();
    }

    pub(super) fn phone_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
        let here = self.phone.is_some();
        let on = self.ui.phone_access;
        let note = |text: &str| {
            div()
                .text_sm()
                .text_color(muted)
                .child(text.to_string())
                .into_any_element()
        };

        let switch = Switch::new("phone-access")
            .checked(on)
            .small()
            .label("Let my phone connect")
            .on_click(cx.listener(|ws, on: &bool, window, cx| ws.phone_toggle(*on, window, cx)))
            .into_any_element();
        let mut rows = vec![switch];
        if here {
            rows.push(note("Phone connects to this window"));
        } else if on && self.phone_seeking {
            rows.push(note("Phone connects to another hydraui window"));
        }

        if on {
            if let Some(phone) = &self.ui.phone {
                let short: String = phone.chars().take(10).collect();
                let state = if self.phone_connected {
                    "Connected"
                } else {
                    "Not connected"
                };
                let control = h_flex()
                    .gap_3()
                    .child(div().font_family("monospace").child(short))
                    .child(note(state))
                    .when(here, |d| {
                        d.child(
                            Button::new("phone-unpair")
                                .label("Unpair")
                                .small()
                                .on_click(cx.listener(|ws, _, window, cx| {
                                    ws.phone_unpair_dialog(window, cx)
                                })),
                        )
                    })
                    .into_any_element();
                rows.push(setting_row("Paired phone", control));
            }
            if here {
                rows.push(self.pair_row(border, muted, cx));
            } else if self.phone_seeking {
                rows.push(note("Pair or unpair from the window that holds the link."));
            }
        }
        v_flex()
            .gap_5()
            .child(page_title(
                "Phone",
                "Watch your terminals and boards from the Hydra app on your phone.".into(),
                div(),
                cx,
            ))
            .child(group("Phone link", None, rows, cx))
            .into_any_element()
    }

    /// The pairing controls: the button, then the code with its countdown or the word to check.
    fn pair_row(&self, border: Hsla, muted: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let button = Button::new("phone-pair")
            .label("Pair a phone")
            .small()
            .on_click(cx.listener(|ws, _, window, cx| ws.phone_pair(window, cx)));
        let mut col = v_flex().gap_3().items_start().child(button);
        if let Some(word) = &self.phone_word {
            col = col.child(div().child(format!("Paired. Check your phone shows {word}.")));
        }
        if let Some(shown) = &self.phone_offer {
            let code = shown.code.clone();
            let qr = v_flex().flex_none().bg(rgb(0xffffff)).children(
                runs(&shown.cells).into_iter().map(|row| {
                    h_flex().children(row.into_iter().map(|(dark, n)| {
                        div()
                            .w(px(MODULE * n as f32))
                            .h(px(MODULE))
                            .bg(rgb(if dark { 0x000000 } else { 0xffffff }))
                    }))
                }),
            );
            col = col
                .child(div().p_1().border_1().border_color(border).child(qr))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("Scan this with the Hydra app. It expires in 2 minutes."),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().font_family("monospace").child(clock(shown.left)))
                        .child(
                            Button::new("phone-copy-code")
                                .label("Copy pairing code")
                                .small()
                                .ghost()
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()))
                                }),
                        ),
                );
        }
        col.into_any_element()
    }
}

/// Logs and shows that saving `what` failed with `e`.
fn phone_unsaved(what: &str, e: &str, window: &mut Window, cx: &mut gpui::App) {
    action_log::write(format!("phone: couldn't save {what}: {e}"));
    window.push_notification(
        Notification::warning(format!("Couldn't save {what}: {e}")).title("Phone"),
        cx,
    );
}

fn setting_row(label: &'static str, control: AnyElement) -> AnyElement {
    h_flex()
        .gap_4()
        .child(div().w(px(160.)).flex_none().child(label))
        .child(div().flex_1().min_w_0().child(control))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> String {
        PairingOffer::new("a".repeat(52), Some("https://relay.example/".into())).encode()
    }

    #[test]
    fn qr_grid_matches_the_offer() {
        let text = text();
        let cells = qr_cells(&text);
        let side = QrCode::new(text.as_bytes()).unwrap().width();
        assert_eq!(cells.len(), side);
        assert!(cells.iter().all(|row| row.len() == side));
        assert!(cells.iter().flatten().any(|&c| c), "has dark modules");
    }

    #[test]
    fn the_quiet_zone_surrounds_the_code() {
        let cells = qr_cells(&text());
        let side = cells.len();
        let rows = runs(&cells);
        assert_eq!(rows.len(), side + 2 * QUIET);
        for row in &rows {
            assert_eq!(row.iter().map(|(_, n)| n).sum::<usize>(), side + 2 * QUIET);
        }
        assert_eq!(rows[0], vec![(false, side + 2 * QUIET)]);
        assert!(!rows[QUIET][0].0 && rows[QUIET][0].1 >= QUIET);
    }

    #[test]
    fn the_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(clock(120), "2:00");
        assert_eq!(clock(61), "1:01");
        assert_eq!(clock(0), "0:00");
    }
}
