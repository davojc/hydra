//! Set file → `SetSpec`: inheritance (pane → tab → set → folder binding), folder and shell
//! checks. Collects every problem before failing, so one save shows them all.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hydra_core::bindings;
use hydra_core::name::EnvName;
use hydra_core::paths::{HydraPaths, expand_tilde};
use hydra_platform::shell::ShellKind;

use crate::file::{self, NodeFile, SetFile, TabFile};
use crate::spec::{Node, PaneSpec, SetSpec, SplitDir, TabSpec};

/// What resolution needs from outside the set file.
pub struct Context<'a> {
    pub paths: &'a HydraPaths,
    pub user_home: &'a Path,
    /// hydra's `[bindings]` from config.toml.
    pub bindings: &'a BTreeMap<String, String>,
    /// From hydraui's config.toml; pwsh when unset.
    pub default_shell: ShellKind,
}

#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error(
        "invalid set name {0:?}: use 1-32 lowercase letters, digits and '-', starting with a letter or digit"
    )]
    Name(String),
    #[error("can't read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Toml { path: PathBuf, message: String },
    #[error("{}:\n  {}", path.display(), problems.join("\n  "))]
    Invalid {
        path: PathBuf,
        problems: Vec<String>,
    },
}

pub fn sets_dir(paths: &HydraPaths) -> PathBuf {
    paths.root().join("hydraui").join("sets")
}

pub fn set_file(paths: &HydraPaths, name: &str) -> PathBuf {
    sets_dir(paths).join(format!("{name}.toml"))
}

