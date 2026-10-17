# GPUI API notes — gpui-pre 0.3.9, gpui-component 0.7.3

Collected from the pinned sources (`~/.cargo/registry/src/*/gpui-pre-0.3.9`, `gpui-component-0.7.3`, `gpui-base-0.7.3`) for hydraui plans 3–4. The API changes between versions; check the source when something doesn't compile.

## App and window
- `gpui_platform::application().with_assets(gpui_kit_assets::Assets).run(|cx: &mut App| ..)`. Without the assets, gpui-component's built-in icons render blank.
- `gpui_component::init(cx)` before opening a window; wrap the root view: `cx.new(|cx| gpui_component::Root::new(view, window, cx))`. Root draws dialogs, notifications and tooltips itself.
- `cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))), titlebar: Some(TitlebarOptions { title: Some(..), ..Default::default() }), window_min_size: Some(size(..)), ..Default::default() }, |window, cx| -> Entity<V>)`.
- Quit with the last window: `cx.on_window_closed(|cx, _| if cx.windows().is_empty() { cx.quit() }).detach()`.
- `window.set_window_title(&str)`.

## Theme (gpui-component)
- It starts in light mode: `Theme::change(ThemeMode::Dark, None, cx)`.
- Colours: `cx.theme().background`, `foreground`, `muted_foreground`, `border`, `danger`, `warning`, `tab_bar`, `accent` … (`ActiveTheme`).

## Actions and keys
- `gpui::actions!(ns, [A, B])` for unit actions; `#[derive(Action)]` (+ `#[action(no_json)]`) for data.
- `KeyBinding::new("ctrl-shift-d", A, None)` panics on a bad string; `KeyBinding::load(keys, Box<dyn Action>, None, false, None, &DummyKeyboardMapper) -> Result<_, InvalidKeystrokeError>` for user keys. `cx.bind_keys(..)`.
- Keystroke syntax: `[ctrl-][alt-][shift-][win-]key`, chords space-separated; named keys `enter escape backspace tab space up down left right home end pageup pagedown insert delete f1..f24`. `Keystroke::parse` accepts any key word, rejects a non-modifier before the last component.
- Dispatch: interceptors → keymap match along the focus path → if an action handler takes it, done → else `on_key_down` (capture root→focused, bubble focused→root). **A bound key never reaches `on_key_down`.**
- Element handlers: `.key_context("Ctx")`, `.on_action(cx.listener(Self::f))` with `fn f(&mut self, &A, &mut Window, &mut Context<Self>)`.

## Text input in a custom view
- `KeyDownEvent { keystroke: Keystroke { key, key_char, modifiers }, is_held, prefer_character_input }`.
- On Windows, `cx.stop_propagation()` in `on_key_down` also suppresses `WM_CHAR`. Leave plain text to propagate; it arrives at the `EntityInputHandler` registered each paint with `window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)`.
- `prefer_character_input` is true for AltGr-style input: don't encode it, let it through.

## Focus
- `cx.focus_handle()`, `.track_focus(&h)`, `window.focus(&h, cx)`, `h.is_focused(window)` (exactly this handle, not descendants).
- `cx.on_focus(&h, window, |this, window, cx| ..) -> Subscription` (also `on_blur`, `on_focus_in`, `on_focus_out`).

## Mouse
- `.on_any_mouse_down`, `.on_mouse_down(MouseButton::Left, ..)`, `.on_mouse_up(button, ..)`, `.on_mouse_move`, `.on_scroll_wheel`. Move/up element listeners only fire while hovered.
- Drags: register `window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| ..)` in a canvas paint closure (filter `phase == DispatchPhase::Bubble`); or `window.capture_pointer(hitbox.id)`.
- `MouseDownEvent { button, position, modifiers, click_count }`, `MouseMoveEvent { position, pressed_button, modifiers }` with `dragging()`, `ScrollWheelEvent { delta: ScrollDelta::Lines(Point<f32>) | Pixels(Point<Pixels>), .. }`.
- Cursor: `.cursor(CursorStyle::ResizeLeftRight)`, `.cursor_text()`, `.cursor_pointer()`.

## Painting
- `canvas(|bounds, window, cx| -> T, |bounds, T, window, cx| ..)`; `window.paint_quad(fill(bounds, hsla))`, `outline(bounds, hsla, BorderStyle::default())`.
- `window.text_system().shape_line(text: SharedString, font_size, &[TextRun], force_width: Option<Pixels>) -> ShapedLine`; `line.paint(origin, line_height, TextAlign::Left, None, window, cx)`. `TextRun { len /* bytes */, font, color, background_color, underline: Option<UnderlineStyle>, strikethrough: Option<StrikethroughStyle> }` (Default). `font(family).bold().italic()`.
- Cell size: shape "M" and use `width`, `(ascent + descent) * 1.2`.
- `Pixels`: `px(f32)`, `f32::from(p)`, `Pixels / Pixels -> f32`, `Pixels * f32`.
- A child's bounds: put `canvas(move |bounds, _, _| store(bounds), |_, _, _, _| {}).absolute().top_0().left_0().size_full()` inside it.

## Entities and async
- `cx.new(|cx| ..)`, `entity.update(cx, |v, cx| ..)`, `entity.read(cx)`, `weak.update(cx, ..) -> anyhow::Result<_>`.
- `cx.spawn(async move |this: WeakEntity<T>, cx: &mut AsyncApp| ..) -> Task<_>` (store or `.detach()`); `cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| this.update_in(cx, |v, window, cx| ..))`.
- `cx.background_spawn(fut) -> Task<R>` (Send), `cx.background_executor().timer(Duration)`.
- Events: `impl EventEmitter<E> for V {}`, `cx.emit(e)`, `cx.subscribe_in(&entity, window, |this, entity, ev: &E, window, cx| ..) -> Subscription`.
- Don't block a background executor thread forever (e.g. on a channel): use a dedicated thread feeding an async channel.

## Clipboard, URLs
- `cx.write_to_clipboard(ClipboardItem::new_string(s))`, `cx.read_from_clipboard().and_then(|i| i.text())`, `cx.open_url(&url)`.

## gpui-component widgets
- Input: `cx.new(|cx| InputState::new(window, cx).placeholder("..").masked(true))`, `Input::new(&state).small()`, `state.read(cx).value()`, `state.update(cx, |s, cx| s.focus(window, cx))`, events `InputEvent::{Change, PressEnter { secondary, shift }, Focus, Blur}` (Enter propagates after the event).
- Command palette: `CommandState::new(window, cx)`; `Command::new(&state).items(iter of CommandItem::new().label(..)).placeholder(..).on_confirm(|ix: IndexPath, window, cx| ..)`; `ix.row` is the original index. Substring matching only.
- Dialog: `window.open_dialog(cx, move |dialog, window, cx| dialog.title(..).child(..))` (`use gpui_component::WindowExt`); the builder re-runs on each rebuild, so create entities first and focus once. `window.close_dialog(cx)`. Escape closes it.
- Toasts: `window.push_notification(Notification::warning(msg).title(..), cx)` (`info`, `error`, `success`).
- `Button::new(id).label(..).small().ghost().on_click(cx.listener(|this, _, window, cx| ..))` (`ButtonVariants`, `Sizable`).
- `h_flex()`, `v_flex()`. Resizable panels work in pixels and key their state by element id, so hydraui draws its own split dividers.
