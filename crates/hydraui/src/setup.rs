//! What an environment still needs before it is ready to use (spec §48): the tools most
//! people need, each not added, added but not signed in, or ready; and what the sidebar says
//! about pipelines when Claude isn't ready. No UI.
use hydra_core::provider::{IdentityReport, Status};

use crate::config::SetupPlugin;

/// The tools the New environment dialog offers ticked and the sidebar's checklist goes
/// through: Claude first, since pipelines need it.
pub const ESSENTIALS: &[&str] = &["claude", "github", "git"];

/// How far along one tool is in an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Not in env.toml.
    NotAdded,
    /// In env.toml, but its check says no one is signed in (or its set-up is broken).
    NotSignedIn,
    /// In env.toml and its check passed, or hasn't run yet.
    Ready,
}

/// `tool`'s state: `tools` are the tools in env.toml; `reports` the last checks, `None` before
/// the first. A tool not checked yet counts as ready, so nothing flashes up as missing while
/// the check runs.
pub fn tool_state(tool: &str, tools: &[String], reports: Option<&[IdentityReport]>) -> ToolState {
    if !tools.iter().any(|t| t == tool) {
        return ToolState::NotAdded;
    }
    let missing = reports
        .unwrap_or_default()
        .iter()
        .any(|r| r.provider == tool && r.status == Status::Missing);
    if missing {
        ToolState::NotSignedIn
    } else {
        ToolState::Ready
    }
}

/// What a checklist line is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// An essential tool, by id.
    Tool(&'static str),
    /// One of config.toml's `setup_plugins`, by its place in that list, and how it stands.
    Plugin(usize, PluginState),
}

/// How one of the team's plugins stands for an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    /// Turned on, and its files are where Claude installed them.
    On,
    /// Not turned on (never installed, or turned off).
    Off,
    /// Turned on, but Claude's record of the install is gone or points at a missing folder:
    /// Claude would skip it.
    Missing,
}

/// A plugin's state from whether settings turn it on and whether its files are there.
pub fn plugin_state(enabled: bool, installed: bool) -> PluginState {
    match (enabled, installed) {
        (false, _) => PluginState::Off,
        (true, true) => PluginState::On,
        (true, false) => PluginState::Missing,
    }
}

/// What the Pipelines tab and Settings say about a plugin.
pub fn plugin_state_text(state: PluginState) -> &'static str {
    match state {
        PluginState::On => "on",
        PluginState::Off => "not installed",
        PluginState::Missing => "turned on, but its files are missing",
    }
}

/// Whether Claude's `plugins/installed_plugins.json` (its text) records `plugin` installed at
/// a folder `is_dir` finds.
pub fn plugin_installed(
    installed_plugins: Option<&str>,
    plugin: &str,
    is_dir: impl Fn(&std::path::Path) -> bool,
) -> bool {
    let Some(root) =
        installed_plugins.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
    else {
        return false;
    };
    root.get("plugins")
        .and_then(|p| p.get(plugin))
        .and_then(|e| e.as_array())
        .is_some_and(|installs| {
            installs.iter().any(|i| {
                i.get("installPath")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| is_dir(std::path::Path::new(p)))
            })
        })
}

/// One line of the checklist. A plugin is never `NotSignedIn`: it is installed or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub subject: Subject,
    pub label: String,
    pub state: ToolState,
}

/// The tool's name as the checklist shows it.
pub fn label(tool: &str) -> &'static str {
    match tool {
        "claude" => "Claude",
        "github" => "GitHub",
        "git" => "Git",
        _ => "",
    }
}

/// Each essential tool's state, then each of the team's plugins (`plugins`: label and state),
/// or nothing when they are all ready: the checklist goes once the environment is
/// set up.
pub fn checklist(
    tools: &[String],
    reports: Option<&[IdentityReport]>,
    plugins: &[(String, PluginState)],
) -> Vec<Item> {
    let tools = ESSENTIALS.iter().map(|t| Item {
        subject: Subject::Tool(t),
        label: label(t).to_string(),
        state: tool_state(t, tools, reports),
    });
    let plugins = plugins.iter().enumerate().map(|(i, (label, state))| Item {
        subject: Subject::Plugin(i, *state),
        label: label.clone(),
        state: if *state == PluginState::On {
            ToolState::Ready
        } else {
            ToolState::NotAdded
        },
    });
    let items: Vec<Item> = tools.chain(plugins).collect();
    if items.iter().all(|i| i.state == ToolState::Ready) {
        Vec::new()
    } else {
        items
    }
}

/// What a checklist line says beside its name.
pub fn state_text(item: &Item) -> &'static str {
    match (item.subject, item.state) {
        (Subject::Plugin(_, s), ToolState::NotAdded) => plugin_state_text(s),
        (_, ToolState::NotAdded) => "not added",
        (_, ToolState::NotSignedIn) => "not signed in",
        (_, ToolState::Ready) => "ready",
    }
}

