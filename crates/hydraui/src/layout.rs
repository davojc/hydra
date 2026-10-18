//! A set's tab as panes (spec §5): hydraui-sets' resolved layout becomes a split tree, and
//! each pane what it runs; and back again, to save open tabs in session.toml.
use std::path::PathBuf;

use hydra_platform::shell::ShellKind;
use hydraui_sets::file::{NodeFile, TabFile};
use hydraui_sets::{FilesSpec, Node, SplitDir, TabSpec};

use crate::hydra::PaneRequest;
use crate::split::{Axis, PaneId, Tree};

/// A tab's tree, its terminals' requests in tree order (left to right, top to bottom), and
/// its file pane.
pub struct TabLayout {
    pub tree: Tree,
    pub terminals: Vec<(PaneId, PaneRequest)>,
    pub files: Option<(PaneId, FilesSpec)>,
}

/// The tab's layout, with `next_id` naming each pane.
pub fn tab_layout<F: FnMut() -> PaneId>(tab: &TabSpec, next_id: &mut F) -> TabLayout {
    let mut terminals = Vec::new();
    let mut files = None;
    let tree = node(&tab.root, next_id, &mut terminals, &mut files);
    TabLayout {
        tree,
        terminals,
        files,
    }
}

fn node<F: FnMut() -> PaneId>(
    n: &Node,
    next_id: &mut F,
    panes: &mut Vec<(PaneId, PaneRequest)>,
    files: &mut Option<(PaneId, FilesSpec)>,
) -> Tree {
    match n {
        Node::Pane(p) => {
            let id = next_id();
            let mut req = PaneRequest::new(p.env.to_string(), p.cwd.clone(), p.shell);
            req.command = p.command.clone();
            // An explicit env on a folder bound elsewhere: the only signal (spec §5.2).
            req.note = p
                .bound_elsewhere
                .as_ref()
                .map(|e| format!("this folder is bound to {e}"));
            panes.push((id, req));
            Tree::Leaf(id)
        }
        Node::Files(f) => {
            let id = next_id();
            *files = Some((id, f.clone()));
            Tree::Leaf(id)
        }
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => Tree::Split {
            axis: match dir {
                SplitDir::Right => Axis::Row,
                SplitDir::Down => Axis::Column,
            },
            ratio: *ratio,
            first: Box::new(node(first, next_id, panes, files)),
            second: Box::new(node(second, next_id, panes, files)),
        },
    }
}

/// What a pane needs to come back after a restart.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedPane {
    Terminal {
        env: String,
        cwd: PathBuf,
        shell: ShellKind,
        /// The startup command (set tabs have one; Loose tabs don't).
        command: Option<String>,
    },
    Files {
        files: Vec<PathBuf>,
        active: usize,
    },
}

/// An open tab in the set file's tab format, for session.toml: the reverse of `tab_layout`.
/// A one-pane tab is written on the tab itself. `None` when a pane can't be described, or the
/// tab is only a file pane (not a valid tab).
pub fn tab_file(
    tree: &Tree,
    title: Option<&str>,
    pane: &impl Fn(PaneId) -> Option<SavedPane>,
) -> Option<TabFile> {
    let title = title.map(str::to_string);
    match tree {
        Tree::Leaf(id) => match pane(*id)? {
            SavedPane::Terminal {
                env,
                cwd,
                shell,
                command,
            } => Some(TabFile {
                title,
                env: Some(env),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                shell: Some(shell.as_str().into()),
                command,
                ..Default::default()
            }),
            SavedPane::Files { .. } => None,
        },
        split => Some(TabFile {
            title,
            layout: Some(node_file(split, pane)?),
            ..Default::default()
        }),
    }
}

