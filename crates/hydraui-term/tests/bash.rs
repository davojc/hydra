//! Git Bash in a real ConPTY. Each test prints "skipped" and passes when Git Bash isn't
//! installed.
mod common;

use common::{Fx, at_prompt, prompts, reached, wait_for, wait_text};
use hydra_platform::shell::ShellKind;
use hydraui_term::{PromptState, TermEvent};

const BASH: ShellKind = ShellKind::Bash;

#[test]
fn startup_command_sees_the_environment_and_reports_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(
        BASH,
        Some("echo \"FOO=[$FOO] GH=[$GH_TOKEN] TERM=[$TERM]\"; false"),
    ) else {
        return;
    };
    let events = wait_for(&rx, "the first prompt", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(1)),
            PromptState::AtPrompt,
        ]
    );
    assert!(reached(&events, &fx.start), "{events:?}");
    wait_text(&pane, "FOO=[bar] GH=[] TERM=[xterm-256color]");
}

#[test]
fn cd_commands_and_exit() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(BASH, None) else {
        return;
    };
    let events = wait_for(&rx, "the first prompt", at_prompt);
    assert_eq!(prompts(&events), vec![PromptState::AtPrompt]);

    pane.write(b"cd \"it's here\"\r").unwrap();
    let events = wait_for(&rx, "the prompt after cd", at_prompt);
    assert!(reached(&events, &fx.start.join("it's here")), "{events:?}");

    pane.write(b"(exit 4)\r").unwrap();
    let events = wait_for(&rx, "the prompt after (exit 4)", at_prompt);
    assert_eq!(
        prompts(&events),
        vec![
            PromptState::CommandRunning,
            PromptState::CommandDone(Some(4)),
            PromptState::AtPrompt,
        ]
    );

    pane.write(b"exit 6\r").unwrap();
    wait_for(&rx, "the exit", |e| *e == TermEvent::Exited(Some(6)));
}

// The Unix counterpart of pwsh.rs's Job Object test: the pane's session is its job.
#[cfg(unix)]
#[test]
fn dropping_a_busy_pane_ends_the_whole_tree_promptly() {
    use std::time::{Duration, Instant};

    let fx = Fx::new();
    let pid_file = fx.dir.path().join("pid.txt");
    // sleep is the shell's child, in the background; the second sleep keeps the shell busy
    // so the pane is closed mid-command.
    let command = format!("sleep 120 & echo $! > '{}'; sleep 120", pid_file.display());
    let Some((pane, _rx)) = fx.spawn(BASH, Some(&command)) else {
        return;
    };
    let deadline = Instant::now() + common::TIMEOUT;
    let pid: libc::pid_t = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "the startup command never wrote its pid"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    // SAFETY: signal 0 only checks that the process exists.
    let alive = || unsafe { libc::kill(pid, 0) } == 0;
    assert!(alive(), "sleep should still be running");
    let start = Instant::now();
    drop(pane);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "closing took {:?}",
        start.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive() {
        assert!(Instant::now() < deadline, "sleep outlived its pane");
        std::thread::sleep(Duration::from_millis(50));
    }
}
