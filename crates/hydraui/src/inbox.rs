//! Notifications (core spec §57): every notice hydraui gives lands in one list behind the bell
//! in the tab strip, instead of a toast over the work. Built with the same calls a toast was
//! (`Note::warning(..).title(..)`), and sent with `window.push_note(note, cx)`.

use std::rc::Rc;

use gpui::{App, ClickEvent, Global, SharedString, Window};

/// How a notice reads: its dot's colour, and whether the bell turns amber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info,
    Success,
    Warning,
    Error,
}

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One notice.
#[derive(Clone)]
pub struct Note {
    pub kind: Kind,
    pub title: Option<SharedString>,
    pub message: SharedString,
    /// A later notice with the same key replaces it (a toast's `id1`).
    pub key: Option<String>,
    /// What a click on it opens.
    pub on_click: Option<OnClick>,
}

impl Note {
    fn of(kind: Kind, message: impl Into<SharedString>) -> Self {
        Self {
            kind,
            title: None,
            message: message.into(),
            key: None,
            on_click: None,
        }
    }

    pub fn info(message: impl Into<SharedString>) -> Self {
        Self::of(Kind::Info, message)
    }

    pub fn success(message: impl Into<SharedString>) -> Self {
        Self::of(Kind::Success, message)
    }

    pub fn warning(message: impl Into<SharedString>) -> Self {
        Self::of(Kind::Warning, message)
    }

    pub fn error(message: impl Into<SharedString>) -> Self {
        Self::of(Kind::Error, message)
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Kept so a toast's builder still reads; a notice stays in the list either way.
    pub fn autohide(self, _: bool) -> Self {
        self
    }

    /// Replaces any earlier notice of type `T` with the same `key`.
    pub fn id1<T: 'static>(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(format!("{}:{}", std::any::type_name::<T>(), key.into()));
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

/// A notice as the list keeps it.
#[derive(Clone)]
pub struct Entry {
    pub note: Note,
    /// The log time it came, `2026-11-10 13:50:02.123`.
    pub at: String,
    pub read: bool,
}

/// Every notice since hydraui started, newest first.
#[derive(Default)]
pub struct Inbox {
    pub entries: Vec<Entry>,
}

impl Global for Inbox {}

/// How many notices the list keeps.
pub const KEEP: usize = 200;

impl Inbox {
    /// Adds `note` at log time `at`, newest first, replacing one with its key.
    pub fn add(&mut self, note: Note, at: String) {
        if let Some(key) = &note.key {
            self.entries.retain(|e| e.note.key.as_ref() != Some(key));
        }
        self.entries.insert(
            0,
            Entry {
                note,
                at,
                read: false,
            },
        );
        self.entries.truncate(KEEP);
    }

    pub fn unread(&self) -> usize {
        self.entries.iter().filter(|e| !e.read).count()
    }

    /// Whether an unread notice is a warning or an error: the bell turns amber.
    pub fn urgent(&self) -> bool {
        self.entries
            .iter()
            .any(|e| !e.read && matches!(e.note.kind, Kind::Warning | Kind::Error))
    }

    pub fn mark_all_read(&mut self) {
        for e in &mut self.entries {
            e.read = true;
        }
    }
}

/// Sends a notice to the list (`window.push_note(note, cx)`).
pub trait PushNote {
    fn push_note(&mut self, note: Note, cx: &mut App);
}

impl PushNote for Window {
    fn push_note(&mut self, note: Note, cx: &mut App) {
        crate::action_log::write(format!(
            "notice: {}{}",
            note.title
                .as_ref()
                .map_or(String::new(), |t| format!("{t}: ")),
            note.message
        ));
        cx.default_global::<Inbox>()
            .add(note, crate::action_log::now());
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct A;

    #[test]
    fn newest_first_and_a_key_replaces_its_earlier_notice() {
        let mut inbox = Inbox::default();
        inbox.add(Note::info("one").id1::<A>("x"), "1".into());
        inbox.add(Note::error("two"), "2".into());
        inbox.add(Note::warning("three").id1::<A>("x"), "3".into());
        let messages: Vec<&str> = inbox
            .entries
            .iter()
            .map(|e| e.note.message.as_ref())
            .collect();
        assert_eq!(messages, ["three", "two"]);
        assert_eq!(inbox.unread(), 2);
        assert!(inbox.urgent());
        inbox.mark_all_read();
        assert_eq!(inbox.unread(), 0);
        assert!(!inbox.urgent());
    }

    #[test]
    fn only_warnings_and_errors_are_urgent() {
        let mut inbox = Inbox::default();
        inbox.add(Note::info("i").title("T"), "1".into());
        inbox.add(Note::success("s"), "2".into());
        assert_eq!(inbox.unread(), 2);
        assert!(!inbox.urgent());
        assert_eq!(inbox.entries[1].note.title.as_deref(), Some("T"));
    }

    #[test]
    fn it_keeps_the_newest_few_hundred() {
        let mut inbox = Inbox::default();
        for n in 0..KEEP + 5 {
            inbox.add(Note::info(n.to_string()), n.to_string());
        }
        assert_eq!(inbox.entries.len(), KEEP);
        assert_eq!(
            inbox.entries[0].note.message.as_ref(),
            (KEEP + 4).to_string()
        );
    }
}
