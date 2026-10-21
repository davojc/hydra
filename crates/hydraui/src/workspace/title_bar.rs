//! hydraui's own title bar (spec §7.11): the layout's name and its menu, the Shortcuts (help
//! icon) and Update buttons, and the window controls gpui-component draws.
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Image, ImageFormat, Window, div, img, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, Sizable, TitleBar, WindowExt, h_flex};
use gpui_kit_assets::IconName;

use super::Workspace;
use crate::updater::{self, UpdateState};

/// hydra's icon, drawn small (assets/make_icon.py).
static ICON: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../../../assets/hydra-64.png").to_vec(),
    ))
});

/// How often to ask GitHub again.
const EVERY: Duration = Duration::from_secs(60 * 60);

impl Workspace {
    /// Asks GitHub now and every hour, off the UI thread; failures say nothing. A newer
    /// release replaces an older one on the button.
    pub(super) fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        self._update_checks = Some(cx.spawn(async move |this, cx| {
            loop {
                let found = cx
                    .background_spawn(async { updater::check(&hydra_update::latest_url()) })
                    .await;
                let alive = this.update(cx, |ws, cx| {
                    if let Ok(Some(version)) = found
                        && ws.update.found(version)
                    {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(EVERY).await;
            }
        }));
    }

    /// "Check for updates": asks GitHub now and says what it found; a release found shows
    /// on the Update button as the hourly check's would.
    pub(super) fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async { updater::check(&hydra_update::latest_url()) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                let note = match found {
                    Ok(found) => {
                        if let Some(version) = found {
                            ws.update.found(version);
                        }
                        Notification::info(ws.update.note(crate::version::VERSION))
                    }
                    Err(e) => Notification::error(format!("can't reach GitHub: {e:#}")),
                };
                window.push_notification(note.title("Check for updates"), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Updates both exes off the UI thread; the button shows the progress.
    pub(super) fn start_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Offered(version) = self.update.clone() else {
            return;
        };
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        self.update = UpdateState::Updating(version.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let hydra = updater::find_hydra(
                        &exe,
                        std::env::var_os("PATH").as_deref(),
                        &updater::is_our_hydra,
                    );
                    let result = updater::install(
                        &hydra_update::latest_url(),
                        &exe,
                        hydra.as_deref(),
                        &hydra_update::running_hydra_pids,
                    );
                    if let Ok(done) = &result {
                        updater::record_installed_version(&exe, &done.version);
                    }
                    result
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                match result {
                    Ok(done) => {
                        let mut text = format!("Updated to {}", done.version);
                        if !done.hydra_updated {
                            text.push_str(" (hydraui only: no hydra.exe found)");
                        }
                        window.push_notification(
                            Notification::success(text).title("Update hydra"),
                            cx,
                        );
                        ws.update = UpdateState::Ready(done.version);
                    }
                    Err(e) => {
                        window.push_notification(
                            Notification::error(format!("{e:#}")).title("Update hydra"),
                            cx,
                        );
                        ws.update = UpdateState::Offered(version);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the layout (an untitled window first gets a file, so nothing is lost), lets go
    /// of it, starts the new hydraui with it and closes this window (spec §7.11); unsaved
    /// edits are asked about first, as when closing.
    pub(super) fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.is_none() && !self.envs.is_empty() {
            let dir = hydraui_sets::layouts_dir(&self.hydra.paths);
            let path = hydraui_sets::free_path(&dir, "untitled");
            if !self.save_layout_to(path, window, cx) {
                return;
            }
        }
        self.autosave(window, cx);
        let file = self.layout.clone();
        let all: Vec<_> = self.files.keys().copied().collect();
        if self.unsaved_in(&all, cx).is_empty() {
            Self::relaunch(self, file.as_deref(), window, cx);
        } else {
            self.confirm_unsaved(all, window, cx, move |ws, w, cx| {
                Self::relaunch(ws, file.as_deref(), w, cx)
            });
        }
    }

    fn relaunch(&mut self, file: Option<&Path>, window: &mut Window, cx: &mut gpui::App) {
        // Let go first, or the new hydraui sees the file taken and gives way to this window.
        self.claim = None;
        match crate::window_layout::spawn_hydraui(file) {
            Ok(()) => window.remove_window(),
            Err(e) => window.push_notification(
                Notification::error(format!("can't start the new hydraui: {e}"))
                    .title("Update hydra"),
                cx,
            ),
        }
    }

    pub(super) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let button = match &self.update {
            UpdateState::Idle => None,
            UpdateState::Offered(v) => Some(
                Button::new("update-hydra")
                    .label(format!("Update hydra {v}"))
                    .primary()
                    .xsmall()
                    .on_click(cx.listener(|ws, _, window, cx| ws.start_update(window, cx))),
            ),
            UpdateState::Updating(_) => Some(
                Button::new("update-hydra")
                    .label("Updating…")
                    .xsmall()
                    .disabled(true),
            ),
            UpdateState::Ready(_) => Some(
                Button::new("update-hydra")
                    .label("Restart to finish")
                    .primary()
                    .xsmall()
                    .on_click(cx.listener(|ws, _, window, cx| ws.restart(window, cx))),
            ),
        };
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .child(img(ICON.clone()).size(px(18.)))
                        .children(self.render_layout_name(muted, cx)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        // Occluded, as the Update button below, so a click isn't a drag.
                        .child(
                            div().occlude().child(
                                Button::new("shortcuts")
                                    .icon(IconName::CircleQuestionMark)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Keyboard shortcuts")
                                    .on_click(
                                        cx.listener(|ws, _, window, cx| ws.shortcuts(window, cx)),
                                    ),
                            ),
                        )
                        // The running version, muted, beside where the Update button appears.
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("hydra {}", crate::version::VERSION)),
                        )
                        // The title bar's drag area covers its children, and Windows turns a
                        // click there into a window drag before the button sees it: occluding
                        // stops the hit test at the button, so its clicks stay clicks.
                        .children(button.map(|b| div().occlude().child(b))),
                ),
        )
    }

    /// The layout's name with ▾ and its menu: Save layout as…, Open layout file…, Recent,
    /// Reveal in Explorer (spec §7.11). Muted while untitled; hidden on the start page.
    fn render_layout_name(&self, muted: gpui::Hsla, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.is_start_page() {
            return None;
        }
        let untitled = self.layout.is_none();
        let label = match &self.layout {
            Some(p) => hydraui_sets::layout_name(p),
            None => "untitled".into(),
        };
        let ws = cx.entity().downgrade();
        let current = self.layout.clone();
        let others: Vec<std::path::PathBuf> = hydraui_sets::read_recent(&self.hydra.paths)
            .into_iter()
            .map(|e| e.path)
            .filter(|p| {
                !current
                    .as_deref()
                    .is_some_and(|c| hydraui_sets::same_path(c, p))
            })
            .collect();
        let button = Button::new("layout-name")
            .label(format!("{label} ▾"))
            .ghost()
            .xsmall()
            .when(untitled, |b| {
                b.text_color(muted)
                    .tooltip("Not saved: Save layout as… to keep this window")
            })
            .dropdown_menu(move |menu, window, cx| {
                let (save, open) = (ws.clone(), ws.clone());
                let mut menu = menu
                    .item(
                        PopupMenuItem::new("Save layout as…").on_click(move |_, window, cx| {
                            let _ = save.update(cx, |ws, cx| ws.save_layout_file(window, cx));
                        }),
                    )
                    .item(PopupMenuItem::new("Open layout file…").on_click(
                        move |_, window, cx| {
                            let _ = open.update(cx, |ws, cx| ws.open_layout_file(window, cx));
                        },
                    ));
                if !others.is_empty() {
                    let (ws, others) = (ws.clone(), others.clone());
                    menu = menu.submenu("Recent", window, cx, move |sub, _, _| {
                        others.iter().fold(sub, |sub, p| {
                            let (ws, p) = (ws.clone(), p.clone());
                            sub.item(PopupMenuItem::new(hydraui_sets::layout_name(&p)).on_click(
                                move |_, window, cx| {
                                    let _ = ws.update(cx, |ws, cx| {
                                        ws.open_layout_path(p.clone(), window, cx)
                                    });
                                },
                            ))
                        })
                    });
                }
                if let Some(c) = current.clone() {
                    menu = menu.item(PopupMenuItem::new("Reveal in Explorer").on_click(
                        move |_, _, _| {
                            let _ = std::process::Command::new("explorer.exe")
                                .arg(format!("/select,{}", c.display()))
                                .spawn();
                        },
                    ));
                }
                menu
            });
        // Occluded so a click opens the menu instead of dragging the window (as the Update
        // button).
        Some(div().occlude().child(button))
    }
}
