//! The Projects tab (Projects spec §8, §9.1): every environment's projects, one project's
//! milestones, decisions and progress, a milestone's tickets, what waits on you, a project's
//! settings, and New project with Claude. Reading and writing the repo is `crate::projects`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, ExternalPaths, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{Selectable, Sizable, WindowExt, h_flex, v_flex};
use hydra_core::name::EnvName;
use hydraui_projects::progress::{Counts, is_late, milestone_counts, today};
use hydraui_projects::{
    Decision, MilestoneStatus, Project, ProjectStatus, Ticket, TicketKind, TicketStatus, repo,
};

use super::Workspace;
use crate::envs::PaneId;
use crate::hydra::{PaneRequest, command_line};
use crate::mcp::project_tools::Draft;
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::projects as pj;

/// How often the checkouts are brought up to date while hydraui is open (spec §3).
const FETCH_EVERY: Duration = Duration::from_secs(5 * 60);

/// What the Projects tab shows.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Page {
    Home,
    Project {
        env: String,
        slug: String,
    },
    Milestone {
        env: String,
        slug: String,
        id: String,
    },
    Settings {
        env: String,
        slug: String,
    },
    Waiting,
    Draft {
        env: String,
        dir: PathBuf,
    },
}

pub(super) struct ProjectsTab {
    page: Page,
    /// Paused and done projects show too.
    show_all: bool,
}

/// One environment's projects, as last read.
#[derive(Default)]
pub(super) struct EnvProjects {
    repo: String,
    checkout: PathBuf,
    drafts: PathBuf,
    /// The environment's GitHub login; `None` until known.
    login: Option<String>,
    /// Why the last fetch failed, if it did.
    problem: Option<String>,
    fetched: bool,
    projects: Vec<Project>,
    /// Project folders that don't read, with why.
    broken: Vec<(String, String)>,
    /// Files changed and not sent yet (slice 1: the checkout's working tree).
    pending: BTreeSet<String>,
    /// Drafts waiting in `drafts`, by folder.
    draft_dirs: Vec<PathBuf>,
}

#[derive(Default)]
pub(super) struct ProjectsData {
    envs: BTreeMap<String, EnvProjects>,
    next_fetch: Option<Instant>,
    fetching: bool,
    /// New project sessions, by draft folder.
    sessions: BTreeMap<PathBuf, PaneId>,
    /// The draft shown on a Draft page, read again every second.
    preview: Option<(PathBuf, Result<Project, String>)>,
}

impl ProjectsData {
    /// Questions assigned to you, across every environment.
    pub(super) fn waiting_count(&self) -> usize {
        self.envs
            .values()
            .filter_map(|e| Some(pj::questions_for(e.login.as_deref()?, &e.projects).len()))
            .sum()
    }
}

impl Workspace {
    pub(super) fn projects_shown(&self) -> bool {
        self.projects.is_some()
    }

    pub(super) fn open_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        if self.projects.is_none() {
            self.projects = Some(ProjectsTab {
                page: Page::Home,
                show_all: false,
            });
        }
        self.projects_data.next_fetch = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.projects = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// An environment's `[projects] repo` changed: read them all again.
    pub(super) fn projects_config_changed(&mut self, cx: &mut Context<Self>) {
        self.projects_data.next_fetch = None;
        cx.notify();
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if let Some(tab) = &mut self.projects {
            tab.page = page;
        }
        cx.notify();
    }

