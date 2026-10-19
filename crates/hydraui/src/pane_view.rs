//! One terminal pane (spec §7.2, §7.5, §6.4): starts its shell through hydra off the UI
//! thread, paints the grid, and turns keys, mouse and clipboard into input for the shell.
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    Hsla, KeyDownEvent, Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString,
    StrikethroughStyle, Subscription, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle,
    Window, canvas, div, fill, font, outline, point, px, rgb, size,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use hydraui_term::alacritty_terminal::grid::Scroll;
use hydraui_term::alacritty_terminal::index::{Column, Direction, Point as GridPoint, Side};
use hydraui_term::alacritty_terminal::selection::{Selection, SelectionType};
use hydraui_term::alacritty_terminal::term::search::Match;
use hydraui_term::alacritty_terminal::term::{TermMode, viewport_to_point};
use hydraui_term::{GridSize, Pane, PromptState, TermEvent};

use crate::frame::{
    CursorKind, Frame, Span, Style, build_frame, find_next, link_at, underline_cells, visible_rows,
};
use crate::hydra::{Hydra, PaneRequest, Started};
use crate::input::{
    Mods, MouseKind, ReportButton, Wheel, key_bytes, mouse_report, paste_bytes, wheel,
};
use crate::keys::{CopyText, Find, PasteText, TerminalBackTab, TerminalTab};
use crate::palette::Palette;
use crate::split::PaneId;

/// The font and colours every pane draws with.
pub struct TermStyle {
    pub font: SharedString,
    pub font_size: Pixels,
    pub palette: Palette,
}

/// What a pane tells the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneEvent {
    Focused,
    CwdChanged,
    PromptChanged,
    TitleChanged,
    Exited,
    /// Ctrl+click on a path in the output: open it at a 1-based line and column (spec §7.10).
    OpenPath(PathBuf, usize, usize),
}

enum Slot {
    Starting,
    Running(Pane),
    /// The shell ended; its pane is closed (job, PTY and lock released).
    Exited(Option<i32>),
    /// `prepare` or the spawn failed: hydra's error text.
    Failed(String),
}

struct Finder {
    input: Entity<InputState>,
    current: Option<Match>,
    _sub: Subscription,
}

pub struct PaneView {
    pub id: PaneId,
    pub request: PaneRequest,
    hydra: Arc<Hydra>,
    style: Arc<TermStyle>,
    slot: Slot,
    warnings: Vec<String>,
    cwd: PathBuf,
    prompt: Option<PromptState>,
    title: Option<String>,
    focus: FocusHandle,
    size: GridSize,
    /// Cell width and height, measured at the last paint.
    cell: Option<(Pixels, Pixels)>,
    /// The grid's top-left corner in window coordinates, at the last paint.
    origin: Point<Pixels>,
    cursor_cell: Option<(usize, usize)>,
    selecting: bool,
    /// The mouse's last position over the grid (`None` once it leaves), and whether Ctrl
    /// was last seen held, for the Ctrl-hover underline.
    mouse: Option<Point<Pixels>>,
    ctrl: bool,
    hover: crate::term_paths::HoverCache,
    /// The "Copied" label shows until then.
    copied_until: Option<std::time::Instant>,
    finder: Option<Finder>,
    /// Bumped on every start, so events from an older shell are ignored.
    generation: u64,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for PaneView {}

impl Focusable for PaneView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn color(c: u32) -> Hsla {
    rgb(c).into()
}

fn mods(m: Modifiers) -> Mods {
    Mods {
        ctrl: m.control,
        alt: m.alt,
        shift: m.shift,
    }
}

fn report_button(b: MouseButton) -> Option<ReportButton> {
    match b {
        MouseButton::Left => Some(ReportButton::Left),
        MouseButton::Middle => Some(ReportButton::Middle),
        MouseButton::Right => Some(ReportButton::Right),
        _ => None,
    }
}

/// The cell at an offset (in pixels) from the grid's corner, clamped to the grid, and which
/// half of the cell it is in.
fn grid_cell(dx: f32, dy: f32, cell: (f32, f32), size: GridSize) -> (usize, usize, Side) {
    let x = (dx / cell.0).max(0.0);
    let y = (dy / cell.1).max(0.0);
    let col = (x as usize).min(usize::from(size.cols) - 1);
    let row = (y as usize).min(usize::from(size.rows) - 1);
    let side = if x - x.floor() < 0.5 || x as usize > col {
        Side::Left
    } else {
        Side::Right
    };
    (row, col, side)
}

/// The cell at an offset from the grid's corner, or `None` outside the grid (the warning
/// strip, the find box, the padding): clicks there aren't the program's.
fn hit_cell(dx: f32, dy: f32, cell: (f32, f32), size: GridSize) -> Option<(usize, usize, Side)> {
    let inside = dx >= 0.0
        && dy >= 0.0
        && dx < cell.0 * f32::from(size.cols)
        && dy < cell.1 * f32::from(size.rows);
    inside.then(|| grid_cell(dx, dy, cell, size))
}

/// The console host titles a pane with its program's path (`C:\...\pwsh.exe`) until the
/// program sets a title of its own; that path names nothing useful, so the folder is used.
fn is_program_path(title: &str) -> bool {
    title.to_ascii_lowercase().ends_with(".exe")
}

/// What finishing a selection copies: its text, unless there is none.
fn copy_on_select(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.is_empty())
}

