//! The version 2 layout file (`*.hydraui`, TOML): a window's environments in tab order, each
//! one's terminals and file pane, the tree, and what is selected. A file without
//! `version = 2` is an older hydraui's: it is refused, never converted or modified.
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use hydra_core::bindings;
use hydra_core::name::EnvName;
use hydra_core::paths::{HydraPaths, expand_tilde};
use hydra_platform::shell::ShellKind;
use serde::{Deserialize, Serialize};

use crate::files::write_atomic;
use crate::spec::{FilesSpec, PaneSpec};

pub const LAYOUT_VERSION: u32 = 2;

/// A whole layout file. Fields are ordered plain values first, since TOML needs tables last.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// Always written as [`LAYOUT_VERSION`].
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeFile>,
    #[serde(default, rename = "env", skip_serializing_if = "Vec::is_empty")]
    pub envs: Vec<EnvFile>,
}

/// The file tree: shown or hidden, and its dragged width.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeFile {
    #[serde(default)]
    pub open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
}

/// One environment tab: its terminals, the shown one, and its file pane if open.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvFile {
    pub name: String,
    /// Index into `terminals`: the one shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<FilesFile>,
    #[serde(default, rename = "terminal", skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<TerminalFile>,
}

/// An environment's file pane: the open files and the shown one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesFile {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
}

/// One terminal: its folder when saved, and optionally a title, shell and start command.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Hand-written only: run whenever the layout opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    /// Not a `*.hydraui` file.
    NotLayout(PathBuf),
    /// Made by an older hydraui (no `version = 2`).
    Old(PathBuf),
    /// Unreadable, or not valid TOML for a layout.
    Read { path: PathBuf, message: String },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLayout(p) => write!(f, "{} isn't a layout file (*.hydraui)", p.display()),
            Self::Old(p) => write!(
                f,
                "{} was made by an older hydraui and can't be opened; start a new layout",
                p.display()
            ),
            Self::Read { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Parses layout text read from `path`: anything without `version = 2` is `Old`.
pub fn parse_layout(path: &Path, text: &str) -> Result<Layout, LayoutError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let read_err = |e: toml::de::Error| LayoutError::Read {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    let table: toml::Table = toml::from_str(text).map_err(read_err)?;
    let current = table.get("version").and_then(|v| v.as_integer()) == Some(LAYOUT_VERSION.into());
    if !current {
        return Err(LayoutError::Old(path.to_path_buf()));
    }
    toml::from_str(text).map_err(read_err)
}

/// Reads a `*.hydraui` file (extension in any case).
pub fn read_layout(path: &Path) -> Result<Layout, LayoutError> {
    let is_layout = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("hydraui"));
    if !is_layout {
        return Err(LayoutError::NotLayout(path.to_path_buf()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| LayoutError::Read {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    parse_layout(path, &text)
}

/// Writes `layout` as version 2, replacing the file atomically.
pub fn write_layout(path: &Path, layout: &Layout) -> std::io::Result<()> {
    let mut layout = layout.clone();
    layout.version = LAYOUT_VERSION;
    write_atomic(
        path,
        &toml::to_string(&layout).map_err(std::io::Error::other)?,
    )
}

/// What resolution needs from outside the file being opened.
pub struct Context<'a> {
    pub paths: &'a HydraPaths,
    pub user_home: &'a Path,
    /// hydra's `[bindings]` from config.toml.
    pub bindings: &'a BTreeMap<String, String>,
    /// From hydraui's config.toml; pwsh when unset.
    pub default_shell: ShellKind,
}

/// A terminal that will open: what to start, and the name the layout gave it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedTerminal {
    pub spec: PaneSpec,
    pub title: Option<String>,
}

/// An environment tab that will open.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedEnv {
    pub name: String,
    pub terminals: Vec<PlannedTerminal>,
    /// Index into `terminals`: the one shown; `None` when none opened.
    pub shown: Option<usize>,
    pub files: Option<FilesSpec>,
}

/// A layout resolved against the disk: what opens, what didn't and why.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutPlan {
    pub envs: Vec<PlannedEnv>,
    pub active_env: Option<String>,
    pub tree: Option<TreeFile>,
    /// One toast each.
    pub problems: Vec<String>,
    /// Environments and terminals that didn't open, kept in what is written back.
    pub unopened: Layout,
}

