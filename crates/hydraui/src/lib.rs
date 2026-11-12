//! hydraui: a desktop terminal for hydra environments
//! (spec: docs/superpowers/specs/2026-10-17-hydraui-design.md).
pub mod accounts;
pub mod action_log;
pub mod activity;
pub mod agent_access;
pub mod app_icon;
pub mod asking;
pub mod assets;
pub mod branch_rule;
pub mod claim;
pub mod config;
pub mod envs;
pub mod file_doc;
pub mod file_ops;
pub mod file_pane;
pub mod file_tree;
pub mod file_tree_view;
pub mod frame;
pub mod git_status;
pub mod git_watch;
pub mod gmail;
pub mod grammars;
pub mod hydra;
pub mod inbox;
pub mod input;
pub mod jev;
pub mod jev_stage;
pub mod keys;
pub mod launcher;
pub mod linear;
pub mod linear_project;
pub mod linear_watch;
pub mod link;
pub mod mail_claim;
pub mod mail_watch;
pub mod mcp;
pub mod open_files;
pub mod open_pipelines;
pub mod openapi;
pub mod openapi_view;
pub mod orchestrator_theme;
pub mod palette;
pub mod pane_view;
pub mod pdf;
pub mod pdf_view;
pub mod pickup;
pub mod pipeline;
pub mod pipeline_card;
pub mod pipeline_icon;
pub mod pipeline_share;
pub mod pipeline_view;
pub mod project_check;
pub mod project_run;
pub mod project_send;
pub mod projects;
pub mod quick_open;
pub mod rating;
pub mod readiness;
pub mod run_feed;
pub mod run_history;
pub mod run_log;
pub mod run_park;
pub mod run_resume;
pub mod runs;
pub mod save_dialog;
pub mod schedule;
pub mod search;
pub mod settings;
pub mod settings_words;
pub mod setup;
pub mod skills_list;
pub mod slack;
pub mod slack_connect;
pub mod slack_watch;
pub mod start_page;
pub mod swarms;
pub mod task_board;
pub mod task_board_view;
pub mod tasks;
pub mod tasks_project;
pub mod term_paths;
pub mod updater;
pub mod version;
pub mod waiting;
pub mod window_layout;
pub mod workspace;
pub mod worktrees;

/// The environment strip's height, under hydraui's own title bar.
pub const ENV_STRIP_HEIGHT: f32 = 34.;

/// Where toasts start: below the title bar and the environment strip, so they never cover
/// the strip's Projects and Settings buttons.
pub fn toast_top() -> gpui::Pixels {
    gpui_component::TITLE_BAR_HEIGHT + gpui::px(ENV_STRIP_HEIGHT + 12.)
}

/// Puts toasts below the strip; again after each theme change, which may reset them.
pub fn place_toasts(cx: &mut gpui::App) {
    gpui_component::Theme::global_mut(cx)
        .notification
        .margins
        .top = toast_top();
}

#[cfg(test)]
mod toast_tests {
    #[test]
    fn toasts_start_below_the_title_bar_and_the_environment_strip() {
        let covered = gpui_component::TITLE_BAR_HEIGHT + gpui::px(super::ENV_STRIP_HEIGHT);
        assert!(super::toast_top() > covered);
    }
}