    /// Every second: the checkouts when they're due (on open, then every 5 minutes), and the
    /// draft shown, which its Claude session is writing.
    pub(super) fn projects_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Page::Draft { dir, .. }) = self.projects.as_ref().map(|t| t.page.clone()) {
            let fresh = repo::load(&dir).map_err(|e| e.to_string());
            if self.projects_data.preview.as_ref() != Some(&(dir.clone(), fresh.clone())) {
                self.projects_data.preview = Some((dir, fresh));
                cx.notify();
            }
        }
        let data = &self.projects_data;
        if data.fetching || data.next_fetch.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let jobs: Vec<(String, String, PathBuf, Option<String>)> = self
            .hydra
            .env_names()
            .into_iter()
            .filter_map(|env| {
                let repo = self.hydra.projects_repo(&env)?;
                let state = self.hydra.paths.state_dir(&EnvName::parse(&env).ok()?);
                let login = data
                    .envs
                    .get(&env)
                    .and_then(|e| e.login.clone())
                    .filter(|_| data.envs.get(&env).is_some_and(|e| e.repo == repo));
                Some((env, repo, state, login))
            })
            .collect();
        self.projects_data
            .envs
            .retain(|env, _| jobs.iter().any(|j| &j.0 == env));
        if jobs.is_empty() {
            self.projects_data.next_fetch = Some(Instant::now() + FETCH_EVERY);
            return;
        }
        self.projects_data.fetching = true;
        let exe = server::get()
            .and_then(|sv| server::lock(sv.registry()).hydra_exe.clone())
            .unwrap_or_else(|| PathBuf::from("hydra"));
        cx.spawn_in(window, async move |this, cx| {
            let fetched = cx
                .background_spawn(async move {
                    jobs.into_iter()
                        .map(|(env, repo, state, login)| {
                            let read = fetch_env(&exe, &env, &repo, &state, login);
                            (env, read)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                for (env, read) in fetched {
                    ws.projects_data.envs.insert(env, read);
                }
                ws.projects_data.fetching = false;
                ws.projects_data.next_fetch = Some(Instant::now() + FETCH_EVERY);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads `env`'s checkout again after an edit here (local, so at once).
    fn reload_env(&mut self, env: &str, cx: &mut Context<Self>) {
        if let Some(e) = self.projects_data.envs.get_mut(env) {
            read_checkout(e);
        }
        cx.notify();
    }

    /// Runs `edit` on the folder of project `slug` in `env`, then reads it again; says what
    /// went wrong if it fails.
    fn edit_project(
        &mut self,
        env: &str,
        slug: &str,
        edit: impl FnOnce(&Path, &Project, &str) -> Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (dir, project, login) = {
            let e = self
                .projects_data
                .envs
                .get(env)
                .ok_or_else(|| format!("{env} has no projects"))?;
            let p = e
                .projects
                .iter()
                .find(|p| p.slug == slug)
                .ok_or_else(|| format!("There's no project {slug} any more."))?;
            let login = e
                .login
                .clone()
                .ok_or("hydraui doesn't know your GitHub login yet: is gh signed in?")?;
            (e.checkout.join(slug), p.clone(), login)
        };
        let r = edit(&dir, &project, &login);
        if let Err(e) = &r {
            window.push_notification(Notification::error(e.clone()), cx);
        }
        self.reload_env(env, cx);
        r
    }

    // ---- New project (spec §9.1) ----

    fn new_project(&mut self, env: String, window: &mut Window, cx: &mut Context<Self>) {
        self.text_dialog(
            format!("New project in {env}"),
            String::new(),
            "the project's name",
            move |ws, name, window, cx| {
                let (drafts, login) = {
                    let e = ws
                        .projects_data
                        .envs
                        .get(&env)
                        .ok_or_else(|| format!("{env} has no projects repo"))?;
                    let login = e.login.clone().ok_or(
                        "hydraui doesn't know your GitHub login yet: is gh signed in for this environment?",
                    )?;
                    (e.drafts.clone(), login)
                };
                let dir = pj::start_draft(&drafts, &name, &login)?;
                ws.reload_env(&env, cx);
                ws.open_draft(env.clone(), dir, true, window, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    /// Shows draft `dir`, opening its Claude session if it has none (`fresh`: the draft was
    /// just started, rather than picked up again).
    fn open_draft(
        &mut self,
        env: String,
        dir: PathBuf,
        fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let alive = self
            .projects_data
            .sessions
            .get(&dir)
            .is_some_and(|id| self.panes.contains_key(id));
        if !alive && let Err(e) = self.start_draft_session(&env, &dir, fresh, window, cx) {
            window.push_notification(Notification::error(e), cx);
        }
        self.projects_data.preview = None;
        self.go(Page::Draft { env, dir }, cx);
    }

    fn start_draft_session(
        &mut self,
        env: &str,
        dir: &Path,
        fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let p = repo::load(dir).map_err(|e| e.to_string())?;
        let login = self
            .projects_data
            .envs
            .get(env)
            .and_then(|e| e.login.clone())
            .unwrap_or_else(|| p.file.lead.clone());
        let shell = self.shell();
        let mut request = PaneRequest::new(env, dir.to_path_buf(), shell);
        let argv = vec!["claude".to_string(), draft_prompt(&p, &login, fresh)];
        request.command = Some(command_line(shell, &argv));
        let id = self
            .open_terminal_beside(env, request, window, cx)
            .ok_or_else(|| format!("Open the {env} environment first, then New project again."))?;
        let view = self
            .panes
            .get(&id)
            .cloned()
            .ok_or("its terminal didn't open")?;
        view.update(cx, |v, cx| {
            v.set_name(Some(format!("new project · {}", p.file.name)), cx)
        });
        let token = view
            .read(cx)
            .mcp_token()
            .map(str::to_string)
            .ok_or("its terminal has no MCP token")?;
        let sv = server::get().ok_or("hydraui's MCP server isn't running")?;
        server::lock(sv.registry()).drafts.insert(
            token,
            Draft {
                dir: dir.to_path_buf(),
                login,
            },
        );
        self.projects_data.sessions.insert(dir.to_path_buf(), id);
        Ok(())
    }

    fn end_draft_session(&mut self, dir: &Path) {
        self.projects_data.sessions.remove(dir);
        if let Some(sv) = server::get() {
            server::lock(sv.registry())
                .drafts
                .retain(|_, d| d.dir != dir);
        }
        self.projects_data.preview = None;
    }

    fn create_project(
        &mut self,
        env: String,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(checkout) = self
            .projects_data
            .envs
            .get(&env)
            .map(|e| e.checkout.clone())
        else {
            return;
        };
        match pj::create_from_draft(&dir, &checkout) {
            Ok(slug) => {
                self.end_draft_session(&dir);
                self.reload_env(&env, cx);
                window.push_notification(
                    Notification::success(
                        "Project created. Set its members, repos and sync times here; \
                         it's sent to the repo once sending comes (not sent yet).",
                    ),
                    cx,
                );
                self.go(Page::Settings { env, slug }, cx);
            }
            Err(e) => window.push_notification(Notification::error(e), cx),
        }
    }

    fn discard_draft(&mut self, env: String, dir: PathBuf, cx: &mut Context<Self>) {
        self.end_draft_session(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        self.reload_env(&env, cx);
        self.go(Page::Home, cx);
    }

    /// The folder `target`'s resources live under.
    fn target_dir(&self, target: &LinkTarget) -> Option<PathBuf> {
        match target {
            LinkTarget::Draft(dir) => Some(dir.clone()),
            LinkTarget::Project(env, slug) => self
                .projects_data
                .envs
                .get(env)
                .map(|e| e.checkout.join(slug)),
        }
    }

    /// `target`'s resources changed: show them.
    fn resources_changed(&mut self, target: &LinkTarget, cx: &mut Context<Self>) {
        if let LinkTarget::Project(env, _) = target {
            self.reload_env(env, cx);
        }
        self.projects_data.preview = None;
        cx.notify();
    }

    /// Attaches `paths`. One file is named first (its own name offered); several keep their
    /// names, to rename from the list.
    fn add_files(
        &mut self,
        target: LinkTarget,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.target_dir(&target) else {
            return;
        };
        let files: Vec<PathBuf> = paths.iter().filter(|p| p.is_file()).cloned().collect();
        if let [one] = files.as_slice() {
            let one = one.clone();
            let stem = one
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let own = one
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.text_dialog(
                format!("Name {own}"),
                stem,
                "what it is, e.g. Share flow; its extension is kept",
                move |ws, name, _window, cx| {
                    repo::add_resource_as(&dir, &one, &name).map_err(|e| e.to_string())?;
                    ws.resources_changed(&target, cx);
                    Ok(())
                },
                window,
                cx,
            );
            return;
        }
        for path in &files {
            if let Err(e) = repo::add_resource(&dir, path) {
                window.push_notification(Notification::error(e.to_string()), cx);
            }
        }
        if files.len() > 1 {
            window.push_notification(
                Notification::info(format!(
                    "Attached {} files under their own names; Rename changes them.",
                    files.len()
                )),
                cx,
            );
        }
        self.resources_changed(&target, cx);
    }

    fn pick_file(&mut self, target: LinkTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog_path(false, window, cx, move |ws, path, window, cx| {
            ws.add_files(target, &[path], window, cx)
        })
    }

    fn rename_file_dialog(
        &mut self,
        target: LinkTarget,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.target_dir(&target) else {
            return;
        };
        let stem = Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        self.text_dialog(
            format!("Rename {name}"),
            stem,
            "its new name; the extension is kept",
            move |ws, new, _window, cx| {
                repo::rename_resource(&dir, &name, &new).map_err(|e| e.to_string())?;
                ws.resources_changed(&target, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    fn rename_link_dialog(
        &mut self,
        target: LinkTarget,
        url: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.target_dir(&target) else {
            return;
        };
        self.text_dialog(
            format!("Rename {url}"),
            title,
            "what it is, e.g. Figma: share dialog",
            move |ws, new, _window, cx| {
                repo::rename_link(&dir, &url, &new).map_err(|e| e.to_string())?;
                ws.resources_changed(&target, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    /// Resources as a list: each file and link opens on click and has Rename (spec §8.2).
    fn resource_list(
        &self,
        target: &LinkTarget,
        p: &Project,
        files_dir: &Path,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut list = v_flex().gap_1();
        let row = |id: String,
                   icon: &'static str,
                   label: String,
                   open: Box<dyn Fn(&mut gpui::App)>,
                   rename: Box<Rename>,
                   cx: &mut Context<Self>| {
            h_flex()
                .gap_2()
                .text_sm()
                .child(div().flex_none().child(icon))
                .child(
                    div()
                        .id(SharedString::from(format!("open-{id}")))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(color(theme::TEAL))
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .child(label)
                        .on_click(move |_, _, cx| open(cx)),
                )
                .child(
                    Button::new(SharedString::from(format!("rename-{id}")))
                        .label("Rename")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, window, cx| rename(ws, window, cx))),
                )
        };
        for f in &p.files {
            let path = files_dir.join("resources").join(f);
            let (t, name) = (target.clone(), f.clone());
            list = list.child(row(
                format!("file-{f}"),
                "📎",
                f.clone(),
                Box::new(move |cx| cx.open_with_system(&path)),
                Box::new(move |ws, window, cx| {
                    ws.rename_file_dialog(t.clone(), name.clone(), window, cx)
                }),
                cx,
            ));
        }
        for l in &p.links {
            let (url, t, title) = (l.url.clone(), target.clone(), l.title.clone());
            let open_url = l.url.clone();
            list = list.child(row(
                format!("link-{}", l.url),
                "🔗",
                l.title.clone(),
                Box::new(move |cx| cx.open_url(&open_url)),
                Box::new(move |ws, window, cx| {
                    ws.rename_link_dialog(t.clone(), url.clone(), title.clone(), window, cx)
                }),
                cx,
            ));
        }
        list
    }

    fn add_link_dialog(&mut self, target: LinkTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.two_field_dialog(
            "Add a link".into(),
            ("Link", "https://…", String::new()),
            (
                "What is it?",
                "a title, e.g. Figma: share dialog",
                String::new(),
            ),
            move |ws, url, title, _window, cx| {
                let dir = match &target {
                    LinkTarget::Draft(dir) => dir.clone(),
                    LinkTarget::Project(env, slug) => ws
                        .projects_data
                        .envs
                        .get(env)
                        .map(|e| e.checkout.join(slug))
                        .ok_or("This project isn't in the repo any more.")?,
                };
                pj::add_link(&dir, &url, &title)?;
                if let LinkTarget::Project(env, _) = &target {
                    ws.reload_env(env, cx);
                }
                ws.projects_data.preview = None;
                cx.notify();
                Ok(())
            },
            window,
            cx,
        );
    }

    /// A dialog with two labelled boxes, each `(label, placeholder, initial)`; Save runs
    /// `save` with both, and the dialog stays open showing what's wrong until it succeeds.
    /// (Two dialogs one after the other don't work: the first's Save closes the second.)
    fn two_field_dialog(
        &mut self,
        title: String,
        first: (&'static str, &'static str, String),
        second: (&'static str, &'static str, String),
        save: impl Fn(&mut Self, String, String, &mut Window, &mut Context<Self>) -> Result<(), String>
        + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = |(_, placeholder, initial): &(&'static str, &'static str, String),
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            let (placeholder, initial) = (*placeholder, initial.clone());
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(initial)
            })
        };
        let (a, b) = (input(&first, window, cx), input(&second, window, cx));
        let (label_a, label_b) = (first.0, second.0);
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let save = std::rc::Rc::new(save);
        let focused = std::cell::Cell::new(false);
        window.open_dialog(cx, move |dialog, window, cx| {
            if !focused.replace(true) {
                a.update(cx, |s, cx| s.focus(window, cx));
            }
            let shown = problem.read(cx).clone();
            let (a2, b2, p, ws, save) = (
                a.clone(),
                b.clone(),
                problem.clone(),
                workspace.clone(),
                save.clone(),
            );
            let label = |text: &'static str| {
                div()
                    .text_sm()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(text)
            };
            dialog
                .title(title.clone())
                .w(px(640.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(label(label_a))
                        .child(Input::new(&a))
                        .child(label(label_b))
                        .child(Input::new(&b))
                        .when_some(shown, |d, e| {
                            d.child(div().text_sm().text_color(color(theme::RED)).child(e))
                        }),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("save").label("Save").primary().on_click(
                            move |_, window, cx| {
                                let first = a2.read(cx).value().trim().to_string();
                                let second = b2.read(cx).value().trim().to_string();
                                match ws.update(cx, |ws, cx| save(ws, first, second, window, cx)) {
                                    Ok(()) => window.close_dialog(cx),
                                    Err(e) => p.update(cx, |p, cx| {
                                        *p = Some(e);
                                        cx.notify();
                                    }),
                                }
                            },
                        )),
                )
        });
    }

    // ---- Rendering ----

    pub(super) fn render_projects(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = &self.projects else {
            return div().into_any_element();
        };
        let (page, show_all) = (tab.page.clone(), tab.show_all);
        let body = match &page {
            Page::Home => self.projects_home(show_all, cx),
            Page::Project { env, slug } => self.project_page(env, slug, cx),
            Page::Milestone { env, slug, id } => self.milestone_page(env, slug, id, cx),
            Page::Settings { env, slug } => self.project_settings_page(env, slug, cx),
            Page::Waiting => self.waiting_page(cx),
            Page::Draft { env, dir } => return self.draft_page(env, dir, cx),
        };
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .text_color(color(theme::TEXT))
            .child(self.projects_header(&page, cx))
            .child(
                div()
                    .id("projects-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .py_5()
                    .child(body),
            )
            .into_any_element()
    }

    fn projects_header(&self, page: &Page, cx: &mut Context<Self>) -> AnyElement {
        let crumb = |id: &'static str, label: String, to: Option<Page>, cx: &mut Context<Self>| {
            let d = div().id(id).child(label).text_color(color(if to.is_some() {
                theme::TEXT_SECONDARY
            } else {
                theme::TEXT
            }));
            match to {
                Some(page) => d
                    .cursor_pointer()
                    .hover(|s| s.text_color(color(theme::TEXT)))
                    .on_click(cx.listener(move |ws, _, _, cx| ws.go(page.clone(), cx)))
                    .into_any_element(),
                None => d.into_any_element(),
            }
        };
        let name_of = |env: &str, slug: &str| {
            self.find_project(env, slug)
                .map(|p| p.file.name.clone())
                .unwrap_or_else(|| slug.to_string())
        };
        let sep = || div().text_color(color(theme::TEXT_MUTED)).child("/");
        let mut trail = h_flex().gap_2().text_sm();
        let home = (*page != Page::Home).then_some(Page::Home);
        trail = trail.child(crumb("crumb-home", "Projects".into(), home, cx));
        match page {
            Page::Home => {}
            Page::Waiting => {
                trail =
                    trail
                        .child(sep())
                        .child(crumb("crumb-w", "Waiting on you".into(), None, cx))
            }
            Page::Draft { dir, .. } => {
                let name = self
                    .projects_data
                    .preview
                    .as_ref()
                    .and_then(|(_, p)| p.as_ref().ok())
                    .map(|p| p.file.name.clone())
                    .unwrap_or_else(|| {
                        dir.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    });
                trail = trail.child(sep()).child(crumb(
                    "crumb-d",
                    format!("New project: {name}"),
                    None,
                    cx,
                ))
            }
            Page::Project { env, slug } => {
                trail = trail
                    .child(sep())
                    .child(crumb("crumb-p", name_of(env, slug), None, cx))
            }
            Page::Milestone { env, slug, id } => {
                let title = self
                    .find_project(env, slug)
                    .and_then(|p| p.milestones.iter().find(|m| &m.id == id))
                    .map(|m| format!("{} {}", m.id, m.title))
                    .unwrap_or_else(|| id.clone());
                let back = Page::Project {
                    env: env.clone(),
                    slug: slug.clone(),
                };
                trail = trail
                    .child(sep())
                    .child(crumb("crumb-p", name_of(env, slug), Some(back), cx))
                    .child(sep())
                    .child(crumb("crumb-m", title, None, cx))
            }
            Page::Settings { env, slug } => {
                let back = Page::Project {
                    env: env.clone(),
                    slug: slug.clone(),
                };
                trail = trail
                    .child(sep())
                    .child(crumb("crumb-p", name_of(env, slug), Some(back), cx))
                    .child(sep())
                    .child(crumb("crumb-s", "Settings".into(), None, cx))
            }
        }
        let waiting = self.projects_data.waiting_count();
        h_flex()
            .px_6()
            .py_3()
            .gap_3()
            .border_b_1()
            .border_color(color(theme::BORDER))
            .bg(color(theme::SURFACE))
            .child(trail)
            .child(div().flex_1())
            .when(self.projects_data.fetching, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT_MUTED))
                        .child("Fetching…"),
                )
            })
            .child(
                Button::new("projects-waiting")
                    .label(if waiting > 0 {
                        format!("Waiting on you · {waiting}")
                    } else {
                        "Waiting on you".to_string()
                    })
                    .small()
                    .when(waiting > 0, |b| b.primary())
                    .when(waiting == 0, |b| b.ghost())
                    .on_click(cx.listener(|ws, _, _, cx| ws.go(Page::Waiting, cx))),
            )
            .child(
                Button::new("projects-refresh")
                    .label("Fetch now")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|ws, _, _, cx| ws.projects_config_changed(cx))),
            )
            .into_any_element()
    }

    fn find_project(&self, env: &str, slug: &str) -> Option<&Project> {
        self.projects_data
            .envs
            .get(env)?
            .projects
            .iter()
            .find(|p| p.slug == slug)
    }

    fn projects_home(&self, show_all: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut out = v_flex().gap_6();
        if self.projects_data.envs.is_empty() {
            return out
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_lg().child("No projects repo yet"))
                        .child(div().text_color(color(theme::TEXT_SECONDARY)).child(
                            "Set one in Settings → an environment → Projects (owner/name on \
                                 GitHub, e.g. iov42/iov-projects). Its projects show here.",
                        )),
                )
                .into_any_element();
        }
        let today = today();
        for (env, e) in &self.projects_data.envs {
            let env_s = env.clone();
            let mut header = h_flex()
                .gap_3()
                .items_baseline()
                .child(div().text_lg().child(env.clone()))
                .child(
                    div()
                        .text_sm()
                        .font_family("monospace")
                        .text_color(color(theme::TEXT_MUTED))
                        .child(e.repo.clone()),
                );
            if let Some(login) = &e.login {
                header = header.child(
                    div()
                        .text_sm()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(format!("you: {login}")),
                );
            }
            header = header.child(div().flex_1()).child(
                Button::new(SharedString::from(format!("new-project-{env}")))
                    .label("New project")
                    .primary()
                    .small()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.new_project(env_s.clone(), window, cx)
                    })),
            );
            let mut section = v_flex().gap_3().child(header);
            if let Some(problem) = &e.problem {
                section = section.child(notice(problem, theme::RED, theme::RED_TINT));
            } else if !e.fetched {
                section = section.child(muted("Fetching the repo…"));
            }
            for dir in &e.draft_dirs {
                let (env_d, dir_d) = (env.clone(), dir.clone());
                let name = repo::load(dir)
                    .map(|p| p.file.name)
                    .unwrap_or_else(|_| dir.display().to_string());
                section = section.child(
                    h_flex()
                        .id(SharedString::from(format!("draft-{}", dir.display())))
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(color(theme::AMBER))
                        .bg(color(theme::AMBER_TINT))
                        .cursor_pointer()
                        .child(div().child(format!("Draft: {name}")))
                        .child(muted("not created yet: open it to carry on"))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.open_draft(env_d.clone(), dir_d.clone(), false, window, cx)
                        })),
                );
            }
            let shown: Vec<&Project> = e
                .projects
                .iter()
                .filter(|p| show_all || p.file.status == ProjectStatus::Active)
                .collect();
            if e.fetched && shown.is_empty() && e.draft_dirs.is_empty() {
                section = section.child(muted("No active projects. New project starts one."));
            }
            let mut grid = h_flex().flex_wrap().gap_4().items_start();
            for p in shown {
                grid = grid.child(self.project_card(env, e, p, &today, cx));
            }
            section = section.child(grid);
            for (slug, why) in &e.broken {
                section = section.child(notice(
                    &format!("{slug} doesn't read: {why}"),
                    theme::AMBER,
                    theme::AMBER_TINT,
                ));
            }
            out = out.child(section);
        }
        out.child(
            div()
                .id("projects-show-all")
                .text_sm()
                .text_color(color(theme::TEXT_SECONDARY))
                .cursor_pointer()
                .child(if show_all {
                    "Hide paused and done projects"
                } else {
                    "Show paused and done projects too"
                })
                .on_click(cx.listener(|ws, _, _, cx| {
                    if let Some(t) = &mut ws.projects {
                        t.show_all = !t.show_all;
                    }
                    cx.notify();
                })),
        )
        .into_any_element()
    }

    fn project_card(
        &self,
        env: &str,
        e: &EnvProjects,
        p: &Project,
        today: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let login = e.login.as_deref().unwrap_or("");
        let counts = milestone_counts(p, |t| t.meta.status);
        let open = p.decisions.iter().filter(|d| d.is_open()).count();
        let mine = p
            .decisions
            .iter()
            .filter(|d| d.is_open() && d.meta.assigned_to.eq_ignore_ascii_case(login))
            .count();
        let unsent = pj::project_unsent(&e.pending, &p.slug);
        let role = if p.file.lead.eq_ignore_ascii_case(login) {
            "you lead".to_string()
        } else if p.file.is_on(login) {
            format!("lead {} · you're a member", p.file.lead)
        } else {
            format!("lead {}", p.file.lead)
        };
        let page = Page::Project {
            env: env.to_string(),
            slug: p.slug.clone(),
        };
        let mut card = v_flex()
            .id(SharedString::from(format!("project-{env}-{}", p.slug)))
            .w(px(340.))
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(color(theme::RAISED))
            .border_1()
            .border_color(color(theme::BORDER))
            .cursor_pointer()
            .hover(|s| s.border_color(color(theme::TEAL_BORDER)))
            .on_click(cx.listener(move |ws, _, _, cx| ws.go(page.clone(), cx)))
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(div().text_lg().child(p.file.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(color(theme::TEXT_MUTED))
                            .child(p.file.key.clone()),
                    )
                    .when(p.file.status != ProjectStatus::Active, |d| {
                        d.child(pill(
                            status_word(p.file.status),
                            theme::TEXT_SECONDARY,
                            theme::SURFACE,
                        ))
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(role),
            );
        if p.milestones.is_empty() {
            card = card.child(muted("No milestones yet."));
        }
        for m in &p.milestones {
            let c = counts.get(&m.id).copied().unwrap_or_default();
            card = card.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .text_xs()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .child(format!("{} {}", m.id, m.title)),
                            )
                            .when(is_late(m, &c, today), |d| {
                                d.child(div().text_color(color(theme::RED)).child("late"))
                            })
                            .child(div().text_color(color(theme::TEXT_MUTED)).child(format!(
                                "{}/{}",
                                c.done,
                                c.total()
                            ))),
                    )
                    .child(status_bar(&c, 6.)),
            );
        }
        let mut foot = h_flex()
            .gap_3()
            .text_xs()
            .text_color(color(theme::TEXT_SECONDARY));
        foot = foot.child(format!(
            "{open} open question{}",
            if open == 1 { "" } else { "s" }
        ));
        if mine > 0 {
            foot = foot.child(
                div()
                    .text_color(color(theme::AMBER))
                    .child(format!("{mine} waiting on you")),
            );
        }
        if unsent > 0 {
            foot = foot.child(
                div()
                    .text_color(color(theme::TEAL))
                    .child(format!("{unsent} not sent yet")),
            );
        }
        card.child(foot).into_any_element()
    }

    fn project_page(&self, env: &str, slug: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = self.projects_data.envs.get(env) else {
            return muted("This environment has no projects repo any more.").into_any_element();
        };
        let Some(p) = e.projects.iter().find(|p| p.slug == slug) else {
            return muted("This project isn't in the repo any more.").into_any_element();
        };
        let login = e.login.clone().unwrap_or_default();
        let today = today();
        let counts = milestone_counts(p, |t| t.meta.status);
        let (env_s, slug_s) = (env.to_string(), slug.to_string());

        let title = h_flex()
            .gap_3()
            .items_baseline()
            .child(div().text_2xl().child(p.file.name.clone()))
            .child(
                div()
                    .font_family("monospace")
                    .text_color(color(theme::TEXT_MUTED))
                    .child(p.file.key.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(format!(
                        "lead {} · {} member{} · {}",
                        p.file.lead,
                        p.file.members.len(),
                        if p.file.members.len() == 1 { "" } else { "s" },
                        status_word(p.file.status)
                    )),
            )
            .child(div().flex_1())
            .child({
                let (env_s, slug_s) = (env_s.clone(), slug_s.clone());
                Button::new("project-ask")
                    .label("Ask a question")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.ask_dialog(env_s.clone(), slug_s.clone(), window, cx)
                    }))
            })
            .child({
                let page = Page::Settings {
                    env: env_s.clone(),
                    slug: slug_s.clone(),
                };
                Button::new("project-settings")
                    .label("Settings")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |ws, _, _, cx| ws.go(page.clone(), cx)))
            });

        // Milestones, in order (spec §8.2).
        let mut row = h_flex().gap_3().items_start().flex_wrap();
        for m in &p.milestones {
            let c = counts.get(&m.id).copied().unwrap_or_default();
            let late = is_late(m, &c, &today);
            let page = Page::Milestone {
                env: env_s.clone(),
                slug: slug_s.clone(),
                id: m.id.clone(),
            };
            let done = m.status == MilestoneStatus::Done || (c.total() > 0 && c.done == c.total());
            row = row.child(
                v_flex()
                    .id(SharedString::from(format!("milestone-{}", m.id)))
                    .w(px(250.))
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(color(if m.status == MilestoneStatus::Active {
                        theme::RAISED
                    } else {
                        theme::SURFACE
                    }))
                    .border_1()
                    .border_color(color(if late {
                        theme::RED_BORDER
                    } else if m.status == MilestoneStatus::Active {
                        theme::TEAL_BORDER
                    } else {
                        theme::BORDER
                    }))
                    .cursor_pointer()
                    .hover(|s| s.border_color(color(theme::TEAL)))
                    .on_click(cx.listener(move |ws, _, _, cx| ws.go(page.clone(), cx)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(color(theme::TEXT_MUTED))
                                    .child(m.id.clone()),
                            )
                            .child(div().flex_1())
                            .when(done, |d| {
                                d.child(pill("done", theme::GREEN, theme::GREEN_TINT))
                            })
                            .when(late, |d| d.child(pill("late", theme::RED, theme::RED_TINT))),
                    )
                    .child(div().child(m.title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(
                                m.target
                                    .map(|t| format!("target {t}"))
                                    .unwrap_or_else(|| "no target date".into()),
                            ),
                    )
                    .child(status_bar(&c, 8.))
                    .child(counts_line(&c)),
            );
        }
        if p.milestones.is_empty() {
            row = row.child(muted("No milestones yet."));
        }

        // Decisions at a glance (spec §8.2).
        let now = pj::now_secs();
        let mut open: Vec<&Decision> = p.decisions.iter().filter(|d| d.is_open()).collect();
        open.sort_by_key(|d| d.meta.asked_at.map(|t| t.to_string()).unwrap_or_default());
        let mut answered: Vec<&Decision> = p.decisions.iter().filter(|d| !d.is_open()).collect();
        answered.sort_by_key(|d| {
            std::cmp::Reverse(d.meta.decided_at.map(|t| t.to_string()).unwrap_or_default())
        });
        let mut decisions = v_flex().gap_2().child(section_title("Outstanding"));
        if open.is_empty() {
            decisions = decisions.child(muted("Nothing waiting."));
        }
        for d in open {
            decisions = decisions.child(self.question_row(env, p, d, &login, now, cx));
        }
        decisions = decisions
            .child(div().h(px(8.)))
            .child(section_title("Recently decided"));
        if answered.is_empty() {
            decisions = decisions.child(muted("No decisions yet."));
        }
        for d in answered.into_iter().take(6) {
            decisions =
                decisions.child(
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .bg(color(theme::SURFACE))
                        .child(
                            div()
                                .text_sm()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(format!("{} · {}", d.id, d.meta.question)),
                        )
                        .child(div().child(d.meta.answer.clone()))
                        .child(div().text_xs().text_color(color(theme::TEXT_MUTED)).child(
                            format!(
                                "decided by {}{}",
                                d.meta.decided_by,
                                d.meta
                                    .decided_at
                                    .map(|t| format!(" · {} ago", pj::age(&t, now)))
                                    .unwrap_or_default()
                            ),
                        )),
                );
        }

        // Progress, one row per person (spec §8.2).
        let mut people: Vec<String> = vec![p.file.lead.clone()];
        for m in &p.file.members {
            if !people.iter().any(|x| x.eq_ignore_ascii_case(m)) {
                people.push(m.clone());
            }
        }
        let mut progress = v_flex().gap_2().child(section_title("Progress"));
        for person in &people {
            let latest = p
                .progress
                .iter()
                .find(|w| w.login.eq_ignore_ascii_case(person));
            let working: Vec<&Ticket> = p
                .tickets
                .iter()
                .filter(|t| t.meta.assignee.eq_ignore_ascii_case(person))
                .filter(|t| {
                    matches!(
                        t.meta.status,
                        TicketStatus::InProgress | TicketStatus::InReview
                    )
                })
                .collect();
            progress = progress.child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(color(theme::SURFACE))
                    .child(
                        h_flex().gap_2().child(div().child(person.clone())).child(
                            div().text_xs().text_color(color(theme::TEXT_MUTED)).child(
                                latest
                                    .map(|w| format!("last write-up {}", w.date))
                                    .unwrap_or_else(|| "no write-up yet".into()),
                            ),
                        ),
                    )
                    .when_some(latest, |d, w| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(first_line(&w.body)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(if working.is_empty() {
                                "nothing in progress".to_string()
                            } else {
                                working
                                    .iter()
                                    .map(|t| format!("{} {}", t.id, t.meta.title))
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            }),
                    ),
            );
        }

        // Spec and resources.
        let target = LinkTarget::Project(env_s.clone(), slug_s.clone());
        let mut resources = v_flex()
            .gap_1()
            .child(section_title("Resources"))
            .child(self.resource_list(&target, p, &e.checkout.join(&p.slug), cx));
        if p.files.is_empty() && p.links.is_empty() {
            resources = resources.child(muted("None yet."));
        }
        let (file_target, link_target) = (target.clone(), target);
        resources = resources.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("project-add-file")
                        .label("Add file…")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.pick_file(file_target.clone(), window, cx)
                        })),
                )
                .child(
                    Button::new("project-add-link")
                        .label("Add link…")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.add_link_dialog(link_target.clone(), window, cx)
                        })),
                ),
        );
        let spec =
            v_flex()
                .gap_2()
                .child(section_title("Spec"))
                .child(if p.spec.trim().is_empty() {
                    muted("No spec yet.").into_any_element()
                } else {
                    gpui_base::TextView::markdown(
                        SharedString::from(format!("spec-{slug}")),
                        p.spec.clone(),
                    )
                    .selectable(true)
                    .into_any_element()
                });

        v_flex()
            .gap_6()
            .max_w(px(1200.))
            .child(title)
            .child(
                v_flex()
                    .gap_2()
                    .child(section_title("Milestones"))
                    .child(row),
            )
            .child(
                h_flex()
                    .gap_6()
                    .items_start()
                    .child(v_flex().flex_1().min_w_0().child(decisions))
                    .child(
                        v_flex()
                            .w(px(380.))
                            .flex_none()
                            .gap_6()
                            .child(progress)
                            .child(resources),
                    ),
            )
            .child(spec)
            .into_any_element()
    }

    fn question_row(
        &self,
        env: &str,
        p: &Project,
        d: &Decision,
        login: &str,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mine = d.meta.assigned_to.eq_ignore_ascii_case(login);
        let waited = d
            .meta
            .asked_at
            .map(|t| pj::age(&t, now))
            .unwrap_or_default();
        let mut row = v_flex()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(color(if mine {
                theme::AMBER_TINT
            } else {
                theme::SURFACE
            }))
            .border_1()
            .border_color(color(if mine {
                theme::AMBER
            } else {
                theme::BORDER_SUBTLE
            }))
            .child(div().child(format!("{} · {}", d.id, d.meta.question)))
            .child(
                div()
                    .text_xs()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(format!(
                        "waiting on {}{}{}",
                        if mine {
                            "you".to_string()
                        } else {
                            d.meta.assigned_to.clone()
                        },
                        if waited.is_empty() {
                            String::new()
                        } else {
                            format!(" for {waited}")
                        },
                        if d.meta.asked_by.is_empty() {
                            String::new()
                        } else {
                            format!(" · asked by {}", d.meta.asked_by)
                        },
                    )),
            );
        if mine {
            let mut actions = h_flex().gap_2().flex_wrap();
            for (i, option) in d.meta.options.iter().enumerate() {
                let (env, slug, q, option) =
                    (env.to_string(), p.slug.clone(), d.clone(), option.clone());
                actions = actions.child(
                    Button::new(SharedString::from(format!("opt-{}-{i}", d.id)))
                        .label(option.clone())
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            let (q, option) = (q.clone(), option.clone());
                            let _ = ws.edit_project(
                                &env,
                                &slug,
                                |dir, _, login| pj::answer(dir, &q, &option, login),
                                window,
                                cx,
                            );
                        })),
                );
            }
            let (env, slug, q) = (env.to_string(), p.slug.clone(), d.clone());
            actions = actions.child(
                Button::new(SharedString::from(format!("answer-{}", d.id)))
                    .label("Answer…")
                    .small()
                    .primary()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        let (env, slug, q) = (env.clone(), slug.clone(), q.clone());
                        ws.text_dialog(
                            format!("Answer {}: {}", q.id, q.meta.question),
                            String::new(),
                            "your answer; it's recorded with your name and the time",
                            move |ws, text, window, cx| {
                                let q = q.clone();
                                ws.edit_project(
                                    &env,
                                    &slug,
                                    |dir, _, login| pj::answer(dir, &q, &text, login),
                                    window,
                                    cx,
                                )
                            },
                            window,
                            cx,
                        )
                    })),
            );
            row = row.child(actions);
        }
        row.into_any_element()
    }

    fn ask_dialog(
        &mut self,
        env: String,
        slug: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lead = self
            .find_project(&env, &slug)
            .map(|p| p.file.lead.clone())
            .unwrap_or_default();
        self.two_field_dialog(
            "Ask a question".into(),
            ("Question", "a decision that needs making", String::new()),
            (
                "Who decides?",
                "their GitHub login; the lead when empty",
                lead,
            ),
            move |ws, question, who, window, cx| {
                ws.edit_project(
                    &env,
                    &slug,
                    |dir, p, login| pj::ask(dir, p, &question, &who, login).map(|_| ()),
                    window,
                    cx,
                )
            },
            window,
            cx,
        );
    }

    fn milestone_page(
        &self,
        env: &str,
        slug: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(e) = self.projects_data.envs.get(env) else {
            return muted("This environment has no projects repo any more.").into_any_element();
        };
        let Some(p) = e.projects.iter().find(|p| p.slug == slug) else {
            return muted("This project isn't in the repo any more.").into_any_element();
        };
        let Some(m) = p.milestones.iter().find(|m| m.id == id) else {
            return muted("This milestone isn't in the project any more.").into_any_element();
        };
        let c = milestone_counts(p, |t| t.meta.status)
            .get(id)
            .copied()
            .unwrap_or_default();
        let late = is_late(m, &c, &today());
        let new_button = |label: &'static str, kind: TicketKind, cx: &mut Context<Self>| {
            let (env, slug, id) = (env.to_string(), slug.to_string(), id.to_string());
            Button::new(SharedString::from(format!("new-{label}")))
                .label(label)
                .small()
                .ghost()
                .on_click(cx.listener(move |ws, _, window, cx| {
                    let (env, slug, id) = (env.clone(), slug.clone(), id.clone());
                    ws.text_dialog(
                        format!("{label} in {id}"),
                        String::new(),
                        "a title, in one line",
                        move |ws, title, window, cx| {
                            let id = id.clone();
                            ws.edit_project(
                                &env,
                                &slug,
                                |dir, p, login| {
                                    pj::new_ticket(dir, p, &title, &id, kind, login).map(|_| ())
                                },
                                window,
                                cx,
                            )
                        },
                        window,
                        cx,
                    )
                }))
        };
        let head = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_baseline()
                    .child(div().text_2xl().child(m.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(
                                m.target
                                    .map(|t| format!("target {t}"))
                                    .unwrap_or_else(|| "no target date".into()),
                            ),
                    )
                    .when(late, |d| d.child(pill("late", theme::RED, theme::RED_TINT)))
                    .child(div().flex_1())
                    .child(new_button("New ticket", TicketKind::Feature, cx))
                    .child(new_button("New bug", TicketKind::Bug, cx)),
            )
            .when(!m.description.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(m.description.clone()),
                )
            })
            .child(div().w(px(480.)).child(status_bar(&c, 10.)))
            .child(counts_line(&c));
        let mut board = h_flex().gap_3().items_start();
        for status in TicketStatus::BOARD {
            let tickets: Vec<&Ticket> = p
                .tickets
                .iter()
                .filter(|t| t.meta.milestone == id && t.meta.status == status)
                .collect();
            let mut column = v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .p_2()
                .rounded_lg()
                .bg(color(theme::SURFACE))
                .child(
                    h_flex()
                        .gap_2()
                        .px_1()
                        .child(
                            div()
                                .size(px(8.))
                                .rounded_full()
                                .bg(color(status_colour(status))),
                        )
                        .child(div().text_sm().child(status.label()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(color(theme::TEXT_MUTED))
                                .child(tickets.len().to_string()),
                        ),
                );
            for t in tickets {
                column = column.child(self.ticket_card(env, e, p, t, cx));
            }
            board = board.child(column);
        }
        v_flex().gap_5().child(head).child(board).into_any_element()
    }

    fn ticket_card(
        &self,
        env: &str,
        e: &EnvProjects,
        p: &Project,
        t: &Ticket,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let unsent = pj::ticket_unsent(&e.pending, &p.slug, &t.id);
        let questions = p
            .decisions
            .iter()
            .filter(|d| d.is_open() && d.meta.ticket == t.id)
            .count();
        let ws = cx.entity();
        let (env, slug, ticket) = (env.to_string(), p.slug.clone(), t.clone());
        v_flex()
            .id(SharedString::from(format!("ticket-{}", t.id)))
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(color(theme::RAISED))
            .border_1()
            .border_color(color(if unsent {
                theme::TEAL_BORDER
            } else {
                theme::BORDER_SUBTLE
            }))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(color(theme::TEXT_MUTED))
                            .child(t.id.clone()),
                    )
                    .when(t.meta.kind == TicketKind::Bug, |d| {
                        d.child(pill("bug", theme::RED, theme::RED_TINT))
                    })
                    .when(t.meta.kind == TicketKind::Chore, |d| {
                        d.child(pill("chore", theme::TEXT_SECONDARY, theme::SURFACE))
                    })
                    .child(div().flex_1())
                    .when(unsent, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(color(theme::TEAL))
                                .child("not sent yet"),
                        )
                    }),
            )
            .child(div().text_sm().child(t.meta.title.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(if t.meta.assignee.is_empty() {
                        "nobody yet".to_string()
                    } else {
                        t.meta.assignee.clone()
                    })
                    .when(questions > 0, |d| {
                        d.child(div().text_color(color(theme::AMBER)).child(format!(
                            "{questions} open question{}",
                            if questions == 1 { "" } else { "s" }
                        )))
                    }),
            )
            .context_menu(move |menu, _, _| {
                let mut menu = menu;
                for status in [
                    TicketStatus::Todo,
                    TicketStatus::InProgress,
                    TicketStatus::InReview,
                    TicketStatus::Done,
                    TicketStatus::Dropped,
                ] {
                    if status == ticket.meta.status {
                        continue;
                    }
                    let (ws, env, slug, t) =
                        (ws.clone(), env.clone(), slug.clone(), ticket.clone());
                    menu = menu.item(
                        PopupMenuItem::new(format!("Move to {}", status.label())).on_click(
                            move |_, window, cx| {
                                ws.update(cx, |ws, cx| {
                                    let t = t.clone();
                                    let _ = ws.edit_project(
                                        &env,
                                        &slug,
                                        |dir, _, _| pj::move_ticket(dir, &t, status),
                                        window,
                                        cx,
                                    );
                                })
                            },
                        ),
                    );
                }
                let (ws2, env2, slug2, t2) =
                    (ws.clone(), env.clone(), slug.clone(), ticket.clone());
                menu.separator()
                    .item(
                        PopupMenuItem::new("Assign…").on_click(move |_, window, cx| {
                            let (env, slug, t) = (env2.clone(), slug2.clone(), t2.clone());
                            ws2.update(cx, |ws, cx| {
                                ws.text_dialog(
                                    format!("Who works on {}?", t.id),
                                    t.meta.assignee.clone(),
                                    "a GitHub login; empty for nobody",
                                    move |ws, who, window, cx| {
                                        let t = t.clone();
                                        ws.edit_project(
                                            &env,
                                            &slug,
                                            |dir, _, _| pj::assign_ticket(dir, &t, &who),
                                            window,
                                            cx,
                                        )
                                    },
                                    window,
                                    cx,
                                )
                            })
                        }),
                    )
            })
            .into_any_element()
    }

    fn waiting_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = pj::now_secs();
        let mut out = v_flex()
            .gap_3()
            .max_w(px(900.))
            .child(div().text_2xl().child("Waiting on you"));
        let mut any = false;
        for (env, e) in &self.projects_data.envs {
            let Some(login) = e.login.as_deref() else {
                continue;
            };
            for (p, d) in pj::questions_for(login, &e.projects) {
                any = true;
                out = out
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_MUTED))
                            .child(format!("{} · {}", p.file.name, env)),
                    )
                    .child(self.question_row(env, p, d, login, now, cx));
            }
        }
        if !any {
            out = out.child(muted(
                "Nothing waits on you. Questions assigned to you show here.",
            ));
        }
        out.into_any_element()
    }

    fn project_settings_page(&self, env: &str, slug: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = self.projects_data.envs.get(env) else {
            return muted("This environment has no projects repo any more.").into_any_element();
        };
        let Some(p) = e.projects.iter().find(|p| p.slug == slug) else {
            return muted("This project isn't in the repo any more.").into_any_element();
        };
        let lead = e
            .login
            .as_deref()
            .is_some_and(|l| p.file.lead.eq_ignore_ascii_case(l));
        let f = &p.file;
        let field = |label: &'static str, value: String, field: Field, cx: &mut Context<Self>| {
            let (env, slug) = (env.to_string(), slug.to_string());
            let initial = value.clone();
            h_flex()
                .gap_4()
                .py_2()
                .border_b_1()
                .border_color(color(theme::BORDER_SUBTLE))
                .child(
                    div()
                        .w(px(160.))
                        .flex_none()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(if value.is_empty() {
                    "none".to_string()
                } else {
                    value
                }))
                .when(lead, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("set-{label}")))
                            .label("Change…")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                let (env, slug) = (env.clone(), slug.clone());
                                ws.text_dialog(
                                    format!("{label} for {slug}"),
                                    initial.clone(),
                                    field.hint(),
                                    move |ws, text, window, cx| {
                                        ws.edit_project(
                                            &env,
                                            &slug,
                                            |dir, p, _| set_field(dir, p, field, &text),
                                            window,
                                            cx,
                                        )
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    )
                })
        };
        let status_buttons = {
            let mut row = h_flex().gap_2();
            for s in [
                ProjectStatus::Active,
                ProjectStatus::Paused,
                ProjectStatus::Done,
            ] {
                let (env, slug) = (env.to_string(), slug.to_string());
                let b = Button::new(SharedString::from(format!("status-{}", status_word(s))))
                    .label(status_word(s))
                    .small()
                    .selected(f.status == s)
                    .when(lead, |b| {
                        b.on_click(cx.listener(move |ws, _, window, cx| {
                            let _ = ws.edit_project(
                                &env,
                                &slug,
                                |dir, p, _| {
                                    let mut file = p.file.clone();
                                    file.status = s;
                                    repo::write_project_file(dir, &file).map_err(|e| e.to_string())
                                },
                                window,
                                cx,
                            );
                        }))
                    });
                row = row.child(b);
            }
            row
        };
        v_flex()
            .gap_1()
            .max_w(px(900.))
            .child(
                div()
                    .text_2xl()
                    .pb_3()
                    .child(format!("{} settings", f.name)),
            )
            .when(!lead, |d| {
                d.child(notice(
                    &format!("Only the lead, {}, changes these.", f.lead),
                    theme::TEXT_SECONDARY,
                    theme::SURFACE,
                ))
            })
            .child(field("Name", f.name.clone(), Field::Name, cx))
            .child(field("Key", f.key.clone(), Field::Key, cx))
            .child(field("Lead", f.lead.clone(), Field::Lead, cx))
            .child(field("Members", f.members.join(", "), Field::Members, cx))
            .child(field("Code repos", f.repos.join(", "), Field::Repos, cx))
            .child(field(
                "Sync times",
                f.sync.times.join(", "),
                Field::Sync,
                cx,
            ))
            .child(
                h_flex()
                    .gap_4()
                    .py_2()
                    .child(
                        div()
                            .w(px(160.))
                            .flex_none()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child("Status"),
                    )
                    .child(status_buttons),
            )
            .child(div().pt_3().child(muted(
                "Sync times are when each member's hydraui sends the day's progress to the repo \
                 (once or twice a day). Sending comes in the next slice; until then changes stay \
                 on this machine, marked not sent yet.",
            )))
            .into_any_element()
    }

    fn draft_page(&mut self, env: &str, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let terminal = self
            .projects_data
            .sessions
            .get(dir)
            .and_then(|id| self.panes.get(id))
            .cloned();
        let preview = match self
            .projects_data
            .preview
            .as_ref()
            .filter(|(d, _)| d == dir)
        {
            Some((_, Ok(p))) => self.draft_preview(env, dir, p, cx),
            Some((_, Err(e))) => notice(e, theme::RED, theme::RED_TINT).into_any_element(),
            None => muted("Reading the draft…").into_any_element(),
        };
        let (env_c, dir_c) = (env.to_string(), dir.to_path_buf());
        let (env_d, dir_d) = (env.to_string(), dir.to_path_buf());
        let (env_r, dir_r) = (env.to_string(), dir.to_path_buf());
        let dir_f = dir.to_path_buf();
        let dir_drop = dir.to_path_buf();
        let target = LinkTarget::Draft(dir.to_path_buf());
        let actions = h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("draft-create")
                    .label("Create project")
                    .primary()
                    .small()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.create_project(env_c.clone(), dir_c.clone(), window, cx)
                    })),
            )
            .child(
                Button::new("draft-add-file")
                    .label("Add file…")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.pick_file(LinkTarget::Draft(dir_f.clone()), window, cx)
                    })),
            )
            .child(
                Button::new("draft-add-link")
                    .label("Add link…")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.add_link_dialog(target.clone(), window, cx)
                    })),
            )
            .when(terminal.is_none(), |d| {
                d.child(
                    Button::new("draft-restart")
                        .label("Start Claude")
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.open_draft(env_r.clone(), dir_r.clone(), false, window, cx)
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("draft-discard")
                    .label("Discard")
                    .small()
                    .danger()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        ws.discard_draft(env_d.clone(), dir_d.clone(), cx)
                    })),
            );
        let right = v_flex()
            .id("draft-preview")
            .w(px(460.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(color(theme::BORDER))
            .bg(color(theme::SURFACE))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(color(theme::TEAL_TINT)))
            .on_drop(cx.listener(move |ws, paths: &ExternalPaths, window, cx| {
                ws.add_files(LinkTarget::Draft(dir_drop.clone()), paths.paths(), window, cx)
            }))
            .child(v_flex().p_3().gap_2().border_b_1().border_color(color(theme::BORDER)).child(actions).child(muted(
                "Talk the project through with Claude on the left; the draft fills in here. \
                 Drop files here to attach them.",
            )))
            .child(div().id("draft-preview-body").flex_1().min_h_0().overflow_y_scroll().p_4().child(preview));
        let left = match terminal {
            Some(view) => div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(view)
                .into_any_element(),
            None => div()
                .flex_1()
                .p_6()
                .child(muted(
                    "Claude isn't running for this draft. Start Claude picks it up again.",
                ))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .text_color(color(theme::TEXT))
            .child(self.projects_header(
                &Page::Draft {
                    env: env.to_string(),
                    dir: dir.to_path_buf(),
                },
                cx,
            ))
            .child(h_flex().flex_1().min_h_0().child(left).child(right))
            .into_any_element()
    }

    fn draft_preview(
        &self,
        _env: &str,
        dir: &Path,
        p: &Project,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut out = v_flex().gap_4().child(
            h_flex()
                .gap_2()
                .items_baseline()
                .child(div().text_xl().child(p.file.name.clone()))
                .child(
                    div()
                        .font_family("monospace")
                        .text_color(color(theme::TEXT_MUTED))
                        .child(p.file.key.clone()),
                ),
        );
        let mut ms = v_flex().gap_2().child(section_title("Milestones"));
        if p.milestones.is_empty() {
            ms = ms.child(muted("None yet."));
        }
        for m in &p.milestones {
            let mut block = v_flex()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(color(theme::RAISED))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(color(theme::TEXT_MUTED))
                                .child(m.id.clone()),
                        )
                        .child(div().text_sm().child(m.title.clone()))
                        .child(div().flex_1())
                        .when_some(m.target, |d, t| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(color(theme::TEXT_SECONDARY))
                                    .child(t.to_string()),
                            )
                        }),
                );
            for t in p.tickets.iter().filter(|t| t.meta.milestone == m.id) {
                block = block.child(
                    h_flex()
                        .gap_2()
                        .pl_3()
                        .text_xs()
                        .child(
                            div()
                                .font_family("monospace")
                                .text_color(color(theme::TEXT_MUTED))
                                .child(t.id.clone()),
                        )
                        .child(
                            div()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(t.meta.title.clone()),
                        )
                        .when(t.meta.kind == TicketKind::Bug, |d| {
                            d.child(pill("bug", theme::RED, theme::RED_TINT))
                        }),
                );
            }
            ms = ms.child(block);
        }
        out = out.child(ms);
        let mut qs = v_flex().gap_1().child(section_title("Open questions"));
        if p.decisions.is_empty() {
            qs = qs.child(muted("None yet."));
        }
        for d in &p.decisions {
            qs = qs.child(div().text_sm().child(format!(
                "{} · {} (for {})",
                d.id, d.meta.question, d.meta.assigned_to
            )));
        }
        out = out.child(qs);
        let mut res = v_flex()
            .gap_1()
            .child(section_title("Resources"))
            .child(self.resource_list(&LinkTarget::Draft(dir.to_path_buf()), p, dir, cx));
        if p.files.is_empty() && p.links.is_empty() {
            res = res.child(muted("None yet: drop files here, or Add file… / Add link…"));
        }
        out = out.child(res);
        out.child(v_flex().gap_2().child(section_title("Spec")).child(
            if p.spec.trim().is_empty() {
                muted("Not written yet.").into_any_element()
            } else {
                gpui_base::TextView::markdown("draft-spec", p.spec.clone())
                    .selectable(true)
                    .into_any_element()
            },
        ))
        .into_any_element()
    }
}

