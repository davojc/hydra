//! hydraui's own title bar (spec §7.11): the title, the Update button, and the window
//! controls gpui-component draws.
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Image, ImageFormat, Window, div, img, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, Sizable, TitleBar, WindowExt, h_flex};

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
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

impl Workspace {
    /// Asks GitHub now and every 6 hours, off the UI thread; failures say nothing.
    pub(super) fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        self._update_checks = Some(cx.spawn(async move |this, cx| {
            loop {
                let found = cx
                    .background_spawn(async { updater::check(&hydra_update::latest_url()) })
                    .await;
                let alive = this.update(cx, |ws, cx| {
                    if let Ok(Some(version)) = found
                        && ws.update == UpdateState::Idle
                    {
                        ws.update = UpdateState::Offered(version);
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
                    let hydra = updater::find_hydra(&exe, std::env::var_os("PATH").as_deref());
                    updater::install(
                        &hydra_update::latest_url(),
                        &exe,
                        hydra.as_deref(),
                        &hydra_update::running_hydra_pids,
                    )
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

    /// Saves the session, starts the new hydraui and closes this window; unsaved edits are
    /// asked about first, as when closing.
    pub(super) fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(window, cx);
        let relaunch = |window: &mut Window, cx: &mut gpui::App| match std::env::current_exe()
            .and_then(|exe| std::process::Command::new(exe).spawn())
        {
            Ok(_) => window.remove_window(),
            Err(e) => window.push_notification(
                Notification::error(format!("can't start the new hydraui: {e}"))
                    .title("Update hydra"),
                cx,
            ),
        };
        let all: Vec<_> = self.files.keys().copied().collect();
        if self.unsaved_in(&all, cx).is_empty() {
            relaunch(window, cx);
        } else {
            self.confirm_unsaved(all, window, cx, move |_, w, cx| relaunch(w, cx));
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
                        .child(div().text_sm().text_color(muted).child(self.title.clone())),
                )
                .children(button),
        )
    }
}