/// `raw` as a path: `~` is the user's home, a relative path is under it.
fn resolve_path(ctx: &Context, raw: &str) -> PathBuf {
    let p = expand_tilde(raw, ctx.user_home);
    if p.is_absolute() {
        p
    } else {
        ctx.user_home.join(p)
    }
}

/// Resolves a layout into the tabs and terminals that open (spec §3). Anything that can't open
/// (a deleted environment, a missing folder, an unknown shell) is reported and kept in `unopened`.
pub fn plan_layout(ctx: &Context, layout: &Layout) -> LayoutPlan {
    let mut plan = LayoutPlan {
        envs: Vec::new(),
        active_env: None,
        tree: layout.tree.clone(),
        problems: Vec::new(),
        unopened: Layout {
            version: LAYOUT_VERSION,
            ..Layout::default()
        },
    };
    // A repeated environment merges into its first entry, whose `active` and `files` win.
    let mut merged: Vec<EnvFile> = Vec::new();
    for env in &layout.envs {
        match merged.iter_mut().find(|e| e.name == env.name) {
            Some(first) => first.terminals.extend(env.terminals.iter().cloned()),
            None => merged.push(env.clone()),
        }
    }
    for env in merged {
        let exists = EnvName::parse(&env.name)
            .map(|n| ctx.paths.env_file(&n).is_file())
            .unwrap_or(false);
        if !exists {
            plan.problems
                .push(format!("environment {} no longer exists", env.name));
            plan.unopened.envs.push(env);
            continue;
        }
        let planned = plan_env(ctx, &env, &mut plan);
        plan.envs.push(planned);
    }
    plan.active_env = match &layout.active_env {
        Some(a) if plan.envs.iter().any(|e| &e.name == a) => Some(a.clone()),
        _ => plan.envs.first().map(|e| e.name.clone()),
    };
    plan
}

/// One existing environment's tab; terminals that can't open go to `plan.unopened`.
fn plan_env(ctx: &Context, env: &EnvFile, plan: &mut LayoutPlan) -> PlannedEnv {
    let name = EnvName::parse(&env.name).expect("checked by the caller");
    let mut terminals = Vec::new();
    let mut shown = None;
    for (i, t) in env.terminals.iter().enumerate() {
        match plan_terminal(ctx, &name, t) {
            Ok(planned) => {
                if env.active == Some(i) {
                    shown = Some(terminals.len());
                }
                terminals.push(planned);
            }
            Err(problem) => {
                plan.problems.push(problem);
                match plan.unopened.envs.iter_mut().find(|e| e.name == env.name) {
                    Some(kept) => kept.terminals.push(t.clone()),
                    None => plan.unopened.envs.push(EnvFile {
                        name: env.name.clone(),
                        terminals: vec![t.clone()],
                        ..EnvFile::default()
                    }),
                }
            }
        }
    }
    if shown.is_none() && !terminals.is_empty() {
        shown = Some(0);
    }
    PlannedEnv {
        name: env.name.clone(),
        terminals,
        shown,
        files: env.files.as_ref().and_then(|f| plan_files(ctx, f)),
    }
}

/// A terminal in `env`: its folder must exist and its shell be known; `Err` is the problem.
fn plan_terminal(
    ctx: &Context,
    env: &EnvName,
    t: &TerminalFile,
) -> Result<PlannedTerminal, String> {
    let cwd = resolve_path(ctx, &t.cwd);
    if !cwd.is_dir() {
        return Err(format!("{env}: folder {} doesn't exist", cwd.display()));
    }
    let shell = match t.shell.as_deref() {
        None => ctx.default_shell,
        Some(s) => ShellKind::parse(s)
            .ok_or_else(|| format!("{env}: unknown shell {s:?}; use pwsh or bash"))?,
    };
    let bound_elsewhere = bindings::resolve(&cwd, ctx.bindings)
        .filter(|b| b.env != env.as_str())
        .map(|b| b.env);
    Ok(PlannedTerminal {
        spec: PaneSpec {
            env: env.clone(),
            cwd,
            shell,
            command: t.command.clone(),
            bound_elsewhere,
        },
        title: t.title.clone(),
    })
}