/// Names of the `*.toml` files in the sets folder, sorted. No folder means no sets.
pub fn list_sets(paths: &HydraPaths) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(sets_dir(paths)) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let p = entry?.path();
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            && let Some(stem) = p.file_stem()
        {
            names.push(stem.to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Reads and resolves `sets/<name>.toml`. Set names follow environment name rules.
pub fn load_set(ctx: &Context, name: &str) -> Result<SetSpec, SetError> {
    EnvName::parse(name).map_err(|_| SetError::Name(name.to_string()))?;
    let file = set_file(ctx.paths, name);
    let text = std::fs::read_to_string(&file).map_err(|source| SetError::Io {
        path: file.clone(),
        source,
    })?;
    parse_set(ctx, name, &file, &text)
}

pub fn parse_set(ctx: &Context, name: &str, file: &Path, text: &str) -> Result<SetSpec, SetError> {
    let raw = file::parse(text).map_err(|e| SetError::Toml {
        path: file.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut r = Resolver {
        ctx,
        problems: Vec::new(),
    };
    let tabs = r.set(&raw);
    if r.problems.is_empty() {
        Ok(SetSpec {
            name: name.to_string(),
            file: file.to_path_buf(),
            tabs,
        })
    } else {
        Err(SetError::Invalid {
            path: file.to_path_buf(),
            problems: r.problems,
        })
    }
}

/// A tab saved in session.toml, checked like a set's tab with nothing inherited from a set:
/// the tab, or what's wrong with it (an environment deleted, a folder gone).
pub fn resolve_loose(ctx: &Context, tab: &TabFile) -> Result<TabSpec, Vec<String>> {
    let mut r = Resolver {
        ctx,
        problems: Vec::new(),
    };
    let inherited = Inherited {
        env: None,
        cwd: ctx.user_home.to_path_buf(),
        shell: None,
    };
    match r.tab(1, tab, &inherited) {
        Some(spec) if r.problems.is_empty() => Ok(spec),
        _ => Err(r.problems),
    }
}

/// Values handed down from set to tab to pane.
#[derive(Clone)]
struct Inherited {
    env: Option<String>,
    cwd: PathBuf,
    shell: Option<String>,
}

struct Resolver<'a> {
    ctx: &'a Context<'a>,
    problems: Vec<String>,
}

impl Resolver<'_> {
    fn problem(&mut self, p: String) {
        if !self.problems.contains(&p) {
            self.problems.push(p);
        }
    }

    /// `raw` relative to `base`; `~` is the user's home.
    fn cwd(&self, raw: Option<&str>, base: &Path) -> PathBuf {
        match raw {
            None => base.to_path_buf(),
            Some(r) => {
                let p = expand_tilde(r, self.ctx.user_home);
                if p.is_absolute() { p } else { base.join(p) }
            }
        }
    }

    fn set(&mut self, s: &SetFile) -> Vec<TabSpec> {
        if s.tabs.is_empty() {
            self.problem("the set has no [[tab]]".into());
        }
        let inh = Inherited {
            env: s.env.clone(),
            cwd: self.cwd(s.cwd.as_deref(), self.ctx.user_home),
            shell: s.shell.clone(),
        };
        s.tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| self.tab(i + 1, t, &inh))
            .collect()
    }

    fn tab(&mut self, n: usize, t: &TabFile, set: &Inherited) -> Option<TabSpec> {
        let loc = match &t.title {
            Some(title) => format!("tab {title:?}"),
            None => format!("tab {n}"),
        };
        let inh = Inherited {
            env: t.env.clone().or_else(|| set.env.clone()),
            cwd: self.cwd(t.cwd.as_deref(), &set.cwd),
            shell: t.shell.clone().or_else(|| set.shell.clone()),
        };
        let root = match &t.layout {
            None => Node::Pane(self.pane(
                &loc,
                &inh,
                &NodeFile {
                    command: t.command.clone(),
                    ..Default::default()
                },
            )?),
            Some(layout) => {
                if t.command.is_some() {
                    self.problem(format!(
                        "{loc}: command goes on a pane when the tab has a layout"
                    ));
                    return None;
                }
                let mut count = 0;
                self.node(&loc, layout, &inh, &mut count)?
            }
        };
        let title = t.title.clone().unwrap_or_else(|| default_title(&root));
        Some(TabSpec { title, root })
    }

    fn node(
        &mut self,
        tab: &str,
        n: &NodeFile,
        inh: &Inherited,
        count: &mut usize,
    ) -> Option<Node> {
        let Some(dir) = &n.split else {
            *count += 1;
            let loc = format!("pane {count} of {tab}");
            if n.panes.is_some() || n.ratio.is_some() {
                self.problem(format!(
                    "{loc}: panes and ratio need split = \"right\" or \"down\""
                ));
                return None;
            }
            return self.pane(&loc, inh, n).map(Node::Pane);
        };
        let loc = format!("a split in {tab}");
        let dir = match dir.as_str() {
            "right" => SplitDir::Right,
            "down" => SplitDir::Down,
            other => {
                self.problem(format!(
                    "{loc}: split must be \"right\" or \"down\", not {other:?}"
                ));
                return None;
            }
        };
        if n.env.is_some() || n.cwd.is_some() || n.shell.is_some() || n.command.is_some() {
            self.problem(format!(
                "{loc}: env, cwd, shell and command go on its panes"
            ));
            return None;
        }
        let ratio = n.ratio.unwrap_or(0.5);
        if !(0.1..=0.9).contains(&ratio) {
            self.problem(format!("{loc}: ratio {ratio} must be between 0.1 and 0.9"));
            return None;
        }
        let panes = n.panes.as_deref().unwrap_or(&[]);
        if panes.len() != 2 {
            self.problem(format!("{loc}: needs exactly 2 panes, has {}", panes.len()));
            return None;
        }
        // Both sides, so problems in the second are found even when the first has some.
        let first = self.node(tab, &panes[0], inh, count);
        let second = self.node(tab, &panes[1], inh, count);
        Some(Node::Split {
            dir,
            ratio,
            first: Box::new(first?),
            second: Box::new(second?),
        })
    }

    /// A leaf: `n`'s own env/cwd/shell/command over `inh`.
    fn pane(&mut self, loc: &str, inh: &Inherited, n: &NodeFile) -> Option<PaneSpec> {
        let cwd = self.cwd(n.cwd.as_deref(), &inh.cwd);
        let shell = match n.shell.as_deref().or(inh.shell.as_deref()) {
            None => self.ctx.default_shell,
            Some(s) => match ShellKind::parse(s) {
                Some(k) => k,
                None => {
                    self.problem(format!("{loc}: unknown shell {s:?}; use pwsh or bash"));
                    return None;
                }
            },
        };
        let explicit = n.env.as_deref().or(inh.env.as_deref());
        let binding = if cwd.is_dir() {
            bindings::resolve(&cwd, self.ctx.bindings)
        } else {
            None
        };
        let env_name = match (explicit, &binding) {
            (Some(e), _) => e.to_string(),
            (None, Some(b)) => b.env.clone(),
            (None, None) if !cwd.is_dir() => String::new(),
            (None, None) => {
                self.problem(format!(
                    "{loc} has no environment and {} isn't bound",
                    cwd.display()
                ));
                return None;
            }
        };
        let env = if env_name.is_empty() {
            None
        } else {
            self.env(loc, &env_name)
        };
        if !cwd.is_dir() {
            self.problem(format!("{loc}: folder {} doesn't exist", cwd.display()));
            return None;
        }
        let env = env?;
        let bound_elsewhere = binding
            .filter(|b| explicit.is_some() && b.env != env.as_str())
            .map(|b| b.env);
        Some(PaneSpec {
            env,
            cwd,
            shell,
            command: n.command.clone(),
            bound_elsewhere,
        })
    }

    fn env(&mut self, loc: &str, name: &str) -> Option<EnvName> {
        match EnvName::parse(name) {
            Err(e) => {
                self.problem(format!("{loc}: {e}"));
                None
            }
            Ok(n) if !self.ctx.paths.env_file(&n).is_file() => {
                self.problem(format!("{loc}: environment {n} doesn't exist"));
                None
            }
            Ok(n) => Some(n),
        }
    }
}

/// A tab without a title: its first pane's program, else its folder's name.
fn default_title(root: &Node) -> String {
    let p = root.panes()[0];
    if let Some(word) = p
        .command
        .as_deref()
        .and_then(|c| c.split_whitespace().next())
    {
        return word.to_string();
    }
    p.cwd
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shell".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        _d: tempfile::TempDir,
        paths: HydraPaths,
        home: PathBuf,
        root: PathBuf,
        bindings: BTreeMap<String, String>,
    }

    fn fwd(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    impl Fx {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            let paths = HydraPaths::new(d.path().join(".hydra"));
            for env in ["work", "personal"] {
                let n = EnvName::parse(env).unwrap();
                std::fs::create_dir_all(paths.env_dir(&n)).unwrap();
                std::fs::write(paths.env_file(&n), "").unwrap();
            }
            let home = d.path().join("home");
            let root = d.path().join("client-a");
            for dir in ["api", "web", "bound", "it's here"] {
                std::fs::create_dir_all(root.join(dir)).unwrap();
            }
            std::fs::create_dir_all(&home).unwrap();
            let bindings =
                BTreeMap::from([(format!("{}/bound/**", fwd(&root)), "personal".to_string())]);
            Self {
                _d: d,
                paths,
                home,
                root,
                bindings,
            }
        }

        fn ctx(&self) -> Context<'_> {
            Context {
                paths: &self.paths,
                user_home: &self.home,
                bindings: &self.bindings,
                default_shell: ShellKind::Pwsh,
            }
        }

        /// Parses `text` with ROOT replaced by the fixture's set folder.
        fn parse(&self, text: &str) -> Result<SetSpec, SetError> {
            let text = text.replace("ROOT", &fwd(&self.root));
            parse_set(&self.ctx(), "client-a", Path::new("client-a.toml"), &text)
        }

        /// `sub` under ROOT as the set file spells it (forward slashes), for expected messages.
        fn written(&self, sub: &str) -> PathBuf {
            Path::new(&fwd(&self.root)).join(sub)
        }

        fn problems(&self, text: &str) -> Vec<String> {
            match self.parse(text) {
                Err(SetError::Invalid { problems, .. }) => problems,
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn single_pane_tab_inherits_from_the_set() {
        let fx = Fx::new();
        let s = fx
            .parse("env = \"work\"\ncwd = \"ROOT\"\n[[tab]]\ncwd = \"api\"\n")
            .unwrap();
        assert_eq!(s.name, "client-a");
        let tab = &s.tabs[0];
        assert_eq!(tab.title, "api");
        let Node::Pane(p) = &tab.root else { panic!() };
        assert_eq!(p.env.as_str(), "work");
        assert_eq!(p.cwd, fx.root.join("api"));
        assert_eq!(p.shell, ShellKind::Pwsh);
        assert_eq!(p.command, None);
        assert_eq!(p.bound_elsewhere, None);
    }

    #[test]
    fn layout_resolves_every_pane() {
        let fx = Fx::new();
        let s = fx
            .parse(
                r#"env = "work"
cwd = "ROOT"
[[tab]]
cwd = "api"
layout = { split = "right", ratio = 0.6, panes = [
  { command = "claude" },
  { split = "down", panes = [ { command = "npm run dev", cwd = "../web" }, { shell = "bash" } ] },
] }
"#,
            )
            .unwrap();
        let tab = &s.tabs[0];
        assert_eq!(tab.title, "claude");
        let Node::Split {
            dir, ratio, second, ..
        } = &tab.root
        else {
            panic!()
        };
        assert_eq!((*dir, *ratio), (SplitDir::Right, 0.6));
        let Node::Split { dir, ratio, .. } = second.as_ref() else {
            panic!()
        };
        assert_eq!((*dir, *ratio), (SplitDir::Down, 0.5));
        let panes = tab.root.panes();
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].command.as_deref(), Some("claude"));
        assert_eq!(panes[1].cwd, fx.root.join("api").join("../web"));
        assert_eq!(panes[2].shell, ShellKind::Bash);
        assert_eq!(panes[2].cwd, fx.root.join("api"));
    }

    #[test]
    fn pane_env_beats_tab_env_beats_set_env() {
        let fx = Fx::new();
        let s = fx
            .parse(
                r#"env = "work"
cwd = "ROOT/api"
[[tab]]
env = "personal"
layout = { split = "right", panes = [ { }, { env = "work" } ] }
"#,
            )
            .unwrap();
        let envs: Vec<&str> = s.tabs[0]
            .root
            .panes()
            .iter()
            .map(|p| p.env.as_str())
            .collect();
        assert_eq!(envs, vec!["personal", "work"]);
    }

    #[test]
    fn folder_binding_supplies_a_missing_env() {
        let fx = Fx::new();
        let s = fx.parse("[[tab]]\ncwd = \"ROOT/bound\"\n").unwrap();
        let Node::Pane(p) = &s.tabs[0].root else {
            panic!()
        };
        assert_eq!(p.env.as_str(), "personal");
        assert_eq!(p.bound_elsewhere, None);
    }

    #[test]
    fn explicit_env_on_a_folder_bound_elsewhere_loads_with_a_note() {
        let fx = Fx::new();
        let s = fx
            .parse("env = \"work\"\n[[tab]]\ncwd = \"ROOT/bound\"\n")
            .unwrap();
        let Node::Pane(p) = &s.tabs[0].root else {
            panic!()
        };
        assert_eq!(p.env.as_str(), "work");
        assert_eq!(p.bound_elsewhere.as_deref(), Some("personal"));
    }

    #[test]
    fn tilde_and_awkward_folders_resolve() {
        let fx = Fx::new();
        let s = fx
            .parse("env = \"work\"\n[[tab]]\ncwd = \"~\"\n[[tab]]\ncwd = \"ROOT/it's here\"\n")
            .unwrap();
        assert_eq!(s.tabs[0].root.panes()[0].cwd, fx.home);
        assert_eq!(s.tabs[1].root.panes()[0].cwd, fx.root.join("it's here"));
    }

    #[test]
    fn unbound_folder_without_env_is_a_problem() {
        let fx = Fx::new();
        let problems = fx.problems(
            "cwd = \"ROOT\"\n[[tab]]\ntitle = \"api\"\nlayout = { split = \"down\", panes = [ { env = \"work\" }, { cwd = \"api\" } ] }\n",
        );
        assert_eq!(
            problems,
            vec![format!(
                "pane 2 of tab \"api\" has no environment and {} isn't bound",
                fx.written("api").display()
            )]
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let fx = Fx::new();
        let problems = fx.problems(
            r#"env = "work"
cwd = "ROOT"
[[tab]]
title = "a"
env = "nobody"
[[tab]]
title = "b"
shell = "fish"
[[tab]]
title = "c"
cwd = "missing"
[[tab]]
title = "d"
command = "x"
layout = { split = "right", panes = [ { }, { } ] }
[[tab]]
title = "e"
layout = { split = "right", ratio = 0.95, panes = [ { }, { } ] }
[[tab]]
title = "f"
layout = { split = "down", panes = [ { }, { }, { } ] }
[[tab]]
title = "g"
layout = { split = "sideways", panes = [ { }, { } ] }
[[tab]]
title = "h"
layout = { split = "right", command = "x", panes = [ { }, { } ] }
"#,
        );
        assert_eq!(
            problems,
            vec![
                "tab \"a\": environment nobody doesn't exist".to_string(),
                "tab \"b\": unknown shell \"fish\"; use pwsh or bash".to_string(),
                format!(
                    "tab \"c\": folder {} doesn't exist",
                    fx.written("missing").display()
                ),
                "tab \"d\": command goes on a pane when the tab has a layout".to_string(),
                "a split in tab \"e\": ratio 0.95 must be between 0.1 and 0.9".to_string(),
                "a split in tab \"f\": needs exactly 2 panes, has 3".to_string(),
                "a split in tab \"g\": split must be \"right\" or \"down\", not \"sideways\""
                    .to_string(),
                "a split in tab \"h\": env, cwd, shell and command go on its panes".to_string(),
            ]
        );
    }

    #[test]
    fn a_set_needs_a_tab() {
        let fx = Fx::new();
        assert_eq!(
            fx.problems("env = \"work\"\n"),
            vec!["the set has no [[tab]]"]
        );
    }

    #[test]
    fn toml_errors_name_the_file() {
        let fx = Fx::new();
        let Err(SetError::Toml { path, message }) = fx.parse("[[tab]]\ntitel = 1\n") else {
            panic!()
        };
        assert_eq!(path, PathBuf::from("client-a.toml"));
        assert!(message.contains("titel"), "{message}");
    }

    #[test]
    fn load_and_list_from_the_sets_folder() {
        let fx = Fx::new();
        let dir = sets_dir(&fx.paths);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("client-a.toml"),
            format!("env = \"work\"\n[[tab]]\ncwd = \"{}\"\n", fwd(&fx.root)),
        )
        .unwrap();
        std::fs::write(dir.join("b.toml"), "").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(list_sets(&fx.paths).unwrap(), vec!["b", "client-a"]);
        let s = load_set(&fx.ctx(), "client-a").unwrap();
        assert_eq!(s.file, set_file(&fx.paths, "client-a"));
        assert!(matches!(
            load_set(&fx.ctx(), "Bad Name"),
            Err(SetError::Name(_))
        ));
        assert!(matches!(
            load_set(&fx.ctx(), "absent"),
            Err(SetError::Io { .. })
        ));
    }

    #[test]
    fn no_sets_folder_means_no_sets() {
        let fx = Fx::new();
        assert!(list_sets(&fx.paths).unwrap().is_empty());
    }

    #[test]
    fn a_saved_loose_tab_resolves_or_says_why_not() {
        let fx = Fx::new();
        let tab = |dir: &str| {
            let text = format!(
                "[[tab]]
env = \"work\"
cwd = \"{}\"
",
                fwd(&fx.root.join(dir))
            );
            file::parse(&text).unwrap().tabs.remove(0)
        };
        let spec = resolve_loose(&fx.ctx(), &tab("api")).unwrap();
        assert_eq!(spec.root.panes()[0].cwd, fx.root.join("api"));
        assert_eq!(spec.root.panes()[0].env.as_str(), "work");
        let problems = resolve_loose(&fx.ctx(), &tab("gone")).unwrap_err();
        assert!(problems[0].contains("doesn't exist"), "{problems:?}");
    }
}
