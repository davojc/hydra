//! A set's tab as panes (spec §5): hydraui-sets' resolved layout becomes a split tree, and
//! each pane what it runs; and back again, to save open tabs in session.toml.
use std::path::PathBuf;

use hydra_platform::shell::ShellKind;
use hydraui_sets::file::{NodeFile, TabFile};
use hydraui_sets::{Node, SplitDir, TabSpec};

use crate::hydra::PaneRequest;
use crate::split::{Axis, PaneId, Tree};

/// The tab's tree, with `next_id` naming each pane, and each pane's request in tree order
/// (left to right, top to bottom).
pub fn tab_layout<F: FnMut() -> PaneId>(
    tab: &TabSpec,
    next_id: &mut F,
) -> (Tree, Vec<(PaneId, PaneRequest)>) {
    let mut panes = Vec::new();
    let tree = node(&tab.root, next_id, &mut panes);
    (tree, panes)
}

fn node<F: FnMut() -> PaneId>(
    n: &Node,
    next_id: &mut F,
    panes: &mut Vec<(PaneId, PaneRequest)>,
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
        Node::Files(_) => Tree::Leaf(next_id()),
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
            first: Box::new(node(first, next_id, panes)),
            second: Box::new(node(second, next_id, panes)),
        },
    }
}

/// What a pane needs to come back after a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedPane {
    pub env: String,
    pub cwd: PathBuf,
    pub shell: ShellKind,
}

/// An open tab in the set file's tab format, for session.toml: the reverse of `tab_layout`.
/// A one-pane tab is written on the tab itself. `None` when a pane can't be described.
pub fn tab_file(tree: &Tree, pane: &impl Fn(PaneId) -> Option<SavedPane>) -> Option<TabFile> {
    match tree {
        Tree::Leaf(id) => {
            let p = pane(*id)?;
            Some(TabFile {
                env: Some(p.env),
                cwd: Some(p.cwd.to_string_lossy().into_owned()),
                shell: Some(p.shell.as_str().into()),
                ..Default::default()
            })
        }
        split => Some(TabFile {
            layout: Some(node_file(split, pane)?),
            ..Default::default()
        }),
    }
}

fn node_file(tree: &Tree, pane: &impl Fn(PaneId) -> Option<SavedPane>) -> Option<NodeFile> {
    match tree {
        Tree::Leaf(id) => {
            let p = pane(*id)?;
            Some(NodeFile {
                env: Some(p.env),
                cwd: Some(p.cwd.to_string_lossy().into_owned()),
                shell: Some(p.shell.as_str().into()),
                ..Default::default()
            })
        }
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
        let (tree, panes) = tab_layout(&tab, &mut ids());
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
        let (tree, panes) = tab_layout(&tab, &mut ids());
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
        Some(SavedPane {
            env: "work".into(),
            cwd: PathBuf::from(format!("C:/w/{id}")),
            shell: if id == 12 {
                ShellKind::Bash
            } else {
                ShellKind::Pwsh
            },
        })
    }

    fn node(id: PaneId) -> NodeFile {
        let p = saved(id).unwrap();
        NodeFile {
            env: Some(p.env),
            cwd: Some(p.cwd.to_string_lossy().into_owned()),
            shell: Some(p.shell.as_str().into()),
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
        let tab = tab_file(&three(), &saved).unwrap();
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
        let tab = tab_file(&Tree::Leaf(10), &saved).unwrap();
        assert_eq!(tab.layout, None);
        assert_eq!(tab.env.as_deref(), Some("work"));
        assert_eq!(tab.shell.as_deref(), Some("pwsh"));
        assert_eq!(tab_file(&Tree::Leaf(10), &|_| None), None);
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
            saved(id).map(|p| SavedPane {
                cwd: folder(id),
                ..p
            })
        };
        let tab = tab_file(&three(), &saved_here).unwrap();
        let bindings = std::collections::BTreeMap::new();
        let ctx = hydraui_sets::Context {
            paths: &paths,
            user_home: d.path(),
            bindings: &bindings,
            default_shell: ShellKind::Pwsh,
        };
        let spec = hydraui_sets::resolve_loose(&ctx, &tab).unwrap();
        let (tree, panes) = tab_layout(&spec, &mut ids());
        assert_eq!(tree, three());
        let cwds: Vec<PathBuf> = panes.iter().map(|(_, r)| r.cwd.clone()).collect();
        assert_eq!(cwds, vec![folder(10), folder(11), folder(12)]);
        assert_eq!(panes[2].1.shell, ShellKind::Bash);
    }
}
