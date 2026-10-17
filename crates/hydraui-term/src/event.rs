//! What a pane reports to the app, and the `alacritty_terminal` listener that produces part
//! of it.
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use alacritty_terminal::event::{Event, EventListener};

use crate::osc::PromptState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    /// New output is in the grid. Sent once until the app next calls `Pane::with_term`.
    Wakeup,
    /// OSC 7: the shell's current folder.
    CwdChanged(PathBuf),
    /// OSC 133.
    Prompt(PromptState),
    /// The window title a program set; empty when it reset it.
    Title(String),
    Bell,
    /// The shell process ended, with its exit code when known.
    Exited(Option<i32>),
}

/// Bytes for the shell. A writer thread owns the PTY's input pipe, so a shell that isn't
/// reading never blocks the caller (the UI thread, or the reader thread answering ConPTY).
pub(crate) type Writer = Sender<Vec<u8>>;

/// Receives `Term`'s events: writes the terminal's replies back to the PTY (ConPTY waits for
/// the cursor-position reply at startup) and forwards title and bell to the app.
#[derive(Clone)]
pub struct Listener {
    pub(crate) writer: Writer,
    pub(crate) events: Sender<TermEvent>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let forward = match event {
            Event::PtyWrite(s) => {
                let _ = self.writer.send(s.into_bytes());
                return;
            }
            Event::Title(t) => TermEvent::Title(t),
            Event::ResetTitle => TermEvent::Title(String::new()),
            Event::Bell => TermEvent::Bell,
            _ => return,
        };
        let _ = self.events.send(forward);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_go_to_the_pty_and_title_and_bell_to_the_app() {
        let (writer, written) = std::sync::mpsc::channel();
        let (tx, rx) = std::sync::mpsc::channel();
        let l = Listener { writer, events: tx };
        l.send_event(Event::PtyWrite("\x1b[1;1R".into()));
        l.send_event(Event::Title("vim".into()));
        l.send_event(Event::ResetTitle);
        l.send_event(Event::Bell);
        l.send_event(Event::MouseCursorDirty);
        assert_eq!(
            written.try_iter().collect::<Vec<_>>(),
            vec![b"\x1b[1;1R".to_vec()]
        );
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                TermEvent::Title("vim".into()),
                TermEvent::Title(String::new()),
                TermEvent::Bell,
            ]
        );
    }
}