/// The jump-to-bottom arrow shows while the view is scrolled up into the history.
fn shows_jump_to_bottom(display_offset: usize) -> bool {
    display_offset > 0
}

/// How long the "Copied" label shows after a selection is copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

fn grid_point(display_offset: usize, row: usize, col: usize) -> GridPoint {
    viewport_to_point(display_offset, GridPoint::new(row, Column(col)))
}

impl PaneView {
    pub fn new(
        id: PaneId,
        request: PaneRequest,
        hydra: Arc<Hydra>,
        style: Arc<TermStyle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let on_focus = cx.on_focus(&focus, window, |_, _, cx| cx.emit(PaneEvent::Focused));
        let warnings: Vec<String> = request.note.iter().cloned().collect();
        let mut view = Self {
            id,
            cwd: request.cwd.clone(),
            request,
            hydra,
            style,
            slot: Slot::Starting,
            warnings,
            prompt: None,
            title: None,
            focus,
            size: GridSize { cols: 80, rows: 24 },
            cell: None,
            origin: Point::default(),
            cursor_cell: None,
            mouse: None,
            ctrl: false,
            hover: Default::default(),
            selecting: false,
            copied_until: None,
            finder: None,
            generation: 0,
            _tasks: Vec::new(),
            _subs: vec![on_focus],
        };
        view.start(cx);
        view
    }

    /// The shell's current folder (OSC 7), else where it started.
    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn env(&self) -> &str {
        &self.request.env
    }

    /// Where the shell is (OSC 133); `None` when it doesn't say.
    pub fn prompt(&self) -> Option<PromptState> {
        self.prompt
    }

    /// What the terminal is doing now, for the tab strip.
    pub fn activity(&self, now: std::time::Instant) -> crate::activity::Activity {
        // The pane's own clock: a hidden pane gets no Wakeups after the first.
        let (running, last) = match &self.slot {
            Slot::Running(pane) => (true, pane.last_output()),
            _ => (false, None),
        };
        crate::activity::activity(running, self.prompt, last, now)
    }

