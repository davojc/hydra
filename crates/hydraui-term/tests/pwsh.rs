//! pwsh in a real ConPTY. Each test prints "skipped" and passes when pwsh isn't installed.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Fx, at_prompt, prompts, reached, wait_for, wait_text};
use hydra_core::lock::is_running;
use hydra_platform::shell::ShellKind;
use hydraui_term::alacritty_terminal::grid::Dimensions;
use hydraui_term::{GridSize, PromptState, TermEvent};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

const PWSH: ShellKind = ShellKind::Pwsh;

#[test]
fn startup_command_sees_the_environment_and_reports_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(
        PWSH,
        Some("Write-Output \"FOO=[$env:FOO] GH=[$env:GH_TOKEN] TERM=[$env:TERM] 'héllo' wörld\"; cmd /c exit 5"),
    ) else {
        return;
    };
    let events = wait_for(&rx, "the first prompt", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(5)),
            PromptState::AtPrompt,
        ]
    );
    assert!(reached(&events, &fx.start), "{events:?}");
    wait_text(&pane, "FOO=[bar] GH=[] TERM=[xterm-256color] 'héllo' wörld");
}

#[test]
fn cd_reports_the_new_folder() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.write(b"Set-Location -LiteralPath \"it's here\"\r")
        .unwrap();
    let target = fx.start.join("it's here");
    let events = wait_for(&rx, "the next prompt", at_prompt);
    assert!(reached(&events, &target), "{events:?}");
}

#[test]
fn a_command_goes_running_then_done_with_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.write(b"cmd /c exit 3\r").unwrap();
    let events = wait_for(&rx, "the next prompt", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(3)),
            PromptState::AtPrompt,
        ]
    );
}

#[test]
fn exit_reports_the_code_and_a_new_pane_starts_after() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.write(b"exit 7\r").unwrap();
    wait_for(&rx, "the exit", |e| *e == TermEvent::Exited(Some(7)));
    drop(pane);
    // Restart is a fresh spawn (the app runs prepare again first).
    let (_again, rx) = fx.spawn(PWSH, None).unwrap();
    wait_for(&rx, "the restarted prompt", at_prompt);
}

#[test]
fn the_pane_holds_the_shell_lock_and_its_script_until_dropped() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    assert!(is_running(&fx.paths, &fx.env()).unwrap());
    assert_eq!(fx.scripts(), 1);
    drop(pane);
    assert!(!is_running(&fx.paths, &fx.env()).unwrap());
    assert_eq!(fx.scripts(), 0);
}

#[test]
fn resize_reaches_the_shell_and_the_grid() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    pane.resize(GridSize { cols: 80, rows: 20 }).unwrap();
    assert_eq!(
        pane.with_term(|t| (t.columns(), t.screen_lines())),
        (80, 20)
    );
    pane.write(
        b"Write-Output \"size=$($Host.UI.RawUI.WindowSize.Width)x$($Host.UI.RawUI.WindowSize.Height)\"\r",
    )
    .unwrap();
    wait_text(&pane, "size=80x20");
}

#[test]
fn a_flood_of_output_sends_one_wakeup_until_the_app_renders() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(PWSH, Some("1..20000 | % { \"line $_\" }")) else {
        return;
    };
    let mut events = wait_for(&rx, "the first prompt", at_prompt);
    wait_text(&pane, "line 20000");
    events.extend(rx.try_iter());
    let wakeups = events.iter().filter(|e| **e == TermEvent::Wakeup).count();
    assert_eq!(wakeups, 1, "{events:?}");
    pane.with_term(|_| ());
    pane.write(b"Write-Output again\r").unwrap();
    wait_for(&rx, "a new wakeup after rendering", |e| {
        *e == TermEvent::Wakeup
    });
}

#[test]
fn dropping_a_busy_pane_ends_the_whole_tree_promptly() {
    let fx = Fx::new();
    let pid_file = fx.dir.path().join("pid.txt");
    // ping is the shell's child, started from the startup command; Start-Sleep keeps the
    // shell busy so the pane is closed mid-command.
    let command = format!(
        "$p = [System.Diagnostics.Process]::Start('ping.exe', '-n 120 127.0.0.1'); Set-Content -LiteralPath '{}' $p.Id; Start-Sleep 120",
        pid_file.display()
    );
    let Some((pane, _rx)) = fx.spawn(PWSH, Some(&command)) else {
        return;
    };
    let ping = open_process(wait_for_pid(&pid_file));
    assert!(!exited(ping, 0), "ping should still be running");
    let start = Instant::now();
    drop(pane);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "closing took {:?}",
        start.elapsed()
    );
    assert!(exited(ping, 10_000), "ping outlived its pane");
    // SAFETY: the handle came from `open_process` and is closed once.
    unsafe { CloseHandle(ping) };
}

fn wait_for_pid(file: &Path) -> u32 {
    let deadline = Instant::now() + common::TIMEOUT;
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the startup command never wrote {}",
            file.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn open_process(pid: u32) -> HANDLE {
    // SAFETY: a plain call; the result is checked.
    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!h.is_null(), "can't open process {pid}");
    h
}

/// Whether the process ended within `ms` milliseconds.
fn exited(process: HANDLE, ms: u32) -> bool {
    // SAFETY: `process` is a live handle from `open_process`.
    unsafe { WaitForSingleObject(process, ms) == WAIT_OBJECT_0 }
}
