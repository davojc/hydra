//! Stage terminals (orchestrator spec §3.2): the gate let a stage start, so hydraui opens its
//! terminal beside the user, in the run's worktree or the orchestrator's folder, and links it.
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{Context, Window};

use super::Workspace;
use crate::hydra::{PaneRequest, command_line};
use crate::mcp::server::{self, McpEvent};
use crate::runs::{Change, Role, StartStage};
use crate::worktrees;

/// A stage terminal's name, e.g. `s-02 · Develop`.
pub fn stage_title(label: &str, title: &str) -> String {
    format!("{label} · {title}")
}

/// Writes `changes` to their runs' logs and tells the app (its views, and the orchestrator).
fn tell(changes: Vec<Change>) {
    let Some(server) = server::get() else {
        return;
    };
    if changes.is_empty() {
        return;
    }
    {
        let reg = server::lock(server.registry());
        if let Some(root) = &reg.hydra_root {
            crate::run_log::record(root, &reg.runs, &changes);
        }
    }
    server.broadcast(McpEvent::RunChanged(changes));
}

impl Workspace {
    /// The gate let stage `s` start: find its orchestrator's environment and folder, make the
    /// run's worktree when the stage wants one and the run has none (off the UI thread), then
    /// open the stage's terminal.
    pub(super) fn start_stage(
        &mut self,
        s: StartStage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let orchestrator = self.panes.values().find_map(|v| {
            let v = v.read(cx);
            (v.mcp_token() == Some(s.orchestrator.as_str()))
                .then(|| (v.request.env.clone(), v.cwd().clone()))
        });
        let Some((env, cwd)) = orchestrator else {
            return stage_failed(&s, "its orchestrator's terminal is gone".into());
        };
        if !s.worktree {
            return self.open_stage(s, &env, cwd, window, cx);
        }
        let made = server::get().and_then(|sv| {
            server::lock(sv.registry())
                .runs
                .run(&s.run)?
                .worktree
                .clone()
        });
        if let Some(path) = made {
            return self.open_stage(s, &env, path, window, cx);
        }
        let Some(repo) = crate::tasks_project::git_root(&cwd) else {
            return stage_failed(
                &s,
                format!(
                    "{} isn't in a git repository, so it has no worktree",
                    cwd.display()
                ),
            );
        };
        let run = s.run.clone();
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_spawn(async move { worktrees::create(&repo, &run) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| match made {
                Ok(wt) => {
                    if let Some(sv) = server::get() {
                        server::lock(sv.registry())
                            .runs
                            .set_worktree(&s.run, wt.path.clone());
                    }
                    ws.open_stage(s, &env, wt.path, window, cx);
                }
                Err(e) => stage_failed(&s, format!("couldn't make its worktree: {e}")),
            });
        })
        .detach();
    }

    /// Opens stage `s`'s terminal in `cwd` running Claude with its prompt, names it and links its
    /// token to the run.
    fn open_stage(
        &mut self,
        s: StartStage,
        env: &str,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shell = self.shell();
        let mut request = PaneRequest::new(env, cwd, shell);
        request.command = Some(command_line(
            shell,
            &["claude".to_string(), s.prompt.clone()],
        ));
        let Some(id) = self.open_terminal_beside(env, request, window, cx) else {
            return stage_failed(&s, format!("environment {env} isn't open"));
        };
        let Some(view) = self.panes.get(&id).cloned() else {
            return stage_failed(&s, "its terminal didn't open".into());
        };
        let name = stage_title(&s.label, &s.title);
        view.update(cx, |v, cx| v.set_name(Some(name.clone()), cx));
        let token = view.read(cx).mcp_token().map(str::to_string);
        let taker = view.read(cx).taker_name().unwrap_or(name);
        match (token, server::get()) {
            (Some(token), Some(sv)) => {
                server::lock(sv.registry())
                    .runs
                    .link(&s.run, s.stage, &token, &taker);
                crate::action_log::write(format!("run {}: {taker} opened", s.run));
                cx.notify();
            }
            _ => stage_failed(&s, "its terminal has no MCP token".into()),
        }
    }

    /// The shell or Claude session behind `token` ended: when it was running a stage, the stage
    /// fails (§3.4). An orchestrator's run stops only when its terminal closes.
    pub(super) fn stage_session_gone(&mut self, token: &str) {
        let changes = server::get().map_or_else(Vec::new, |sv| {
            let mut reg = server::lock(sv.registry());
            match reg.runs.role(token) {
                Role::Stage(..) => reg.runs.session_gone(token),
                _ => Vec::new(),
            }
        });
        tell(changes);
    }
}

/// Stage `s` couldn't open: it fails, as a session that ended without reporting does.
fn stage_failed(s: &StartStage, why: String) {
    crate::action_log::write(format!(
        "run {}: stage {} didn't open: {why}",
        s.run, s.title
    ));
    let changes = server::get().map_or_else(Vec::new, |sv| {
        server::lock(sv.registry())
            .runs
            .start_failed(&s.run, s.stage, &why)
    });
    tell(changes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_terminal_title_names_label_and_stage() {
        assert_eq!(stage_title("s-02", "Develop"), "s-02 · Develop");
    }
}