    /// The tab label: the title a program set, else the folder's name.
    pub fn label(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.cwd.display().to_string())
        })
    }

    /// Text for the shell as if typed (a path from the file tree, a `cd`).
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.send(text.as_bytes(), cx);
    }

    /// New font and colours (Reload config): drawn from the next frame.
    pub fn set_style(&mut self, style: Arc<TermStyle>, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    /// Ends the shell (and whatever it runs) and starts a new one in the folder it was in, so
    /// an edited env.toml applies.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.request.cwd = self.cwd.clone();
        self.start(cx);
    }

    /// Runs `prepare` and starts the shell off the UI thread. Also Retry and restart, so an
    /// edited env.toml applies (spec §6.4).
    fn start(&mut self, cx: &mut Context<Self>) {
        self.slot = Slot::Starting;
        self.prompt = None;
        self.generation += 1;
        let generation = self.generation;
        let (hydra, request, size) = (self.hydra.clone(), self.request.clone(), self.size);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { hydra.start(&request, size) })
                .await;
            let _ = this.update(cx, |view, cx| view.started(generation, result, cx));
        });
        self._tasks = vec![task];
        cx.notify();
    }

    fn started(
        &mut self,
        generation: u64,
        result: Result<Started, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        match result {
            Ok(s) => {
                // The view may have been laid out at another size while the shell started.
                let _ = s.pane.resize(self.size);
                self.warnings = self
                    .request
                    .note
                    .iter()
                    .cloned()
                    .chain(s.warnings)
                    .collect();
                self._tasks.push(pump(s.events, generation, cx));
                self.slot = Slot::Running(s.pane);
            }
            Err(e) => self.slot = Slot::Failed(e),
        }
        cx.notify();
    }

    fn on_term_event(&mut self, generation: u64, event: TermEvent, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        match event {
            TermEvent::Wakeup => cx.notify(),
            TermEvent::CwdChanged(p) => {
                self.cwd = p;
                cx.emit(PaneEvent::CwdChanged);
            }
            TermEvent::Prompt(p) => {
                self.prompt = Some(p);
                cx.emit(PaneEvent::PromptChanged);
            }
            TermEvent::Title(t) => {
                self.title = (!t.is_empty() && !is_program_path(&t)).then_some(t);
                cx.emit(PaneEvent::TitleChanged);
            }
            TermEvent::Bell => {}
            TermEvent::Exited(code) => {
                self.slot = Slot::Exited(code);
                self.prompt = None;
                self.finder = None;
                cx.emit(PaneEvent::Exited);
                cx.notify();
            }
        }
    }

    fn mode(&self) -> Option<TermMode> {
        match &self.slot {
            Slot::Running(pane) => Some(pane.with_term(|t| *t.mode())),
            _ => None,
        }
    }

    /// Input for the shell. Typing returns the view to the live screen.
    fn send(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if let Slot::Running(pane) = &self.slot {
            pane.with_term(|t| t.scroll_display(Scroll::Bottom));
            let _ = pane.write(bytes);
            cx.notify();
        }
    }

    fn resize(&mut self, size: GridSize) {
        if size != self.size {
            self.size = size;
            if let Slot::Running(pane) = &self.slot {
                let _ = pane.resize(size);
            }
        }
    }

    fn frame(&self, focused: bool) -> Option<Frame> {
        let Slot::Running(pane) = &self.slot else {
            return None;
        };
        let mut frame = pane.with_term(|t| build_frame(t, &self.style.palette, focused));
        underline_cells(&mut frame, self.hover.spans());
        Some(frame)
    }

    /// Ctrl pressed or released, or the mouse moved: the underline follows.
    fn update_hover(&mut self, ctrl: bool, cx: &mut Context<Self>) {
        self.ctrl = ctrl;
        if self.refresh_hover() {
            cx.notify();
        }
    }

    /// While Ctrl is held, underlines the link or file path under the mouse: what Ctrl+click
    /// would open (spec §7.10). Runs at every paint too, so output or a scroll under a still
    /// mouse moves the underline; it's worked out again only when the cell's line changed.
    /// `true` when the underline changed.
    fn refresh_hover(&mut self) -> bool {
        // Smoke tests can't hold a real Ctrl: debug builds take HYDRAUI_TEST_CTRL_HOVER.
        let ctrl = self.ctrl
            || (cfg!(debug_assertions) && std::env::var_os("HYDRAUI_TEST_CTRL_HOVER").is_some());
        let cell = if ctrl {
            self.mouse
                .and_then(|p| self.hit_at(p))
                .map(|(row, col, _)| (row, col))
        } else {
            None
        };
        let (Some((row, col)), Slot::Running(pane)) = (cell, &self.slot) else {
            return self.hover.update(None, Vec::new);
        };
        let cwd = self.cwd.clone();
        let hover = &mut self.hover;
        pane.with_term(|t| {
            let rows = visible_rows(t);
            if row >= rows.len() {
                return hover.update(None, Vec::new);
            }
            let (text, at) = crate::term_paths::join_wrapped(&rows, row, col);
            hover.update(Some((row, col, text.clone())), || {
                if let Some(url) = link_at(t, row, col) {
                    let same = |c: usize| link_at(t, row, c).as_deref() == Some(url.as_str());
                    let mut start = col;
                    while start > 0 && same(start - 1) {
                        start -= 1;
                    }
                    let mut end = col + 1;
                    while same(end) {
                        end += 1;
                    }
                    return vec![Span { row, start, end }];
                }
                crate::term_paths::path_span_at(&text, at, &cwd, &|p| p.is_file())
                    .map(|(_, span)| crate::term_paths::wrapped_spans(&rows, row, span))
                    .unwrap_or_default()
            })
        })
    }

    /// The cell under a window position, or `None` off the grid.
    fn hit_at(&self, pos: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let (cw, ch) = self.cell?;
        hit_cell(
            f32::from(pos.x - self.origin.x),
            f32::from(pos.y - self.origin.y),
            (f32::from(cw), f32::from(ch)),
            self.size,
        )
    }

    /// The nearest cell to a window position, for drags that leave the grid.
    fn cell_at(&self, pos: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let (cw, ch) = self.cell?;
        Some(grid_cell(
            f32::from(pos.x - self.origin.x),
            f32::from(pos.y - self.origin.y),
            (f32::from(cw), f32::from(ch)),
            self.size,
        ))
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if !self.focus.is_focused(window) {
            // The find box has the focus: Escape closes it, other keys are the box's.
            if k.key == "escape" && self.finder.is_some() {
                self.close_finder(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if ev.prefer_character_input {
            // AltGr text: it arrives through the input handler.
            return;
        }
        if matches!(self.slot, Slot::Exited(_) | Slot::Failed(_)) {
            if k.key == "enter" && !k.modifiers.modified() {
                self.start(cx);
                cx.stop_propagation();
            }
            return;
        }
        let Some(mode) = self.mode() else {
            return;
        };
        if let Some(bytes) = key_bytes(&k.key, k.key_char.as_deref(), mods(k.modifiers), mode) {
            self.send(&bytes, cx);
            // Also stops Windows turning the key into text for the input handler.
            cx.stop_propagation();
        }
    }

    /// Tab or Shift+Tab to the shell, as the key would be sent (bound so the window doesn't
    /// take it to move the focus).
    fn send_tab(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || !matches!(self.slot, Slot::Running(_)) {
            return;
        }
        let Some(mode) = self.mode() else {
            return;
        };
        let modifiers = Modifiers {
            shift,
            ..Default::default()
        };
        if let Some(bytes) = key_bytes("tab", None, mods(modifiers), mode) {
            self.send(&bytes, cx);
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (Slot::Running(pane), Some((row, col, side))) = (&self.slot, self.hit_at(ev.position))
        else {
            return;
        };
        if ev.button == MouseButton::Left && ev.modifiers.control {
            if let Some(url) = pane.with_term(|t| link_at(t, row, col)) {
                cx.open_url(&url);
            } else if let Some(hit) = pane.with_term(|t| {
                // A long path may have wrapped onto the next row: read the whole line.
                let rows = visible_rows(t);
                if row >= rows.len() {
                    return None;
                }
                let (text, at) = crate::term_paths::join_wrapped(&rows, row, col);
                crate::term_paths::path_at(&text, at, &self.cwd, &|p| p.is_file())
            }) {
                // A path in the output: open it at its line (spec §7.10).
                cx.emit(PaneEvent::OpenPath(hit.path, hit.line, hit.column));
            }
            return;
        }
        let mode = pane.with_term(|t| *t.mode());
        // Shift+click selects even when the program reports the mouse.
        if !ev.modifiers.shift
            && let Some(button) = report_button(ev.button)
            && let Some(bytes) = mouse_report(
                Some(button),
                MouseKind::Press,
                col,
                row,
                mods(ev.modifiers),
                mode,
            )
        {
            let _ = pane.write(&bytes);
            return;
        }
        if ev.button == MouseButton::Left {
            let ty = match ev.click_count {
                2 => SelectionType::Semantic,
                n if n >= 3 => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            pane.with_term(|t| {
                let at = grid_point(t.grid().display_offset(), row, col);
                t.selection = Some(Selection::new(ty, at, side));
            });
            self.selecting = true;
            cx.notify();
        }
    }

    /// A selection drag, from the window-wide listener (it keeps going outside the pane).
    fn drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let (Slot::Running(pane), Some((row, col, side)), Some((_, ch))) =
            (&self.slot, self.cell_at(pos), self.cell)
        else {
            return;
        };
        let above = pos.y < self.origin.y;
        let below = pos.y > self.origin.y + ch * f32::from(self.size.rows);
        pane.with_term(|t| {
            if above {
                t.scroll_display(Scroll::Delta(1));
            } else if below {
                t.scroll_display(Scroll::Delta(-1));
            }
            let at = grid_point(t.grid().display_offset(), row, col);
            if let Some(s) = t.selection.as_mut() {
                s.update(at, side);
            }
        });
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Slot::Running(pane) = &self.slot {
            let text = pane.with_term(|t| {
                if t.selection.as_ref().is_some_and(|s| s.is_empty()) {
                    t.selection = None;
                }
                t.selection_to_string()
            });
            // A finished selection is copied, with a moment's "Copied" to say so.
            if let Some(text) = copy_on_select(text) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.copied_until = Some(std::time::Instant::now() + COPIED_FOR);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    let _ = this.update(cx, |v, cx| {
                        if v.copied_until
                            .is_some_and(|t| t <= std::time::Instant::now())
                        {
                            v.copied_until = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        if let Slot::Running(pane) = &self.slot {
            pane.with_term(|t| t.scroll_display(Scroll::Bottom));
        }
        cx.notify();
    }

    fn report(
        &self,
        button: Option<ReportButton>,
        kind: MouseKind,
        pos: Point<Pixels>,
        m: Modifiers,
    ) {
        let (Slot::Running(pane), Some((row, col, _))) = (&self.slot, self.hit_at(pos)) else {
            return;
        };
        if m.shift {
            return;
        }
        let mode = pane.with_term(|t| *t.mode());
        if let Some(bytes) = mouse_report(button, kind, col, row, mods(m), mode) {
            let _ = pane.write(&bytes);
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.selecting {
            self.report(
                report_button(ev.button),
                MouseKind::Release,
                ev.position,
                ev.modifiers,
            );
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.mouse = Some(ev.position);
        self.update_hover(ev.modifiers.control, cx);
        if !self.selecting {
            self.report(
                ev.pressed_button.and_then(report_button),
                MouseKind::Move,
                ev.position,
                ev.modifiers,
            );
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.ctrl = ev.modifiers.control;
        let (Slot::Running(pane), Some((_, ch))) = (&self.slot, self.cell) else {
            return;
        };
        let lines = match ev.delta {
            ScrollDelta::Lines(p) => (p.y * 3.0).round() as i32,
            ScrollDelta::Pixels(p) => (f32::from(p.y) / f32::from(ch)).round() as i32,
        };
        if lines == 0 {
            return;
        }
        let (row, col, _) = self.hit_at(ev.position).unwrap_or((0, 0, Side::Left));
        let mode = pane.with_term(|t| *t.mode());
        match wheel(lines, col, row, mods(ev.modifiers), mode) {
            Wheel::Send(bytes) => {
                let _ = pane.write(&bytes);
            }
            Wheel::Scroll(n) => {
                pane.with_term(|t| t.scroll_display(Scroll::Delta(n)));
                cx.notify();
            }
        }
    }

    fn copy_text(&mut self, _: &CopyText, _: &mut Window, cx: &mut Context<Self>) {
        if let Slot::Running(pane) = &self.slot
            && let Some(text) = pane.with_term(|t| t.selection_to_string())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_text(&mut self, _: &PasteText, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(mode) = self.mode() else {
            return;
        };
        self.send(
            &paste_bytes(&text, mode.contains(TermMode::BRACKETED_PASTE)),
            cx,
        );
    }

    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.slot, Slot::Running(_)) {
            return;
        }
        let input = match &self.finder {
            Some(f) => f.input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
                let sub = cx.subscribe_in(&input, window, |view, _, ev: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { shift, .. } = ev {
                        let direction = if *shift {
                            Direction::Right
                        } else {
                            Direction::Left
                        };
                        view.find_step(direction, cx);
                    }
                });
                self.finder = Some(Finder {
                    input: input.clone(),
                    current: None,
                    _sub: sub,
                });
                input
            }
        };
        input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// The next match up (Enter) or down (Shift+Enter), selected and scrolled into view.
    fn find_step(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let (Some(finder), Slot::Running(pane)) = (&mut self.finder, &self.slot) else {
            return;
        };
        let query = finder.input.read(cx).value().to_string();
        let current = finder.current.clone();
        finder.current = pane.with_term(|t| {
            let found = find_next(t, &query, current.as_ref(), direction);
            if let Some(m) = &found {
                t.scroll_to_point(*m.start());
                let mut s = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                s.update(*m.end(), Side::Right);
                t.selection = Some(s);
            }
            found
        });
        cx.notify();
    }

    fn close_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn grid(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let view = cx.entity();
        let style = self.style.clone();
        let focus = self.focus.clone();
        let selecting = self.selecting;
        let prepaint = {
            let (view, style, focus) = (view.clone(), style.clone(), focus.clone());
            move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                let cell = cell_size(window, &style);
                let size = GridSize {
                    cols: ((bounds.size.width / cell.0) as u16).max(2),
                    rows: ((bounds.size.height / cell.1) as u16).max(1),
                };
                let focused = focus.is_focused(window);
                let frame = view.update(cx, |view, _| {
                    view.cell = Some(cell);
                    view.origin = bounds.origin;
                    view.resize(size);
                    view.refresh_hover();
                    let frame = view.frame(focused);
                    view.cursor_cell = frame
                        .as_ref()
                        .and_then(|f| f.cursor)
                        .map(|c| (c.row, c.col));
                    frame
                });
                (frame, cell)
            }
        };
        canvas(prepaint, move |bounds, (frame, cell), window, cx| {
            if let Some(frame) = frame {
                paint(bounds, &frame, cell, &style, window, cx);
            }
            window.handle_input(&focus, ElementInputHandler::new(bounds, view.clone()), cx);
            if selecting {
                let on_move = view.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && ev.dragging() {
                        on_move.update(cx, |view, cx| view.drag_to(ev.position, cx));
                    }
                });
                let on_up = view.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_up.update(cx, |view, cx| view.end_drag(cx));
                    }
                });
            }
        })
        .size_full()
    }
}

/// Forwards a pane's events to the view: a thread blocks on the pane's channel and the view
/// awaits an async one (a blocked executor thread per pane would starve GPUI's pool).
fn pump(events: Receiver<TermEvent>, generation: u64, cx: &mut Context<PaneView>) -> Task<()> {
    let (tx, mut rx) = unbounded();
    let _ = std::thread::Builder::new()
        .name("hydraui-events".into())
        .spawn(move || {
            while let Ok(e) = events.recv() {
                if tx.unbounded_send(e).is_err() {
                    break;
                }
            }
        });
    cx.spawn(async move |this, cx| {
        while let Some(e) = rx.next().await {
            if this
                .update(cx, |view, cx| view.on_term_event(generation, e, cx))
                .is_err()
            {
                break;
            }
        }
    })
}

/// One cell: the advance of "M" and 1.2 × its height, as the spike measured it.
fn cell_size(window: &Window, style: &TermStyle) -> (Pixels, Pixels) {
    let run = TextRun {
        len: 1,
        font: font(style.font.clone()),
        ..Default::default()
    };
    let m = window
        .text_system()
        .shape_line("M".into(), style.font_size, &[run], None);
    (m.width, (m.ascent + m.descent) * 1.2)
}

fn text_run(len: usize, s: &Style, base: &Font) -> TextRun {
    let mut f = base.clone();
    if s.bold {
        f = f.bold();
    }
    if s.italic {
        f = f.italic();
    }
    TextRun {
        len,
        font: f,
        color: color(s.fg),
        background_color: None,
        underline: s.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: None,
            wavy: false,
        }),
        strikethrough: s.strikeout.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    }
}

