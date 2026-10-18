//! Finding files from the window (spec §7.10): quick open, and search in files. The logic is
//! in `quick_open` and `search`; this is their dialogs.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Context, Window, div};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::{ActiveTheme, WindowExt, v_flex};

use super::Workspace;
use crate::keys::OpenFile;
use crate::quick_open::{list_files, score, split_line};

/// The folder's files once listed, and whether the list was cut off.
type Listing = Rc<RefCell<Option<(Vec<PathBuf>, bool)>>>;

/// How many files quick open lists at most, and how many matches it shows.
const MAX_FILES: usize = 50_000;
const SHOWN: usize = 50;

impl Workspace {
    pub(super) fn open_file_action(
        &mut self,
        _: &OpenFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_finder(window, cx);
    }

    /// Quick open (spec §7.10): the focused terminal's folder's files, fuzzy-matched as typed;
    /// `name:42` opens at line 42.
    pub(super) fn open_file_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.focused_cwd(cx);
        let state = cx.new(|cx| CommandState::new(window, cx));
        let files: Listing = Rc::new(RefCell::new(None));
        let (listed, from) = (files.clone(), root.clone());
        cx.spawn_in(window, async move |_, cx| {
            let found = cx
                .background_spawn(async move { list_files(&from, MAX_FILES) })
                .await;
            *listed.borrow_mut() = Some(found);
            let _ = cx.update(|window, _| window.refresh());
        })
        .detach();
        // The last query and its ranked matches, so a render doesn't rank again.
        let ranked: Rc<RefCell<(Option<String>, Vec<PathBuf>)>> = Rc::default();
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let query = state.read(cx).query(cx).to_string();
            let (name, line) = split_line(&query);
            let note = match &*files.borrow() {
                None => Some("Listing files…".to_string()),
                Some((list, cut)) => {
                    let mut r = ranked.borrow_mut();
                    if r.0.as_deref() != Some(query.as_str()) {
                        let mut scored: Vec<(i32, &PathBuf)> = list
                            .iter()
                            .filter_map(|p| score(name, &p.to_string_lossy()).map(|s| (s, p)))
                            .collect();
                        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
                        r.1 = scored
                            .into_iter()
                            .take(SHOWN)
                            .map(|(_, p)| p.clone())
                            .collect();
                        r.0 = Some(query.clone());
                    }
                    cut.then(|| format!("Showing the first {MAX_FILES} files"))
                }
            };
            let matches = ranked.borrow().1.clone();
            let (root, workspace, chosen) = (root.clone(), workspace.clone(), matches.clone());
            let muted = cx.theme().muted_foreground;
            dialog.title("Open file").child(
                v_flex()
                    .gap_2()
                    .child(
                        Command::new(&state)
                            .filterable(false)
                            .items(
                                matches
                                    .iter()
                                    .map(|p| CommandItem::new().label(p.display().to_string())),
                            )
                            .placeholder("File name, or name:line")
                            .on_confirm(move |ix, window, cx| {
                                if let Some(rel) = chosen.get(ix.row) {
                                    window.close_dialog(cx);
                                    let path = root.join(rel);
                                    workspace.update(cx, |ws, cx| {
                                        ws.show_file_at(path, line, 0, window, cx)
                                    });
                                }
                            }),
                    )
                    .children(note.map(|n| div().text_sm().text_color(muted).child(n))),
            )
        });
    }
}
