//! OpenAPI specs as API docs (spec §7.8): a read-only view of an `Api` in the file pane's
//! Rendered mode. What is open is kept here; schema trees are built only when opened.
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, Hsla, SharedString, Subscription,
    Task, Window, div, px,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;

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
                self.api = Some(Rc::new(api));
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
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

    fn heading(text: &'static str) -> gpui::Div {
        div().font_weight(FontWeight::SEMIBOLD).child(text)
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
        if let Some((n, required)) = name {
            head = head
                .child(div().font_family(mono.clone()).child(n))
                .when(required, |d| d.child(div().text_color(danger).child("*")));
        }
        head = head.child(div().text_color(muted).child(shown));
        let head = match &lvl.kind {
            Kind::External { file, .. } => {
                let path = self.dir.join(file);
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

    fn render_operation(&self, api: &Api, op: &Operation, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("op:{}", op.key());
        let open = self.is_open(&key);
        let (muted, accent, mono) = (
            cx.theme().muted_foreground,
            cx.theme().accent,
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
            .hover(move |d| d.bg(accent))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(k.clone(), cx)))
            .child(
                div()
                    .w(px(64.))
                    .flex_none()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(method_colour(&op.method, cx))
                    .child(op.method.clone()),
            )
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
            );
        let mut block = v_flex().child(row);
        if !open {
            return block.into_any_element();
        }
        let mut detail = v_flex().pl(px(84.)).pb_2().gap_2();
        if let Some(d) = &op.description {
            detail = detail.child(div().text_sm().child(d.clone()));
        }
        if !op.parameters.is_empty() {
            detail = detail.child(Self::heading("Parameters"));
            for p in &op.parameters {
                let pkey = format!("{key}/param/{}/{}", p.location, p.name);
                detail = detail.child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            div()
                                .w(px(56.))
                                .flex_none()
                                .text_color(muted)
                                .child(p.location.clone()),
                        )
                        .child(self.render_schema(
                            api,
                            pkey,
                            Some((p.name.clone(), p.required)),
                            &p.schema,
                            Vec::new(),
                            cx,
                        ))
                        .child(
                            div()
                                .text_color(muted)
                                .child(p.description.clone().unwrap_or_default()),
                        ),
                );
            }
        }
        if let Some(body) = &op.request_body {
            detail = detail.child(Self::heading("Request body"));
            if let Some(d) = &body.description {
                detail = detail.child(div().text_color(muted).child(d.clone()));
            }
            for (media, schema) in &body.content {
                detail = detail
                    .child(div().text_sm().text_color(muted).child(media.clone()))
                    .child(self.render_schema(
                        api,
                        format!("{key}/body/{media}"),
                        None,
                        schema,
                        Vec::new(),
                        cx,
                    ));
            }
        }
        if !op.responses.is_empty() {
            detail = detail.child(Self::heading("Responses"));
            for r in &op.responses {
                detail = detail.child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .w(px(56.))
                                .flex_none()
                                .text_color(status_colour(&r.status, cx))
                                .child(r.status.clone()),
                        )
                        .child(
                            div()
                                .text_color(muted)
                                .child(r.description.clone().unwrap_or_default()),
                        ),
                );
                for (media, schema) in &r.content {
                    detail = detail.child(
                        v_flex()
                            .pl(px(68.))
                            .child(div().text_sm().text_color(muted).child(media.clone()))
                            .child(self.render_schema(
                                api,
                                format!("{key}/resp/{}/{media}", r.status),
                                None,
                                schema,
                                Vec::new(),
                                cx,
                            )),
                    );
                }
            }
        }
        block = block.child(detail);
        block.into_any_element()
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
        content = content.child(Input::new(&self.filter).small());
        for g in &api.groups {
            let ops: Vec<&Operation> = g
                .operations
                .iter()
                .filter(|o| matches(o, &filter))
                .collect();
            if ops.is_empty() {
                continue;
            }
            // Groups start open: the key marks a collapsed one.
            let tkey = format!("tag:{}", g.name);
            let collapsed = self.is_open(&tkey);
            let mut group = v_flex().child(self.section_header(
                &tkey,
                g.name.clone(),
                g.description.clone(),
                !collapsed,
                cx,
            ));
            if !collapsed {
                for op in ops {
                    group = group.child(self.render_operation(&api, op, cx));
                }
            }
            content = content.child(group);
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
}