/// The file pane with missing files dropped; `None` when none are left.
fn plan_files(ctx: &Context, f: &FilesFile) -> Option<FilesSpec> {
    let files: Vec<PathBuf> = f
        .files
        .iter()
        .map(|p| resolve_path(ctx, p))
        .filter(|p| p.is_file())
        .collect();
    if files.is_empty() {
        return None;
    }
    let active = f.active.unwrap_or(0).min(files.len() - 1);
    Some(FilesSpec { files, active })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
version = 2
active_env = "work"
tree = { open = true, width = 280.0 }

[[env]]
name = "work"
active = 0                       # index into this env's terminals: the one shown
files = { files = ["E:/work/api/README.md"], active = 0 }   # the env's file pane, if open

[[env.terminal]]
title = "claude"                 # optional: set by Rename
cwd = "E:/work/api"              # its current folder when saved
shell = "pwsh"                   # optional
command = "claude"               # optional: run when the layout opens (hand-written only)

[[env.terminal]]
cwd = "E:/work/api"

[[env]]
name = "personal"

[[env.terminal]]
cwd = "~/blog"
"#;

    #[test]
    fn the_spec_example_parses() {
        let l = parse_layout(Path::new("a.hydraui"), SPEC).unwrap();
        assert_eq!(l.active_env, Some("work".into()));
        assert_eq!(
            l.tree,
            Some(TreeFile {
                open: true,
                width: Some(280.0)
            })
        );
        assert_eq!(l.envs.len(), 2);
        assert_eq!(l.envs[0].terminals.len(), 2);
        assert_eq!(l.envs[0].terminals[0].command, Some("claude".into()));
        assert_eq!(
            l.envs[0].files.as_ref().unwrap().files,
            ["E:/work/api/README.md"]
        );
        assert_eq!(l.envs[1].terminals[0].cwd, "~/blog");
    }

    #[test]
    fn a_layout_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.hydraui");
        let layout = Layout {
            version: LAYOUT_VERSION,
            active_env: Some("b".into()),
            tree: Some(TreeFile {
                open: false,
                width: None,
            }),
            envs: vec![
                EnvFile {
                    name: "a".into(),
                    active: Some(1),
                    files: Some(FilesFile {
                        files: vec!["C:/x.md".into()],
                        active: Some(0),
                    }),
                    terminals: vec![
                        TerminalFile {
                            title: Some("t".into()),
                            cwd: "C:/a".into(),
                            shell: Some("pwsh".into()),
                            command: Some("claude".into()),
                        },
                        TerminalFile {
                            cwd: "C:/b".into(),
                            ..Default::default()
                        },
                    ],
                },
                EnvFile {
                    name: "b".into(),
                    ..Default::default()
                },
            ],
        };
        write_layout(&p, &layout).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .starts_with("version = 2")
        );
        assert_eq!(read_layout(&p).unwrap(), layout);
    }

    #[test]
    fn a_layout_without_version_2_is_old() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("old.hydraui");
        for text in [
            "sets = [\"a\"]\n",
            "[[loose]]\nenv = \"w\"\n",
            "version = 1\n",
            "",
        ] {
            std::fs::write(&p, text).unwrap();
            assert!(
                matches!(read_layout(&p), Err(LayoutError::Old(_))),
                "{text:?}"
            );
            assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        }
    }

    #[test]
    fn only_hydraui_files_are_layouts() {
        let d = tempfile::tempdir().unwrap();
        for name in ["notes.txt", "a.hydraui.toml"] {
            let p = d.path().join(name);
            std::fs::write(&p, "version = 2\n").unwrap();
            let e = read_layout(&p).unwrap_err();
            assert!(matches!(e, LayoutError::NotLayout(_)));
            assert!(e.to_string().contains(name));
        }
        let p = d.path().join("UP.HYDRAUI");
        std::fs::write(&p, "version = 2\n").unwrap();
        assert!(read_layout(&p).is_ok());
    }

    #[test]
    fn unknown_keys_and_bad_toml_name_the_file() {
        for text in ["version = 2\nbogus = 1\n", "version = 2\n[[env]]\n"] {
            let e = parse_layout(Path::new("dir/bad.hydraui"), text).unwrap_err();
            assert!(matches!(e, LayoutError::Read { .. }), "{text:?}");
            assert!(e.to_string().contains("bad.hydraui"));
        }
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        assert!(parse_layout(Path::new("a.hydraui"), "\u{feff}version = 2\n").is_ok());
    }

    struct Fx {
        _d: tempfile::TempDir,
        paths: HydraPaths,
        home: PathBuf,
        bindings: BTreeMap<String, String>,
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
            for dir in ["api", "web", "bound"] {
                std::fs::create_dir_all(home.join(dir)).unwrap();
            }
            let bound = home.join("bound").to_string_lossy().replace('\\', "/");
            let bindings = BTreeMap::from([(format!("{bound}/**"), "personal".to_string())]);
            Self {
                _d: d,
                paths,
                home,
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

        /// A folder under the fixture's home, spelled as a layout file would.
        fn dir(&self, sub: &str) -> String {
            self.home.join(sub).to_string_lossy().into_owned()
        }
    }

    fn term(cwd: &str) -> TerminalFile {
        TerminalFile {
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    fn env(name: &str, terminals: Vec<TerminalFile>) -> EnvFile {
        EnvFile {
            name: name.into(),
            terminals,
            ..Default::default()
        }
    }

    fn layout(envs: Vec<EnvFile>) -> Layout {
        Layout {
            version: LAYOUT_VERSION,
            envs,
            ..Default::default()
        }
    }

    #[test]
    fn a_layout_plans_each_env_and_terminal() {
        let fx = Fx::new();
        let mut work = env(
            "work",
            vec![
                TerminalFile {
                    title: Some("claude".into()),
                    cwd: fx.dir("api"),
                    shell: Some("bash".into()),
                    command: Some("claude".into()),
                },
                term(&fx.dir("web")),
            ],
        );
        work.active = Some(1);
        let l = layout(vec![work, env("personal", vec![term(&fx.dir("api"))])]);
        let plan = plan_layout(&fx.ctx(), &l);
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        assert_eq!(plan.unopened.envs, vec![]);
        let names: Vec<&str> = plan.envs.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["work", "personal"]);
        let work = &plan.envs[0];
        assert_eq!(work.shown, Some(1));
        let t0 = &work.terminals[0];
        assert_eq!(t0.title.as_deref(), Some("claude"));
        assert_eq!(t0.spec.env.as_str(), "work");
        assert_eq!(t0.spec.cwd, fx.home.join("api"));
        assert_eq!(t0.spec.shell, ShellKind::Bash);
        assert_eq!(t0.spec.command.as_deref(), Some("claude"));
        let t1 = &work.terminals[1];
        assert_eq!(t1.title, None);
        assert_eq!(t1.spec.shell, ShellKind::Pwsh);
        assert_eq!(t1.spec.command, None);
        assert_eq!(plan.envs[1].shown, Some(0));
    }

    #[test]
    fn a_missing_env_is_a_problem_and_kept() {
        let fx = Fx::new();
        let mut gone = env("gone", vec![term(&fx.dir("api"))]);
        gone.active = Some(0);
        let l = layout(vec![
            gone.clone(),
            env("Bad Name", vec![]),
            env("work", vec![]),
        ]);
        let plan = plan_layout(&fx.ctx(), &l);
        assert_eq!(
            plan.problems,
            [
                "environment gone no longer exists",
                "environment Bad Name no longer exists"
            ]
        );
        assert_eq!(plan.unopened.envs[0], gone);
        assert_eq!(plan.unopened.envs[1].name, "Bad Name");
        assert_eq!(plan.envs.len(), 1);
        assert_eq!(plan.envs[0].name, "work");
    }

    #[test]
    fn a_missing_folder_skips_only_that_terminal() {
        let fx = Fx::new();
        let bad = TerminalFile {
            title: Some("lost".into()),
            ..term(&fx.dir("nope"))
        };
        let l = layout(vec![env("work", vec![term(&fx.dir("api")), bad.clone()])]);
        let plan = plan_layout(&fx.ctx(), &l);
        assert_eq!(
            plan.problems,
            [format!("work: folder {} doesn't exist", fx.dir("nope"))]
        );
        assert_eq!(plan.unopened.envs.len(), 1);
        assert_eq!(plan.unopened.envs[0].name, "work");
        assert_eq!(plan.unopened.envs[0].terminals, vec![bad]);
        assert_eq!(plan.envs[0].terminals.len(), 1);
    }

    #[test]
    fn an_unknown_shell_skips_the_terminal() {
        let fx = Fx::new();
        let t = TerminalFile {
            shell: Some("fish".into()),
            ..term(&fx.dir("api"))
        };
        let plan = plan_layout(&fx.ctx(), &layout(vec![env("work", vec![t.clone()])]));
        assert_eq!(
            plan.problems,
            ["work: unknown shell \"fish\"; use pwsh or bash"]
        );
        assert_eq!(plan.unopened.envs[0].terminals, vec![t]);
        assert!(plan.envs[0].terminals.is_empty());
        assert_eq!(plan.envs[0].shown, None);
    }

    #[test]
    fn duplicate_envs_merge_into_one_tab() {
        let fx = Fx::new();
        let mut first = env("work", vec![term(&fx.dir("api"))]);
        first.active = Some(0);
        let mut second = env("work", vec![term(&fx.dir("web"))]);
        second.active = Some(1);
        second.files = Some(FilesFile {
            files: vec![fx.dir("web")],
            active: None,
        });
        let plan = plan_layout(&fx.ctx(), &layout(vec![first, second]));
        assert_eq!(plan.envs.len(), 1);
        let cwds: Vec<_> = plan.envs[0].terminals.iter().map(|t| &t.spec.cwd).collect();
        assert_eq!(cwds, [&fx.home.join("api"), &fx.home.join("web")]);
        assert_eq!(plan.envs[0].shown, Some(0));
        assert_eq!(plan.envs[0].files, None);
    }

    #[test]
    fn active_falls_back_to_the_first_opened_terminal() {
        let fx = Fx::new();
        let with = |active, terminals| {
            let mut e = env("work", terminals);
            e.active = Some(active);
            plan_layout(&fx.ctx(), &layout(vec![e])).envs[0].shown
        };
        let ok = || term(&fx.dir("api"));
        assert_eq!(with(5, vec![ok(), ok()]), Some(0));
        assert_eq!(with(1, vec![ok(), term(&fx.dir("nope"))]), Some(0));
        assert_eq!(with(0, vec![]), None);
        // The index maps through a skipped terminal before it.
        assert_eq!(with(2, vec![ok(), term(&fx.dir("nope")), ok()]), Some(1));
    }

    #[test]
    fn tilde_and_relative_folders_resolve_from_the_user_home() {
        let fx = Fx::new();
        let l = layout(vec![env(
            "work",
            vec![term("~/api"), term("web"), term("~")],
        )]);
        let plan = plan_layout(&fx.ctx(), &l);
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        let cwds: Vec<_> = plan.envs[0].terminals.iter().map(|t| &t.spec.cwd).collect();
        assert_eq!(cwds, [&fx.home.join("api"), &fx.home.join("web"), &fx.home]);
    }

    #[test]
    fn a_folder_bound_elsewhere_is_noted() {
        let fx = Fx::new();
        let l = layout(vec![
            env("work", vec![term(&fx.dir("bound")), term(&fx.dir("api"))]),
            env("personal", vec![term(&fx.dir("bound"))]),
        ]);
        let plan = plan_layout(&fx.ctx(), &l);
        let note = |e: usize, t: usize| plan.envs[e].terminals[t].spec.bound_elsewhere.as_deref();
        assert_eq!(note(0, 0), Some("personal"));
        assert_eq!(note(0, 1), None);
        assert_eq!(note(1, 0), None);
    }

    #[test]
    fn missing_files_drop_and_active_clamps() {
        let fx = Fx::new();
        let a = fx.home.join("a.md");
        let b = fx.home.join("b.md");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let files = |list: Vec<String>, active| {
            let mut e = env("work", vec![]);
            e.files = Some(FilesFile {
                files: list,
                active,
            });
            plan_layout(&fx.ctx(), &layout(vec![e])).envs[0]
                .files
                .clone()
        };
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let got = files(vec![s(&a), fx.dir("gone.md"), "b.md".into()], Some(9)).unwrap();
        assert_eq!(got.files, vec![a.clone(), b]);
        assert_eq!(got.active, 1);
        assert_eq!(files(vec![s(&a)], None).unwrap().active, 0);
        assert_eq!(files(vec![fx.dir("gone.md")], Some(0)), None);
    }

    #[test]
    fn active_env_falls_back_to_the_first_planned() {
        let fx = Fx::new();
        let mut l = layout(vec![
            env("gone", vec![]),
            env("work", vec![]),
            env("personal", vec![]),
        ]);
        l.tree = Some(TreeFile {
            open: true,
            width: None,
        });
        l.active_env = Some("personal".into());
        let plan = plan_layout(&fx.ctx(), &l);
        assert_eq!(plan.active_env.as_deref(), Some("personal"));
        assert_eq!(plan.tree, l.tree);
        for active in [Some("gone".to_string()), Some("nobody".into()), None] {
            l.active_env = active;
            let plan = plan_layout(&fx.ctx(), &l);
            assert_eq!(plan.active_env.as_deref(), Some("work"));
        }
        assert_eq!(plan_layout(&fx.ctx(), &layout(vec![])).active_env, None);
    }
}