/// What a resource row's Rename does.
type Rename = dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

#[derive(Clone)]
enum LinkTarget {
    Draft(PathBuf),
    Project(String, String),
}

/// A project setting the lead changes (spec §9.1, step 5).
#[derive(Debug, Clone, Copy)]
enum Field {
    Name,
    Key,
    Lead,
    Members,
    Repos,
    Sync,
}

impl Field {
    fn hint(self) -> &'static str {
        match self {
            Field::Name => "the project's name",
            Field::Key => "the ticket prefix, e.g. PG; fixed once tickets exist",
            Field::Lead => "the new lead's GitHub login: hands the project over",
            Field::Members => "GitHub logins, separated by commas",
            Field::Repos => "owner/name, separated by commas: a hint for the checks",
            Field::Sync => "one or two times a day, e.g. 12:30, 17:30",
        }
    }
}

fn set_field(dir: &Path, p: &Project, field: Field, text: &str) -> Result<(), String> {
    let mut f = p.file.clone();
    let text = text.trim();
    match field {
        Field::Name if text.is_empty() => return Err("Give it a name.".into()),
        Field::Name => f.name = text.to_string(),
        Field::Key => {
            if !p.tickets.is_empty() && !text.eq_ignore_ascii_case(&f.key) {
                return Err("The key is fixed once the project has tickets.".into());
            }
            if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err("A key is letters and numbers, e.g. PG.".into());
            }
            f.key = text.to_ascii_uppercase();
        }
        Field::Lead => {
            let lead = text.trim_start_matches('@');
            if lead.is_empty() {
                return Err("A project always has a lead.".into());
            }
            if !f.members.iter().any(|m| m.eq_ignore_ascii_case(&f.lead)) {
                f.members.push(f.lead.clone());
            }
            f.lead = lead.to_string();
        }
        Field::Members => f.members = pj::list_of(text),
        Field::Repos => f.repos = pj::list_of(text),
        Field::Sync => f.sync.times = pj::sync_times(text)?,
    }
    repo::write_project_file(dir, &f).map_err(|e| e.to_string())
}

