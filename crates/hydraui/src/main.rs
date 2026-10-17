//! hydraui.exe: a desktop terminal for hydra environments.
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, Theme, ThemeMode};
use hydraui::config::{self, ThemeChoice};
use hydraui::hydra::Hydra;
use hydraui::keys;
use hydraui::palette::Palette;
use hydraui::pane_view::TermStyle;
use hydraui::workspace::Workspace;

fn main() {
    let (hydra, hydra_problem) = match Hydra::from_env() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hydraui: {e}");
            std::process::exit(1);
        }
    };
    let (ui, ui_problems) = config::load(&hydra.paths);
    let (palette, colour_problems) = Palette::from_config(&ui.colors);
    let (keymap, key_problems) = keys::keymap(&ui.keys);
    let problems: Vec<String> = hydra_problem
        .into_iter()
        .chain(ui_problems)
        .chain(colour_problems)
        .chain(key_problems)
        .collect();
    // Init scripts left by earlier runs (spec §4.5).
    hydraui_term::clear_pane_inits(&hydra.paths);
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let mode = match ui.theme {
                ThemeChoice::Dark => ThemeMode::Dark,
                ThemeChoice::Light => ThemeMode::Light,
            };
            Theme::change(mode, None, cx);
            cx.bind_keys(keys::bindings(&keymap));
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let style = TermStyle {
                font: ui.font.clone().into(),
                font_size: px(ui.font_size),
                palette,
            };
            let bounds = Bounds::centered(None, size(px(1200.), px(760.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("hydraui".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(480.), px(320.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                let workspace =
                    cx.new(|cx| Workspace::new(Arc::new(hydra), ui, style, problems, window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("hydraui can't open its window");
            cx.activate(true);
        });
}
