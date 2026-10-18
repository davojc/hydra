//! The launcher's entries (spec §7.4): sets, environments, sign-in, secrets and config, each
//! with what Enter does. The palette filters them by substring.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetState {
    Loaded,
    Unloaded,
    /// Why the set file doesn't validate.
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Opens the set, or focuses it when it is already loaded.
    LoadSet(String),
    UnloadSet(String),
    /// A Loose tab in the environment.
    OpenEnv(String),
    SignIn {
        env: String,
        tool: String,
    },
    /// Quick open (spec §7.10).
    OpenFile,
    /// Search in files (spec §7.10).
    SearchFiles,
    /// Add a layout file's groups and tabs to the window (spec §7.9).
    OpenLayout,
    /// Save the window to a layout file.
    SaveLayout,
    /// Open a group file as a group.
    OpenGroupFile,
    /// Save every Loose tab into one group file.
    SaveLooseAsGroup,
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
    /// Says why the entry can't be used (an invalid set).
    Problem(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub action: Action,
}

pub fn entries(
    sets: &[(String, SetState)],
    envs: &[String],
    sign_ins: &[(String, String)],
    secrets: &[String],
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut add = |label: String, action: Action| out.push(Entry { label, action });
    for (name, state) in sets {
        match state {
            SetState::Loaded => {
                add(
                    format!("Set: {name} (loaded)"),
                    Action::LoadSet(name.clone()),
                );
                add(
                    format!("Unload set: {name}"),
                    Action::UnloadSet(name.clone()),
                );
            }
            SetState::Unloaded => add(format!("Set: {name}"), Action::LoadSet(name.clone())),
            SetState::Invalid(e) => {
                add(format!("Set: {name} (invalid)"), Action::Problem(e.clone()))
            }
        }
    }
    for env in envs {
        add(format!("New tab: {env}"), Action::OpenEnv(env.clone()));
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
    add("Open group file…".into(), Action::OpenGroupFile);
    add("Save Loose tabs as group…".into(), Action::SaveLooseAsGroup);
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
    fn entries_cover_sets_environments_sign_in_secrets_and_config() {
        let sets = [
            ("client-a".to_string(), SetState::Loaded),
            ("personal".to_string(), SetState::Unloaded),
            (
                "broken".to_string(),
                SetState::Invalid("line 3: unknown field".into()),
            ),
        ];
        let got = entries(
            &sets,
            &["work".to_string()],
            &[("work".to_string(), "github".to_string())],
            &["work/linear".to_string()],
        );
        let labels: Vec<&str> = got.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Set: client-a (loaded)",
                "Unload set: client-a",
                "Set: personal",
                "Set: broken (invalid)",
                "New tab: work",
                "Sign in: github in work",
                "Set secret: work/linear",
                "Open file…",
                "Search in files…",
                "Open layout file…",
                "Save layout as…",
                "Open group file…",
                "Save Loose tabs as group…",
                "Delete environment…",
                "Set environment home…",
                "New environment…",
                "Set secret: other…",
                "Open config folder",
                "Reload config",
            ]
        );
        assert_eq!(got[0].action, Action::LoadSet("client-a".into()));
        assert_eq!(got[1].action, Action::UnloadSet("client-a".into()));
        assert_eq!(
            got[3].action,
            Action::Problem("line 3: unknown field".into())
        );
        assert_eq!(got[4].action, Action::OpenEnv("work".into()));
        assert_eq!(
            got[5].action,
            Action::SignIn {
                env: "work".into(),
                tool: "github".into()
            }
        );
        assert_eq!(got[6].action, Action::SetSecret("work/linear".into()));
        assert_eq!(got[7].action, Action::OpenFile);
        assert_eq!(got[8].action, Action::SearchFiles);
        assert_eq!(got[9].action, Action::OpenLayout);
        assert_eq!(got[10].action, Action::SaveLayout);
        assert_eq!(got[11].action, Action::OpenGroupFile);
        assert_eq!(got[12].action, Action::SaveLooseAsGroup);
        assert_eq!(got[13].action, Action::DeleteEnv);
        assert_eq!(got[14].action, Action::SetEnvHome);
        assert_eq!(got[15].action, Action::NewEnv);
        assert_eq!(got[16].action, Action::NewSecret);
    }

    #[test]
    fn with_nothing_configured_only_the_fixed_entries_remain() {
        let got = entries(&[], &[], &[], &[]);
        let labels: Vec<&str> = got.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Open file…",
                "Search in files…",
                "Open layout file…",
                "Save layout as…",
                "Open group file…",
                "Save Loose tabs as group…",
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
        assert_eq!(got[6].action, Action::DeleteEnv);
        assert_eq!(got[7].action, Action::SetEnvHome);
        assert_eq!(got[8].action, Action::NewEnv);
    }
}
