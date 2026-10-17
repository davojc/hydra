//! A set after resolution: every pane has an environment, an existing folder and a shell.
use std::path::PathBuf;

use hydra_core::name::EnvName;
use hydra_platform::shell::ShellKind;

#[derive(Debug, Clone, PartialEq)]
pub struct SetSpec {
    pub name: String,
    pub file: PathBuf,
    pub tabs: Vec<TabSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabSpec {
    pub title: String,
    pub root: Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Side by side; `ratio` is the left pane's share.
    Right,
    /// Stacked; `ratio` is the top pane's share.
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
    Pane(PaneSpec),
}

impl Node {
    /// Panes left to right / top to bottom (depth first).
    pub fn panes(&self) -> Vec<&PaneSpec> {
        match self {
            Node::Pane(p) => vec![p],
            Node::Split { first, second, .. } => {
                let mut v = first.panes();
                v.extend(second.panes());
                v
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneSpec {
    pub env: EnvName,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    /// Shell text run after the shell starts.
    pub command: Option<String>,
    /// The folder's bound environment, when an explicit `env` differs from it.
    pub bound_elsewhere: Option<String>,
}
