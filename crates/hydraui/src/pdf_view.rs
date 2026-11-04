//! A PDF file's pages in the file pane (spec §7.8): drawn off the UI thread and shown as they
//! come, fitted to the pane's width, one under another. Only the pages in view are on screen.
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::prelude::*;
use gpui::{Context, ListAlignment, ListState, Task, Window, div, list, px};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::{ActiveTheme, v_flex};

use crate::file_doc;
use crate::pdf::{self, PdfPage};

enum Msg {
    /// The page count and the file's size.
    Count(usize, u64),
    Page(PdfPage),
    Failed(String),
}

struct Shown {
    image: Arc<gpui::Image>,
    width: u32,
    height: u32,
}

pub struct PdfView {
    pages: Vec<Shown>,
    /// How many pages the PDF has and its size, once it parses.
    count: Option<(usize, u64)>,
    failed: Option<String>,
    list: ListState,
    _render: Option<Task<()>>,
}

impl PdfView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            pages: Vec::new(),
            count: None,
            failed: None,
            list: ListState::new(0, ListAlignment::Top, px(1000.)),
            _render: None,
        };
        view.load(path, cx);
        view
    }

    /// Reads and draws the file again (it changed on disk); the pages shown stay until the new
    /// ones come. A newer call stops an older one.
    pub fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self._render = Some(cx.spawn(async move |this, cx| {
            let (tx, mut rx) = unbounded::<Msg>();
            // Drawing stops at its next page once `rx` is gone (the file closed or reloaded).
            let _work = cx.background_spawn(async move {
                let result = pdf::load(&path).and_then(|bytes| {
                    let size = bytes.len() as u64;
                    pdf::render(
                        bytes,
                        |n| tx.unbounded_send(Msg::Count(n, size)).is_ok(),
                        |p| tx.unbounded_send(Msg::Page(p)).is_ok(),
                    )
                });
                if let Err(why) = result {
                    let _ = tx.unbounded_send(Msg::Failed(why));
                }
            });
            while let Some(msg) = rx.next().await {
                if this.update(cx, |v, cx| v.apply(msg, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn apply(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Count(n, size) => {
                self.pages.clear();
                self.list.reset(0);
                self.count = Some((n, size));
                self.failed = None;
            }
            Msg::Page(p) => {
                let at = self.pages.len();
                self.pages.push(Shown {
                    image: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, p.png)),
                    width: p.width,
                    height: p.height,
                });
                self.list.splice(at..at, 1);
            }
            Msg::Failed(why) => {
                self.pages.clear();
                self.list.reset(0);
                self.count = None;
                self.failed = Some(why);
            }
        }
        cx.notify();
    }

    /// What the pane's header says about the file, once it parses.
    pub fn summary(&self) -> Option<String> {
        let (n, size) = self.count?;
        let pages = match n {
            1 => "1 page".to_string(),
            n if n > pdf::MAX_PAGES => format!("{n} pages, the first {} shown", pdf::MAX_PAGES),
            n => format!("{n} pages"),
        };
        Some(format!("PDF, {pages}, {}", file_doc::size_label(size)))
    }
}

impl Render for PdfView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        if let Some(why) = &self.failed {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child(format!("Can't show this PDF: {why}"))
                .into_any_element();
        }
        if self.pages.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child("Drawing the PDF…")
                .into_any_element();
        }
        // Each page fits the pane's width, never drawn bigger than it was rendered.
        let pages: Vec<(Arc<gpui::Image>, u32, u32)> = self
            .pages
            .iter()
            .map(|p| (p.image.clone(), p.width, p.height))
            .collect();
        let pages_list = list(self.list.clone(), move |i, _, _| {
            let (image, w, h) = pages[i].clone();
            v_flex()
                .w_full()
                .items_center()
                .px_3()
                .py_1p5()
                .child(
                    gpui::img(image)
                        .w_full()
                        .max_w(px(w as f32))
                        .aspect_ratio(w as f32 / h.max(1) as f32)
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .into_any_element()
        })
        .size_full();
        div()
            .relative()
            .size_full()
            .bg(cx.theme().secondary)
            .child(pages_list)
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.list)
                        .id("pdf-scrollbar")
                        .mode(ScrollbarMode::Always),
                ),
            )
            .into_any_element()
    }
}
