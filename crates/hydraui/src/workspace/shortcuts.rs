//! The shortcuts list: every key hydraui answers to, opened from the title bar's Shortcuts
//! button or the launcher.
use gpui::prelude::*;
use gpui::{Context, Window, div, px};
use gpui_component::{ActiveTheme, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::keys::{Command, FIXED, display};

/// (keys, what they do): the bound commands in their order, then the keys `[keys]` can't change.
fn rows(keymap: &[(Command, String)]) -> Vec<(String, String)> {
    keymap
        .iter()
        .map(|(c, k)| (display(k), c.label().to_string()))
        .chain(
            FIXED
                .iter()
                .map(|(k, what)| (k.to_string(), what.to_string())),
        )
        .collect()
}

impl Workspace {
    /// Opens the shortcuts dialog.
    pub(super) fn shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = rows(&self.keymap);
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let list = rows.iter().fold(v_flex().gap_1(), |list, (keys, what)| {
                list.child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .w(px(160.))
                                .flex_none()
                                .font_family("Consolas")
                                .text_sm()
                                .child(keys.clone()),
                        )
                        .child(div().text_sm().child(what.clone())),
                )
            });
            dialog.title("Keyboard shortcuts").w(px(520.)).child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("shortcuts")
                            .max_h(px(520.))
                            .overflow_y_scroll()
                            .child(list),
                    )
                    .child(div().text_xs().text_color(muted).child(
                        "Change a command's keys under [keys] in config.toml (Launcher → Open \
                         config folder); changes apply when hydraui next starts.",
                    )),
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_show_bound_keys_then_the_fixed_ones() {
        let keymap = vec![
            (Command::Launcher, "ctrl-shift-p".to_string()),
            (Command::NewTerminal, "ctrl-alt-n".to_string()),
        ];
        let got = rows(&keymap);
        assert_eq!(got[0], ("Ctrl+Shift+P".into(), "Launcher".into()));
        assert_eq!(got[1], ("Ctrl+Alt+N".into(), "New terminal".into()));
        assert_eq!(got.len(), 2 + FIXED.len());
        assert_eq!(got[2].0, FIXED[0].0);
        let save = if cfg!(target_os = "macos") {
            "Cmd+S"
        } else {
            "Ctrl+S"
        };
        assert_eq!(got[2].0, save);
    }
}
