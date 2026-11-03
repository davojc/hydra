//! The Projects tab (Projects spec §8, §14): every environment's projects; one project's
//! milestones, decisions and progress, with its planning session beside it; a milestone's
//! tickets; what waits on you; a project's settings. Milestones and tickets are proposed by the
//! planning session and reviewed here by the lead; approved tickets are worked by sessions
//! started here. Reading and writing the repo is `crate::projects`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, ExternalPaths, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::select::{Select, SelectState};
use gpui_component::{Disableable, IndexPath, Selectable, Sizable, WindowExt, h_flex, v_flex};
use hydra_core::name::EnvName;
use hydraui_projects::progress::{Counts, is_late, milestone_counts, today};
use hydraui_projects::{
    Decision, Milestone, MilestoneStatus, Project, ProjectStatus, Review, Ticket, TicketKind,
    TicketStatus, repo,
};

use super::Workspace;
use crate::envs::PaneId;
use crate::hydra::{PaneRequest, command_line};
use crate::mcp::project_tools::{ASKED_BY_CLAUDE, Session};
use crate::mcp::server;
use crate::orchestrator_theme::{self as theme, color};
use crate::project_send::{self as ps, OpenPr, SendError, Sent};
use crate::projects::{self as pj, Item};

/// How often the checkouts are brought up to date while hydraui is open (spec §3).
const FETCH_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often the checkouts are read again while the tab shows: sessions write to them.
const READ_EVERY: Duration = Duration::from_secs(2);

/// Projects sessions ask through hydraui (plan_add_question, ticket_ask), not Claude Code's own
/// question tool, so every question is a recorded decision (spec §14). One word: the flag takes
/// a list, and would take the prompt after it too. People can still type to the session.
const NO_TERMINAL_QUESTIONS: &str = "--disallowedTools=AskUserQuestion";

/// Every review button's width, so the four line up as one set.
const REVIEW_BUTTON_WIDTH: f32 = 108.;

const MODELS: [&str; 4] = ["default", "opus", "sonnet", "haiku"];
const EFFORTS: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];

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
}

/// The project page's tabs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Section {
    #[default]
    Overview,
    Milestones,
    Questions,
}

pub(super) struct ProjectsTab {
    page: Page,
    /// Which tab a project page shows.
    section: Section,
    /// Paused and done projects show too.
    show_all: bool,
    /// Rejected milestones and tickets show too.
    show_rejected: bool,
    /// The project page shows its planning session beside it.
    show_claude: bool,
    /// The milestone points opened to their full explanation: (project, milestone, index).
    open_points: BTreeSet<(String, String, usize)>,
    /// The send bar lists the changes not sent yet.
    show_changes: bool,
}

/// One environment's projects, as last read.
#[derive(Default)]
pub(super) struct EnvProjects {
    repo: String,
    checkout: PathBuf,
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
    /// The checkout's `git status`, for listing those changes in words.
    status: String,
    /// Projects already in the repo, not only on this machine.
    sent: BTreeSet<String>,
    /// hydraui's open pull requests in the repo, as last fetched.
    prs: Vec<OpenPr>,
    /// Projects whose pending change is exactly what was sent for the lead's approval.
    sent_for_lead: BTreeSet<String>,
}

impl EnvProjects {
    fn lead_of(&self, slug: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|p| p.slug == slug)
            .map(|p| p.file.lead.as_str())
    }

    /// Whether `pr` waits on `login`: someone else's change to a project they lead, not sent
    /// back yet; or their own change the lead sent back (spec §8.4).
    fn pr_waits_on(&self, login: &str, pr: &OpenPr) -> bool {
        let mine = pr.author.eq_ignore_ascii_case(login);
        let leads = self
            .lead_of(&pr.slug)
            .is_some_and(|l| l.eq_ignore_ascii_case(login));
        (!mine && leads && pr.sent_back.is_none()) || (mine && pr.sent_back.is_some())
    }

    /// `login`'s open pull request for project `slug`.
    fn my_pr(&self, login: &str, slug: &str) -> Option<&OpenPr> {
        self.prs
            .iter()
            .find(|pr| pr.slug == slug && pr.author.eq_ignore_ascii_case(login))
    }
}

#[derive(Default)]
pub(super) struct ProjectsData {
    envs: BTreeMap<String, EnvProjects>,
    next_fetch: Option<Instant>,
    fetching: bool,
    next_read: Option<Instant>,
    /// Planning sessions, by (environment, project).
    planners: BTreeMap<(String, String), PaneId>,
    /// Work sessions, by (environment, project, ticket).
    workers: BTreeMap<(String, String, String), PaneId>,
    /// The questions waiting on you when last read, as (environment, project, id); `None`
    /// before the first read, so what was already waiting doesn't pop up at start.
    asked: Option<BTreeSet<(String, String, String)>>,
    /// New questions not toasted yet, shown together at most once a minute.
    toasts: QuestionToasts,
    /// Projects being sent now, as (environment, project).
    sending: BTreeSet<(String, String)>,
    /// Files that clashed in a project's last send, by (environment, project).
    clashes: BTreeMap<(String, String), Vec<String>>,
    /// Clashing files the engineer chose to keep their own version of.
    keep_mine: BTreeMap<(String, String), BTreeSet<String>>,
    /// Pull requests being approved or sent back, by URL.
    reviewing: BTreeSet<String>,
}

impl ProjectsData {
    /// What waits on you, across every environment: questions assigned to you, and as a lead,
    /// milestones and tickets to review.
    pub(super) fn waiting_count(&self) -> usize {
        self.envs
            .values()
            .filter_map(|e| {
                let login = e.login.as_deref()?;
                let reviews: usize = e
                    .projects
                    .iter()
                    .filter(|p| p.file.lead.eq_ignore_ascii_case(login))
                    .map(|p| {
                        let (m, t) = pj::to_review(p);
                        m + t
                    })
                    .sum();
                let prs = e.prs.iter().filter(|pr| e.pr_waits_on(login, pr)).count();
                Some(pj::questions_for(login, &e.projects).len() + reviews + prs)
            })
            .sum()
    }
}

/// New questions waiting on you, toasted together and at most once a minute: the count on
/// the tabs carries every question; the toast only says some arrived.
#[derive(Default)]
struct QuestionToasts {
    /// Each new question, as one line.
    pending: Vec<String>,
    last: Option<Instant>,
}

impl QuestionToasts {
    const EVERY: Duration = Duration::from_secs(60);

    /// The toast to show now, if one is due: the question itself when there's one, else how
    /// many.
    fn take(&mut self, now: Instant) -> Option<String> {
        if self.pending.is_empty() || self.last.is_some_and(|t| now < t + Self::EVERY) {
            return None;
        }
        self.last = Some(now);
        let text = match self.pending.as_slice() {
            [one] => format!("{one}\nAnswer it in Projects → Waiting on you."),
            many => format!(
                "{} new questions are waiting on you.\nAnswer them in Projects → Waiting on you.",
                many.len()
            ),
        };
        self.pending.clear();
        Some(text)
    }
}

/// Where resources go: a project, by environment and slug.
#[derive(Clone)]
struct Target {
    env: String,
    slug: String,
}

/// What a resource row's Rename does.
type Rename = dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

/// The choices Start work asks for.
struct WorkChoice {
    skill: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    repo: String,
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
                section: Section::Overview,
                show_all: false,
                show_rejected: false,
                show_claude: true,
                open_points: BTreeSet::new(),
                show_changes: false,
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

    /// An environment's projects repo changed: read them all again.
    pub(super) fn projects_config_changed(&mut self, cx: &mut Context<Self>) {
        self.projects_data.next_fetch = None;
        cx.notify();
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if let Some(tab) = &mut self.projects {
            let project = |p: &Page| match p {
                Page::Project { env, slug }
                | Page::Milestone { env, slug, .. }
                | Page::Settings { env, slug } => Some((env.clone(), slug.clone())),
                _ => None,
            };
            // Another project opens on its Overview; a milestone's way back keeps the tab.
            if project(&page) != project(&tab.page) {
                tab.section = Section::Overview;
            }
            tab.page = page;
        }
        cx.notify();
    }

