//! The launcher's entries (spec §7.4): environments, sign-in, secrets and config, each
//! with what Enter does. The palette filters them by substring.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Adds the environment's tab, or selects it when it is open already.
    OpenEnv(String),
    /// The environment's env.toml in the file pane.
    EditEnv(String),
    SignIn {
        env: String,
        tool: String,
    },
    /// Quick open (spec §7.10).
    OpenFile,
    /// Search in files (spec §7.10).
    SearchFiles,
    /// Open a layout file (spec §7.12).
    OpenLayout,
    /// Save the window to a layout file.
    SaveLayout,
    /// A new window showing the start page (spec §7.12).
    StartPage,
    /// Delete an environment, picked in the dialog, as `hydra env rm` does.
    DeleteEnv,
    /// Set an environment's home folder, picked in the dialogs.
    SetEnvHome,
    /// Create an environment, named in the dialog.
    NewEnv,
    /// Store a value for this `env/key`.
    SetSecret(String),
    /// Store a value for an `env/key` typed in the dialog.
    NewSecret,
    OpenConfigFolder,
    /// hydraui.log, what hydraui did and why (`action_log`), in the file pane.
    OpenLog,
    ReloadConfig,
    /// Ask GitHub for a newer hydra now (spec §7.11).
    CheckForUpdates,
    /// The selected environment's Tasks: all projects (task board spec §10).
    AllBoards,
    /// The list of every key hydraui answers to.
    Shortcuts,
    /// The Settings tab.
    Settings,
    /// Store the Jev API key, typed in the dialog.
    SetJevKey,
    /// Forget the stored Jev API key, after asking.
    RemoveJevKey,
    /// jev.toml, the questions auto-pickup asks Jev, in the file pane (written first if new).
    EditJevQuestions,
    /// A Claude terminal that orchestrates a pipeline (orchestrator spec §3.0): an environment
    /// with Claude and a pipeline, picked in the dialogs.
    StartOrchestrator,
    /// The pipelines folder, opened by the system (the example written first if it is empty).
    EditPipelines,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub action: Action,
}

