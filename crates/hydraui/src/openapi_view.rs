//! OpenAPI specs as API docs (spec §7.8): a read-only view of an `Api` in the file pane's
//! Rendered mode. What is open is kept here; schema trees are built only when opened.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, Hsla, SharedString, Subscription,
    Task, Window, div, px, relative,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use serde_json::Value;

use crate::openapi::example::{curl, example, operation_line, rules};
use crate::openapi::{Api, Kind, Operation, ParseError, level, matches, parse};

/// The last good parse, the current error if the text no longer parses, and what is open.
#[derive(Default)]
pub struct ViewState {
    /// Shared, so drawing doesn't copy the whole spec.
    pub api: Option<Rc<Api>>,
    pub error: Option<ParseError>,
    pub open: HashSet<String>,
}

impl ViewState {
    /// A parse result: a good one replaces the API; a failed one keeps the last good API
    /// (shown greyed) beside the error. What is open survives either way.
    pub fn apply(&mut self, result: Result<Api, ParseError>) {
        match result {
            Ok(api) => {
                // A big API starts folded to its tags, so it opens fast and reads as a list;
                // only the first good parse decides, so groups opened since stay open.
                if self.api.is_none() {
                    let count: usize = api.groups.iter().map(|g| g.operations.len()).sum();
                    if count > COLLAPSE_ABOVE {
                        self.open
                            .extend(api.groups.iter().map(|g| format!("tag:{}", g.name)));
                    }
                }
                self.api = Some(Rc::new(api));
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

/// The file a `$ref` names, relative to the spec's folder, with `.` and `..` worked out, so
/// it opens in the same file tab as the file opened any other way.
pub fn ref_file(dir: &std::path::Path, file: &str) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in dir.join(file).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Above this many operations, tag groups start collapsed.
const COLLAPSE_ABOVE: usize = 500;
/// At most this many operation rows are drawn at once; the rest wait for the filter.
pub const ROW_CAP: usize = 300;

/// Which operation rows to draw: each group with matches, as (group index, operation
/// indices); a collapsed group, and every group once `cap` rows are drawn, lists none. A
/// filter looks inside collapsed groups. Also returns how many matching rows the cap hid.
pub fn rows_to_show(
    api: &Api,
    filter: &str,
    open: &HashSet<String>,
    cap: usize,
) -> (Vec<(usize, Vec<usize>)>, usize) {
    let filtering = !filter.trim().is_empty();
    let (mut shown, mut hidden) = (0usize, 0usize);
    let mut out = Vec::new();
    for (gi, g) in api.groups.iter().enumerate() {
        let hits: Vec<usize> = g
            .operations
            .iter()
            .enumerate()
            .filter(|(_, o)| matches(o, filter))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            continue;
        }
        // The `tag:` key marks a collapsed group (groups start open).
        let collapsed = !filtering && open.contains(&format!("tag:{}", g.name));
        let mut rows = Vec::new();
        if !collapsed {
            let room = cap.saturating_sub(shown);
            rows = hits.iter().copied().take(room).collect();
            hidden += hits.len() - rows.len();
            shown += rows.len();
        }
        out.push((gi, rows));
    }
    (out, hidden)
}

pub enum ApiViewEvent {
    /// Go to line: Raw, at this 1-based line.
    GoToLine(usize),
    /// A `$ref` into another file was clicked.
    OpenFile(PathBuf),
}

pub struct ApiView {
    state: ViewState,
    json: bool,
    /// The spec's folder, for refs into other files.
    dir: PathBuf,
    /// The spec's text as last given, to find an operation's line.
    text: Rc<str>,
    /// Each open operation's chosen response, by its `op:` key; the first when unset.
    tabs: HashMap<String, String>,
    filter: Entity<InputState>,
    _filter_sub: Subscription,
    _pending: Option<Task<()>>,
}

impl EventEmitter<ApiViewEvent> for ApiView {}

fn method_colour(method: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    match method {
        "GET" => t.info,
        "POST" => t.success,
        "PUT" => t.warning,
        "PATCH" => gpui::hsla(174. / 360., 0.6, 0.45, 1.),
        "DELETE" => t.danger,
        _ => t.muted_foreground,
    }
}

fn status_colour(status: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    match status.as_bytes().first() {
        Some(b'2') => t.success,
        Some(b'4') => t.warning,
        Some(b'5') => t.danger,
        _ => t.muted_foreground,
    }
}

impl ApiView {
    pub fn new(
        text: String,
        json: bool,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter operations"));
        let sub = cx.subscribe(&filter, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut view = Self {
            state: ViewState::default(),
            json,
            dir,
            text: Rc::from(text.as_str()),
            tabs: HashMap::new(),
            filter,
            _filter_sub: sub,
            _pending: None,
        };
        view.parse_after(text, Duration::ZERO, cx);
        view
    }

    /// Parses off the UI thread after `wait`; a newer call replaces an older one.
    fn parse_after(&mut self, text: String, wait: Duration, cx: &mut Context<Self>) {
        let json = self.json;
        self._pending = Some(cx.spawn(async move |this, cx| {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            let result = cx.background_spawn(async move { parse(&text, json) }).await;
            let _ = this.update(cx, |v, cx| {
                v.state.apply(result);
                cx.notify();
            });
        }));
    }

    /// The text changed (unsaved edits): parse again once typing stops for 300 ms.
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.text = Rc::from(text.as_str());
        self.parse_after(text, Duration::from_millis(300), cx);
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.state.open.remove(&key) {
            self.state.open.insert(key);
        }
        cx.notify();
    }

    fn is_open(&self, key: &str) -> bool {
        self.state.open.contains(key)
    }

    /// A schema node: its label, and when open its next level. `key` names it in `open`;
    /// `above` is the refs open above it, so recursion stops at `↻`.
    fn render_schema(
        &self,
        api: &Api,
        key: String,
        name: Option<(String, bool)>,
        schema: &Value,
        above: Vec<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lvl = level(&api.root, schema, &above);
        let (muted, danger, mono) = (
            cx.theme().muted_foreground,
            cx.theme().danger,
            cx.theme().mono_font_family.clone(),
        );
        let open = self.is_open(&key);
        let expandable = matches!(lvl.kind, Kind::Object(_) | Kind::OneOf(_));
        let arrow = match (expandable, open) {
            (false, _) => " ",
            (true, true) => "▾",
            (true, false) => "▸",
        };
        let shown = match &lvl.kind {
            Kind::Cycle(n) => format!("↻ {n}"),
            Kind::Missing(r) => format!("⚠ {r} (missing)"),
            _ => lvl.label.clone(),
        };
        let mut head = h_flex()
            .id(SharedString::from(key.clone()))
            .gap_2()
            .child(div().w(px(12.)).text_color(muted).child(arrow));
        // A schema listed by its own name (Schemas) doesn't repeat it as its type.
        let repeats = name.as_ref().is_some_and(|(n, _)| *n == shown);
        if let Some((n, required)) = name {
            head = head
                .child(div().font_family(mono.clone()).child(n))
                .when(required, |d| d.child(div().text_color(danger).child("*")));
        }
        if !repeats {
            head = head.child(div().text_color(muted).child(shown));
        }
        let head = match &lvl.kind {
            Kind::External { file, .. } => {
                let path = ref_file(&self.dir, file);
                head.cursor_pointer().underline().on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(ApiViewEvent::OpenFile(path.clone()))),
                )
            }
            _ if expandable => {
                let k = key.clone();
                head.cursor_pointer()
                    .on_click(cx.listener(move |v, _, _, cx| v.toggle(k.clone(), cx)))
            }
            _ => head,
        };
        let mut node = v_flex().child(head);
        if open {
            let mut above = above;
            for r in [
                schema.get("$ref").and_then(Value::as_str),
                schema.pointer("/items/$ref").and_then(Value::as_str),
            ]
            .into_iter()
            .flatten()
            {
                above.push(r.to_string());
            }
            let children: Vec<AnyElement> = match &lvl.kind {
                Kind::Object(props) => props
                    .iter()
                    .map(|p| {
                        self.render_schema(
                            api,
                            format!("{key}/{}", p.name),
                            Some((p.name.clone(), p.required)),
                            &p.schema,
                            above.clone(),
                            cx,
                        )
                    })
                    .collect(),
                Kind::OneOf(choices) => choices
                    .iter()
                    .enumerate()
                    .map(|(i, (_, s))| {
                        self.render_schema(api, format!("{key}/{i}"), None, s, above.clone(), cx)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            node = node.child(v_flex().pl(px(16.)).children(children));
        }
        node.into_any_element()
    }

    /// A small caps label over a part of an operation.
    fn label(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
    }

    /// A rule as a chip: `max 200 chars`, `≥ 1`.
    fn chip(text: String, cx: &App) -> gpui::Div {
        div()
            .flex_none()
            .px_1p5()
            .rounded_sm()
            .text_xs()
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .child(text)
    }

    /// The column headings of a field table.
    fn table_head(first: &'static str, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .px_2()
            .py_0p5()
            .gap_2()
            .text_xs()
            .text_color(muted)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().w(relative(0.32)).flex_none().child(first))
            .child(div().w(relative(0.24)).flex_none().child("Type"))
            .child(div().flex_1().child("Rules"))
            .into_any_element()
    }

    /// One field of a table: its name (indented by `depth`, `*` when required, `▸` when it
    /// opens), its type, and its rules and description; when open, its own fields below.
    #[allow(clippy::too_many_arguments)]
    fn render_field(
        &self,
        api: &Api,
        key: String,
        name: String,
        required: bool,
        schema: &Value,
        above: Vec<String>,
        depth: usize,
        out: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let lvl = level(&api.root, schema, &above);
        let (muted, warning, info, mono) = (
            cx.theme().muted_foreground,
            cx.theme().warning,
            cx.theme().info,
            cx.theme().mono_font_family.clone(),
        );
        let expandable = matches!(lvl.kind, Kind::Object(_) | Kind::OneOf(_));
        let open = expandable && self.is_open(&key);
        let resolved = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| crate::openapi::resolve(&api.root, r))
            .unwrap_or(schema);
        let shown = match &lvl.kind {
            Kind::Cycle(n) => format!("↻ {n}"),
            Kind::Missing(r) => format!("⚠ {r} (missing)"),
            // An enum's values are a rule chip; its type is just its type.
            _ if resolved.get("enum").is_some() && !lvl.label.ends_with("[]") => resolved
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("enum")
                .to_string(),
            _ => lvl.label.clone(),
        };
        let description = schema
            .get("description")
            .or_else(|| resolved.get("description"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let chips: Vec<AnyElement> = rules(resolved)
            .into_iter()
            .map(|r| Self::chip(r, cx).into_any_element())
            .collect();
        let arrow = match (expandable, open) {
            (false, _) => " ",
            (true, true) => "▾",
            (true, false) => "▸",
        };
        let mut row = h_flex()
            .id(SharedString::from(key.clone()))
            .px_2()
            .py_0p5()
            .gap_2()
            .items_start()
            .text_sm()
            .child(
                h_flex()
                    .w(relative(0.32))
                    .flex_none()
                    .min_w_0()
                    .pl(px(depth as f32 * 14.))
                    .gap_1()
                    .child(div().w(px(10.)).flex_none().text_color(muted).child(arrow))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .child(name),
                    )
                    .when(required, |d| d.child(div().text_color(warning).child("*"))),
            )
            .child(
                div()
                    .w(relative(0.24))
                    .flex_none()
                    .min_w_0()
                    .truncate()
                    .font_family(mono)
                    .text_color(info)
                    .child(shown),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .when(!chips.is_empty(), |d| {
                        d.child(h_flex().flex_wrap().gap_1().children(chips))
                    })
                    .when_some(description, |d, text| {
                        d.child(div().text_color(muted).child(text))
                    }),
            );
        match &lvl.kind {
            Kind::External { file, .. } => {
                let path = ref_file(&self.dir, file);
                row =
                    row.cursor_pointer()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ApiViewEvent::OpenFile(path.clone()))
                        }));
            }
            _ if expandable => {
                let k = key.clone();
                row = row
                    .cursor_pointer()
                    .on_click(cx.listener(move |v, _, _, cx| v.toggle(k.clone(), cx)));
            }
            _ => {}
        }
        out.push(row.into_any_element());
        if !open {
            return;
        }
        let mut above = above;
        for r in [
            schema.get("$ref").and_then(Value::as_str),
            schema.pointer("/items/$ref").and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            above.push(r.to_string());
        }
        match &lvl.kind {
            Kind::Object(props) => {
                for p in props {
                    self.render_field(
                        api,
                        format!("{key}/{}", p.name),
                        p.name.clone(),
                        p.required,
                        &p.schema,
                        above.clone(),
                        depth + 1,
                        out,
                        cx,
                    );
                }
            }
            Kind::OneOf(choices) => {
                for (i, (_, s)) in choices.iter().enumerate() {
                    self.render_field(
                        api,
                        format!("{key}/{i}"),
                        format!("option {}", i + 1),
                        false,
                        s,
                        above.clone(),
                        depth + 1,
                        out,
                        cx,
                    );
                }
            }
            _ => {}
        }
    }

    /// A schema as a field table: an object lists its fields; anything else is one row.
    fn render_fields(
        &self,
        api: &Api,
        key: String,
        schema: &Value,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let border = cx.theme().border;
        let mut rows = vec![Self::table_head("Field", cx)];
        let lvl = level(&api.root, schema, &[]);
        let mut above = Vec::new();
        for r in [
            schema.get("$ref").and_then(Value::as_str),
            schema.pointer("/items/$ref").and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            above.push(r.to_string());
        }
        match &lvl.kind {
            Kind::Object(props) => {
                // An array of objects says so above its item's fields.
                if lvl.label.ends_with("[]") {
                    rows.insert(
                        0,
                        Self::label(
                            format!("A list of {}", lvl.label.trim_end_matches("[]")),
                            cx,
                        )
                        .px_2()
                        .into_any_element(),
                    );
                }
                for p in props {
                    self.render_field(
                        api,
                        format!("{key}/{}", p.name),
                        p.name.clone(),
                        p.required,
                        &p.schema,
                        above.clone(),
                        0,
                        &mut rows,
                        cx,
                    );
                }
            }
            _ => self.render_field(
                api,
                key,
                "(body)".into(),
                false,
                schema,
                Vec::new(),
                0,
                &mut rows,
                cx,
            ),
        }
        v_flex()
            .border_1()
            .border_color(border)
            .rounded_md()
            .py_0p5()
            .children(rows)
            .into_any_element()
    }

    /// An example value as indented JSON, in a code block.
    fn render_example(value: &Value, cx: &App) -> AnyElement {
        let text = serde_json::to_string_pretty(value).unwrap_or_default();
        v_flex()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .font_family(cx.theme().mono_font_family.clone())
            .text_sm()
            .children(
                text.lines()
                    .map(|l| div().whitespace_nowrap().child(l.to_string()))
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    /// The method as a filled badge.
    fn method_badge(method: &str, cx: &App) -> gpui::Div {
        div()
            .w(px(58.))
            .flex_none()
            .py_0p5()
            .rounded_sm()
            .text_center()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .bg(method_colour(method, cx))
            .text_color(cx.theme().background)
            .child(method.to_string())
    }

    fn render_operation(&self, api: &Api, op: &Operation, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("op:{}", op.key());
        let open = self.is_open(&key);
        let (muted, accent, border, mono) = (
            cx.theme().muted_foreground,
            cx.theme().accent,
            cx.theme().border,
            cx.theme().mono_font_family.clone(),
        );
        let k = key.clone();
        let row = h_flex()
            .id(SharedString::from(key.clone()))
            .gap_3()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(!open, |d| d.hover(move |d| d.bg(accent)))
            .when(op.deprecated, |d| d.opacity(0.6))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(k.clone(), cx)))
            .child(Self::method_badge(&op.method, cx))
            .child(
                div()
                    .flex_none()
                    .font_family(mono)
                    .when(op.deprecated, |d| d.line_through())
                    .child(op.path.clone()),
            )
            .child(
                div()
                    .min_w_0()
                    .text_color(muted)
                    .truncate()
                    .child(op.summary.clone().unwrap_or_default()),
            )
            .when(op.deprecated, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded_sm()
                        .border_1()
                        .border_color(muted)
                        .text_xs()
                        .text_color(muted)
                        .child("deprecated"),
                )
            });
        if !open {
            return v_flex().child(row).into_any_element();
        }
        // The open operation is a card: its row, then what to do with it, then its parts.
        let line = operation_line(&self.text, &op.path, &op.method);
        let command = curl(api, op);
        let actions = h_flex()
            .gap_1()
            .justify_end()
            .child(
                Button::new(SharedString::from(format!("{key}/curl")))
                    .label("Copy as curl")
                    .xsmall()
                    .on_click(move |_, window, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()));
                        window.push_notification(
                            Notification::success("The curl command is on the clipboard"),
                            cx,
                        );
                    }),
            )
            .when_some(line, |d, l| {
                d.child(
                    Button::new(SharedString::from(format!("{key}/line")))
                        .label(format!("Go to line {l}"))
                        .xsmall()
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(ApiViewEvent::GoToLine(l))),
                        ),
                )
            });
        let mut detail = v_flex().px_3().pb_3().gap_2().child(actions);
        // A path item defined elsewhere: its ref, as a link to the file (or ⚠ missing).
        if let Some(r) = &op.reference {
            detail = detail.child(self.render_schema(
                api,
                format!("{key}/ref"),
                None,
                &serde_json::json!({ "$ref": r }),
                Vec::new(),
                cx,
            ));
        }
        if let Some(d) = &op.description {
            detail = detail.child(div().text_sm().child(d.clone()));
        }
        if !op.parameters.is_empty() {
            let mut rows = vec![Self::table_head("Parameter", cx)];
            for p in &op.parameters {
                let mut schema = p.schema.clone();
                if let (Some(d), Value::Object(m)) = (&p.description, &mut schema) {
                    m.entry("description").or_insert(Value::String(d.clone()));
                }
                self.render_field(
                    api,
                    format!("{key}/param/{}/{}", p.location, p.name),
                    format!("{} ({})", p.name, p.location),
                    p.required,
                    &schema,
                    Vec::new(),
                    0,
                    &mut rows,
                    cx,
                );
            }
            detail = detail.child(Self::label("PARAMETERS", cx)).child(
                v_flex()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .py_0p5()
                    .children(rows),
            );
        }
        if let Some(body) = &op.request_body {
            for (media, schema) in &body.content {
                detail = detail
                    .child(Self::label(format!("REQUEST BODY · {media}"), cx))
                    .when_some(body.description.clone(), |d, text| {
                        d.child(div().text_sm().text_color(muted).child(text))
                    })
                    .child(self.render_fields(api, format!("{key}/body/{media}"), schema, cx));
            }
        }
        if !op.responses.is_empty() {
            let chosen = self
                .tabs
                .get(&key)
                .and_then(|s| op.responses.iter().find(|r| &r.status == s))
                .unwrap_or(&op.responses[0]);
            let example_mode = self.is_open(&format!("{key}/example"));
            let mut tabs = h_flex()
                .gap_1()
                .flex_wrap()
                .child(Self::label("RESPONSES", cx).mr_2());
            for r in &op.responses {
                let picked = r.status == chosen.status;
                let colour = status_colour(&r.status, cx);
                let (k, status) = (key.clone(), r.status.clone());
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("{key}/tab/{}", r.status)))
                        .px_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(if picked { colour } else { border })
                        .when(picked, |d| d.bg(colour.opacity(0.15)))
                        .text_sm()
                        .text_color(colour)
                        .cursor_pointer()
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.tabs.insert(k.clone(), status.clone());
                            cx.notify();
                        }))
                        .child(match &r.description {
                            Some(d) => format!("{} {}", r.status, d),
                            None => r.status.clone(),
                        }),
                );
            }
            let has_content = !chosen.content.is_empty();
            if has_content {
                let ek = format!("{key}/example");
                tabs = tabs.child(div().flex_1()).child(
                    Button::new(SharedString::from(format!("{key}/mode")))
                        .label(if example_mode { "Schema" } else { "Example" })
                        .xsmall()
                        .on_click(cx.listener(move |v, _, _, cx| v.toggle(ek.clone(), cx))),
                );
            }
            detail = detail.child(tabs);
            if !has_content {
                detail = detail.child(div().text_sm().text_color(muted).child("No body"));
            }
            for (media, schema) in &chosen.content {
                detail = detail.child(Self::label(media.clone(), cx));
                detail = detail.child(if example_mode {
                    Self::render_example(&example(&api.root, schema), cx)
                } else {
                    self.render_fields(
                        api,
                        format!("{key}/resp/{}/{media}", chosen.status),
                        schema,
                        cx,
                    )
                });
            }
        }
        v_flex()
            .my_1()
            .border_1()
            .border_color(border)
            .rounded_md()
            .child(
                div()
                    .rounded_t_md()
                    .bg(method_colour(&op.method, cx).opacity(0.08))
                    .child(row),
            )
            .child(detail)
            .into_any_element()
    }

    fn section_header(
        &self,
        key: &str,
        title: String,
        note: Option<String>,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
        let k = key.to_string();
        h_flex()
            .id(SharedString::from(key.to_string()))
            .gap_2()
            .cursor_pointer()
            .border_b_1()
            .border_color(border)
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(k.clone(), cx)))
            .child(div().text_color(muted).child(if open { "▾" } else { "▸" }))
            .child(div().text_lg().child(title))
            .child(
                div()
                    .min_w_0()
                    .text_sm()
                    .text_color(muted)
                    .truncate()
                    .child(note.unwrap_or_default()),
            )
            .into_any_element()
    }
}