    fn tab_flag(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ProjectsTab)) {
        if let Some(tab) = &mut self.projects {
            f(tab);
        }
        cx.notify();
    }

    /// Every second: the checkouts' files while the tab shows (sessions write to them), and a
    /// fetch when one is due (on open, then every 5 minutes).
    pub(super) fn projects_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Sessions ask and propose while the tab is closed too.
        let sessions =
            !self.projects_data.planners.is_empty() || !self.projects_data.workers.is_empty();
        if (self.projects.is_some() || sessions)
            && !self.projects_data.fetching
            && self
                .projects_data
                .next_read
                .is_none_or(|t| Instant::now() >= t)
        {
            self.projects_data.next_read = Some(Instant::now() + READ_EVERY);
            for e in self.projects_data.envs.values_mut().filter(|e| e.fetched) {
                read_checkout(e);
            }
            self.announce_questions(window, cx);
            cx.notify();
        }
        let data = &self.projects_data;
        if data.fetching || data.next_fetch.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let root = self.hydra.paths.root().to_path_buf();
        let jobs: Vec<(String, String, PathBuf, Option<String>)> = self
            .hydra
            .env_names()
            .into_iter()
            .filter_map(|env| {
                let repo = pj::configured_repo(&root, &env)?;
                let state = self.hydra.paths.state_dir(&EnvName::parse(&env).ok()?);
                let login = data
                    .envs
                    .get(&env)
                    .filter(|e| e.repo == repo)
                    .and_then(|e| e.login.clone());
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
        let exe = hydra_exe();
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
            let _ = this.update_in(cx, |ws, window, cx| {
                for (env, read) in fetched {
                    ws.projects_data.envs.insert(env, read);
                }
                ws.projects_data.fetching = false;
                ws.projects_data.next_fetch = Some(Instant::now() + FETCH_EVERY);
                ws.announce_questions(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// A toast for each question that started waiting on you since the last read (spec §8.4).
    fn announce_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut now = BTreeSet::new();
        let mut fresh = Vec::new();
        for (env, e) in &self.projects_data.envs {
            let Some(login) = e.login.as_deref() else {
                continue;
            };
            for (p, d) in pj::questions_for(login, &e.projects) {
                let key = (env.clone(), p.slug.clone(), d.id.clone());
                if self
                    .projects_data
                    .asked
                    .as_ref()
                    .is_some_and(|seen| !seen.contains(&key))
                {
                    let who = if d.meta.asked_by == ASKED_BY_CLAUDE {
                        "Claude asks".to_string()
                    } else {
                        format!("{} asks", d.meta.asked_by)
                    };
                    fresh.push(format!(
                        "{who} ({}, {}): {}",
                        p.file.name, d.id, d.meta.question
                    ));
                }
                now.insert(key);
            }
        }
        self.projects_data.asked = Some(now);
        self.projects_data.toasts.pending.extend(fresh);
        if let Some(text) = self.projects_data.toasts.take(Instant::now()) {
            window.push_notification(Notification::info(text), cx);
        }
    }

    /// Reads `env`'s checkout again after an edit here (local, so at once).
    fn reload_env(&mut self, env: &str, cx: &mut Context<Self>) {
        if let Some(e) = self.projects_data.envs.get_mut(env) {
            read_checkout(e);
        }
        cx.notify();
    }

    fn find_project(&self, env: &str, slug: &str) -> Option<&Project> {
        self.projects_data
            .envs
            .get(env)?
            .projects
            .iter()
            .find(|p| p.slug == slug)
    }

    fn project_dir(&self, env: &str, slug: &str) -> Option<PathBuf> {
        self.projects_data
            .envs
            .get(env)
            .map(|e| e.checkout.join(slug))
    }

    fn login_of(&self, env: &str) -> Option<String> {
        self.projects_data.envs.get(env)?.login.clone()
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

    // ---- Sending and approving (spec §5) ----

    /// Sends project `slug`'s pending change in the background (spec §5.1). `quiet` (a send
    /// after answering or asking) only speaks up when something goes wrong.
    fn send_project(
        &mut self,
        env: &str,
        slug: &str,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (env.to_string(), slug.to_string());
        if self.projects_data.sending.contains(&key) {
            return;
        }
        let Some(e) = self.projects_data.envs.get(env) else {
            return;
        };
        if !pj::is_github(&e.repo) {
            if !quiet {
                window.push_notification(
                    Notification::error(
                        "Sending needs a GitHub repo (owner/name) as the environment's projects repo.",
                    ),
                    cx,
                );
            }
            return;
        }
        let Some(login) = e.login.clone() else {
            window.push_notification(
                Notification::error("hydraui doesn't know your GitHub login yet: is gh signed in?"),
                cx,
            );
            return;
        };
        let Some(p) = e.projects.iter().find(|p| p.slug == slug) else {
            return;
        };
        let (repo, dir, name, lead) = (
            e.repo.clone(),
            e.checkout.clone(),
            p.file.name.clone(),
            p.file.lead.clone(),
        );
        let mine = self
            .projects_data
            .keep_mine
            .get(&key)
            .cloned()
            .unwrap_or_default();
        self.projects_data.sending.insert(key.clone());
        cx.notify();
        let exe = hydra_exe();
        let (env_s, slug_s) = key.clone();
        let lead_name = lead.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let r = ps::EnvRunner {
                        hydra: &exe,
                        env: &env_s,
                        cwd: &dir,
                    };
                    ps::send(
                        &r,
                        &ps::Send {
                            repo: &repo,
                            dir: &dir,
                            slug: &slug_s,
                            name: &name,
                            lead: &lead,
                            login: &login,
                            today: &today(),
                            mine: &mine,
                        },
                    )
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.after_send(key, &lead_name, result, quiet, window, cx)
            });
        })
        .detach();
    }

    fn after_send(
        &mut self,
        key: (String, String),
        lead: &str,
        result: Result<Sent, SendError>,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let data = &mut self.projects_data;
        data.sending.remove(&key);
        data.clashes.remove(&key);
        let said = match result {
            Ok(sent) => {
                data.keep_mine.remove(&key);
                match sent {
                    Sent::Merged(_) => Some(Notification::success("Sent: it's in the repo.")),
                    Sent::Waiting(_) => Some(Notification::success(format!(
                        "Sent to {lead} for approval."
                    ))),
                    Sent::Nothing => Some(Notification::info(
                        "Nothing to send: the repo has all of it.",
                    )),
                }
                .filter(|_| !quiet)
            }
            Err(SendError::Clash(paths)) => {
                data.clashes.insert(key.clone(), paths);
                None
            }
            Err(SendError::Failed(why)) => {
                Some(Notification::error(format!("Couldn't send: {why}")))
            }
        };
        if let Some(n) = said {
            window.push_notification(n, cx);
        }
        // Fetch again soon, for the pull request's state.
        data.next_fetch = None;
        self.reload_env(&key.0, cx);
    }

    /// Settles one clashing file: `mine` keeps this side's version, else the repo's. When the
    /// last one is settled, the project is sent again.
    fn settle_clash(
        &mut self,
        env: &str,
        slug: &str,
        path: &str,
        mine: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (env.to_string(), slug.to_string());
        if mine {
            self.projects_data
                .keep_mine
                .entry(key.clone())
                .or_default()
                .insert(path.to_string());
        } else if let Some(e) = self.projects_data.envs.get(env) {
            let r = ps::EnvRunner {
                hydra: &hydra_exe(),
                env,
                cwd: &e.checkout,
            };
            if let Err(why) = ps::take_theirs(&r, &e.checkout, path) {
                window.push_notification(Notification::error(why), cx);
                return;
            }
        }
        let left = self.projects_data.clashes.get_mut(&key).map(|c| {
            c.retain(|p| p != path);
            c.len()
        });
        if left == Some(0) {
            self.send_project(env, slug, false, window, cx);
        }
        self.reload_env(env, cx);
    }

    /// The lead approves someone's change and merges it (spec §5.2).
    fn approve_pr(&mut self, env: &str, pr: OpenPr, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.projects_data.envs.get(env).map(|e| e.checkout.clone()) else {
            return;
        };
        if !self.projects_data.reviewing.insert(pr.url.clone()) {
            return;
        }
        cx.notify();
        let (exe, env_s) = (hydra_exe(), env.to_string());
        cx.spawn_in(window, async move |this, cx| {
            let (url, author) = (pr.url.clone(), pr.author.clone());
            let result = cx
                .background_spawn(async move {
                    let r = ps::EnvRunner {
                        hydra: &exe,
                        env: &env_s,
                        cwd: &dir,
                    };
                    ps::approve(&r, &dir, &pr)
                })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.projects_data.reviewing.remove(&url);
                ws.projects_data.next_fetch = None;
                let n = match result {
                    Ok(()) => Notification::success(format!(
                        "Approved {author}'s change: it's in the repo."
                    )),
                    Err(why) => Notification::error(format!("Couldn't approve it: {why}")),
                };
                window.push_notification(n, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The lead sends someone's change back with a note (spec §5.2).
    fn send_back_dialog(
        &mut self,
        env: String,
        pr: OpenPr,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields_dialog(
            format!("Send back {}'s change", pr.author),
            vec![(
                "Note",
                "what to change before you approve it",
                String::new(),
            )],
            move |ws, v, window, cx| {
                if v[0].is_empty() {
                    return Err("Say what to change.".into());
                }
                let Some(dir) = ws.projects_data.envs.get(&env).map(|e| e.checkout.clone()) else {
                    return Err(format!("{env} has no projects repo any more."));
                };
                ws.projects_data.reviewing.insert(pr.url.clone());
                let (exe, env_s, pr, note) = (hydra_exe(), env.clone(), pr.clone(), v[0].clone());
                cx.spawn_in(window, async move |this, cx| {
                    let url = pr.url.clone();
                    let result = cx
                        .background_spawn(async move {
                            let r = ps::EnvRunner {
                                hydra: &exe,
                                env: &env_s,
                                cwd: &dir,
                            };
                            ps::send_back(&r, &pr, &note)
                        })
                        .await;
                    let _ = this.update_in(cx, |ws, window, cx| {
                        ws.projects_data.reviewing.remove(&url);
                        ws.projects_data.next_fetch = None;
                        if let Err(why) = result {
                            window.push_notification(
                                Notification::error(format!("Couldn't send it back: {why}")),
                                cx,
                            );
                        }
                        cx.notify();
                    });
                })
                .detach();
                Ok(())
            },
            window,
            cx,
        );
    }

    /// What's waiting to be sent for a project, and the button that sends it; with clashes to
    /// settle and the lead's note when they sent a change back (spec §5, §8.4).
    fn send_bar(&self, env: &str, p: &Project, cx: &mut Context<Self>) -> Option<AnyElement> {
        let e = self.projects_data.envs.get(env)?;
        if !pj::is_github(&e.repo) {
            return None;
        }
        let login = e.login.clone()?;
        let lead = p.file.lead.eq_ignore_ascii_case(&login);
        let key = (env.to_string(), p.slug.clone());
        let unsent = pj::project_unsent(&e.pending, &p.slug);
        let my_pr = e.my_pr(&login, &p.slug).cloned();
        let (env_s, slug_s) = key.clone();
        let boxed = |edge: u32, tint: u32| {
            v_flex()
                .gap_2()
                .px_4()
                .py_3()
                .rounded_lg()
                .bg(color(tint))
                .border_1()
                .border_color(color(edge))
        };
        let view = |url: String| {
            Button::new(SharedString::from(format!("view-pr-{url}")))
                .label("View on GitHub")
                .xsmall()
                .ghost()
                .on_click(move |_, _, cx| cx.open_url(&url))
        };

        if let Some(paths) = self
            .projects_data
            .clashes
            .get(&key)
            .filter(|c| !c.is_empty())
        {
            let mut b = boxed(theme::AMBER, theme::AMBER_TINT).child(div().text_sm().child(
                "These files changed here and in the repo in the same places. Pick which \
                     to keep, then it sends again:",
            ));
            for path in paths {
                let (e1, s1, p1) = (env_s.clone(), slug_s.clone(), path.clone());
                let (e2, s2, p2) = (env_s.clone(), slug_s.clone(), path.clone());
                b = b.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("monospace")
                                .text_sm()
                                .child(path.clone()),
                        )
                        .child(
                            Button::new(SharedString::from(format!("mine-{path}")))
                                .label("Keep mine")
                                .small()
                                .outline()
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.settle_clash(&e1, &s1, &p1, true, window, cx)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("theirs-{path}")))
                                .label("Use theirs")
                                .small()
                                .outline()
                                .on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.settle_clash(&e2, &s2, &p2, false, window, cx)
                                })),
                        ),
                );
            }
            return Some(b.into_any_element());
        }

        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(pr) = my_pr.as_ref().filter(|pr| pr.sent_back.is_some()) {
            let note = pr.sent_back.clone().unwrap_or_default();
            lines.push(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(theme::AMBER))
                            .child(format!(
                                "{} sent your change back{}",
                                p.file.lead,
                                if note.is_empty() { "." } else { ":" }
                            )),
                    )
                    .when(!note.is_empty(), |d| {
                        d.child(div().text_sm().child(format!("“{note}”")))
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child("Change it here, then send again: it updates the same change."),
                    )
                    .into_any_element(),
            );
        }
        let sending = self.projects_data.sending.contains(&key);
        let waiting = e.sent_for_lead.contains(&p.slug);
        let status = if sending {
            "Sending…".to_string()
        } else if unsent > 0 && waiting {
            format!("Sent to {} for approval.", p.file.lead)
        } else if unsent > 0 {
            format!(
                "{unsent} change{} not sent yet.",
                if unsent == 1 { "" } else { "s" }
            )
        } else if my_pr.is_some() {
            format!("Waiting for {} to approve your change.", p.file.lead)
        } else if lines.is_empty() {
            return None;
        } else {
            return Some(
                boxed(theme::AMBER, theme::AMBER_TINT)
                    .children(lines)
                    .into_any_element(),
            );
        };
        let can_send = unsent > 0 && !waiting && !sending;
        let show_changes = unsent > 0 && self.projects.as_ref().is_some_and(|t| t.show_changes);
        let row = h_flex()
            .gap_3()
            .items_center()
            .flex_wrap()
            .child(div().text_sm().child(status))
            .child(div().flex_1())
            .when(unsent > 0, |d| {
                d.child(
                    Button::new("show-changes")
                        .label(if show_changes {
                            "Hide changes"
                        } else {
                            "Show changes"
                        })
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|ws, _, _, cx| {
                            ws.tab_flag(cx, |t| t.show_changes = !t.show_changes)
                        })),
                )
            })
            .when_some(my_pr.map(|pr| pr.url), |d, url| d.child(view(url)))
            .when(can_send, |d| {
                let (e, s) = (env_s.clone(), slug_s.clone());
                d.child(
                    Button::new("send-now")
                        .label(if lead {
                            "Send now".to_string()
                        } else {
                            format!("Send to {}", p.file.lead)
                        })
                        .small()
                        .primary()
                        .tooltip(if lead {
                            "Puts your changes in the projects repo"
                        } else {
                            "Sends your changes to the lead to approve"
                        })
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.send_project(&e, &s, false, window, cx)
                        })),
                )
            });
        let sent_back = !lines.is_empty();
        let (edge, tint) = if sent_back {
            (theme::AMBER, theme::AMBER_TINT)
        } else if can_send {
            (theme::TEAL_BORDER, theme::TEAL_TINT)
        } else {
            (theme::BORDER, theme::SURFACE)
        };
        let mut changes = v_flex().gap_1();
        if show_changes {
            for (i, (words, path)) in ps::pending_changes(&e.status, &p.slug)
                .into_iter()
                .enumerate()
            {
                let file = e.checkout.join(&path);
                let exists = file.is_file();
                changes = changes.child(
                    h_flex()
                        .id(SharedString::from(format!("change-{i}")))
                        .gap_3()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .when(exists, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(color(theme::RAISED)))
                                .on_click(move |_, _, cx| cx.open_with_system(&file))
                        })
                        .child(div().flex_none().text_sm().child(words))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(color(theme::TEXT_MUTED))
                                .child(
                                    path.trim_start_matches(&format!("{}/", p.slug)).to_string(),
                                ),
                        ),
                );
            }
        }
        Some(
            boxed(edge, tint)
                .children(lines)
                .child(row)
                .when(show_changes, |d| {
                    d.child(
                        div()
                            .pt_2()
                            .border_t_1()
                            .border_color(color(theme::BORDER_SUBTLE))
                            .child(changes),
                    )
                })
                .into_any_element(),
        )
    }

    /// Other people's changes to projects `login` leads, for them to approve or send back,
    /// optionally only project `slug`'s (spec §5.2).
    fn approvals(&self, env: &str, slug: Option<&str>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(e) = self.projects_data.envs.get(env) else {
            return Vec::new();
        };
        let Some(login) = e.login.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for pr in e.prs.iter().filter(|pr| {
            slug.is_none_or(|s| pr.slug == s)
                && !pr.author.eq_ignore_ascii_case(login)
                && e.lead_of(&pr.slug)
                    .is_some_and(|l| l.eq_ignore_ascii_case(login))
        }) {
            let busy = self.projects_data.reviewing.contains(&pr.url);
            let items: Vec<String> = pr
                .body
                .lines()
                .filter_map(|l| l.trim().strip_prefix("- "))
                .map(str::to_string)
                .collect();
            let (e1, p1) = (env.to_string(), pr.clone());
            let (e2, p2) = (env.to_string(), pr.clone());
            let url = pr.url.clone();
            let mut card = v_flex()
                .gap_2()
                .px_4()
                .py_3()
                .rounded_lg()
                .bg(color(theme::SURFACE))
                .border_1()
                .border_color(color(if pr.sent_back.is_some() {
                    theme::BORDER
                } else {
                    theme::AMBER
                }))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(pr.author.clone()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child("wants to change"),
                        )
                        .when(pr.sent_back.is_some(), |d| {
                            d.child(pill("sent back", theme::TEXT_SECONDARY, theme::RAISED))
                        }),
                )
                .child(div().text_sm().child(pr.title.clone()));
            for item in items {
                card = card.child(
                    v_flex()
                        .relative()
                        .w_full()
                        .pl(px(16.))
                        .child(
                            div()
                                .absolute()
                                .left(px(4.))
                                .top(px(8.))
                                .size(px(5.))
                                .rounded_full()
                                .bg(color(theme::TEXT_MUTED)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(item),
                        ),
                );
            }
            card = card.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .pt_2()
                    .border_t_1()
                    .border_color(color(theme::BORDER_SUBTLE))
                    .child(
                        Button::new(SharedString::from(format!("approve-pr-{}", pr.url)))
                            .label(if busy { "Working…" } else { "Approve" })
                            .small()
                            .primary()
                            .w(px(REVIEW_BUTTON_WIDTH))
                            .disabled(busy)
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.approve_pr(&e1, p1.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("back-pr-{}", pr.url)))
                            .label("Send back…")
                            .small()
                            .outline()
                            .w(px(REVIEW_BUTTON_WIDTH))
                            .disabled(busy)
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.send_back_dialog(e2.clone(), p2.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("open-pr-{}", pr.url)))
                            .label("View on GitHub")
                            .small()
                            .ghost()
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            );
            out.push(card.into_any_element());
        }
        out
    }

    // ---- Starting a project (spec §14) ----

    fn new_project(&mut self, env: String, window: &mut Window, cx: &mut Context<Self>) {
        self.text_dialog(
            format!("New project in {env}"),
            String::new(),
            "the project's name",
            move |ws, name, _window, cx| {
                let (checkout, login) = {
                    let e = ws
                        .projects_data
                        .envs
                        .get(&env)
                        .ok_or_else(|| format!("{env} has no projects repo"))?;
                    let login = e.login.clone().ok_or(
                        "hydraui doesn't know your GitHub login yet: is gh signed in for this environment?",
                    )?;
                    (e.checkout.clone(), login)
                };
                let slug = pj::new_project(&checkout, &name, &login)?;
                ws.reload_env(&env, cx);
                ws.go(
                    Page::Project {
                        env: env.clone(),
                        slug,
                    },
                    cx,
                );
                Ok(())
            },
            window,
            cx,
        );
    }

    /// The planning session of project `slug`, if it's running.
    fn planner(&self, env: &str, slug: &str) -> Option<PaneId> {
        self.projects_data
            .planners
            .get(&(env.to_string(), slug.to_string()))
            .copied()
            .filter(|id| self.panes.contains_key(id))
    }

    /// Gives the planning session of project `slug` `prompt`: typed into it when it's running,
    /// else as the first prompt of a new one. Shows it beside the project.
    fn tell_planner(
        &mut self,
        env: &str,
        slug: &str,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.planner(env, slug)
            && let Some(view) = self.panes.get(&id).cloned()
        {
            let typed = view.update(cx, |v, cx| v.type_prompt(&prompt, cx));
            if !typed {
                window.push_notification(
                    Notification::error("The planning session isn't running; start it again."),
                    cx,
                );
            }
        } else if let Err(e) = self.start_planner(env, slug, prompt, window, cx) {
            window.push_notification(Notification::error(e), cx);
            return;
        }
        self.tab_flag(cx, |t| t.show_claude = true);
    }

    fn start_planner(
        &mut self,
        env: &str,
        slug: &str,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let dir = self.project_dir(env, slug).ok_or("no such project")?;
        let name = self
            .find_project(env, slug)
            .map(|p| p.file.name.clone())
            .unwrap_or_else(|| slug.to_string());
        let login = self
            .login_of(env)
            .ok_or("hydraui doesn't know your GitHub login yet")?;
        let shell = self.shell();
        let mut request = PaneRequest::new(env, dir.clone(), shell);
        let argv = vec![
            "claude".to_string(),
            NO_TERMINAL_QUESTIONS.to_string(),
            prompt,
        ];
        request.command = Some(command_line(shell, &argv));
        let id = self
            .open_terminal_beside(env, request, window, cx)
            .ok_or_else(|| format!("Open the {env} environment first, then try again."))?;
        let view = self
            .panes
            .get(&id)
            .cloned()
            .ok_or("its terminal didn't open")?;
        view.update(cx, |v, cx| v.set_name(Some(format!("plan · {name}")), cx));
        let token = view
            .read(cx)
            .mcp_token()
            .map(str::to_string)
            .ok_or("its terminal has no MCP token")?;
        let sv = server::get().ok_or("hydraui's MCP server isn't running")?;
        server::lock(sv.registry())
            .project_sessions
            .insert(token, Session::Plan { dir, login });
        self.projects_data
            .planners
            .insert((env.to_string(), slug.to_string()), id);
        Ok(())
    }

    fn start_project(
        &mut self,
        env: String,
        slug: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.find_project(&env, &slug) else {
            return;
        };
        let prompt = start_prompt(p);
        self.tell_planner(&env, &slug, prompt, window, cx);
    }

    fn create_tickets(
        &mut self,
        env: String,
        slug: String,
        m: &Milestone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = format!(
            "The lead approved milestone {} \"{}\" ({}). Propose its tickets now with \
             plan_propose_ticket: each small enough to finish in a few days, each with what to \
             do and how to tell it's done. Call plan_show first to see what exists and what \
             was rejected. Ask anything you need decided with plan_add_question, not here. Then \
             tell the lead they're ready to review in hydraui.",
            m.id, m.title, m.description
        );
        self.tell_planner(&env, &slug, prompt, window, cx);
    }

    /// Records `answer` to question `q`, by you, now; a question a Claude session asked goes
    /// back to it, typed into its terminal (spec §14).
    fn answer_question(
        &mut self,
        env: &str,
        slug: &str,
        q: &Decision,
        answer: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.edit_project(
            env,
            slug,
            |dir, _, login| pj::answer(dir, q, answer, login),
            window,
            cx,
        )?;
        // An answer goes out at once, so it reaches the lead without waiting (spec §4).
        self.send_project(env, slug, true, window, cx);
        if q.meta.asked_by != ASKED_BY_CLAUDE {
            return Ok(());
        }
        let who = self.login_of(env).unwrap_or_default();
        let text = format!(
            "{} was decided in hydraui by {who}.\nQuestion: {}\nAnswer: {}\n\nCarry on with that.",
            q.id,
            q.meta.question,
            answer.trim()
        );
        // A ticket's question goes to its work session only; the rest to the planning session.
        let pane = if q.meta.ticket.is_empty() {
            self.planner(env, slug)
        } else {
            self.worker(env, slug, &q.meta.ticket)
        };
        match pane.and_then(|id| self.panes.get(&id).cloned()) {
            Some(view) if view.update(cx, |v, cx| v.type_prompt(&text, cx)) => {
                window.push_notification(Notification::success(format!("Recorded, and sent to the session that asked {}.", q.id)), cx);
            }
            _ => window.push_notification(
                Notification::info(format!(
                    "Recorded. The session that asked {} isn't running; it sees the answer when it next reads the project.",
                    q.id
                )),
                cx,
            ),
        }
        Ok(())
    }

    // ---- Review (spec §14) ----

    #[allow(clippy::too_many_arguments)] // What the review is of, and what it says.
    fn set_review(
        &mut self,
        env: &str,
        slug: &str,
        item: Item,
        to: Review,
        note: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.edit_project(
            env,
            slug,
            |dir, _, login| pj::review(dir, &item, to, note, login),
            window,
            cx,
        )
    }

    fn note_dialog(
        &mut self,
        env: String,
        slug: String,
        item: Item,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_dialog(
            format!("What should change in {}?", item.id()),
            String::new(),
            "e.g. split it in two; Claude revises it and proposes it again",
            move |ws, note, window, cx| {
                ws.set_review(
                    &env,
                    &slug,
                    item.clone(),
                    Review::Changes,
                    &note,
                    window,
                    cx,
                )?;
                let prompt = format!(
                    "The lead asked for changes to {} \"{title}\": {note}\n\nRevise it with \
                     plan_revise (or plan_withdraw it and propose new ones if it should be \
                     split), then tell the lead it's ready to review again.",
                    item.id()
                );
                ws.tell_planner(&env, &slug, prompt, window, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    fn reject_dialog(
        &mut self,
        env: String,
        slug: String,
        item: Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_dialog(
            format!("Reject {}", item.id()),
            String::new(),
            "why; it stays, marked rejected, so it isn't proposed again",
            move |ws, reason, window, cx| {
                ws.set_review(
                    &env,
                    &slug,
                    item.clone(),
                    Review::Rejected,
                    &reason,
                    window,
                    cx,
                )
            },
            window,
            cx,
        );
    }

    fn edit_milestone_dialog(
        &mut self,
        env: String,
        slug: String,
        m: Milestone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields_dialog(
            format!("Edit {}", m.id),
            vec![
                ("Title", "the milestone, in a few words", m.title.clone()),
                (
                    "Target date",
                    "YYYY-MM-DD, or empty",
                    m.target.map(|t| t.to_string()).unwrap_or_default(),
                ),
                (
                    "Points",
                    "- a point\n  its full explanation, indented under it",
                    m.description.clone(),
                ),
            ],
            move |ws, v, window, cx| {
                let id = m.id.clone();
                ws.edit_project(
                    &env,
                    &slug,
                    |dir, _, _| pj::edit_milestone(dir, &id, &v[0], &v[1], &v[2]),
                    window,
                    cx,
                )
            },
            window,
            cx,
        );
    }

    fn edit_ticket_dialog(
        &mut self,
        env: String,
        slug: String,
        t: Ticket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields_dialog(
            format!("Edit {}", t.id),
            vec![
                ("Title", "the ticket, in one line", t.meta.title.clone()),
                (
                    "Description",
                    "what to do and how to tell it's done",
                    t.body.trim().to_string(),
                ),
            ],
            move |ws, v, window, cx| {
                let t = t.clone();
                ws.edit_project(
                    &env,
                    &slug,
                    |dir, _, _| pj::edit_ticket(dir, &t, &v[0], &v[1]),
                    window,
                    cx,
                )
            },
            window,
            cx,
        );
    }

    /// A milestone's points as a list, each clicked open to its full explanation. An older
    /// paragraph shows one point per sentence, with nothing more to open.
    fn milestone_points(
        &self,
        env: &str,
        slug: &str,
        m: &Milestone,
        lead: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let points = pj::points_of(&m.description).unwrap_or_else(|| {
            pj::sentences_of(&m.description)
                .into_iter()
                .map(|s| pj::Point {
                    text: s.to_string(),
                    detail: String::new(),
                })
                .collect()
        });
        let open = |i: usize| {
            self.projects
                .as_ref()
                .is_some_and(|t| t.open_points.contains(&(slug.to_string(), m.id.clone(), i)))
        };
        let mut list = v_flex().gap_1();
        for (i, point) in points.into_iter().enumerate() {
            let is_open = open(i);
            let key = (slug.to_string(), m.id.clone(), i);
            // The chevron is placed absolutely so the point's text is a stack child with the
            // full width (text in a growing column of a row wraps narrow).
            list = list.child(
                v_flex()
                    .id(SharedString::from(format!("point-{}-{}-{i}", slug, m.id)))
                    .relative()
                    .w_full()
                    .pl(px(20.))
                    .pr_2()
                    .py(px(2.))
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(color(theme::RAISED)))
                    .child(
                        div()
                            .absolute()
                            .left(px(5.))
                            .top(px(3.))
                            .text_xs()
                            .text_color(color(if is_open {
                                theme::TEAL
                            } else {
                                theme::TEXT_MUTED
                            }))
                            .child(if is_open { "▾" } else { "▸" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(if is_open {
                                theme::TEXT
                            } else {
                                theme::TEXT_SECONDARY
                            }))
                            .child(point.text.clone()),
                    )
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if let Some(t) = ws.projects.as_mut()
                            && !t.open_points.remove(&key)
                        {
                            t.open_points.insert(key.clone());
                        }
                        cx.notify();
                    })),
            );
            if !is_open {
                continue;
            }
            let mut detail = v_flex()
                .ml(px(20.))
                .mb_1()
                .pl_3()
                .py_1()
                .gap_1()
                .border_l_2()
                .border_color(color(theme::TEAL_BORDER));
            if point.detail.is_empty() {
                detail = detail.child(
                    div()
                        .text_sm()
                        .text_color(color(theme::TEXT_MUTED))
                        .child("No full explanation was written for this point."),
                );
                if lead && m.review.waiting() {
                    let (e, s, prompt) = (
                        env.to_string(),
                        slug.to_string(),
                        format!(
                            "The lead wants the full explanation of \"{}\" in {}. Call \
                             plan_show, then give every point of {} its detail with plan_revise.",
                            point.text, m.id, m.id
                        ),
                    );
                    detail = detail.child(
                        h_flex().child(
                            Button::new(SharedString::from(format!(
                                "explain-{}-{}-{i}",
                                slug, m.id
                            )))
                            .label("Ask Claude to explain")
                            .xsmall()
                            .outline()
                            .on_click(cx.listener(
                                move |ws, _, window, cx| {
                                    ws.tell_planner(&e, &s, prompt.clone(), window, cx)
                                },
                            )),
                        ),
                    );
                }
            } else {
                for line in point.detail.lines() {
                    detail = detail.child(if line.trim().is_empty() {
                        div().h(px(4.))
                    } else {
                        div()
                            .text_sm()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(line.to_string())
                    });
                }
            }
            list = list.child(detail);
        }
        list
    }

    /// Approve, Edit…, Changes…, Reject…, for the lead on an item still to review.
    fn review_actions(
        &self,
        env: &str,
        slug: &str,
        item: Item,
        title: &str,
        edit: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = item.id().to_string();
        let (e1, s1, i1) = (env.to_string(), slug.to_string(), item.clone());
        let (e2, s2, i2, t2) = (
            env.to_string(),
            slug.to_string(),
            item.clone(),
            title.to_string(),
        );
        let (e3, s3, i3) = (env.to_string(), slug.to_string(), item);
        // One size and one order everywhere: the yes first, the no last.
        let sized = |b: Button| b.small().w(px(REVIEW_BUTTON_WIDTH));
        h_flex()
            .flex_wrap()
            .gap_2()
            .child(
                sized(Button::new(SharedString::from(format!("approve-{id}"))))
                    .label("Approve")
                    .primary()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        let _ =
                            ws.set_review(&e1, &s1, i1.clone(), Review::Approved, "", window, cx);
                    })),
            )
            .child(
                sized(Button::new(SharedString::from(format!("note-{id}"))))
                    .label("Ask Claude…")
                    .outline()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.note_dialog(e2.clone(), s2.clone(), i2.clone(), t2.clone(), window, cx)
                    })),
            )
            .child(
                sized(Button::new(SharedString::from(format!("edit-{id}"))))
                    .label("Edit…")
                    .outline()
                    .on_click(cx.listener(move |ws, _, window, cx| edit(ws, window, cx))),
            )
            .child(
                sized(Button::new(SharedString::from(format!("reject-{id}"))))
                    .label("Reject…")
                    .danger()
                    .outline()
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.reject_dialog(e3.clone(), s3.clone(), i3.clone(), window, cx)
                    })),
            )
    }

    // ---- Work (spec §14) ----

    fn worker(&self, env: &str, slug: &str, ticket: &str) -> Option<PaneId> {
        self.projects_data
            .workers
            .get(&(env.to_string(), slug.to_string(), ticket.to_string()))
            .copied()
            .filter(|id| self.panes.contains_key(id))
    }

    fn start_work_dialog(
        &mut self,
        env: String,
        slug: String,
        ticket: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.find_project(&env, &slug) else {
            return;
        };
        if p.file.repos.is_empty() {
            window.push_notification(
                Notification::error("Set the project's code repos in its Settings first: work happens in a worktree of one of them."),
                cx,
            );
            return;
        }
        let repos = p.file.repos.clone();
        let dir = self.project_dir(&env, &slug).unwrap_or_default();
        let mut skills = vec!["none".to_string()];
        let env_claude = EnvName::parse(&env)
            .ok()
            .map(|n| self.hydra.paths.state_dir(&n).join("claude"));
        let mut found: BTreeSet<String> =
            crate::skills_list::installed(env_claude.as_deref(), &dir)
                .into_iter()
                .collect();
        found.extend(crate::skills_list::installed(
            Some(&self.hydra.user_home.join(".claude")),
            &dir,
        ));
        skills.extend(found);
        let select = |items: Vec<String>, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                SelectState::new(items, Some(IndexPath::new(0)), window, cx).searchable(true)
            })
        };
        let skill = select(skills, window, cx);
        let model = select(MODELS.iter().map(|s| s.to_string()).collect(), window, cx);
        let effort = select(EFFORTS.iter().map(|s| s.to_string()).collect(), window, cx);
        let repo = select(repos, window, cx);
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let row = |label: &'static str, select: &Entity<SelectState<Vec<String>>>| {
                h_flex()
                    .gap_3()
                    .child(div().w(px(90.)).flex_none().text_sm().text_color(color(theme::TEXT_SECONDARY)).child(label))
                    .child(div().flex_1().child(Select::new(select).small()))
            };
            let (skill2, model2, effort2, repo2, ws) = (skill.clone(), model.clone(), effort.clone(), repo.clone(), workspace.clone());
            let (env, slug, ticket) = (env.clone(), slug.clone(), ticket.clone());
            dialog
                .title(format!("Start work on {ticket}"))
                .w(px(560.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(row("Skill", &skill))
                        .child(row("Model", &model))
                        .child(row("Effort", &effort))
                        .child(row("Repo", &repo))
                        .child(muted("hydra makes a fresh worktree of the repo on a branch named after the ticket, and starts Claude there. The session ends by reporting its pull requests here.")),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("start").label("Start work").primary().on_click(move |_, window, cx| {
                            let pick = |s: &Entity<SelectState<Vec<String>>>, cx: &gpui::App| {
                                s.read(cx).selected_value().cloned().filter(|v| v != "none" && v != "default")
                            };
                            let choice = WorkChoice {
                                skill: pick(&skill2, cx),
                                model: pick(&model2, cx),
                                effort: pick(&effort2, cx),
                                repo: repo2.read(cx).selected_value().cloned().unwrap_or_default(),
                            };
                            window.close_dialog(cx);
                            let (env, slug, ticket) = (env.clone(), slug.clone(), ticket.clone());
                            ws.update(cx, |ws, cx| ws.begin_work(env, slug, ticket, choice, window, cx));
                        })),
                )
        });
    }

    fn begin_work(
        &mut self,
        env: String,
        slug: String,
        ticket: String,
        choice: WorkChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        match pj::clone_of(&root, &choice.repo) {
            Some(clone) => self.work_in(env, slug, ticket, choice, clone, window, cx),
            None => {
                window.push_notification(
                    Notification::info(format!(
                        "Where is {} cloned on this Mac? Pick its folder.",
                        choice.repo
                    )),
                    cx,
                );
                self.open_dialog_path(true, window, cx, move |ws, path, window, cx| {
                    match pj::set_clone(&root, &choice.repo, &path) {
                        Ok(()) => ws.work_in(env, slug, ticket, choice, path, window, cx),
                        Err(e) => window.push_notification(Notification::error(e), cx),
                    }
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // One call site; a struct would only rename them.
    fn work_in(
        &mut self,
        env: String,
        slug: String,
        ticket: String,
        choice: WorkChoice,
        clone: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self
            .find_project(&env, &slug)
            .and_then(|p| p.tickets.iter().find(|t| t.id == ticket))
            .cloned()
        else {
            return;
        };
        window.push_notification(
            Notification::info(format!("Making a worktree for {ticket}…")),
            cx,
        );
        let exe = hydra_exe();
        let env_bg = env.clone();
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_spawn(async move { pj::ticket_worktree(&exe, &env_bg, &clone, &t) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| match made {
                Ok((path, branch)) => {
                    if let Err(e) =
                        ws.open_worker(&env, &slug, &ticket, &choice, path, &branch, window, cx)
                    {
                        window.push_notification(Notification::error(e), cx);
                    }
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("Can't make the worktree: {e}")),
                    cx,
                ),
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)] // One call site; a struct would only rename them.
    fn open_worker(
        &mut self,
        env: &str,
        slug: &str,
        ticket: &str,
        choice: &WorkChoice,
        path: PathBuf,
        branch: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let how = (
            choice.skill.as_deref().unwrap_or(""),
            choice.model.as_deref().unwrap_or(""),
            choice.effort.as_deref().unwrap_or(""),
        );
        self.edit_project(
            env,
            slug,
            |dir, p, login| {
                let t = p
                    .tickets
                    .iter()
                    .find(|t| t.id == ticket)
                    .ok_or("the ticket is gone")?;
                pj::start_work(dir, t, branch, login, how)
            },
            window,
            cx,
        )?;
        let p = self
            .find_project(env, slug)
            .ok_or("the project is gone")?
            .clone();
        let t = p
            .tickets
            .iter()
            .find(|t| t.id == ticket)
            .ok_or("the ticket is gone")?
            .clone();
        let dir = self.project_dir(env, slug).ok_or("the project is gone")?;
        let shell = self.shell();
        let mut request = PaneRequest::new(env, path, shell);
        let mut argv = vec!["claude".to_string(), NO_TERMINAL_QUESTIONS.to_string()];
        if let Some(m) = &choice.model {
            argv.extend(["--model".to_string(), m.clone()]);
        }
        if let Some(e) = &choice.effort {
            argv.extend(["--effort".to_string(), e.clone()]);
        }
        argv.push(work_prompt(
            &p,
            &t,
            branch,
            &choice.repo,
            choice.skill.as_deref(),
        ));
        request.command = Some(command_line(shell, &argv));
        let id = self
            .open_terminal_beside(env, request, window, cx)
            .ok_or_else(|| format!("Open the {env} environment first, then try again."))?;
        let view = self
            .panes
            .get(&id)
            .cloned()
            .ok_or("its terminal didn't open")?;
        view.update(cx, |v, cx| {
            v.set_name(Some(format!("{} · {}", t.id, t.meta.title)), cx)
        });
        let token = view
            .read(cx)
            .mcp_token()
            .map(str::to_string)
            .ok_or("its terminal has no MCP token")?;
        let sv = server::get().ok_or("hydraui's MCP server isn't running")?;
        server::lock(sv.registry()).project_sessions.insert(
            token,
            Session::Work {
                dir,
                ticket: ticket.to_string(),
            },
        );
        self.projects_data
            .workers
            .insert((env.to_string(), slug.to_string(), ticket.to_string()), id);
        window.push_notification(
            Notification::success(format!(
                "{ticket} is being worked on in its own terminal ({branch})."
            )),
            cx,
        );
        Ok(())
    }

    // ---- Resources ----

    fn resources_changed(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.reload_env(&target.env, cx);
    }

    /// Attaches `paths`. One file is named first (its own name offered); several keep their
    /// names, to rename from the list.
    fn add_files(
        &mut self,
        target: Target,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.project_dir(&target.env, &target.slug) else {
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

    fn pick_file(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog_path(false, window, cx, move |ws, path, window, cx| {
            ws.add_files(target, &[path], window, cx)
        })
    }

    fn add_link_dialog(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.fields_dialog(
            "Add a link".into(),
            vec![
                ("Link", "https://…", String::new()),
                (
                    "What is it?",
                    "a title, e.g. Figma: share dialog",
                    String::new(),
                ),
            ],
            move |ws, v, _window, cx| {
                let dir = ws
                    .project_dir(&target.env, &target.slug)
                    .ok_or("This project isn't in the repo any more.")?;
                pj::add_link(&dir, &v[0], &v[1])?;
                ws.resources_changed(&target, cx);
                Ok(())
            },
            window,
            cx,
        );
    }

    fn rename_file_dialog(
        &mut self,
        target: Target,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.project_dir(&target.env, &target.slug) else {
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
        target: Target,
        url: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.project_dir(&target.env, &target.slug) else {
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

    /// A dialog with a labelled box per field, each `(label, placeholder, initial)`; Save runs
    /// `save` with every value, and the dialog stays open showing what's wrong until it
    /// succeeds. (Two dialogs one after the other don't work: the first's Save closes the
    /// second.)
    fn fields_dialog(
        &mut self,
        title: String,
        fields: Vec<(&'static str, &'static str, String)>,
        save: impl Fn(&mut Self, Vec<String>, &mut Window, &mut Context<Self>) -> Result<(), String>
        + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A milestone's points (with their explanations) and a ticket's description run over
        // several lines; the rest are one line.
        let inputs: Vec<(&'static str, FieldBox)> = fields
            .into_iter()
            .map(|(label, placeholder, initial)| {
                let input = if matches!(label, "Points" | "Description") {
                    FieldBox::Lines(cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .auto_grow(3, 14)
                            .placeholder(placeholder)
                            .default_value(initial)
                    }))
                } else {
                    FieldBox::Line(cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .default_value(initial)
                    }))
                };
                (label, input)
            })
            .collect();
        let problem = cx.new(|_| None::<String>);
        let workspace = cx.entity();
        let save = std::rc::Rc::new(save);
        let focused = std::cell::Cell::new(false);
        window.open_dialog(cx, move |dialog, window, cx| {
            if !focused.replace(true)
                && let Some((_, first)) = inputs.first()
            {
                first.focus(window, cx);
            }
            let shown = problem.read(cx).clone();
            let (all, p, ws, save) = (
                inputs.clone(),
                problem.clone(),
                workspace.clone(),
                save.clone(),
            );
            let mut body = v_flex().gap_2();
            for (label, input) in &inputs {
                body = body
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(*label),
                    )
                    .child(match input {
                        FieldBox::Line(i) => Input::new(i).into_any_element(),
                        FieldBox::Lines(i) => Textarea::new(i).into_any_element(),
                    });
            }
            dialog
                .title(title.clone())
                .w(px(640.))
                .child(body.when_some(shown, |d, e| {
                    d.child(div().text_sm().text_color(color(theme::RED)).child(e))
                }))
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
                                let values: Vec<String> =
                                    all.iter().map(|(_, i)| i.value(cx)).collect();
                                match ws.update(cx, |ws, cx| save(ws, values, window, cx)) {
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

    /// Resources as a list: each file and link opens on click and has Rename (spec §8.2).
    fn resource_list(&self, target: &Target, p: &Project, cx: &mut Context<Self>) -> gpui::Div {
        let Some(dir) = self.project_dir(&target.env, &target.slug) else {
            return div();
        };
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
            let path = dir.join("resources").join(f);
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

    fn resources_block(&self, target: &Target, p: &Project, cx: &mut Context<Self>) -> gpui::Div {
        let mut block = v_flex()
            .gap_1()
            .child(section_title("Resources"))
            .child(self.resource_list(target, p, cx));
        if p.files.is_empty() && p.links.is_empty() {
            block = block.child(muted("None yet."));
        }
        let (ft, lt) = (target.clone(), target.clone());
        block.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("project-add-file")
                        .label("Add file…")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.pick_file(ft.clone(), window, cx)
                        })),
                )
                .child(
                    Button::new("project-add-link")
                        .label("Add link…")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.add_link_dialog(lt.clone(), window, cx)
                        })),
                ),
        )
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
        };
        // The planning session sits beside its project's pages.
        let planner = match &page {
            Page::Project { env, slug } | Page::Milestone { env, slug, .. }
                if self.projects.as_ref().is_some_and(|t| t.show_claude) =>
            {
                self.planner(env, slug)
                    .and_then(|id| self.panes.get(&id).cloned())
            }
            _ => None,
        };
        let main = div()
            .id("projects-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_5()
            .child(body);
        let mut row = h_flex().flex_1().min_h_0().child(main);
        if let Some(view) = planner {
            row = row.child(
                v_flex()
                    .w(px(620.))
                    .flex_none()
                    .h_full()
                    .border_l_1()
                    .border_color(color(theme::BORDER))
                    .child(
                        h_flex()
                            .px_3()
                            .py_2()
                            .gap_2()
                            .bg(color(theme::SURFACE))
                            .border_b_1()
                            .border_color(color(theme::BORDER))
                            .child(div().text_sm().child("Planning session"))
                            .child(div().flex_1())
                            .child(
                                Button::new("hide-claude")
                                    .label("Hide")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|ws, _, _, cx| {
                                        ws.tab_flag(cx, |t| t.show_claude = false)
                                    })),
                            ),
                    )
                    .child(div().flex_1().min_h_0().child(view)),
            );
        }
        v_flex()
            .size_full()
            .bg(color(theme::PANEL))
            .text_color(color(theme::TEXT))
            .child(self.projects_header(&page, cx))
            .child(row)
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
                    .child(crumb("crumb-m", title, None, cx));
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
                    .child(crumb("crumb-s", "Settings".into(), None, cx));
            }
        }
        let waiting = self.projects_data.waiting_count();
        let planner_hidden = match page {
            Page::Project { env, slug } | Page::Milestone { env, slug, .. } => {
                self.planner(env, slug).is_some()
                    && self.projects.as_ref().is_some_and(|t| !t.show_claude)
            }
            _ => false,
        };
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
            .when(planner_hidden, |d| {
                d.child(
                    Button::new("show-claude")
                        .label("Show planning session")
                        .ghost()
                        .small()
                        .on_click(
                            cx.listener(|ws, _, _, cx| ws.tab_flag(cx, |t| t.show_claude = true)),
                        ),
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

    fn projects_home(&self, show_all: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut out = v_flex().gap_6();
        if self.projects_data.envs.is_empty() {
            return out
                .child(
                    v_flex().gap_2().child(div().text_lg().child("No projects repo yet")).child(
                        div().text_color(color(theme::TEXT_SECONDARY)).child(
                            "Set one in Settings → an environment → Projects (owner/name on GitHub, e.g. \
                             iov42/iov-projects). Its projects show here.",
                        ),
                    ),
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
            let shown: Vec<&Project> = e
                .projects
                .iter()
                .filter(|p| show_all || p.file.status == ProjectStatus::Active)
                .collect();
            if e.fetched && shown.is_empty() {
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
                .on_click(
                    cx.listener(|ws, _, _, cx| ws.tab_flag(cx, |t| t.show_all = !t.show_all)),
                ),
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
        let (rm, rt) = pj::to_review(p);
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
        let live: Vec<&Milestone> = p
            .milestones
            .iter()
            .filter(|m| m.review != Review::Rejected)
            .collect();
        if live.is_empty() {
            card = card.child(muted(if p.spec.trim().is_empty() {
                "Not started: attach resources, then Start project."
            } else {
                "No milestones yet."
            }));
        }
        for m in live {
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
                            .when(m.review != Review::Approved, |d| {
                                d.child(
                                    div()
                                        .text_color(color(theme::AMBER))
                                        .child(m.review.label()),
                                )
                            })
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
            .flex_wrap()
            .text_xs()
            .text_color(color(theme::TEXT_SECONDARY));
        foot = foot.child(format!(
            "{open} open question{}",
            if open == 1 { "" } else { "s" }
        ));
        if rm + rt > 0 {
            foot = foot.child(
                div()
                    .text_color(color(theme::AMBER))
                    .child(format!("{} to review", rm + rt)),
            );
        }
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
        let lead = p.file.lead.eq_ignore_ascii_case(&login);
        let section = self
            .projects
            .as_ref()
            .map(|t| t.section)
            .unwrap_or_default();
        let (env_s, slug_s) = (env.to_string(), slug.to_string());

        let title = h_flex()
            .gap_3()
            .items_baseline()
            .flex_wrap()
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
                let folder = e.checkout.join(&p.slug);
                Button::new("project-files")
                    .label("Show files")
                    .small()
                    .ghost()
                    .tooltip("The project's folder in hydra's checkout of the repo")
                    .on_click(move |_, _, cx| cx.reveal_path(&folder))
            })
            .child(match pj::github_url(&e.repo, &p.slug) {
                Some(url) if e.sent.contains(&p.slug) => Button::new("project-github")
                    .label("Open on GitHub")
                    .small()
                    .ghost()
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
                Some(_) => pill("not on GitHub yet", theme::TEXT_SECONDARY, theme::SURFACE)
                    .into_any_element(),
                None => div().into_any_element(),
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

        // The tabs, each with a count of what needs you (spec §8.2).
        let (rm, rt) = pj::to_review(p);
        let review_count = if lead { rm + rt } else { 0 };
        let mine = p
            .decisions
            .iter()
            .filter(|d| d.is_open() && d.meta.assigned_to.eq_ignore_ascii_case(&login))
            .count();
        let open_q = p.decisions.iter().filter(|d| d.is_open()).count();
        let approvals_waiting = e
            .prs
            .iter()
            .filter(|pr| pr.slug == p.slug && e.pr_waits_on(&login, pr))
            .count();
        let mut tabs = h_flex()
            .gap_1()
            .border_b_1()
            .border_color(color(theme::BORDER));
        for (s, label, count, quiet) in [
            (Section::Overview, "Overview", approvals_waiting, 0),
            (Section::Milestones, "Milestones", review_count, 0),
            (Section::Questions, "Questions", mine, open_q),
        ] {
            let selected = s == section;
            tabs = tabs.child(
                h_flex()
                    .id(SharedString::from(format!("project-tab-{label}")))
                    .gap_2()
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(color(if selected { theme::TEAL } else { theme::PANEL }))
                    .text_color(color(if selected {
                        theme::TEXT
                    } else {
                        theme::TEXT_SECONDARY
                    }))
                    .hover(|d| d.text_color(color(theme::TEXT)))
                    .child(label)
                    .when(count > 0, |d| d.child(count_badge(count, true)))
                    .when(count == 0 && quiet > 0, |d| {
                        d.child(count_badge(quiet, false))
                    })
                    .on_click(cx.listener(move |ws, _, _, cx| ws.tab_flag(cx, |t| t.section = s))),
            );
        }

        let body = match section {
            Section::Overview => self.project_overview(env, p, lead, cx),
            Section::Milestones => self.project_milestones(env, p, lead, cx),
            Section::Questions => self.project_questions(env, p, &login, cx),
        };
        v_flex()
            .gap_5()
            .max_w(px(1400.))
            .child(title)
            .children(self.send_bar(env, p, cx))
            .child(tabs)
            .child(body)
            .into_any_element()
    }

    fn project_overview(
        &self,
        env: &str,
        p: &Project,
        lead: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (env_s, slug_s) = (env.to_string(), p.slug.clone());
        let target = Target {
            env: env_s.clone(),
            slug: slug_s.clone(),
        };
        let planning = self.planner(env, &p.slug).is_some();
        let started = !p.spec.trim().is_empty() || !p.milestones.is_empty();
        let mut page = v_flex().gap_6();
        let approvals = self.approvals(env, Some(&p.slug), cx);
        if !approvals.is_empty() {
            page = page.child(
                v_flex()
                    .gap_3()
                    .child(section_title(&format!(
                        "Changes waiting for you · {}",
                        approvals.len()
                    )))
                    .children(approvals),
            );
        }

        // Not started: resources first, then Start project (spec §14).
        if !started {
            let (env_b, slug_b) = (env_s.clone(), slug_s.clone());
            page = page.child(
                v_flex()
                    .gap_3()
                    .p_4()
                    .rounded_lg()
                    .bg(color(theme::TEAL_TINT))
                    .border_1()
                    .border_color(color(theme::TEAL_BORDER))
                    .id("start-project")
                    .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(color(theme::TEAL)))
                    .on_drop({
                        let t = target.clone();
                        cx.listener(move |ws, paths: &ExternalPaths, window, cx| {
                            ws.add_files(t.clone(), paths.paths(), window, cx)
                        })
                    })
                    .child(div().text_lg().child("Start the project"))
                    .child(div().text_sm().text_color(color(theme::TEXT_SECONDARY)).child(
                        "1. Attach what Claude should read: files (drop them here) and links. \
                         2. Start project: a Claude session reads them, writes the spec and \
                         proposes milestones for you to review here.",
                    ))
                    .child(self.resources_block(&target, p, cx))
                    .when(lead, |d| {
                        d.child(
                            h_flex().child(
                                Button::new("start-project-button")
                                    .label(if planning {
                                        "Start project (session running)"
                                    } else {
                                        "Start project"
                                    })
                                    .primary()
                                    .on_click(cx.listener(move |ws, _, window, cx| {
                                        ws.start_project(env_b.clone(), slug_b.clone(), window, cx)
                                    })),
                            ),
                        )
                    })
                    .when(!lead, |d| {
                        d.child(muted(&format!("{} (the lead) starts it.", p.file.lead)))
                    }),
            );
        }

        // What needs you, at a glance.
        let (rm, rt) = pj::to_review(p);
        let counts = milestone_counts(p, |t| t.meta.status);
        let live: Vec<&Milestone> = p
            .milestones
            .iter()
            .filter(|m| m.review != Review::Rejected)
            .collect();
        let ms_done = live
            .iter()
            .filter(|m| {
                let c = counts.get(&m.id).copied().unwrap_or_default();
                m.status == MilestoneStatus::Done || (c.total() > 0 && c.done == c.total())
            })
            .count();
        let tickets: Vec<&Ticket> = p
            .tickets
            .iter()
            .filter(|t| t.meta.review != Review::Rejected && t.meta.status != TicketStatus::Dropped)
            .collect();
        let t_done = tickets
            .iter()
            .filter(|t| t.meta.status == TicketStatus::Done)
            .count();
        let open_q = p.decisions.iter().filter(|d| d.is_open()).count();
        let tile = |label: &str,
                    value: String,
                    note: String,
                    to: Section,
                    warn: bool,
                    cx: &mut Context<Self>| {
            v_flex()
                .id(SharedString::from(format!("tile-{label}")))
                .flex_1()
                .min_w(px(180.))
                .gap_1()
                .p_4()
                .rounded_lg()
                .bg(color(theme::RAISED))
                .border_1()
                .border_color(color(if warn { theme::AMBER } else { theme::BORDER }))
                .cursor_pointer()
                .hover(|d| d.border_color(color(theme::TEAL)))
                .on_click(cx.listener(move |ws, _, _, cx| ws.tab_flag(cx, |t| t.section = to)))
                .child(section_title(label))
                .child(div().text_2xl().child(value))
                .child(
                    div()
                        .text_xs()
                        .text_color(color(if warn {
                            theme::AMBER
                        } else {
                            theme::TEXT_SECONDARY
                        }))
                        .child(note),
                )
        };
        if started {
            page = page.child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(tile(
                        "Milestones",
                        format!("{ms_done}/{}", live.len()),
                        if lead && rm > 0 {
                            format!("{rm} to review")
                        } else {
                            "done".into()
                        },
                        Section::Milestones,
                        lead && rm > 0,
                        cx,
                    ))
                    .child(tile(
                        "Tickets",
                        format!("{t_done}/{}", tickets.len()),
                        if lead && rt > 0 {
                            format!("{rt} to review")
                        } else {
                            "done".into()
                        },
                        Section::Milestones,
                        lead && rt > 0,
                        cx,
                    ))
                    .child(tile(
                        "Questions",
                        open_q.to_string(),
                        "open".into(),
                        Section::Questions,
                        false,
                        cx,
                    )),
            );
            // Milestones, one line each.
            let today = today();
            let mut lines = v_flex().gap_2().child(section_title("Milestones"));
            for m in &live {
                let c = counts.get(&m.id).copied().unwrap_or_default();
                let page_to = Page::Milestone {
                    env: env_s.clone(),
                    slug: slug_s.clone(),
                    id: m.id.clone(),
                };
                lines = lines.child(
                    h_flex()
                        .id(SharedString::from(format!("overview-m-{}", m.id)))
                        .gap_3()
                        .cursor_pointer()
                        .hover(|d| d.text_color(color(theme::TEAL)))
                        .on_click(cx.listener(move |ws, _, _, cx| ws.go(page_to.clone(), cx)))
                        .child(
                            div()
                                .w(px(36.))
                                .flex_none()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(color(theme::TEXT_MUTED))
                                .child(m.id.clone()),
                        )
                        .child(
                            div()
                                .w(px(280.))
                                .flex_none()
                                .truncate()
                                .child(m.title.clone()),
                        )
                        .child(div().flex_1().child(status_bar(&c, 6.)))
                        .child(
                            div()
                                .w(px(60.))
                                .flex_none()
                                .text_xs()
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(format!("{}/{}", c.done, c.total())),
                        )
                        .child(div().w(px(110.)).flex_none().child(
                            if m.review != Review::Approved {
                                review_pill(m.review).into_any_element()
                            } else if is_late(m, &c, &today) {
                                pill("late", theme::RED, theme::RED_TINT).into_any_element()
                            } else {
                                div()
                                    .text_xs()
                                    .text_color(color(theme::TEXT_MUTED))
                                    .child(m.target.map(|t| t.to_string()).unwrap_or_default())
                                    .into_any_element()
                            },
                        )),
                );
            }
            if live.is_empty() {
                lines = lines.child(muted(if planning {
                    "Claude is working on them…"
                } else {
                    "No milestones yet."
                }));
            }
            page = page.child(lines);
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
                        h_flex()
                            .gap_3()
                            .child(div().flex_none().child(person.clone()))
                            .child(div().flex_1())
                            .when_some(latest, |d, w| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(color(theme::TEXT_MUTED))
                                        .child(format!("last write-up {}", w.date)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
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
                    )
                    .when_some(latest, |d, w| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(color(theme::TEXT_MUTED))
                                .child(first_line(&w.body)),
                        )
                    }),
            );
        }
        page = page.child(progress);
        if started {
            page = page.child(self.resources_block(&target, p, cx));
        }
        if started && lead && !planning {
            let (env_b, slug_b) = (env_s.clone(), slug_s.clone());
            page = page.child(
                h_flex().child(
                    Button::new("resume-planning")
                        .label("Open planning session")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            let prompt = "Pick up planning this project: call plan_show, sum up \
                                          what's approved, what's waiting for review and what had \
                                          changes asked, and act on any notes. Milestones are short \
                                          and purpose-driven, in plain words anyone can read: \
                                          rewrite with plan_revise any still to review that are \
                                          technical, have a title over 60 characters, or aren't \
                                          1 to 4 short points (give points)."
                                .to_string();
                            ws.tell_planner(&env_b, &slug_b, prompt, window, cx)
                        })),
                ),
            );
        }
        page.child(v_flex().gap_2().child(section_title("Spec")).child(
            if p.spec.trim().is_empty() {
                muted(if planning {
                    "Claude is writing it…"
                } else {
                    "No spec yet."
                })
                .into_any_element()
            } else {
                gpui_base::TextView::markdown(
                    SharedString::from(format!("spec-{}", p.slug)),
                    p.spec.clone(),
                )
                .selectable(true)
                .into_any_element()
            },
        ))
        .into_any_element()
    }

    fn project_milestones(
        &self,
        env: &str,
        p: &Project,
        lead: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let show_rejected = self.projects.as_ref().is_some_and(|t| t.show_rejected);
        let planning = self.planner(env, &p.slug).is_some();
        let today = today();
        let counts = milestone_counts(p, |t| t.meta.status);
        let (rm, rt) = pj::to_review(p);
        let mut page = v_flex().gap_3();
        if lead && rm + rt > 0 {
            let mut where_tickets: BTreeMap<&str, usize> = BTreeMap::new();
            for t in p.tickets.iter().filter(|t| t.meta.review.waiting()) {
                *where_tickets.entry(t.meta.milestone.as_str()).or_default() += 1;
            }
            let mut bar = h_flex()
                .gap_3()
                .flex_wrap()
                .px_4()
                .py_3()
                .rounded_lg()
                .bg(color(theme::AMBER_TINT))
                .border_1()
                .border_color(color(theme::AMBER))
                .child(div().text_color(color(theme::AMBER)).child(format!(
                    "To review: {rm} milestone{} · {rt} ticket{}",
                    if rm == 1 { "" } else { "s" },
                    if rt == 1 { "" } else { "s" }
                )));
            for (mid, n) in where_tickets {
                let page_to = Page::Milestone {
                    env: env.to_string(),
                    slug: p.slug.clone(),
                    id: mid.to_string(),
                };
                bar = bar.child(
                    Button::new(SharedString::from(format!("review-in-{mid}")))
                        .label(format!(
                            "{mid}: {n} ticket{}",
                            if n == 1 { "" } else { "s" }
                        ))
                        .xsmall()
                        .on_click(cx.listener(move |ws, _, _, cx| ws.go(page_to.clone(), cx))),
                );
            }
            page = page.child(bar);
        }
        let rejected = p
            .milestones
            .iter()
            .filter(|m| m.review == Review::Rejected)
            .count();
        if rejected > 0 {
            page = page.child(
                div()
                    .id("toggle-rejected")
                    .text_xs()
                    .text_color(color(theme::TEXT_MUTED))
                    .cursor_pointer()
                    .child(if show_rejected {
                        format!("hide {rejected} rejected")
                    } else {
                        format!("show {rejected} rejected")
                    })
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.tab_flag(cx, |t| t.show_rejected = !t.show_rejected)
                    })),
            );
        }
        for m in p
            .milestones
            .iter()
            .filter(|m| show_rejected || m.review != Review::Rejected)
        {
            let c = counts.get(&m.id).copied().unwrap_or_default();
            page = page.child(self.milestone_row(env, p, m, &c, &today, lead, cx));
        }
        if p.milestones.is_empty() {
            page = page.child(muted(if planning {
                "Claude is working on them…"
            } else {
                "No milestones yet: Start project on the Overview has Claude propose them."
            }));
        }
        page.into_any_element()
    }

    /// One milestone across the page: what it is on the left, its progress in the middle,
    /// what you can do on the right.
    #[allow(clippy::too_many_arguments)] // The row's inputs, all read-only.
    fn milestone_row(
        &self,
        env: &str,
        p: &Project,
        m: &Milestone,
        c: &Counts,
        today: &str,
        lead: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (env_s, slug_s) = (env.to_string(), p.slug.clone());
        let late = is_late(m, c, today);
        let done = m.status == MilestoneStatus::Done || (c.total() > 0 && c.done == c.total());
        let waiting = m.review.waiting();
        let page_to = Page::Milestone {
            env: env_s.clone(),
            slug: slug_s.clone(),
            id: m.id.clone(),
        };
        let edge = if waiting {
            theme::AMBER
        } else if m.review == Review::Rejected {
            theme::BORDER_SUBTLE
        } else if late {
            theme::RED_BORDER
        } else if m.status == MilestoneStatus::Active {
            theme::TEAL_BORDER
        } else {
            theme::BORDER
        };
        let ticket_count = p
            .tickets
            .iter()
            .filter(|t| t.meta.milestone == m.id && t.meta.review != Review::Rejected)
            .count();
        let tickets_waiting = p
            .tickets
            .iter()
            .filter(|t| t.meta.milestone == m.id && t.meta.review.waiting())
            .count();

        // A stack, not columns: text in a growing column of a row is wrapped at an early,
        // narrow width; text straight in a stack gets the row's whole width. The actions sit
        // in a footer of their own, where they wrap rather than run off the page.
        let mut actions = h_flex().flex_wrap().gap_2();
        let mut has_actions = false;
        if lead && waiting {
            let mc = m.clone();
            let (e, s) = (env_s.clone(), slug_s.clone());
            has_actions = true;
            actions = actions.child(self.review_actions(
                env,
                &p.slug,
                Item::Milestone(m.id.clone()),
                &m.title,
                move |ws, window, cx| {
                    ws.edit_milestone_dialog(e.clone(), s.clone(), mc.clone(), window, cx)
                },
                cx,
            ));
        }
        if m.review == Review::Approved {
            has_actions = true;
            let page_to = Page::Milestone {
                env: env_s.clone(),
                slug: slug_s.clone(),
                id: m.id.clone(),
            };
            actions = actions.child(
                Button::new(SharedString::from(format!("board-{}", m.id)))
                    .label(format!("Tickets ({ticket_count})"))
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |ws, _, _, cx| ws.go(page_to.clone(), cx))),
            );
            if lead {
                let mc = m.clone();
                let (e, s) = (env_s.clone(), slug_s.clone());
                actions = actions.child(
                    Button::new(SharedString::from(format!("tickets-{}", m.id)))
                        .label(if ticket_count > 0 {
                            "More tickets"
                        } else {
                            "Create tickets"
                        })
                        .small()
                        .when(ticket_count == 0, |b| b.primary())
                        .when(ticket_count > 0, |b| b.ghost())
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.create_tickets(e.clone(), s.clone(), &mc, window, cx)
                        })),
                );
            }
        }
        let header = h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(color(theme::TEXT_MUTED))
                    .child(m.id.clone()),
            )
            .child(
                div()
                    .id(SharedString::from(format!("open-milestone-{}", m.id)))
                    .flex_none()
                    .text_lg()
                    .cursor_pointer()
                    .hover(|s| s.text_color(color(theme::TEAL)))
                    .child(m.title.clone())
                    .on_click(cx.listener(move |ws, _, _, cx| ws.go(page_to.clone(), cx))),
            )
            .when(m.review != Review::Approved, |d| {
                d.child(review_pill(m.review))
            })
            .when(done, |d| {
                d.child(pill("done", theme::GREEN, theme::GREEN_TINT))
            })
            .when(late && m.review == Review::Approved, |d| {
                d.child(pill("late", theme::RED, theme::RED_TINT))
            });
        let meta = {
            let mut parts = vec![
                m.target
                    .map(|t| format!("target {t}"))
                    .unwrap_or_else(|| "no target date".into()),
            ];
            if tickets_waiting > 0 {
                parts.push(format!(
                    "{tickets_waiting} ticket{} to review",
                    if tickets_waiting == 1 { "" } else { "s" }
                ));
            } else if m.review == Review::Approved && ticket_count == 0 {
                parts.push("no tickets yet".into());
            }
            parts.join(" · ")
        };
        v_flex()
            .id(SharedString::from(format!("milestone-{}", m.id)))
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .rounded_lg()
            .bg(color(if m.status == MilestoneStatus::Active {
                theme::RAISED
            } else {
                theme::SURFACE
            }))
            .border_1()
            .border_color(color(edge))
            .when(m.review == Review::Rejected, |d| d.opacity(0.6))
            .child(header)
            .child(
                div()
                    .text_xs()
                    .text_color(color(if tickets_waiting > 0 {
                        theme::AMBER
                    } else {
                        theme::TEXT_SECONDARY
                    }))
                    .child(meta),
            )
            .when(!m.description.is_empty(), |d| {
                d.child(self.milestone_points(env, &p.slug, m, lead, cx))
            })
            .when(
                !m.review_note.is_empty() && m.review != Review::Approved,
                |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(color(theme::AMBER))
                            .child(format!("“{}”", m.review_note)),
                    )
                },
            )
            .when(m.review == Review::Approved && ticket_count > 0, |d| {
                d.child(
                    h_flex()
                        .gap_4()
                        .items_center()
                        .child(div().flex_1().child(status_bar(c, 8.)))
                        .child(div().flex_none().child(counts_line(c))),
                )
            })
            .when(has_actions, |d| {
                d.child(
                    div()
                        .mt_1()
                        .pt_3()
                        .border_t_1()
                        .border_color(color(theme::BORDER_SUBTLE))
                        .child(actions),
                )
            })
            .into_any_element()
    }

    fn project_questions(
        &self,
        env: &str,
        p: &Project,
        login: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = pj::now_secs();
        let mut open: Vec<&Decision> = p.decisions.iter().filter(|d| d.is_open()).collect();
        // Yours first, then the oldest.
        open.sort_by_key(|d| {
            (
                !d.meta.assigned_to.eq_ignore_ascii_case(login),
                d.meta.asked_at.map(|t| t.to_string()).unwrap_or_default(),
            )
        });
        let mut answered: Vec<&Decision> = p.decisions.iter().filter(|d| !d.is_open()).collect();
        answered.sort_by_key(|d| {
            std::cmp::Reverse(d.meta.decided_at.map(|t| t.to_string()).unwrap_or_default())
        });
        let (env_s, slug_s) = (env.to_string(), p.slug.clone());
        let mut page = v_flex().gap_3().child(
            h_flex()
                .gap_3()
                .child(section_title(&format!("Outstanding · {}", open.len())))
                .child(div().flex_1())
                .child(
                    Button::new("project-ask")
                        .label("Ask a question")
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.ask_dialog(env_s.clone(), slug_s.clone(), window, cx)
                        })),
                ),
        );
        if open.is_empty() {
            page = page.child(muted("Nothing waiting."));
        }
        for d in open {
            page = page.child(self.question_row(env, p, d, login, now, cx));
        }
        page = page
            .child(div().h(px(12.)))
            .child(section_title(&format!("Decided · {}", answered.len())));
        if answered.is_empty() {
            page = page.child(muted("No decisions yet."));
        }
        for d in answered {
            page = page.child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .rounded_md()
                    .bg(color(theme::SURFACE))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(color(theme::TEXT_MUTED))
                                    .child(d.id.clone()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(color(theme::TEXT_MUTED))
                                    .child(format!(
                                        "decided by {}{}",
                                        d.meta.decided_by,
                                        d.meta
                                            .decided_at
                                            .map(|t| format!(" · {} ago", pj::age(&t, now)))
                                            .unwrap_or_default()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(color(theme::TEXT_SECONDARY))
                            .child(d.meta.question.clone()),
                    )
                    .child(div().child(d.meta.answer.clone())),
            );
        }
        page.into_any_element()
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
        let meta = format!(
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
            match d.meta.asked_by.as_str() {
                "" => String::new(),
                ASKED_BY_CLAUDE if d.meta.ticket.is_empty() => {
                    " · asked by the planning session".to_string()
                }
                ASKED_BY_CLAUDE => format!(" · asked by {}'s work session", d.meta.ticket),
                who => format!(" · asked by {who}"),
            },
        );
        // A stack, not columns, so the question and its context wrap across the row.
        let mut row = v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
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
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(color(theme::TEXT_MUTED))
                            .child(if d.meta.ticket.is_empty() {
                                d.id.clone()
                            } else {
                                format!("{} · {}", d.id, d.meta.ticket)
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(color(if mine {
                                theme::AMBER
                            } else {
                                theme::TEXT_SECONDARY
                            }))
                            .child(meta),
                    ),
            )
            .child(div().child(d.meta.question.clone()))
            .when(!d.body.trim().is_empty(), |r| {
                r.child(
                    div()
                        .text_sm()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(d.body.trim().to_string()),
                )
            });
        if mine {
            // The suggested answers as one even list, lettered, each across the row (their text
            // wraps inside), then a way to answer something else.
            let mut choices = v_flex().gap_1();
            for (i, option) in d.meta.options.iter().enumerate() {
                let (env, slug, q, option) =
                    (env.to_string(), p.slug.clone(), d.clone(), option.clone());
                choices = choices.child(
                    // A stack with the letter pinned at its left, so the answer's text wraps
                    // across the row (text in a growing column of a row wraps narrow).
                    v_flex()
                        .id(SharedString::from(format!("opt-{}-{i}", d.id)))
                        .relative()
                        .w_full()
                        .pl(px(44.))
                        .pr_3()
                        .py_2()
                        .rounded_md()
                        .bg(color(theme::RAISED))
                        .border_1()
                        .border_color(color(theme::BORDER))
                        .cursor_pointer()
                        .hover(|s| {
                            s.border_color(color(theme::TEAL))
                                .bg(color(theme::TEAL_TINT))
                        })
                        .child(
                            div()
                                .absolute()
                                .left(px(12.))
                                .top(px(7.))
                                .size(px(22.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .bg(color(theme::SURFACE))
                                .border_1()
                                .border_color(color(theme::BORDER_STRONG))
                                .text_color(color(theme::TEXT_SECONDARY))
                                .child(option_letter(i)),
                        )
                        .child(div().text_sm().py(px(2.)).child(option.clone()))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            let (q, option) = (q.clone(), option.clone());
                            let _ = ws.answer_question(&env, &slug, &q, &option, window, cx);
                        })),
                );
            }
            let (env, slug, q) = (env.to_string(), p.slug.clone(), d.clone());
            let other = Button::new(SharedString::from(format!("answer-{}", d.id)))
                .label(if d.meta.options.is_empty() {
                    "Answer…"
                } else {
                    "Something else…"
                })
                .small()
                .when(d.meta.options.is_empty(), |b| b.primary())
                .when(!d.meta.options.is_empty(), |b| b.outline())
                .on_click(cx.listener(move |ws, _, window, cx| {
                    let (env, slug, q) = (env.clone(), slug.clone(), q.clone());
                    ws.text_dialog(
                        format!("Answer {}: {}", q.id, q.meta.question),
                        String::new(),
                        "your answer; it's recorded with your name and the time",
                        move |ws, text, window, cx| {
                            let q = q.clone();
                            ws.answer_question(&env, &slug, &q, &text, window, cx)
                        },
                        window,
                        cx,
                    )
                }));
            row = row.child(choices).child(h_flex().child(other));
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
        self.fields_dialog(
            "Ask a question".into(),
            vec![
                ("Question", "a decision that needs making", String::new()),
                (
                    "Who decides?",
                    "their GitHub login; the lead when empty",
                    lead,
                ),
            ],
            move |ws, v, window, cx| {
                ws.edit_project(
                    &env,
                    &slug,
                    |dir, p, login| pj::ask(dir, p, &v[0], &v[1], login).map(|_| ()),
                    window,
                    cx,
                )?;
                // A question goes out at once, so it reaches its person (spec §4).
                ws.send_project(&env, &slug, true, window, cx);
                Ok(())
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
        let login = e.login.clone().unwrap_or_default();
        let lead = p.file.lead.eq_ignore_ascii_case(&login);
        let member = p.file.is_on(&login);
        let show_rejected = self.projects.as_ref().is_some_and(|t| t.show_rejected);
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
        let (rm_t, rejected) = (
            p.tickets
                .iter()
                .filter(|t| t.meta.milestone == id && t.meta.review.waiting())
                .count(),
            p.tickets
                .iter()
                .filter(|t| t.meta.milestone == id && t.meta.review == Review::Rejected)
                .count(),
        );
        let mut head = v_flex().gap_2().child(
            h_flex()
                .gap_3()
                .items_baseline()
                .flex_wrap()
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
                .when(m.review != Review::Approved, |d| {
                    d.child(review_pill(m.review))
                })
                .when(late, |d| d.child(pill("late", theme::RED, theme::RED_TINT)))
                .child(div().flex_1())
                .when(lead && m.review == Review::Approved, |d| {
                    let (e2, s2, mc) = (env.to_string(), slug.to_string(), m.clone());
                    d.child(
                        Button::new("milestone-create-tickets")
                            .label("Create tickets")
                            .small()
                            .primary()
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.create_tickets(e2.clone(), s2.clone(), &mc, window, cx)
                            })),
                    )
                })
                .child(new_button("New ticket", TicketKind::Feature, cx))
                .child(new_button("New bug", TicketKind::Bug, cx)),
        );
        if !m.description.is_empty() {
            head = head.child(
                div()
                    .text_sm()
                    .text_color(color(theme::TEXT_SECONDARY))
                    .child(m.description.clone()),
            );
        }
        head = head
            .child(div().w(px(480.)).child(status_bar(&c, 10.)))
            .child(counts_line(&c));
        if rm_t > 0 {
            head = head.child(notice(
                &format!(
                    "{rm_t} ticket{} to review: they're in Todo, marked to review.",
                    if rm_t == 1 { "" } else { "s" }
                ),
                theme::AMBER,
                theme::AMBER_TINT,
            ));
        }
        if rejected > 0 {
            head = head.child(
                div()
                    .id("toggle-rejected-tickets")
                    .text_xs()
                    .text_color(color(theme::TEXT_MUTED))
                    .cursor_pointer()
                    .child(if show_rejected {
                        format!("hide {rejected} rejected")
                    } else {
                        format!("show {rejected} rejected")
                    })
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.tab_flag(cx, |t| t.show_rejected = !t.show_rejected)
                    })),
            );
        }
        let mut board = h_flex().gap_3().items_start();
        for status in TicketStatus::BOARD {
            let tickets: Vec<&Ticket> = p
                .tickets
                .iter()
                .filter(|t| t.meta.milestone == id && t.meta.status == status)
                .filter(|t| show_rejected || t.meta.review != Review::Rejected)
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
                column = column.child(self.ticket_card(env, e, p, t, lead, member, cx));
            }
            board = board.child(column);
        }
        v_flex().gap_5().child(head).child(board).into_any_element()
    }

    #[allow(clippy::too_many_arguments)] // The card's inputs, all read-only.
    fn ticket_card(
        &self,
        env: &str,
        e: &EnvProjects,
        p: &Project,
        t: &Ticket,
        lead: bool,
        member: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let unsent = pj::ticket_unsent(&e.pending, &p.slug, &t.id);
        let questions = p
            .decisions
            .iter()
            .filter(|d| d.is_open() && d.meta.ticket == t.id)
            .count();
        let waiting = t.meta.review.waiting();
        let ws = cx.entity();
        let (env_s, slug_s) = (env.to_string(), p.slug.clone());
        let mut card = v_flex()
            .id(SharedString::from(format!("ticket-{}", t.id)))
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(color(theme::RAISED))
            .border_1()
            .border_color(color(if waiting {
                theme::AMBER
            } else if unsent {
                theme::TEAL_BORDER
            } else {
                theme::BORDER_SUBTLE
            }))
            .when(t.meta.review == Review::Rejected, |d| d.opacity(0.6))
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
                    .when(t.meta.review != Review::Approved, |d| {
                        d.child(review_pill(t.meta.review))
                    })
                    .when(unsent && t.meta.review == Review::Approved, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(color(theme::TEAL))
                                .child("not sent yet"),
                        )
                    }),
            )
            .child(div().text_sm().child(t.meta.title.clone()))
            .when(waiting && !t.body.trim().is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(color(theme::TEXT_SECONDARY))
                        .child(first_line(&t.body)),
                )
            })
            .when(
                !t.meta.review_note.is_empty() && t.meta.review != Review::Approved,
                |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(color(theme::AMBER))
                            .child(format!("“{}”", t.meta.review_note)),
                    )
                },
            )
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
                    .when(!t.meta.branch.is_empty(), |d| {
                        d.child(
                            div()
                                .font_family("monospace")
                                .truncate()
                                .child(t.meta.branch.clone()),
                        )
                    })
                    .when(questions > 0, |d| {
                        d.child(div().text_color(color(theme::AMBER)).child(format!(
                            "{questions} open question{}",
                            if questions == 1 { "" } else { "s" }
                        )))
                    }),
            );
        for (i, url) in t.meta.prs.iter().enumerate() {
            let u = url.clone();
            card = card.child(
                div()
                    .id(SharedString::from(format!("pr-{}-{i}", t.id)))
                    .text_xs()
                    .text_color(color(theme::TEAL))
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .truncate()
                    .child(pr_label(url))
                    .on_click(move |_, _, cx| cx.open_url(&u)),
            );
        }
        if lead && waiting {
            let (e2, s2, tc) = (env_s.clone(), slug_s.clone(), t.clone());
            card = card.child(self.review_actions(
                env,
                &p.slug,
                Item::Ticket(t.id.clone()),
                &t.meta.title,
                move |ws, window, cx| {
                    ws.edit_ticket_dialog(e2.clone(), s2.clone(), tc.clone(), window, cx)
                },
                cx,
            ));
        }
        let live = self.worker(env, &p.slug, &t.id);
        if let Some(id) = live {
            card = card.child(
                h_flex().child(
                    Button::new(SharedString::from(format!("session-{}", t.id)))
                        .label("Open session")
                        .xsmall()
                        .on_click(
                            cx.listener(move |ws, _, window, cx| ws.show_terminal(id, window, cx)),
                        ),
                ),
            );
        } else if member
            && pj::can_start(p, t)
            && matches!(t.meta.status, TicketStatus::Todo | TicketStatus::InProgress)
        {
            let (e3, s3, id3) = (env_s.clone(), slug_s.clone(), t.id.clone());
            card = card.child(
                h_flex().child(
                    Button::new(SharedString::from(format!("work-{}", t.id)))
                        .label(if t.meta.status == TicketStatus::Todo {
                            "Start work"
                        } else {
                            "Start again"
                        })
                        .xsmall()
                        .primary()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.start_work_dialog(e3.clone(), s3.clone(), id3.clone(), window, cx)
                        })),
                ),
            );
        }
        let ticket = t.clone();
        card.context_menu(move |menu, _, _| {
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
                    (ws.clone(), env_s.clone(), slug_s.clone(), ticket.clone());
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
                (ws.clone(), env_s.clone(), slug_s.clone(), ticket.clone());
            menu.separator().item(
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
            for p in e
                .projects
                .iter()
                .filter(|p| p.file.lead.eq_ignore_ascii_case(login))
            {
                let (rm, rt) = pj::to_review(p);
                if rm + rt == 0 {
                    continue;
                }
                any = true;
                let page = Page::Project {
                    env: env.clone(),
                    slug: p.slug.clone(),
                };
                out = out.child(
                    h_flex()
                        .id(SharedString::from(format!("review-{env}-{}", p.slug)))
                        .gap_3()
                        .p_3()
                        .rounded_md()
                        .bg(color(theme::AMBER_TINT))
                        .border_1()
                        .border_color(color(theme::AMBER))
                        .cursor_pointer()
                        .child(div().child(format!(
                            "Review {}: {rm} milestone{} · {rt} ticket{}",
                            p.file.name,
                            if rm == 1 { "" } else { "s" },
                            if rt == 1 { "" } else { "s" }
                        )))
                        .on_click(cx.listener(move |ws, _, _, cx| ws.go(page.clone(), cx))),
                );
            }
            for (slug, approvals) in e
                .projects
                .iter()
                .map(|p| (p, self.approvals(env, Some(&p.slug), cx)))
                .filter(|(_, a)| !a.is_empty())
            {
                any = true;
                out = out
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(theme::TEXT_MUTED))
                            .child(format!("{} · {} · changes to approve", slug.file.name, env)),
                    )
                    .children(approvals);
            }
            for pr in e
                .prs
                .iter()
                .filter(|pr| pr.author.eq_ignore_ascii_case(login) && pr.sent_back.is_some())
            {
                let Some(p) = e.projects.iter().find(|p| p.slug == pr.slug) else {
                    continue;
                };
                any = true;
                let page = Page::Project {
                    env: env.clone(),
                    slug: p.slug.clone(),
                };
                let note = pr.sent_back.clone().unwrap_or_default();
                out = out.child(
                    v_flex()
                        .id(SharedString::from(format!("sent-back-{}", pr.url)))
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .bg(color(theme::AMBER_TINT))
                        .border_1()
                        .border_color(color(theme::AMBER))
                        .cursor_pointer()
                        .child(div().child(format!(
                            "{} sent your change to {} back",
                            p.file.lead, p.file.name
                        )))
                        .when(!note.is_empty(), |d| {
                            d.child(div().text_sm().child(format!("“{note}”")))
                        })
                        .on_click(cx.listener(move |ws, _, _, cx| ws.go(page.clone(), cx))),
                );
            }
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
                "Nothing waits on you. Reviews and questions for you show here.",
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
        let root = self.hydra.paths.root().to_path_buf();
        let clones = f.repos.iter().map(|r| match pj::clone_of(&root, r) {
            Some(path) => format!("{r} → {}", path.display()),
            None => format!("{r} → asked on first Start work"),
        });
        v_flex()
            .gap_1()
            .max_w(px(900.))
            .child(div().text_2xl().pb_3().child(format!("{} settings", f.name)))
            .when(!lead, |d| d.child(notice(&format!("Only the lead, {}, changes these.", f.lead), theme::TEXT_SECONDARY, theme::SURFACE)))
            .child(field("Name", f.name.clone(), Field::Name, cx))
            .child(field("Key", f.key.clone(), Field::Key, cx))
            .child(field("Lead", f.lead.clone(), Field::Lead, cx))
            .child(field("Members", f.members.join(", "), Field::Members, cx))
            .child(field("Code repos", f.repos.join(", "), Field::Repos, cx))
            .child(field("Sync times", f.sync.times.join(", "), Field::Sync, cx))
            .child(
                h_flex()
                    .gap_4()
                    .py_2()
                    .child(div().w(px(160.)).flex_none().text_color(color(theme::TEXT_SECONDARY)).child("Status"))
                    .child(status_buttons),
            )
            .child(div().pt_3().child(section_title("Local clones (this Mac)")))
            .children(clones.map(|c| div().text_sm().font_family("monospace").child(c)))
            .child(div().pt_3().child(muted(
                "Code repos are where work happens: Start work makes a worktree of one. Sync times are \
                 when each member's hydraui sends the day's progress to the repo (once or twice a day); \
                 sending comes in the next slice.",
            )))
            .into_any_element()
    }
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
            Field::Repos => "owner/name, separated by commas: where work happens",
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
        Field::Repos => {
            let repos = pj::list_of(text);
            if let Some(bad) = repos.iter().find(|r| !pj::is_github(r)) {
                return Err(format!(
                    "{bad} isn't owner/name, e.g. iov42/version2-frontend."
                ));
            }
            f.repos = repos;
        }
        Field::Sync => f.sync.times = pj::sync_times(text)?,
    }
    repo::write_project_file(dir, &f).map_err(|e| e.to_string())
}

