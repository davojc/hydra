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
    ReloadConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub action: Action,
}

pub fn entries(envs: &[String], sign_ins: &[(String, String)], secrets: &[String]) -> Vec<Entry> {
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
    add("Reload config".into(), Action::ReloadConfig);
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
                "Reload config",
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
    }

    #[test]
    fn with_nothing_configured_only_the_fixed_entries_remain() {
        let got = entries(&[], &[], &[]);
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
                "Reload config"
            ]
        );
        assert_eq!(got[0].action, Action::OpenFile);
        assert_eq!(got[1].action, Action::SearchFiles);
        assert_eq!(got[4].action, Action::StartPage);
        assert_eq!(got[5].action, Action::DeleteEnv);
        assert_eq!(got[6].action, Action::SetEnvHome);
        assert_eq!(got[7].action, Action::NewEnv);
    }
}