/// The checklist line's button: what its next step is called; none once it's ready.
pub fn action_text(item: &Item) -> Option<&'static str> {
    match (item.subject, item.state) {
        (_, ToolState::Ready) => None,
        (Subject::Plugin(_, PluginState::Missing), _) => Some("Reinstall"),
        (Subject::Plugin(..), _) => Some("Install"),
        (_, ToolState::NotAdded) => Some("Add"),
        (_, ToolState::NotSignedIn) => Some("Sign in"),
    }
}

/// The sidebar's line on the Pipelines tab when no pipeline is running, by Claude's state.
pub fn pipeline_hint(claude: ToolState) -> &'static str {
    match claude {
        ToolState::NotAdded => {
            "Pipelines run in Claude, and Claude isn't added to this environment yet. Add it, \
             then sign in."
        }
        ToolState::NotSignedIn => {
            "Claude is added to this environment but not signed in here. Each environment has \
             its own Claude login, so sign in here even if you have elsewhere."
        }
        ToolState::Ready => {
            "No pipeline is running here. Start one to have an orchestrator work through \
             tickets or pull requests."
        }
    }
}

/// Whether Claude has `plugin` turned on for an environment: what the environment's own
/// settings.json (`over`) says about it, else what the shared one (`base`) says. Each is the
/// file's text, or `None` when there is none; text that isn't JSON says nothing.
pub fn plugin_enabled(base: Option<&str>, over: Option<&str>, plugin: &str) -> bool {
    let says = |text: Option<&str>| {
        let root: serde_json::Value = serde_json::from_str(text?).ok()?;
        root.get("enabledPlugins")?.get(plugin)?.as_bool()
    };
    says(over).or_else(|| says(base)).unwrap_or(false)
}

/// Whether Claude already knows the marketplace `plugin` (`name@marketplace`) comes from,
/// from the text of `plugins/known_marketplaces.json`.
pub fn marketplace_known(known_marketplaces: Option<&str>, plugin: &str) -> bool {
    let Some((_, market)) = plugin.split_once('@') else {
        return false;
    };
    known_marketplaces
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .is_some_and(|root| root.get(market).is_some())
}

/// The commands that install `p` for every environment: adding its marketplace first when
/// Claude doesn't know it and the config says where it is.
pub fn plugin_install_commands(p: &SetupPlugin, marketplace_known: bool) -> Vec<Vec<String>> {
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    if let (false, Some(source)) = (marketplace_known, &p.marketplace) {
        out.push(words(&["claude", "plugin", "marketplace", "add", source]));
    }
    out.push(words(&["claude", "plugin", "install", &p.plugin]));
    out
}

/// The checklist's name for `p`: its label, else its name without the marketplace.
pub fn plugin_label(p: &SetupPlugin) -> String {
    p.label
        .clone()
        .unwrap_or_else(|| p.plugin.split('@').next().unwrap_or(&p.plugin).to_string())
}

/// Whether `p` is the team's skill router (`skill-router@<marketplace>`).
pub fn is_router(p: &SetupPlugin) -> bool {
    p.plugin.split('@').next() == Some("skill-router")
}

/// What Pipeline settings says about the skill router, called `label`, in `env` (core spec §55),
/// and whether it's working there.
pub fn router_note(label: &str, state: PluginState, env: &str) -> (String, bool) {
    match state {
        PluginState::On => (
            format!(
                "{label} (skill-router) is on in {env}. Every stage loads the team skills its \
                 changes need before it edits (angular-developer before a .ts change, for \
                 example), whatever skill is set below. A stage's own skill adds how to do that \
                 stage; a stage set to Router only is guided by the router alone."
            ),
            true,
        ),
        PluginState::Off => (
            format!(
                "{label} (skill-router) isn't on in {env}, so each stage uses only the skill set \
                 below. Install it from the Pipelines tab."
            ),
            false,
        ),
        PluginState::Missing => (
            format!(
                "{label} (skill-router) is turned on in {env}, but its files are missing, so \
                 Claude skips it and each stage uses only the skill set below. Reinstall it from \
                 the Pipelines tab."
            ),
            false,
        ),
    }
}