/// Reads one environment's checkout: login and pull first, then the projects (off the UI
/// thread).
fn fetch_env(
    exe: &Path,
    env: &str,
    repo: &str,
    state: &Path,
    login: Option<String>,
) -> EnvProjects {
    let mut e = EnvProjects {
        repo: repo.to_string(),
        checkout: pj::checkout_dir(state, repo),
        drafts: pj::drafts_dir(state),
        login,
        ..Default::default()
    };
    if e.login.is_none() {
        match crate::branch_rule::gh_login(exe, env) {
            Ok(login) => e.login = Some(login),
            Err(why) => {
                e.problem = Some(format!(
                    "Can't tell who you are on GitHub ({env}'s gh): {why}"
                ))
            }
        }
    }
    if let Err(why) = pj::sync_checkout(exe, env, repo, &e.checkout) {
        e.problem = Some(format!("Can't fetch {repo}: {why}"));
    }
    read_checkout(&mut e);
    e
}

fn read_checkout(e: &mut EnvProjects) {
    e.projects.clear();
    e.broken.clear();
    for (slug, p) in repo::load_all(&e.checkout) {
        match p {
            Ok(p) => e.projects.push(p),
            Err(why) => e.broken.push((slug, why.to_string())),
        }
    }
    e.pending = pj::pending(&e.checkout);
    e.draft_dirs = std::fs::read_dir(&e.drafts)
        .map(|entries| {
            entries
                .flatten()
                .map(|d| d.path())
                .filter(|d| d.join(repo::PROJECT_FILE).is_file())
                .collect()
        })
        .unwrap_or_default();
    e.draft_dirs.sort();
    e.fetched = true;
}

