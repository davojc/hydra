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
    hydraui::action_log::init(hydraui::action_log::file_for(hydra.paths.root()));
    hydraui::action_log::log_panics();
    hydraui::action_log::write(format!("hydraui {} started", hydraui::version::VERSION));
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
    if let Some(server) = hydraui::mcp::server::init() {
        // Pipelines and run logs live in the hydra home (orchestrator spec §2, §8).
        server.set_hydra_root(hydra.paths.root().to_path_buf());
    }
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
        // A pipeline's branch rule asks GitHub through it.
        if let Some(server) = hydraui::mcp::server::get() {
            server.set_hydra_exe(hydra);
        }
    }
    gpui_platform::application()
        .with_assets(hydraui::assets::AppAssets)
        .run(move |cx: &mut App| {
            // The Dock shows hydraui's icon, not a bare executable's (macOS).
            hydraui::app_icon::set();
            gpui_component::init(cx);
            hydraui::grammars::register();
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
                font: config::terminal_font(&ui.font, &cx.text_system().all_font_names()).into(),
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
#[cfg(windows)]
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

/// Says why hydraui can't start, on stderr (for a launch from a terminal) and in an alert
/// (for one from Finder or the Dock), then exits.
#[cfg(target_os = "macos")]
fn fatal(message: &str) -> ! {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication};
    use objc2_foundation::NSString;
    eprintln!("hydraui: {message}");
    // AppKit is main-thread only; off it, stderr has to do.
    if let Some(mtm) = MainThreadMarker::new() {
        let _ = NSApplication::sharedApplication(mtm);
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Critical);
        alert.setMessageText(&NSString::from_str("hydraui can't start"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
    std::process::exit(1)
}

/// Says why hydraui can't start, on stderr, then exits.
#[cfg(all(unix, not(target_os = "macos")))]
fn fatal(message: &str) -> ! {
    eprintln!("hydraui: {message}");
    std::process::exit(1)
}