/// Backgrounds, selection, cursor, then text: one shaped line per row with a fixed advance,
/// plus wide and combined characters drawn at their own columns.
fn paint(
    bounds: Bounds<Pixels>,
    frame: &Frame,
    (cw, ch): (Pixels, Pixels),
    style: &TermStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let p = &style.palette;
    window.paint_quad(fill(bounds, color(p.background)));
    let at = |row: usize, col: usize| {
        point(
            bounds.origin.x + cw * col as f32,
            bounds.origin.y + ch * row as f32,
        )
    };
    let cells = |row: usize, start: usize, end: usize| {
        Bounds::new(at(row, start), size(cw * (end - start) as f32, ch))
    };
    for row in &frame.rows {
        for (s, c) in &row.backgrounds {
            window.paint_quad(fill(cells(s.row, s.start, s.end), color(*c)));
        }
    }
    for s in &frame.selection {
        window.paint_quad(fill(cells(s.row, s.start, s.end), color(p.selection)));
    }
    if let Some(c) = frame.cursor {
        let w = if c.wide { cw * 2.0 } else { cw };
        let b = Bounds::new(at(c.row, c.col), size(w, ch));
        let cc = color(p.cursor);
        let quad = match c.kind {
            CursorKind::Block => fill(b, cc.opacity(0.6)),
            CursorKind::HollowBlock => outline(b, cc, BorderStyle::default()),
            CursorKind::Underline => fill(
                Bounds::new(point(b.origin.x, b.origin.y + ch - px(2.)), size(w, px(2.))),
                cc,
            ),
            CursorKind::Beam => fill(Bounds::new(b.origin, size(px(2.), ch)), cc),
        };
        window.paint_quad(quad);
    }
    let base = font(style.font.clone());
    for (r, row) in frame.rows.iter().enumerate() {
        if !row.text.trim_end().is_empty() {
            let runs: Vec<TextRun> = row
                .runs
                .iter()
                .map(|run| text_run(run.len, &run.style, &base))
                .collect();
            let line = window.text_system().shape_line(
                row.text.clone().into(),
                style.font_size,
                &runs,
                Some(cw),
            );
            let _ = line.paint(at(r, 0), ch, TextAlign::Left, None, window, cx);
        }
        for g in &row.glyphs {
            let run = text_run(g.text.len(), &g.style, &base);
            let line = window.text_system().shape_line(
                g.text.clone().into(),
                style.font_size,
                &[run],
                None,
            );
            let _ = line.paint(at(r, g.col), ch, TextAlign::Left, None, window, cx);
        }
    }
}