fn draft_prompt(p: &Project, login: &str, fresh: bool) -> String {
    let start = if fresh {
        "Start by calling draft_show, then ask the user to describe the project in their own words."
    } else {
        "The draft already has work in it: call draft_show, sum up where it stands, and ask the \
         user what's next."
    };
    format!(
        "You're helping {login} start a new project, \"{name}\" (ticket key {key}), in hydraui's \
         Projects tab. The user sees a live preview of the draft beside you.\n\n\
         Talk it through with them: what it's for and who for, goals and non-goals, \
         constraints, and any links or files they have. Then write the draft with the draft \
         tools only (never create files yourself):\n\
         - draft_set_spec: the spec, in short numbered sections in plain words, with the \
         decisions and their reasons;\n\
         - draft_add_milestone: milestones in order, with target dates where the user gives \
         them;\n\
         - draft_add_ticket: tickets under each milestone, each small enough to finish in a \
         few days;\n\
         - draft_add_question: decisions still to be made, assigned to whoever should decide \
         (a GitHub login; {login} by default).\n\
         The user can drop files and add links beside you; draft_show lists them and \
         draft_read_resource reads them. Keep the draft and the conversation in step: show \
         what you changed after each round. When the user is happy, tell them to press \
         Create project.\n\n{start}",
        name = p.file.name,
        key = p.file.key,
    )
}

