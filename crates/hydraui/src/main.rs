#![windows_subsystem = "windows"]
//! hydraui.exe: a desktop terminal for hydra environments.
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};
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
        Err(e) => fatal(&e),
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
    // A layout file to open (spec §7.12): if another window has it, bring that one forward
    // and stop here, before touching anything another hydraui uses.
    let file = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .map(|p| std::path::absolute(&p).unwrap_or(p));
    if let Some(f) = &file
        && hydraui::claim::is_claimed(f)
    {
        let running = hydraui::claim::running_dir(&hydra.paths);
        if let Some(h) = hydraui::claim::holder_window(&running, f) {
            hydraui::claim::bring_forward(h);
        }
        return;
    }
    let start = match file {
        Some(f) => hydraui::workspace::StartWith::File(f),
        None => hydraui::workspace::StartWith::Page,
    };
    // Terminals get this server's address and a token (spec §2).
    hydraui::mcp::server::init();
    // Init scripts left by earlier runs (spec §4.5).
    hydraui_term::clear_pane_inits(&hydra.paths);
    // Exes a previous update replaced (spec §7.11).
    if let Ok(exe) = std::env::current_exe() {
        let hydra = hydraui::updater::find_hydra(
            &exe,
            std::env::var_os("PATH").as_deref(),
            &hydraui::updater::is_our_hydra,
        );
        hydraui::updater::clean_old(&exe, hydra.as_deref());
    }
    gpui_platform::application()
        .with_assets(hydraui::assets::AppAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let mode = match ui.theme {
                ThemeChoice::Dark => ThemeMode::Dark,
                ThemeChoice::Light => ThemeMode::Light,
            };
            Theme::change(mode, None, cx);
            cx.bind_keys(keys::bindings(&keymap));
            cx.bind_keys(keys::terminal_bindings());
            cx.bind_keys(keys::file_pane_bindings());
            cx.bind_keys(keys::test_bindings());
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
                window_min_size: Some(size(px(480.), px(320.))),
                // hydraui draws its own title bar (spec §7.11).
                ..gpui_component::TitleBar::window_options()
            };
            cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| {
                    Workspace::new(Arc::new(hydra), ui, style, problems, start, window, cx)
                });
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap_or_else(|e| fatal(&format!("hydraui can't open its window: {e}")));
            cx.activate(true);
        });
}

/// A GUI exe has no console: say why it can't start in a message box, then exit.
fn fatal(message: &str) -> ! {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(message), wide("hydraui"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    std::process::exit(1)
}