fn node_file(tree: &Tree, pane: &impl Fn(PaneId) -> Option<SavedPane>) -> Option<NodeFile> {
    match tree {
        Tree::Leaf(id) => Some(match pane(*id)? {
            SavedPane::Terminal {
                env,
                cwd,
                shell,
                command,
            } => NodeFile {
                env: Some(env),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                shell: Some(shell.as_str().into()),
                command,
                ..Default::default()
            },
            SavedPane::Files { files, active } => NodeFile {
                files: Some(
                    files
                        .iter()
                        .map(|f| f.to_string_lossy().into_owned())
                        .collect(),
                ),
                active: (active > 0).then_some(active),
                ..Default::default()
            },
        }),
        Tree::Split {
            axis,
            ratio,
            first,
            second,
        } => Some(NodeFile {
            split: Some(
                match axis {
                    Axis::Row => "right",
                    Axis::Column => "down",
                }
                .into(),
            ),
            ratio: Some(*ratio),
            panes: Some(vec![node_file(first, pane)?, node_file(second, pane)?]),
            ..Default::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hydra_core::name::EnvName;
    use hydra_platform::shell::ShellKind;
    use hydraui_sets::PaneSpec;

    use super::*;

    fn pane(command: Option<&str>, bound_elsewhere: Option<&str>) -> Node {
        Node::Pane(PaneSpec {
            env: EnvName::parse("work").unwrap(),
            cwd: PathBuf::from("C:\\work\\api"),
            shell: ShellKind::Pwsh,
            command: command.map(str::to_string),
            bound_elsewhere: bound_elsewhere.map(str::to_string),
        })
    }

    fn ids() -> impl FnMut() -> PaneId {
        let mut n = 9;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn a_single_pane_tab_is_one_leaf() {
        let tab = TabSpec {
            title: "infra".into(),
            root: pane(Some("terraform plan"), None),
        };
        let l = tab_layout(&tab, &mut ids());
        let (tree, panes) = (l.tree, l.terminals);
        assert_eq!(tree, Tree::Leaf(10));
        assert_eq!(panes.len(), 1);
        let (id, req) = &panes[0];
        assert_eq!(*id, 10);
        assert_eq!(req.env, "work");
        assert_eq!(req.cwd, PathBuf::from("C:\\work\\api"));
        assert_eq!(req.shell, ShellKind::Pwsh);
        assert_eq!(req.command.as_deref(), Some("terraform plan"));
        assert_eq!(req.note, None);
    }

    #[test]
    fn splits_become_the_same_tree_with_fresh_ids() {
        let tab = TabSpec {
            title: "api".into(),
            root: Node::Split {
                dir: SplitDir::Right,
                ratio: 0.6,
                first: Box::new(pane(Some("claude"), None)),
                second: Box::new(Node::Split {
                    dir: SplitDir::Down,
                    ratio: 0.5,
                    first: Box::new(pane(Some("npm run dev"), None)),
                    second: Box::new(pane(None, Some("personal"))),
                }),
            },
        };
        let l = tab_layout(&tab, &mut ids());
        let (tree, panes) = (l.tree, l.terminals);
        assert_eq!(
            tree,
            Tree::Split {
                axis: Axis::Row,
                ratio: 0.6,
                first: Box::new(Tree::Leaf(10)),
                second: Box::new(Tree::Split {
                    axis: Axis::Column,
                    ratio: 0.5,
                    first: Box::new(Tree::Leaf(11)),
                    second: Box::new(Tree::Leaf(12)),
                }),
            }
        );
        let ids: Vec<PaneId> = panes.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![10, 11, 12]);
        assert_eq!(panes[1].1.command.as_deref(), Some("npm run dev"));
        assert_eq!(
            panes[2].1.note.as_deref(),
            Some("this folder is bound to personal")
        );
    }

    fn saved(id: PaneId) -> Option<SavedPane> {
        Some(SavedPane::Terminal {
            env: "work".into(),
            cwd: PathBuf::from(format!("C:/w/{id}")),
            shell: if id == 12 {
                ShellKind::Bash
            } else {
                ShellKind::Pwsh
            },
            command: None,
        })
    }

    fn node(id: PaneId) -> NodeFile {
        let Some(SavedPane::Terminal {
            env, cwd, shell, ..
        }) = saved(id)
        else {
            unreachable!()
        };
        NodeFile {
            env: Some(env),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            shell: Some(shell.as_str().into()),
            ..Default::default()
        }
    }

    fn three() -> Tree {
        Tree::Split {
            axis: Axis::Row,
            ratio: 0.6,
            first: Box::new(Tree::Leaf(10)),
            second: Box::new(Tree::Split {
                axis: Axis::Column,
                ratio: 0.5,
                first: Box::new(Tree::Leaf(11)),
                second: Box::new(Tree::Leaf(12)),
            }),
        }
    }

    #[test]
    fn an_open_tab_saves_in_the_set_tab_format() {
        let tab = tab_file(&three(), None, &saved).unwrap();
        assert_eq!(
            tab.layout,
            Some(NodeFile {
                split: Some("right".into()),
                ratio: Some(0.6),
                panes: Some(vec![
                    node(10),
                    NodeFile {
                        split: Some("down".into()),
                        ratio: Some(0.5),
                        panes: Some(vec![node(11), node(12)]),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            })
        );
        assert_eq!(tab.env, None);
    }

    #[test]
    fn a_one_pane_tab_saves_on_the_tab_itself() {
        let tab = tab_file(&Tree::Leaf(10), None, &saved).unwrap();
        assert_eq!(tab.layout, None);
        assert_eq!(tab.env.as_deref(), Some("work"));
        assert_eq!(tab.shell.as_deref(), Some("pwsh"));
        assert_eq!(tab_file(&Tree::Leaf(10), None, &|_| None), None);
    }

    #[test]
    fn a_saved_tab_reopens_as_the_same_layout() {
        let d = tempfile::tempdir().unwrap();
        let paths = hydra_core::paths::HydraPaths::new(d.path().join(".hydra"));
        let work = EnvName::parse("work").unwrap();
        std::fs::create_dir_all(paths.env_dir(&work)).unwrap();
        std::fs::write(paths.env_file(&work), "").unwrap();
        let folder = |id: PaneId| d.path().join(format!("p{id}"));
        for id in [10, 11, 12] {
            std::fs::create_dir_all(folder(id)).unwrap();
        }
        let saved_here = |id: PaneId| {
            saved(id).map(|p| match p {
                SavedPane::Terminal { env, shell, .. } => SavedPane::Terminal {
                    env,
                    cwd: folder(id),
                    shell,
                    command: None,
                },
                other => other,
            })
        };
        let tab = tab_file(&three(), None, &saved_here).unwrap();
        let bindings = std::collections::BTreeMap::new();
        let ctx = hydraui_sets::Context {
            paths: &paths,
            user_home: d.path(),
            bindings: &bindings,
            default_shell: ShellKind::Pwsh,
        };
        let spec = hydraui_sets::resolve_loose(&ctx, &tab).unwrap();
        let l = tab_layout(&spec, &mut ids());
        let (tree, panes) = (l.tree, l.terminals);
        assert_eq!(tree, three());
        let cwds: Vec<PathBuf> = panes.iter().map(|(_, r)| r.cwd.clone()).collect();
        assert_eq!(cwds, vec![folder(10), folder(11), folder(12)]);
        assert_eq!(panes[2].1.shell, ShellKind::Bash);
    }

    #[test]
    fn a_file_pane_saves_as_a_files_node_and_reopens() {
        let d = tempfile::tempdir().unwrap();
        let paths = hydra_core::paths::HydraPaths::new(d.path().join(".hydra"));
        let work = EnvName::parse("work").unwrap();
        std::fs::create_dir_all(paths.env_dir(&work)).unwrap();
        std::fs::write(paths.env_file(&work), "").unwrap();
        let bindings = std::collections::BTreeMap::new();
        let ctx = hydraui_sets::Context {
            paths: &paths,
            user_home: d.path(),
            bindings: &bindings,
            default_shell: ShellKind::Pwsh,
        };
        let a = d.path().join("a.md");
        let b = d.path().join("b.rs");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let tree = Tree::Split {
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(Tree::Leaf(1)),
            second: Box::new(Tree::Leaf(2)),
        };
        let saved = |id| match id {
            1 => Some(SavedPane::Terminal {
                env: "work".into(),
                cwd: d.path().to_path_buf(),
                shell: ShellKind::Pwsh,
                command: None,
            }),
            2 => Some(SavedPane::Files {
                files: vec![a.clone(), b.clone()],
                active: 1,
            }),
            _ => None,
        };
        let file = tab_file(&tree, None, &saved).unwrap();
        let node = file.layout.as_ref().unwrap();
        let fnode = &node.panes.as_ref().unwrap()[1];
        assert_eq!(fnode.active, Some(1));
        assert_eq!(fnode.files.as_ref().unwrap().len(), 2);
        let spec = hydraui_sets::resolve_loose(&ctx, &file).unwrap();
        let mut n = 10;
        let l = tab_layout(&spec, &mut || {
            n += 1;
            n
        });
        assert_eq!(l.terminals.len(), 1);
        let (fid, files) = l.files.unwrap();
        assert_eq!(files.files, vec![a, b]);
        assert_eq!(files.active, 1);
        assert_eq!(l.tree.panes(), vec![l.terminals[0].0, fid]);
    }

    #[test]
    fn a_tab_that_is_only_a_file_pane_isnt_saved() {
        let saved = |_| {
            Some(SavedPane::Files {
                files: vec![PathBuf::from("C:/x.md")],
                active: 0,
            })
        };
        assert_eq!(tab_file(&Tree::Leaf(1), None, &saved), None);
    }

    #[test]
    fn set_tabs_keep_their_title_and_command() {
        let pane = |id| {
            Some(SavedPane::Terminal {
                env: "work".into(),
                cwd: PathBuf::from(format!("C:/w/{id}")),
                shell: ShellKind::Pwsh,
                command: (id == 2).then(|| "npm run dev".to_string()),
            })
        };
        let single = tab_file(&Tree::Leaf(2), Some("api"), &pane).unwrap();
        assert_eq!(single.title.as_deref(), Some("api"));
        assert_eq!(single.command.as_deref(), Some("npm run dev"));
        let split = Tree::Split {
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(Tree::Leaf(1)),
            second: Box::new(Tree::Leaf(2)),
        };
        let tab = tab_file(&split, Some("web"), &pane).unwrap();
        assert_eq!(tab.title.as_deref(), Some("web"));
        let panes = tab.layout.unwrap().panes.unwrap();
        assert_eq!(panes[0].command, None);
        assert_eq!(panes[1].command.as_deref(), Some("npm run dev"));
    }
}