fn hydra_exe() -> PathBuf {
    server::get()
        .and_then(|sv| server::lock(sv.registry()).hydra_exe.clone())
        .unwrap_or_else(|| PathBuf::from("hydra"))
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
    if pj::is_github(repo) {
        let r = ps::EnvRunner {
            hydra: exe,
            env,
            cwd: state,
        };
        match ps::open_prs(&r, repo) {
            Ok(prs) => e.prs = prs,
            Err(why) if e.problem.is_none() => {
                e.problem = Some(format!("Can't read {repo}'s pull requests: {why}"))
            }
            Err(_) => {}
        }
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
    e.status = ps::status_text(&ps::Local, &e.checkout).unwrap_or_default();
    e.pending = pj::parse_status(&e.status);
    e.sent = pj::sent_projects(&e.checkout);
    e.sent_for_lead = e
        .projects
        .iter()
        .map(|p| p.slug.clone())
        .filter(|slug| {
            pj::project_unsent(&e.pending, slug) > 0 && ps::is_sent(&ps::Local, &e.checkout, slug)
        })
        .collect();
    e.fetched = true;
}

/// The planning session's first prompt (spec §14).
fn start_prompt(p: &Project) -> String {
    format!(
        "You're planning the project \"{name}\" (ticket key {key}, lead {lead}) in hydraui's \
         Projects tab. The lead sees what you write in hydraui beside you and reviews each \
         proposal there: approve, edit, ask you for changes, or reject.\n\n\
         1. Call plan_show, then read every attached resource (plan_read_resource; Read for \
         PDFs and images).\n\
         2. Ask about anything unclear (goals, who it's for, constraints, what's out of \
         scope) with plan_add_question, never in this terminal: each question shows in \
         hydraui as a decision to make, with your options as buttons, and the answer comes \
         back to you here. Carry on with what doesn't depend on it.\n\
         3. Write the spec with plan_set_spec: short numbered sections in plain words, with \
         the decisions and their reasons.\n\
         4. Propose the milestones in order with plan_propose_milestone. Keep them few, short \
         and purpose-driven, readable by anyone on the team: each an outcome people care \
         about, in plain words with no technical terms (e.g. \"Partners can see shared \
         groups\", not \"Implement group ACL service\"), with 1 to 4 short points on what \
         it's for and how we'll know it's done (not a paragraph), and a target date where you \
         know one. Technical detail belongs in the tickets.\n\
         Don't propose tickets yet: the lead asks for them per milestone once it's approved. \
         Use only the plan tools to change the project; never write its files yourself. When \
         you've proposed the milestones, tell the lead they're ready to review.",
        name = p.file.name,
        key = p.file.key,
        lead = p.file.lead,
    )
}

/// A work session's prompt (spec §14).
fn work_prompt(p: &Project, t: &Ticket, branch: &str, repo: &str, skill: Option<&str>) -> String {
    let skill = skill
        .map(|s| format!("Use the /{s} skill for the work.\n"))
        .unwrap_or_default();
    format!(
        "You're doing ticket {id} \"{title}\" of the project \"{name}\", in a fresh worktree of \
         {repo} on branch {branch}. Call ticket_show first: the ticket, its milestone and \
         where the project's spec is.\n{skill}\
         Do the work here, commit it on this branch, push it and open a pull request (gh pr \
         create) with {id} in its title. Finish by calling ticket_report with the pull request \
         URL(s) and a short summary. If you need a decision, ask it with ticket_ask, never in \
         this terminal: it shows in hydraui and the answer comes back to you here.",
        id = t.id,
        title = t.meta.title,
        name = p.file.name,
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
    // A fixed height with the text centred, never wrapped or squeezed by its row.
    h_flex()
        .flex_none()
        .items_center()
        .h(px(20.))
        .px(px(8.))
        .rounded_full()
        .text_xs()
        .line_height(px(20.))
        .whitespace_nowrap()
        .bg(color(bg))
        .text_color(color(fg))
        .child(text.to_string())
}

/// A box in [`Workspace::fields_dialog`].
#[derive(Clone)]
enum FieldBox {
    Line(Entity<InputState>),
    Lines(Entity<TextareaState>),
}

impl FieldBox {
    fn focus(&self, window: &mut Window, cx: &mut gpui::App) {
        match self {
            FieldBox::Line(i) => i.update(cx, |s, cx| s.focus(window, cx)),
            FieldBox::Lines(i) => i.update(cx, |s, cx| s.focus(window, cx)),
        }
    }

    fn value(&self, cx: &gpui::App) -> String {
        match self {
            FieldBox::Line(i) => i.read(cx).value().trim().to_string(),
            FieldBox::Lines(i) => i.read(cx).value().trim().to_string(),
        }
    }
}

/// `A`, `B`, … for a question's suggested answers.
fn option_letter(i: usize) -> String {
    char::from_u32(u32::from(b'A') + (i % 26) as u32)
        .map(String::from)
        .unwrap_or_default()
}

/// A tab's count: amber when it's waiting on you, grey when it's only there.
fn count_badge(n: usize, yours: bool) -> gpui::Div {
    let (fg, bg) = if yours {
        (theme::ON_TEAL, theme::AMBER)
    } else {
        (theme::TEXT_SECONDARY, theme::RAISED)
    };
    div()
        .min_w(px(18.))
        .h(px(18.))
        .px_1()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .bg(color(bg))
        .text_color(color(fg))
        .child(n.to_string())
}

fn review_pill(r: Review) -> gpui::Div {
    match r {
        Review::Proposed => pill("to review", theme::AMBER, theme::AMBER_TINT),
        Review::Changes => pill("with Claude", theme::TEAL, theme::TEAL_TINT),
        Review::Rejected => pill("rejected", theme::RED, theme::RED_TINT),
        Review::Approved => pill("approved", theme::GREEN, theme::GREEN_TINT),
    }
}

/// `iov42/fe#12` for a GitHub pull request URL, else the URL.
fn pr_label(url: &str) -> String {
    url.strip_prefix("https://github.com/")
        .and_then(|rest| {
            let mut parts = rest.split('/');
            let (owner, repo, pull, n) =
                (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            (pull == "pull").then(|| format!("{owner}/{repo}#{n}"))
        })
        .unwrap_or_else(|| url.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_toasts_come_together_at_most_once_a_minute() {
        let start = Instant::now();
        let mut t = QuestionToasts::default();
        assert_eq!(t.take(start), None, "nothing new");
        t.pending.push("Claude asks (PG, D-1): Postgres?".into());
        assert!(t.take(start).unwrap().starts_with("Claude asks (PG, D-1)"));
        t.pending
            .extend(["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(t.take(start + Duration::from_secs(30)), None, "too soon");
        let later = t.take(start + Duration::from_secs(61)).unwrap();
        assert!(later.starts_with("3 new questions"), "{later}");
        assert!(t.pending.is_empty());
    }

    #[test]
    fn options_are_lettered() {
        assert_eq!(option_letter(0), "A");
        assert_eq!(option_letter(2), "C");
    }

    #[test]
    fn pull_request_links_read_shortly() {
        assert_eq!(
            pr_label("https://github.com/iov42/fe/pull/12"),
            "iov42/fe#12"
        );
        assert_eq!(
            pr_label("https://gitlab.com/x/-/merge_requests/3"),
            "https://gitlab.com/x/-/merge_requests/3"
        );
    }

    #[test]
    fn prompts_name_the_ticket_and_the_report() {
        let p: Project = Project {
            slug: "pg".into(),
            file: toml::from_str("name = \"Product Groups\"\nkey = \"PG\"\nlead = \"me\"").unwrap(),
            spec: String::new(),
            milestones: vec![],
            tickets: vec![],
            decisions: vec![],
            meetings: vec![],
            files: vec![],
            links: vec![],
            progress: vec![],
        };
        let t = Ticket {
            id: "PG-3".into(),
            meta: hydraui_projects::TicketMeta {
                title: "Share dialog".into(),
                ..Default::default()
            },
            body: String::new(),
        };
        let w = work_prompt(&p, &t, "pg-3-share-dialog", "iov42/fe", Some("angular"));
        assert!(
            w.contains("/angular skill")
                && w.contains("ticket_report")
                && w.contains("pg-3-share-dialog")
        );
        assert!(!work_prompt(&p, &t, "b", "r", None).contains("skill"));
        assert!(start_prompt(&p).contains("plan_propose_milestone"));
    }

    #[test]
    fn code_repos_must_be_github_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.toml"),
            "name = \"PG\"\nkey = \"PG\"\nlead = \"me\"\n",
        )
        .unwrap();
        let p = repo::load(&dir).unwrap();
        assert!(set_field(&dir, &p, Field::Repos, "/Users/me/fe").is_err());
        set_field(&dir, &p, Field::Repos, "iov42/fe, iov42/be").unwrap();
        assert_eq!(
            repo::load(&dir).unwrap().file.repos,
            ["iov42/fe", "iov42/be"]
        );
    }
}