/// A stage's skill as its button and picker show it: with no skill, "Router only" when the
/// router is on, else "No skill".
pub fn stage_skill_label(skill: Option<&str>, router_on: bool) -> String {
    match skill {
        Some(s) => s.to_string(),
        None if router_on => "Router only".into(),
        None => "No skill".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_router_is_known_by_its_plugin_name() {
        let p = |plugin: &str| SetupPlugin {
            plugin: plugin.into(),
            marketplace: None,
            label: None,
        };
        assert!(is_router(&p("skill-router@iov42")));
        assert!(!is_router(&p("skill-routers@iov42")));
        assert!(!is_router(&p("qa-engineer@iov42")));
    }

    #[test]
    fn the_router_note_says_whether_stages_get_team_skills() {
        let (on, ok) = router_note("Team skills", PluginState::On, "iov42");
        assert!(ok);
        assert!(
            on.starts_with("Team skills (skill-router) is on in iov42."),
            "{on}"
        );
        assert!(on.contains("whatever skill is set below"), "{on}");
        let (off, ok) = router_note("Team skills", PluginState::Off, "iov42");
        assert!(!ok);
        assert!(off.contains("only the skill set below"), "{off}");
        let (gone, ok) = router_note("Team skills", PluginState::Missing, "iov42");
        assert!(!ok);
        assert!(gone.contains("Reinstall"), "{gone}");
    }

    #[test]
    fn a_stage_without_a_skill_reads_as_router_only_when_the_router_is_on() {
        assert_eq!(stage_skill_label(None, true), "Router only");
        assert_eq!(stage_skill_label(None, false), "No skill");
        assert_eq!(stage_skill_label(Some("qa"), true), "qa");
    }

    fn r(provider: &str, status: Status) -> IdentityReport {
        IdentityReport {
            provider: provider.into(),
            status,
            detail: String::new(),
        }
    }

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_tool_not_in_env_toml_is_not_added_whatever_the_checks_say() {
        let reports = [r("claude", Status::Ok)];
        assert_eq!(
            tool_state("claude", &tools(&["github"]), Some(&reports)),
            ToolState::NotAdded
        );
    }

    #[test]
    fn an_added_tool_whose_check_finds_no_login_is_not_signed_in() {
        let reports = [r("claude", Status::Missing), r("github", Status::Ok)];
        let t = tools(&["claude", "github"]);
        assert_eq!(
            tool_state("claude", &t, Some(&reports)),
            ToolState::NotSignedIn
        );
        assert_eq!(tool_state("github", &t, Some(&reports)), ToolState::Ready);
    }

    #[test]
    fn an_added_tool_not_checked_yet_or_signed_in_some_other_way_is_ready() {
        let t = tools(&["claude", "git"]);
        assert_eq!(tool_state("claude", &t, None), ToolState::Ready);
        let reports = [r("claude", Status::Info), r("git", Status::Mismatch)];
        assert_eq!(tool_state("claude", &t, Some(&reports)), ToolState::Ready);
        assert_eq!(tool_state("git", &t, Some(&reports)), ToolState::Ready);
    }

    #[test]
    fn a_new_environment_lists_every_essential_tool_as_not_added_claude_first() {
        let got = checklist(&[], None, &[]);
        let names: Vec<_> = got.iter().map(|i| (i.label.as_str(), i.state)).collect();
        assert_eq!(
            names,
            vec![
                ("Claude", ToolState::NotAdded),
                ("GitHub", ToolState::NotAdded),
                ("Git", ToolState::NotAdded),
            ]
        );
    }

    #[test]
    fn the_checklist_keeps_ready_tools_listed_until_every_one_is_ready_then_goes() {
        let reports = [r("claude", Status::Ok), r("github", Status::Missing)];
        let half = checklist(&tools(&["claude", "github", "git"]), Some(&reports), &[]);
        assert_eq!(half.len(), 3);
        assert_eq!(half[0].state, ToolState::Ready);
        assert_eq!(half[1].state, ToolState::NotSignedIn);
        let done = [r("claude", Status::Ok), r("github", Status::Ok)];
        let all = tools(&["claude", "github", "git", "linear"]);
        assert!(checklist(&all, Some(&done), &[]).is_empty());
    }

    #[test]
    fn a_team_plugin_not_installed_keeps_the_checklist_up_after_the_tools_are_ready() {
        let done = [r("claude", Status::Ok), r("github", Status::Ok)];
        let all = tools(&["claude", "github", "git"]);
        let off = [("Team skills".to_string(), PluginState::Off)];
        let got = checklist(&all, Some(&done), &off);
        assert_eq!(got.len(), 4);
        let last = &got[3];
        assert_eq!(
            (
                last.subject,
                last.label.as_str(),
                state_text(last),
                action_text(last)
            ),
            (
                Subject::Plugin(0, PluginState::Off),
                "Team skills",
                "not installed",
                Some("Install")
            )
        );
        let missing = [("Team skills".to_string(), PluginState::Missing)];
        let got = checklist(&all, Some(&done), &missing);
        assert_eq!(action_text(&got[3]), Some("Reinstall"));
        assert!(state_text(&got[3]).contains("missing"));
        let on = [("Team skills".to_string(), PluginState::On)];
        assert!(checklist(&all, Some(&done), &on).is_empty());
    }

    #[test]
    fn a_tools_button_adds_it_then_signs_in_then_goes() {
        let item = |state| Item {
            subject: Subject::Tool("claude"),
            label: "Claude".into(),
            state,
        };
        assert_eq!(action_text(&item(ToolState::NotAdded)), Some("Add"));
        assert_eq!(action_text(&item(ToolState::NotSignedIn)), Some("Sign in"));
        assert_eq!(action_text(&item(ToolState::Ready)), None);
        assert_eq!(state_text(&item(ToolState::NotAdded)), "not added");
    }

    #[test]
    fn the_pipelines_hint_names_what_claude_is_missing() {
        assert!(pipeline_hint(ToolState::NotAdded).contains("isn't added"));
        assert!(pipeline_hint(ToolState::NotSignedIn).contains("not signed in"));
        assert!(pipeline_hint(ToolState::Ready).starts_with("No pipeline is running"));
    }

    #[test]
    fn every_essential_tool_has_a_label() {
        assert!(ESSENTIALS.iter().all(|t| !label(t).is_empty()));
    }

    fn router(marketplace: Option<&str>) -> SetupPlugin {
        SetupPlugin {
            plugin: "skill-router@iov42".into(),
            marketplace: marketplace.map(str::to_string),
            label: None,
        }
    }

    #[test]
    fn a_plugin_is_on_when_the_shared_settings_turn_it_on_and_the_environment_doesnt_turn_it_off() {
        let on = r#"{"enabledPlugins":{"skill-router@iov42":true}}"#;
        let off = r#"{"enabledPlugins":{"skill-router@iov42":false}}"#;
        let p = "skill-router@iov42";
        assert!(plugin_enabled(Some(on), None, p));
        assert!(plugin_enabled(Some(on), Some("{}"), p));
        assert!(!plugin_enabled(Some(on), Some(off), p));
        assert!(plugin_enabled(Some(off), Some(on), p));
        assert!(!plugin_enabled(None, None, p));
        assert!(!plugin_enabled(Some("not json"), None, p));
        assert!(!plugin_enabled(Some(on), None, "other@iov42"));
    }

    #[test]
    fn a_marketplace_is_known_when_known_marketplaces_names_it() {
        let known = r#"{"iov42":{"source":{"source":"github","repo":"iov42/iov42-marketplace"}}}"#;
        assert!(marketplace_known(Some(known), "skill-router@iov42"));
        assert!(!marketplace_known(Some(known), "x@other"));
        assert!(!marketplace_known(None, "skill-router@iov42"));
        assert!(!marketplace_known(Some(known), "no-marketplace"));
    }

    #[test]
    fn installing_adds_the_marketplace_only_when_claude_doesnt_know_it() {
        let add = vec![
            "claude",
            "plugin",
            "marketplace",
            "add",
            "iov42/iov42-marketplace",
        ];
        let install = vec!["claude", "plugin", "install", "skill-router@iov42"];
        let p = router(Some("iov42/iov42-marketplace"));
        assert_eq!(
            plugin_install_commands(&p, false),
            vec![add, install.clone()]
        );
        assert_eq!(plugin_install_commands(&p, true), vec![install.clone()]);
        assert_eq!(plugin_install_commands(&router(None), false), vec![install]);
    }

    #[test]
    fn a_plugin_is_labelled_by_its_label_else_its_name() {
        assert_eq!(plugin_label(&router(None)), "skill-router");
        let mut p = router(None);
        p.label = Some("Team skills".into());
        assert_eq!(plugin_label(&p), "Team skills");
    }

    #[test]
    fn a_plugin_is_on_only_when_turned_on_and_its_files_are_there() {
        assert_eq!(plugin_state(true, true), PluginState::On);
        assert_eq!(plugin_state(true, false), PluginState::Missing);
        assert_eq!(plugin_state(false, true), PluginState::Off);
        assert_eq!(plugin_state(false, false), PluginState::Off);
    }

    #[test]
    fn a_plugin_is_installed_when_claudes_record_points_at_a_folder_that_exists() {
        let rec = r#"{"version":2,"plugins":{"skill-router@iov42":[
            {"scope":"project","installPath":"/gone"},
            {"scope":"user","installPath":"/cache/skill-router/0.7.0"}]}}"#;
        let there = |p: &std::path::Path| p == std::path::Path::new("/cache/skill-router/0.7.0");
        assert!(plugin_installed(Some(rec), "skill-router@iov42", there));
        assert!(!plugin_installed(Some(rec), "skill-router@iov42", |_| {
            false
        }));
        assert!(!plugin_installed(Some(rec), "other@iov42", there));
        assert!(!plugin_installed(None, "skill-router@iov42", there));
        assert!(!plugin_installed(Some("[]"), "skill-router@iov42", there));
    }
}
