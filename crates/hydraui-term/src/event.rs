//! What a pane reports to the app, and the `alacritty_terminal` listener that produces part
//! of it.
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

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

pub(crate) type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

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
                if let Ok(mut w) = self.writer.lock() {
                    let _ = w.write_all(s.as_bytes());
                    let _ = w.flush();
                }
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

    /// A writer the test can read back.
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn replies_go_to_the_pty_and_title_and_bell_to_the_app() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let l = Listener {
            writer: Arc::new(Mutex::new(Box::new(Shared(written.clone())))),
            events: tx,
        };
        l.send_event(Event::PtyWrite("\x1b[1;1R".into()));
        l.send_event(Event::Title("vim".into()));
        l.send_event(Event::ResetTitle);
        l.send_event(Event::Bell);
        l.send_event(Event::MouseCursorDirty);
        assert_eq!(&*written.lock().unwrap(), b"\x1b[1;1R");
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
