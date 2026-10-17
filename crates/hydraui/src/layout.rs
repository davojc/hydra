//! A set's tab as panes (spec §5): hydraui-sets' resolved layout becomes a split tree, and
//! each pane what it runs.
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
}
