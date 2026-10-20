//! The task board on screen (spec: docs/superpowers/specs/2026-10-19-hydraui-task-board-design.md
//! §5): four equal columns of cards that scroll on their own; click a card to edit it in place,
//! drag it or use its right-click menu to move it. Every change goes straight to the file.
use gpui::prelude::*;
use gpui::{
    App, Context, DragMoveEvent, Entity, FocusHandle, Focusable, FontWeight, Hsla, MouseButton,
    ScrollHandle, SharedString, Subscription, Window, div, hsla, point, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::task_board::{Applied, TaskBoard, drop_move};
use crate::tasks::{CardRef, Change, Column};
use crate::tasks_project::{self, Privacy};

/// How close to a column's top or bottom edge a dragged card scrolls it, and by how much per
/// mouse move.
const SCROLL_EDGE: f32 = 32.;
const SCROLL_STEP: f32 = 12.;

/// Each column's color, on its heading's dot and its cards' left edge: Backlog grey, Pickup
/// blue, InProgress orange, Verify yellow, Done green. The theme has no orange, so it's fixed
/// here.
fn accent(col: Column, cx: &App) -> Hsla {
    let theme = cx.theme();
    match col {
        Column::Backlog => theme.muted_foreground,
        Column::Pickup => theme.blue,
        Column::InProgress => hsla(28. / 360., 0.9, 0.55, 1.),
        Column::Verify => theme.yellow,
        Column::Done => theme.green,
    }
}

/// A card being dragged; it draws its own small preview under the pointer.
#[derive(Clone)]
pub struct DraggedCard {
    pub card: CardRef,
}

impl Render for DraggedCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(220.))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().drag_border)
            .bg(cx.theme().secondary)
            .text_sm()
            .opacity(0.9)
            .truncate()
            .child(self.card.text.clone())
    }
}

/// The card being edited: `None` is a new card at the top of Backlog, not yet written.
struct Editing {
    card: Option<CardRef>,
    input: Entity<TextareaState>,
    _events: Subscription,
}

/// What the board asks of the window around it.
pub enum TaskBoardViewEvent {
    /// A Verify card's commit was clicked: show what it changed.
    ShowCommit(String),
}

pub struct TaskBoardView {
    board: Entity<TaskBoard>,
    editing: Option<Editing>,
    /// One per column, so a dragged card can scroll the column it's over.
    scroll: [ScrollHandle; 5],
    focus: FocusHandle,
    _observe: Subscription,
}