pub fn entries(
    envs: &[String],
    sign_ins: &[(String, String)],
    secrets: &[String],
    jev_set: bool,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut add = |label: String, action: Action| out.push(Entry { label, action });
    for env in envs {
        add(format!("Environment: {env}"), Action::OpenEnv(env.clone()));
        add(
            format!("Edit environment: {env}"),
            Action::EditEnv(env.clone()),
        );
    }
    for (env, tool) in sign_ins {
        add(
            format!("Sign in: {tool} in {env}"),
            Action::SignIn {
                env: env.clone(),
                tool: tool.clone(),
            },
        );
    }
    for path in secrets {
        add(
            format!("Set secret: {path}"),
            Action::SetSecret(path.clone()),
        );
    }
    add("Open file…".into(), Action::OpenFile);
    add("Search in files…".into(), Action::SearchFiles);
    add("Open layout file…".into(), Action::OpenLayout);
    add("Save layout as…".into(), Action::SaveLayout);
    add("Start page".into(), Action::StartPage);
    add("Delete environment…".into(), Action::DeleteEnv);
    add("Set environment home…".into(), Action::SetEnvHome);
    add("New environment…".into(), Action::NewEnv);
    add("Set secret: other…".into(), Action::NewSecret);
    add("Open config folder".into(), Action::OpenConfigFolder);
    add("Open hydraui log".into(), Action::OpenLog);
    add("Reload config".into(), Action::ReloadConfig);
    add("Check for updates".into(), Action::CheckForUpdates);
    add("Tasks: all projects".into(), Action::AllBoards);
    add("Start orchestrator…".into(), Action::StartOrchestrator);
    add("Edit pipelines".into(), Action::EditPipelines);
    add("Keyboard shortcuts".into(), Action::Shortcuts);
    add("Settings".into(), Action::Settings);
    add("Set Jev API key…".into(), Action::SetJevKey);
    add("Edit Jev questions".into(), Action::EditJevQuestions);
    if jev_set {
        add("Remove Jev API key".into(), Action::RemoveJevKey);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_cover_environments_sign_in_secrets_and_config() {
        let got = entries(
            &["work".to_string()],
            &[("work".to_string(), "github".to_string())],
            &["work/linear".to_string()],
            false,
        );
        let labels: Vec<&str> = got.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Environment: work",
                "Edit environment: work",
                "Sign in: github in work",
                "Set secret: work/linear",
                "Open file…",
                "Search in files…",
                "Open layout file…",
                "Save layout as…",
                "Start page",
                "Delete environment…",
                "Set environment home…",
                "New environment…",
                "Set secret: other…",
                "Open config folder",
                "Open hydraui log",
                "Reload config",
                "Check for updates",
                "Tasks: all projects",
                "Start orchestrator…",
                "Edit pipelines",
                "Keyboard shortcuts",
                "Settings",
                "Set Jev API key…",
                "Edit Jev questions",
            ]
        );
        assert!(!labels.iter().any(|l| l.starts_with("Set:")), "no sets");
        assert!(!labels.iter().any(|l| l.contains("group")), "no groups");
        assert_eq!(got[0].action, Action::OpenEnv("work".into()));
        assert_eq!(got[1].action, Action::EditEnv("work".into()));
        assert_eq!(
            got[2].action,
            Action::SignIn {
                env: "work".into(),
                tool: "github".into()
            }
        );
        assert_eq!(got[3].action, Action::SetSecret("work/linear".into()));
        assert_eq!(got[4].action, Action::OpenFile);
        assert_eq!(got[5].action, Action::SearchFiles);
        assert_eq!(got[6].action, Action::OpenLayout);
        assert_eq!(got[7].action, Action::SaveLayout);
        assert_eq!(got[8].action, Action::StartPage);
        assert_eq!(got[9].action, Action::DeleteEnv);
        assert_eq!(got[10].action, Action::SetEnvHome);
        assert_eq!(got[11].action, Action::NewEnv);
        assert_eq!(got[12].action, Action::NewSecret);
        let action = |label: &str| got.iter().find(|e| e.label == label).map(|e| &e.action);
        assert_eq!(
            action("Start orchestrator…"),
            Some(&Action::StartOrchestrator)
        );
        assert_eq!(action("Edit pipelines"), Some(&Action::EditPipelines));
    }

    #[test]
    fn with_nothing_configured_only_the_fixed_entries_remain() {
        let got = entries(&[], &[], &[], false);
        let labels: Vec<&str> = got.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Open file…",
                "Search in files…",
                "Open layout file…",
                "Save layout as…",
                "Start page",
                "Delete environment…",
                "Set environment home…",
                "New environment…",
                "Set secret: other…",
                "Open config folder",
                "Open hydraui log",
                "Reload config",
                "Check for updates",
                "Tasks: all projects",
                "Start orchestrator…",
                "Edit pipelines",
                "Keyboard shortcuts",
                "Settings",
                "Set Jev API key…",
                "Edit Jev questions"
            ]
        );
        assert_eq!(got[10].action, Action::OpenLog);
        assert_eq!(got[12].action, Action::CheckForUpdates);
        assert_eq!(got[13].action, Action::AllBoards);
        assert_eq!(got[14].action, Action::StartOrchestrator);
        assert_eq!(got[15].action, Action::EditPipelines);
        assert_eq!(got[16].action, Action::Shortcuts);
        assert_eq!(got[0].action, Action::OpenFile);
        assert_eq!(got[1].action, Action::SearchFiles);
        assert_eq!(got[4].action, Action::StartPage);
        assert_eq!(got[5].action, Action::DeleteEnv);
        assert_eq!(got[6].action, Action::SetEnvHome);
        assert_eq!(got[7].action, Action::NewEnv);
    }

    #[test]
    fn jev_entries_follow_whether_a_key_is_set() {
        let labels = |set| {
            entries(&[], &[], &[], set)
                .into_iter()
                .map(|e| e.label)
                .collect::<Vec<_>>()
        };
        assert!(labels(false).contains(&"Set Jev API key…".to_string()));
        assert!(!labels(false).contains(&"Remove Jev API key".to_string()));
        assert!(labels(true).contains(&"Remove Jev API key".to_string()));
    }
}
