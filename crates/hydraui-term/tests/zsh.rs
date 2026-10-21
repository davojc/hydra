//! zsh (macOS's own shell) in a real pseudo-terminal, started through hydra's ZDOTDIR. Each
//! test prints "skipped" and passes when zsh isn't installed.
mod common;

use common::{Fx, at_prompt, prompts, reached, wait_for, wait_text};
use hydra_platform::shell::ShellKind;
use hydraui_term::{PromptState, TermEvent};

const ZSH: ShellKind = ShellKind::Zsh;

#[test]
fn startup_command_sees_the_environment_and_reports_its_exit_code() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(
        ZSH,
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
    let Some((pane, rx)) = fx.spawn(ZSH, None) else {
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

    // Enter on an empty line runs nothing, so it reports no exit code.
    pane.write(b"\r").unwrap();
    let events = wait_for(&rx, "the prompt after an empty line", at_prompt);
    assert_eq!(prompts(&events), vec![PromptState::AtPrompt]);

    pane.write(b"exit 6\r").unwrap();
    wait_for(&rx, "the exit", |e| *e == TermEvent::Exited(Some(6)));
}

#[test]
fn the_prompt_carries_the_environments_tag() {
    let fx = Fx::new();
    let Some((pane, rx)) = fx.spawn(ZSH, None) else {
        return;
    };
    wait_for(&rx, "the first prompt", at_prompt);
    wait_text(&pane, "[work]");
}
