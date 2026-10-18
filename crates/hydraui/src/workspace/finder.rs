//! Finding files from the window (spec §7.10): quick open, and search in files. The logic is
//! in `quick_open` and `search`; this is their dialogs.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Entity, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::{ActiveTheme, Selectable, Sizable, WindowExt, h_flex, v_flex};

use super::Workspace;
use crate::keys::{OpenFile, SearchFiles};
use crate::quick_open::{list_files, score, split_line};
use crate::search::{Generation, Hit, Query, search};

/// The folder's files once listed, and whether the list was cut off.
type Listing = Rc<RefCell<Option<(Vec<PathBuf>, bool)>>>;

/// How many files quick open lists at most, and how many matches it shows.
const MAX_FILES: usize = 50_000;
const SHOWN: usize = 50;

/// How many lines a search finds at most, and how long typing must pause before it runs.
const MAX_HITS: usize = 1000;
const DEBOUNCE: Duration = Duration::from_millis(200);

/// The search dialog's results, and the query, regex and case setting they answer.
#[derive(Default)]
struct Found {
    key: Option<(String, bool, bool)>,
    hits: Vec<Hit>,
    capped: bool,
    error: Option<String>,
}

/// Cancels the dialog's running search when the dialog closes.
struct CancelOnDrop(Generation);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.next();
    }
}

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

    pub(super) fn search_action(
        &mut self,
        _: &SearchFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_dialog(window, cx);
    }

    /// Search in files (spec §7.10): the focused terminal's folder, searched as typed (after
    /// a short pause), plain text or a regex, case-sensitive or not.
    pub(super) fn search_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.focused_cwd(cx);
        let state = cx.new(|cx| CommandState::new(window, cx));
        let (regex, case) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let found: Rc<RefCell<Found>> = Rc::default();
        let generation = Generation::default();
        let guard = CancelOnDrop(generation.clone());
        let workspace = cx.entity();
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let _ = &guard;
            if first.replace(false) {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            let query = Query {
                text: state.read(cx).query(cx).to_string(),
                regex: regex.get(),
                case: case.get(),
            };
            let key = (query.text.clone(), query.regex, query.case);
            if found.borrow().key.as_ref() != Some(&key) {
                found.borrow_mut().key = Some(key);
                let current = generation.next();
                if query.text.is_empty() {
                    let mut f = found.borrow_mut();
                    (f.hits, f.capped, f.error) = (Vec::new(), false, None);
                    state.update(cx, |s, cx| s.set_loading(false, window, cx));
                } else {
                    state.update(cx, |s, cx| s.set_loading(true, window, cx));
                    let (g, root, found, state) = (
                        generation.clone(),
                        root.clone(),
                        found.clone(),
                        state.clone(),
                    );
                    window
                        .spawn(cx, async move |cx| {
                            cx.background_executor().timer(DEBOUNCE).await;
                            if !g.is_current(current) {
                                return;
                            }
                            let still = g.clone();
                            let result = cx
                                .background_spawn(async move {
                                    search(&root, &query, MAX_HITS, &|| !still.is_current(current))
                                })
                                .await;
                            if !g.is_current(current) {
                                return;
                            }
                            {
                                let mut f = found.borrow_mut();
                                match result {
                                    Ok((hits, capped)) => {
                                        (f.hits, f.capped, f.error) = (hits, capped, None)
                                    }
                                    Err(e) => {
                                        (f.hits, f.capped, f.error) = (Vec::new(), false, Some(e))
                                    }
                                }
                            }
                            let _ = cx.update(|window, cx| {
                                state.update(cx, |s, cx| s.set_loading(false, window, cx));
                                window.refresh();
                            });
                        })
                        .detach();
                }
            }
            let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
            let f = found.borrow();
            let note = match &f.error {
                Some(e) => Some(div().text_sm().text_color(danger).child(e.clone())),
                None => f.capped.then(|| {
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(format!("Stopped at {MAX_HITS} results"))
                }),
            };
            let items: Vec<CommandItem> = f
                .hits
                .iter()
                .map(|h| {
                    let rel = h.path.strip_prefix(&root).unwrap_or(&h.path);
                    let place = format!("{}:{}", rel.display(), h.line);
                    let text = h.text.trim().to_string();
                    CommandItem::new().label(text.clone()).child(move |_, _| {
                        h_flex()
                            .gap_2()
                            .overflow_hidden()
                            .child(div().flex_none().text_color(muted).child(place.clone()))
                            .child(div().truncate().child(text.clone()))
                    })
                })
                .collect();
            let chosen = f.hits.clone();
            drop(f);
            let workspace = workspace.clone();
            dialog.title("Search in files").child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1()
                            .justify_end()
                            .child(toggle("case", "Aa", "Match case", &case, &state))
                            .child(toggle("regex", ".*", "Regular expression", &regex, &state)),
                    )
                    .child(
                        Command::new(&state)
                            .filterable(false)
                            .items(items)
                            .placeholder("Text to find")
                            .on_confirm(move |ix, window, cx| {
                                if let Some(hit) = chosen.get(ix.row) {
                                    window.close_dialog(cx);
                                    let (path, line, column) =
                                        (hit.path.clone(), hit.line, hit.column);
                                    workspace.update(cx, |ws, cx| {
                                        ws.show_file_at(path, line, column, window, cx)
                                    });
                                }
                            }),
                    )
                    .children(note),
            )
        });
    }
}

/// A search option's on/off button; the search box keeps the focus.
fn toggle(
    id: &'static str,
    label: &'static str,
    tip: &'static str,
    on: &Rc<Cell<bool>>,
    state: &Entity<CommandState>,
) -> Button {
    let (flag, state) = (on.clone(), state.clone());
    Button::new(id)
        .label(label)
        .xsmall()
        .ghost()
        .selected(on.get())
        .tooltip(tip)
        .on_click(move |_, window, cx| {
            flag.set(!flag.get());
            state.update(cx, |s, cx| s.focus(window, cx));
            window.refresh();
        })
}