// ---- Small pieces ----

fn muted(text: &str) -> gpui::Div {
    div()
        .text_sm()
        .text_color(color(theme::TEXT_MUTED))
        .child(text.to_string())
}

fn section_title(text: &str) -> gpui::Div {
    div()
        .text_xs()
        .text_color(color(theme::TEXT_SECONDARY))
        .child(text.to_uppercase())
}

fn notice(text: &str, fg: u32, bg: u32) -> gpui::Div {
    div()
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .bg(color(bg))
        .border_1()
        .border_color(color(theme::blend(fg, bg, 0.5)))
        .text_color(color(fg))
        .child(text.to_string())
}

fn pill(text: &str, fg: u32, bg: u32) -> gpui::Div {
    div()
        .px_2()
        .rounded_full()
        .text_xs()
        .bg(color(bg))
        .text_color(color(fg))
        .child(text.to_string())
}

fn status_word(s: ProjectStatus) -> &'static str {
    match s {
        ProjectStatus::Active => "active",
        ProjectStatus::Paused => "paused",
        ProjectStatus::Done => "done",
    }
}

fn status_colour(s: TicketStatus) -> u32 {
    match s {
        TicketStatus::Todo => theme::BORDER_STRONG,
        TicketStatus::InProgress => theme::AMBER,
        TicketStatus::InReview => theme::TEAL,
        TicketStatus::Done => theme::GREEN,
        TicketStatus::Dropped => theme::TEXT_MUTED,
    }
}

/// A bar split by ticket status: done, in review, in progress, then todo (spec §8.2).
fn status_bar(c: &Counts, height: f32) -> AnyElement {
    let total = c.total();
    let mut bar = h_flex()
        .w_full()
        .h(px(height))
        .rounded_full()
        .overflow_hidden()
        .bg(color(theme::BORDER));
    if total > 0 {
        for status in [
            TicketStatus::Done,
            TicketStatus::InReview,
            TicketStatus::InProgress,
            TicketStatus::Todo,
        ] {
            let n = c.of(status);
            if n > 0 {
                bar = bar.child(
                    div()
                        .h_full()
                        .flex_grow(n as f32)
                        .flex_basis(px(0.))
                        .bg(color(status_colour(status))),
                );
            }
        }
    }
    bar.into_any_element()
}

fn counts_line(c: &Counts) -> AnyElement {
    h_flex()
        .gap_3()
        .text_xs()
        .text_color(color(theme::TEXT_SECONDARY))
        .child(format!("{} done", c.done))
        .child(format!("{} in review", c.in_review))
        .child(format!("{} in progress", c.in_progress))
        .child(format!("{} todo", c.todo))
        .into_any_element()
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}