fn message(text: String, color: Hsla) -> AnyElement {
    div()
        .size_full()
        .p_4()
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// Text typed into the pane, including IME results, goes straight to the shell. There is no
/// in-line composition preview: the IME's own window shows it until it is committed.
impl EntityInputHandler for PaneView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        None
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !text.is_empty() {
            self.send(text.as_bytes(), cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    /// The IME's candidate window goes at the cursor.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let ((cw, ch), (row, col)) = (self.cell?, self.cursor_cell?);
        Some(Bounds::new(
            point(
                element_bounds.origin.x + cw * col as f32,
                element_bounds.origin.y + ch * row as f32,
            ),
            size(cw, ch),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for PaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, warning, muted, border) = (
            theme.danger,
            theme.warning,
            theme.muted_foreground,
            theme.border,
        );
        let background = color(self.style.palette.background);
        let scrolled_up = match &self.slot {
            Slot::Running(pane) => {
                shows_jump_to_bottom(pane.with_term(|t| t.grid().display_offset()))
            }
            _ => false,
        };
        let body = match &self.slot {
            Slot::Running(_) => self.grid(cx).into_any_element(),
            Slot::Starting => message(format!("starting {}…", self.request.env), muted),
            Slot::Exited(code) => message(
                match code {
                    Some(c) => format!("exited ({c}) - Enter to restart, Ctrl+W to close"),
                    None => "exited - Enter to restart, Ctrl+W to close".into(),
                },
                muted,
            ),
            Slot::Failed(e) => v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .child(div().text_color(danger).child(e.clone()))
                .child(
                    Button::new("retry")
                        .label("Retry")
                        .small()
                        .on_click(cx.listener(|view, _, _, cx| view.start(cx))),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(background)
            .track_focus(&self.focus)
            .key_context("Terminal")
            // On the focused element: modifier changes only reach the focus path.
            .on_modifiers_changed(cx.listener(|v, ev: &ModifiersChangedEvent, _, cx| {
                v.update_hover(ev.modifiers.control, cx)
            }))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy_text))
            .on_action(cx.listener(Self::paste_text))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(|v, _: &TerminalTab, w, cx| v.send_tab(false, w, cx)))
            .on_action(cx.listener(|v, _: &TerminalBackTab, w, cx| v.send_tab(true, w, cx)))
            .when(!self.warnings.is_empty(), |d| {
                d.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .text_color(warning)
                        .text_sm()
                        .child(div().flex_1().child(self.warnings.join(" · ")))
                        .child(
                            Button::new("dismiss")
                                .label("Dismiss")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.warnings.clear();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.finder.as_ref(), |d, f| {
                d.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .child(div().flex_1().child(Input::new(&f.input).small()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("Enter: up · Shift+Enter: down · Esc: close"),
                        ),
                )
            })
            .child(
                div()
                    .id("term-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_1()
                    .cursor_text()
                    // The mouse left: nothing under it to underline.
                    .on_hover(cx.listener(|v, hovered: &bool, _, cx| {
                        if !*hovered {
                            v.mouse = None;
                            let ctrl = v.ctrl;
                            v.update_hover(ctrl, cx);
                        }
                    }))
                    // Only the grid area: clicks in the strips above are theirs.
                    .on_any_mouse_down(cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .when(!self.hover.spans().is_empty(), |d| {
                        d.cursor(CursorStyle::PointingHand)
                    })
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .relative()
                    .child(body)
                    .when(self.copied_until.is_some(), |d| {
                        d.child(
                            div()
                                .absolute()
                                .top_2()
                                .right_3()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_sm()
                                .bg(cx.theme().secondary)
                                .text_color(cx.theme().secondary_foreground)
                                .child("Copied"),
                        )
                    })
                    .when(scrolled_up, |d| {
                        d.child(
                            div()
                                .id("jump-to-bottom")
                                .absolute()
                                .bottom_3()
                                .right_3()
                                .size(px(32.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .bg(cx.theme().secondary)
                                .text_color(cx.theme().secondary_foreground)
                                .hover(|d| d.bg(cx.theme().secondary_hover))
                                .child("↓")
                                // Its own click, not the start of a selection underneath.
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|v, _, _, cx| v.jump_to_bottom(cx))),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishing_a_selection_copies_only_real_text() {
        assert_eq!(
            copy_on_select(Some("ls -la".into())),
            Some("ls -la".to_string())
        );
        assert_eq!(copy_on_select(Some(String::new())), None);
        assert_eq!(copy_on_select(None), None);
    }

    #[test]
    fn the_jump_to_bottom_arrow_shows_only_when_scrolled_up() {
        assert!(!shows_jump_to_bottom(0));
        assert!(shows_jump_to_bottom(1));
        assert!(shows_jump_to_bottom(500));
    }

    #[test]
    fn a_programs_exe_path_isnt_a_title() {
        assert!(is_program_path(
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe"
        ));
        assert!(is_program_path(
            "Administrator: C:\\WINDOWS\\system32\\cmd.exe"
        ));
        assert!(is_program_path(
            "C:\\Program Files\\Git\\usr\\bin\\bash.EXE"
        ));
        assert!(is_program_path("pwsh.exe"));
        assert!(!is_program_path("vim - notes.txt"));
        assert!(!is_program_path("make.exe build"));
        assert!(!is_program_path(""));
    }

    #[test]
    fn positions_outside_the_grid_are_not_cells() {
        let size = GridSize { cols: 80, rows: 24 };
        let cell = (10.0, 20.0);
        assert_eq!(hit_cell(-1.0, 5.0, cell, size), None);
        assert_eq!(hit_cell(5.0, -1.0, cell, size), None);
        assert_eq!(hit_cell(800.0, 5.0, cell, size), None);
        assert_eq!(hit_cell(5.0, 480.0, cell, size), None);
        assert_eq!(hit_cell(26.0, 41.0, cell, size), Some((2, 2, Side::Right)));
    }

    #[test]
    fn cells_are_found_from_offsets_and_clamped_to_the_grid() {
        let size = GridSize { cols: 80, rows: 24 };
        let cell = (10.0, 20.0);
        assert_eq!(grid_cell(0.0, 0.0, cell, size), (0, 0, Side::Left));
        assert_eq!(grid_cell(26.0, 41.0, cell, size), (2, 2, Side::Right));
        assert_eq!(grid_cell(24.0, 41.0, cell, size), (2, 2, Side::Left));
        assert_eq!(grid_cell(-5.0, -5.0, cell, size), (0, 0, Side::Left));
        assert_eq!(grid_cell(5000.0, 5000.0, cell, size), (23, 79, Side::Left));
    }
}