impl Render for ApiView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, warning, mono) = (
            cx.theme().muted_foreground,
            cx.theme().warning,
            cx.theme().mono_font_family.clone(),
        );
        let mut page = v_flex()
            .id("api")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .gap_3();
        if let Some(e) = &self.state.error {
            let line = e.line;
            let text = match line {
                Some(l) => format!("Line {l}: {}", e.message),
                None => e.message.clone(),
            };
            page = page.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(warning)
                    .child(div().min_w_0().child(text))
                    .when_some(line, |d, l| {
                        d.child(
                            Button::new("go-to-line")
                                .label("Go to line")
                                .xsmall()
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ApiViewEvent::GoToLine(l))
                                })),
                        )
                    }),
            );
        }
        let Some(api) = self.state.api.clone() else {
            return page;
        };
        if !api.missing.is_empty() {
            page = page.child(
                div()
                    .text_sm()
                    .text_color(warning)
                    .child(format!("This spec has no {}", api.missing.join(" or "))),
            );
        }
        let filter = self.filter.read(cx).value().to_string();
        let mut content = v_flex()
            .gap_3()
            .when(self.state.error.is_some(), |d| d.opacity(0.5));
        content = content.child(
            h_flex()
                .gap_2()
                .items_baseline()
                .child(div().text_xl().child(api.title.clone()))
                .child(div().text_color(muted).child(api.version.clone())),
        );
        if let Some(d) = &api.description {
            content = content.child(
                gpui_base::TextView::markdown("api-description", d.clone())
                    .selectable(true)
                    // Web and mail links open in the browser; nothing else runs.
                    .on_link_click(|url, _, _, cx| {
                        if crate::file_doc::link_opens(url) {
                            cx.open_url(url);
                        }
                    }),
            );
        }
        for s in &api.servers {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .font_family(mono.clone())
                    .child(s.clone()),
            );
        }
        if !api.groups.is_empty() {
            // Expand all opens every operation; once all are open it closes them.
            let keys: Vec<String> = api
                .groups
                .iter()
                .flat_map(|g| &g.operations)
                .map(|o| format!("op:{}", o.key()))
                .collect();
            let all_open = keys.iter().all(|k| self.is_open(k));
            content = content.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.filter).small()))
                    .child(
                        Button::new("expand-all")
                            .label(if all_open {
                                "Collapse all"
                            } else {
                                "Expand all"
                            })
                            .small()
                            .on_click(cx.listener(move |v, _, _, cx| {
                                for k in &keys {
                                    if all_open {
                                        v.state.open.remove(k);
                                    } else {
                                        v.state.open.insert(k.clone());
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        let (groups, hidden) = rows_to_show(&api, &filter, &self.state.open, ROW_CAP);
        for (gi, ops) in groups {
            let g = &api.groups[gi];
            // Groups start open: the key marks a collapsed one. A filter shows its matches
            // inside collapsed groups too.
            let tkey = format!("tag:{}", g.name);
            let shown_open = !self.is_open(&tkey) || !filter.trim().is_empty();
            let mut group = v_flex().child(self.section_header(
                &tkey,
                g.name.clone(),
                g.description.clone(),
                shown_open,
                cx,
            ));
            for i in ops {
                group = group.child(self.render_operation(&api, &g.operations[i], cx));
            }
            content = content.child(group);
        }
        if hidden > 0 {
            content = content.child(div().text_sm().text_color(muted).child(format!(
                "{hidden} more operations: narrow the filter to see them"
            )));
        }
        if !api.schemas.is_empty() {
            let open = self.is_open("schemas");
            content =
                content.child(self.section_header("schemas", "Schemas".into(), None, open, cx));
            if open {
                for name in &api.schemas {
                    let pointer = format!(
                        "#/components/schemas/{}",
                        name.replace('~', "~0").replace('/', "~1")
                    );
                    let r = serde_json::json!({ "$ref": pointer });
                    content = content.child(self.render_schema(
                        &api,
                        format!("schema:{name}"),
                        Some((name.clone(), false)),
                        &r,
                        Vec::new(),
                        cx,
                    ));
                }
            }
        }
        page.child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::parse;

    const ONE: &str = "openapi: 3.0.0\ninfo: {title: A, version: '1'}\npaths:\n  /a:\n    get:\n      responses: {'200': {description: ok}}\n";

    #[test]
    fn referenced_files_open_under_their_plain_path() {
        let dir = std::path::Path::new(r"C:\x\api");
        assert_eq!(
            ref_file(dir, "./common.yaml"),
            PathBuf::from(r"C:\x\api\common.yaml")
        );
        assert_eq!(
            ref_file(dir, "../shared/./errors.yaml"),
            PathBuf::from(r"C:\x\shared\errors.yaml")
        );
        assert_eq!(
            ref_file(dir, "paths/pets.yaml"),
            PathBuf::from(r"C:\x\api\paths\pets.yaml")
        );
    }

    #[test]
    fn a_failed_parse_keeps_the_last_good_api() {
        let mut s = ViewState::default();
        s.apply(parse(ONE, false));
        s.open.insert("op:GET /a".into());
        s.apply(parse("openapi: 3.0.0\ninfo: [\n", false));
        assert!(s.error.is_some());
        assert_eq!(s.api.as_ref().unwrap().title, "A", "last good kept");
        s.apply(parse(&ONE.replace("title: A", "title: B"), false));
        assert!(s.error.is_none());
        assert_eq!(s.api.as_ref().unwrap().title, "B");
        assert!(s.open.contains("op:GET /a"), "open rows stay open");
    }

    /// `tags` groups of `per` operations each (GET /t<g>/<i>).
    fn spec(tags: usize, per: usize) -> String {
        let mut s = String::from("openapi: 3.0.0\ninfo: {title: B, version: '1'}\npaths:\n");
        for g in 0..tags {
            for i in 0..per {
                s.push_str(&format!(
                    "  /t{g}/{i}:\n    get:\n      tags: [t{g}]\n      responses: {{'200': {{description: ok}}}}\n"
                ));
            }
        }
        s
    }

    #[test]
    fn a_big_api_starts_with_its_groups_collapsed() {
        let mut big = ViewState::default();
        big.apply(parse(&spec(6, 100), false));
        assert!(
            (0..6).all(|g| big.open.contains(&format!("tag:t{g}"))),
            "600 operations"
        );
        let mut small = ViewState::default();
        small.apply(parse(&spec(2, 10), false));
        assert!(small.open.is_empty(), "20 operations stay open");
        // Only the first good parse decides: a group opened by hand stays open after edits.
        big.open.remove("tag:t0");
        big.apply(parse(&spec(6, 100), false));
        assert!(!big.open.contains("tag:t0"));
    }

    #[test]
    fn rows_are_capped_and_a_filter_looks_inside_collapsed_groups() {
        let api = parse(&spec(3, 200), false).unwrap();
        let none = HashSet::new();
        let (groups, hidden) = rows_to_show(&api, "", &none, 300);
        let shown: usize = groups.iter().map(|(_, ops)| ops.len()).sum();
        assert_eq!((groups.len(), shown, hidden), (3, 300, 300));
        assert!(
            groups[2].1.is_empty(),
            "past the cap a group shows only its header"
        );
        let collapsed: HashSet<String> = ["tag:t0", "tag:t1", "tag:t2"].map(String::from).into();
        let (groups, hidden) = rows_to_show(&api, "", &collapsed, 300);
        assert!(groups.iter().all(|(_, ops)| ops.is_empty()));
        assert_eq!(hidden, 0, "collapsed rows aren't counted as hidden");
        let (groups, hidden) = rows_to_show(&api, "/t1/19", &collapsed, 300);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            (groups[0].0, groups[0].1.len(), hidden),
            (1, 11, 0),
            "/t1/19, /t1/190..199"
        );
    }
}