impl TaskBoardView {
    pub fn new(board: Entity<TaskBoard>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&board, |_, _, cx| cx.notify());
        Self {
            board,
            editing: None,
            scroll: std::array::from_fn(|_| ScrollHandle::new()),
            focus: cx.focus_handle(),
            _observe: observe,
        }
    }

    /// The project board this view shows.
    pub fn board(&self) -> &Entity<TaskBoard> {
        &self.board
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Saves the card being edited, if any (§5: Enter and clicking away both save). Saving an
    /// empty card deletes it; an unchanged one writes nothing. When the write fails the editor
    /// stays open with its text (`is_editing` still true), so the idea isn't lost; when the
    /// card has gone from the file, changed text is added to Backlog instead (§6).
    pub fn finish_edit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let text = editing.input.read(cx).value().to_string();
        let change = match &editing.card {
            Some(card) if card.text == text => None,
            Some(card) => Some(Change::Edit {
                card: card.clone(),
                text,
            }),
            None => Some(Change::Add { text }),
        };
        let applied = match change {
            Some(change) => self.board.update(cx, |b, cx| b.change(change, cx)),
            None => Applied::Written,
        };
        if !applied.keeps_edit() {
            self.editing = None;
        }
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens the editor on `card` (or a new card), saving any other edit first; if that one
    /// can't be saved it stays open instead.
    fn start_edit(&mut self, card: Option<CardRef>, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_edit(window, cx);
        if self.is_editing() {
            return;
        }
        let text = card.as_ref().map(|c| c.text.clone()).unwrap_or_default();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 12)
                .submit_on_enter(true)
                .placeholder("New card")
                .default_value(text)
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        let events = cx.subscribe_in(
            &input,
            window,
            |this, input, ev: &InputEvent, window, cx| {
                // Only this editor's events: an older one's blur arrives after it was replaced.
                if this.editing.as_ref().is_none_or(|e| e.input != *input) {
                    return;
                }
                match ev {
                    InputEvent::PressEnter { .. } => {
                        this.finish_edit(window, cx);
                        if !this.is_editing() {
                            window.focus(&this.focus, cx);
                        }
                    }
                    InputEvent::Blur => this.finish_edit(window, cx),
                    _ => {}
                }
            },
        );
        if card.is_none() {
            self.scroll[Column::Backlog as usize].set_offset(point(px(0.), px(0.)));
        }
        self.editing = Some(Editing {
            card,
            input,
            _events: events,
        });
        cx.notify();
    }

    /// `card` dropped before the card at `before` in `to`, or at its end.
    fn drop_card(&mut self, card: &CardRef, to: Column, before: Option<usize>, cx: &mut App) {
        let change = drop_move(self.board.read(cx).board(), card, to, before);
        if let Some(change) = change {
            self.board.update(cx, |b, cx| b.change(change, cx));
        }
    }

    /// Scrolls column `col` while a card is dragged near its top or bottom edge (§5).
    fn drag_scroll(
        &mut self,
        col: Column,
        ev: &DragMoveEvent<DraggedCard>,
        cx: &mut Context<Self>,
    ) {
        let (b, pos) = (ev.bounds, ev.event.position);
        if !b.contains(&pos) {
            return;
        }
        let step = if pos.y < b.top() + px(SCROLL_EDGE) {
            px(SCROLL_STEP)
        } else if pos.y > b.bottom() - px(SCROLL_EDGE) {
            px(-SCROLL_STEP)
        } else {
            return;
        };
        let handle = &self.scroll[col as usize];
        let mut offset = handle.offset();
        let y = offset.y + step;
        offset.y = if y > px(0.) {
            px(0.)
        } else if y < -handle.max_offset().y {
            -handle.max_offset().y
        } else {
            y
        };
        handle.set_offset(offset);
        cx.notify();
    }

    /// "Clear N done cards?" then clears Done (§5).
    fn confirm_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.board.read(cx).board().cards(Column::Done).len();
        if n == 0 {
            return;
        }
        let board = self.board.clone();
        let title = format!("Clear {n} done card{}?", if n == 1 { "" } else { "s" });
        window.open_dialog(cx, move |dialog, _, _| {
            let board = board.clone();
            dialog.title(title.clone()).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("confirm").label("Clear").danger().on_click(
                        move |_, window, cx| {
                            window.close_dialog(cx);
                            board.update(cx, |b, cx| b.change(Change::ClearDone, cx));
                        },
                    )),
            )
        });
    }

    /// "Delete this card?" then deletes it (§5): the card's delete icon and its menu's Delete
    /// ask first.
    fn confirm_delete(&mut self, card: CardRef, window: &mut Window, cx: &mut Context<Self>) {
        let board = self.board.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (board, card) = (board.clone(), card.clone());
            dialog
                .title("Delete this card?")
                .child(div().text_sm().child(SharedString::from(card.text.clone())))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("confirm").label("Delete").danger().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let card = card.clone();
                                board.update(cx, |b, cx| b.change(Change::Delete { card }, cx));
                            },
                        )),
                )
        });
    }

    /// The board's `⋯` menu (§3): the privacy choice the file doesn't have yet.
    fn render_menu(&self) -> impl IntoElement + use<> {
        let board = self.board.clone();
        Button::new("task-board-menu")
            .label("⋯")
            .ghost()
            .xsmall()
            .dropdown_menu(move |menu, _, cx| {
                let board = board.clone();
                let project = board.read(cx).project().to_path_buf();
                let (label, private) = match tasks_project::privacy(&project) {
                    Privacy::Committed => ("Keep private", true),
                    Privacy::PrivateByUs => ("Commit tasks", false),
                    Privacy::IgnoredElsewhere => {
                        return menu.item(
                            PopupMenuItem::new("Ignored by another .gitignore rule").disabled(true),
                        );
                    }
                };
                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    board.update(cx, |b, cx| b.set_private(private, cx));
                }))
            })
    }

    fn render_header(
        &self,
        col: Column,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        let mut header = h_flex()
            .px_2()
            .h_8()
            .flex_none()
            .gap_2()
            .child(div().size_2().rounded_full().bg(accent(col, cx)))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(col.heading()),
            )
            .child(div().text_sm().text_color(muted).child(count.to_string()))
            .child(div().flex_1());
        match col {
            Column::Backlog => {
                header = header.child(
                    Button::new("task-add")
                        .label("+")
                        .ghost()
                        .xsmall()
                        .tooltip("New card")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.start_edit(None, window, cx)),
                        ),
                );
            }
            Column::Done => {
                header = header.child(
                    Button::new("task-clear")
                        .label("Clear")
                        .ghost()
                        .xsmall()
                        .disabled(count == 0)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_clear(window, cx)),
                        ),
                );
                if tasks_project::is_repo(self.board.read(cx).project()) {
                    header = header.child(self.render_menu());
                }
            }
            _ => {}
        }
        header
    }

    fn render_editor(
        &self,
        input: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("task-editor")
            .pt_1()
            .border_t_2()
            .border_color(gpui::transparent_black())
            // Clicks in the editor stay there; the board's own click would save it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.cancel_edit(window, cx)))
            .child(Textarea::new(input))
    }

    /// Card `i` of `col`: click edits it, drag moves it, right-click offers moves and Delete.
    fn render_card(
        &self,
        col: Column,
        i: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let b = self.board.read(cx).board();
        let len = b.cards(col).len();
        let card = b.card_ref(col, i).expect("rendering a card the board has");
        let taker = b.cards(col)[i].taker.clone();
        let commit = b.cards(col)[i].commit.clone();
        let theme = cx.theme();
        let (border, bg, hover, line) = (
            theme.border,
            theme.secondary,
            theme.secondary_hover,
            theme.drag_border,
        );
        let (badge_bg, badge_fg) = (theme.muted, theme.muted_foreground);
        let edge = accent(col, cx);
        let board = self.board.clone();
        let view = cx.entity();
        div()
            .id(("task-card", i))
            .group("task-card")
            .pt_1()
            .border_t_2()
            .border_color(gpui::transparent_black())
            .drag_over::<DraggedCard>(move |s, _, _, _| s.border_color(line))
            .on_drop(cx.listener(move |this, d: &DraggedCard, _, cx| {
                this.drop_card(&d.card, col, Some(i), cx)
            }))
            .on_drag(DraggedCard { card: card.clone() }, |d, _, _, cx| {
                cx.new(|_| d.clone())
            })
            .on_click({
                let card = card.clone();
                cx.listener(move |this, _, window, cx| {
                    this.start_edit(Some(card.clone()), window, cx)
                })
            })
            .context_menu({
                let card = card.clone();
                move |menu, _, _| {
                    let change = |change: Change| {
                        let board = board.clone();
                        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                            board.update(cx, |b, cx| b.change(change.clone(), cx));
                        }
                    };
                    let mut menu = menu;
                    for to in Column::ALL.into_iter().filter(|c| *c != col) {
                        let mv = Change::Move {
                            card: card.clone(),
                            to,
                            index: 0,
                        };
                        menu = menu.item(
                            PopupMenuItem::new(format!("Move to {}", to.heading()))
                                .on_click(change(mv)),
                        );
                    }
                    let within = |index| Change::Move {
                        card: card.clone(),
                        to: col,
                        index,
                    };
                    menu.separator()
                        .item(
                            PopupMenuItem::new("Move up")
                                .disabled(i == 0)
                                .on_click(change(within(i.saturating_sub(1)))),
                        )
                        .item(
                            PopupMenuItem::new("Move down")
                                .disabled(i + 1 >= len)
                                .on_click(change(within(i + 1))),
                        )
                        .separator()
                        .item(PopupMenuItem::new("Delete").on_click({
                            // Asks first, as the delete icon does.
                            let (view, card) = (view.clone(), card.clone());
                            move |_, window, cx| {
                                view.update(cx, |v, cx| v.confirm_delete(card.clone(), window, cx))
                            }
                        }))
                }
            })
            .child(
                div()
                    .flex()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .hover(|s| s.bg(hover))
                    .text_sm()
                    // The column's color down the card's left edge; gpui can't color one side
                    // of a border, so it's a strip.
                    .child(div().w_1().flex_none().bg(edge))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1p5()
                            // Who took the card (MCP server spec §4), above its text; the editor
                            // edits only the text.
                            .children(taker.map(|name| {
                                // A row, so the badge hugs its text instead of spanning the card.
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .mb_1()
                                    .child(
                                        div()
                                            .px_1p5()
                                            .rounded_sm()
                                            .bg(badge_bg)
                                            .text_color(badge_fg)
                                            .text_xs()
                                            .child(SharedString::from(name)),
                                    )
                                    // The commit the card's work is in (MCP server spec §5):
                                    // clicking shows its changes in the file pane.
                                    .children(commit.map(|hash| {
                                        Button::new(("task-commit", i))
                                            .label(SharedString::from(hash.clone()))
                                            .ghost()
                                            .xsmall()
                                            .tooltip("Show this commit's changes")
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                // The click is the button's, not the card's.
                                                cx.stop_propagation();
                                                cx.emit(TaskBoardViewEvent::ShowCommit(
                                                    hash.clone(),
                                                ));
                                            }))
                                    }))
                            }))
                            .child(SharedString::from(card.text.clone())),
                    )
                    // Delete, shown while the pointer is over the card; it asks first.
                    .child(
                        div()
                            .flex_none()
                            .pt_1()
                            .pr_1()
                            .invisible()
                            .group_hover("task-card", |s| s.visible())
                            .child(
                                Button::new(("task-delete", i))
                                    .icon(IconName::Trash)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Delete card")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        // The click is the button's, not the card's (which edits).
                                        cx.stop_propagation();
                                        this.confirm_delete(card.clone(), window, cx)
                                    })),
                            ),
                    ),
            )
    }

    fn render_column(&self, col: Column, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let b = self.board.read(cx).board();
        let count = b.cards(col).len();
        // Where the editor goes in this column: `Some(None)` at the top (a new card, or one
        // the file has lost since), `Some(Some(i))` in place of card `i`.
        let editor = self.editing.as_ref().and_then(|e| match &e.card {
            None => (col == Column::Backlog).then_some(None),
            Some(c) if c.column == col => Some(b.find(c)),
            Some(_) => None,
        });
        let line = cx.theme().drag_border;
        let mut body = v_flex()
            .id(("task-column", col as usize))
            .flex_1()
            .min_h_0()
            .px_2()
            .overflow_y_scroll()
            .track_scroll(&self.scroll[col as usize])
            .on_drag_move::<DraggedCard>(
                cx.listener(move |this, ev, _, cx| this.drag_scroll(col, ev, cx)),
            )
            .on_drop(cx.listener(move |this, d: &DraggedCard, _, cx| {
                this.drop_card(&d.card, col, None, cx)
            }));
        let input = self.editing.as_ref().map(|e| e.input.clone());
        if let (Some(None), Some(input)) = (editor, &input) {
            body = body.child(self.render_editor(input, cx));
        }
        for i in 0..count {
            match (editor, &input) {
                (Some(Some(at)), Some(input)) if at == i => {
                    body = body.child(self.render_editor(input, cx))
                }
                _ => body = body.child(self.render_card(col, i, cx)),
            }
        }
        // The column's empty space below its cards: dropping here appends.
        body = body.child(
            div()
                .id("task-column-end")
                .flex_1()
                .min_h(px(48.))
                .border_t_2()
                .border_color(gpui::transparent_black())
                .drag_over::<DraggedCard>(move |s, _, _, _| s.border_color(line)),
        );
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(col != Column::Backlog, |d| {
                d.border_l_1().border_color(cx.theme().border)
            })
            .child(self.render_header(col, count, cx))
            .child(body)
    }
}

impl gpui::EventEmitter<TaskBoardViewEvent> for TaskBoardView {}

impl Focusable for TaskBoardView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TaskBoardView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("task-board")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            // Clicking anywhere off the editor takes the focus, which saves the edit (§5).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .children(Column::ALL.map(|col| self.render_column(col, cx)))
    }
}
