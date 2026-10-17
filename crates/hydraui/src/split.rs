//! A tab's panes as a binary split tree (spec §7.1), with the geometry for moving focus.
pub type PaneId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side (split right); `ratio` is the left pane's share.
    Row,
    /// Stacked (split down); `ratio` is the top pane's share.
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tree {
    Leaf(PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Tree>,
        second: Box<Tree>,
    },
}

/// A divider can't be dragged closer than this to either edge.
pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;

impl Tree {
    /// Panes left to right, top to bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Tree::Leaf(id) => out.push(*id),
            Tree::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Tree::Leaf(x) => *x == id,
            Tree::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Splits `target` in half: it keeps the first half and `new` takes the second.
    pub fn split(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        if *self == Tree::Leaf(target) {
            *self = Tree::Split {
                axis,
                ratio: 0.5,
                first: Box::new(Tree::Leaf(target)),
                second: Box::new(Tree::Leaf(new)),
            };
            return true;
        }
        match self {
            Tree::Leaf(_) => false,
            Tree::Split { first, second, .. } => {
                first.split(target, axis, new) || second.split(target, axis, new)
            }
        }
    }

    /// Removes a pane; its sibling takes the parent split's place. Returns the pane to focus
    /// next (the sibling's pane nearest the removed one), or `None` when `target` isn't here
    /// or is the whole tree.
    pub fn remove(&mut self, target: PaneId) -> Option<PaneId> {
        let Tree::Split { first, second, .. } = self else {
            return None;
        };
        let (keep, focus_first) = if **first == Tree::Leaf(target) {
            (std::mem::replace(&mut **second, Tree::Leaf(target)), true)
        } else if **second == Tree::Leaf(target) {
            (std::mem::replace(&mut **first, Tree::Leaf(target)), false)
        } else {
            return first.remove(target).or_else(|| second.remove(target));
        };
        *self = keep;
        let panes = self.panes();
        Some(if focus_first {
            panes[0]
        } else {
            panes[panes.len() - 1]
        })
    }

    /// Each pane's rectangle within `area`.
    pub fn layout(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.layout_into(area, &mut out);
        out
    }

    fn layout_into(&self, a: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Tree::Leaf(id) => out.push((*id, a)),
            Tree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (one, two) = match axis {
                    Axis::Row => (
                        Rect {
                            w: a.w * ratio,
                            ..a
                        },
                        Rect {
                            x: a.x + a.w * ratio,
                            w: a.w * (1.0 - ratio),
                            ..a
                        },
                    ),
                    Axis::Column => (
                        Rect {
                            h: a.h * ratio,
                            ..a
                        },
                        Rect {
                            y: a.y + a.h * ratio,
                            h: a.h * (1.0 - ratio),
                            ..a
                        },
                    ),
                };
                first.layout_into(one, out);
                second.layout_into(two, out);
            }
        }
    }

    /// The pane next to `from` in direction `dir`: the nearest one that shares an edge
    /// stretch; on a tie, the one sharing more of it, then the first.
    pub fn neighbor(&self, from: PaneId, dir: Dir) -> Option<PaneId> {
        const EPS: f32 = 1e-4;
        let rects = self.layout(Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        });
        let (_, f) = *rects.iter().find(|(id, _)| *id == from)?;
        let overlap = |a: f32, al: f32, b: f32, bl: f32| (a + al).min(b + bl) - a.max(b);
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, r)| {
                let (gap, shared) = match dir {
                    Dir::Right => (r.x - (f.x + f.w), overlap(f.y, f.h, r.y, r.h)),
                    Dir::Left => (f.x - (r.x + r.w), overlap(f.y, f.h, r.y, r.h)),
                    Dir::Down => (r.y - (f.y + f.h), overlap(f.x, f.w, r.x, r.w)),
                    Dir::Up => (f.y - (r.y + r.h), overlap(f.x, f.w, r.x, r.w)),
                };
                (gap > -EPS && shared > EPS).then_some((gap, shared, *id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0).then(b.1.total_cmp(&a.1)))
            .map(|(_, _, id)| id)
    }

    /// Sets the ratio of the split at `path` (0 = first child, 1 = second, from the root),
    /// clamped to `MIN_RATIO..=MAX_RATIO`.
    pub fn set_ratio(&mut self, path: &[usize], ratio: f32) -> bool {
        if !ratio.is_finite() {
            return false;
        }
        match (self, path.split_first()) {
            (Tree::Split { ratio: r, .. }, None) => {
                *r = ratio.clamp(MIN_RATIO, MAX_RATIO);
                true
            }
            (Tree::Split { first, .. }, Some((0, rest))) => first.set_ratio(rest, ratio),
            (Tree::Split { second, .. }, Some((1, rest))) => second.set_ratio(rest, ratio),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 | (2 / 3)
    fn three() -> Tree {
        let mut t = Tree::Leaf(1);
        assert!(t.split(1, Axis::Row, 2));
        assert!(t.split(2, Axis::Column, 3));
        t
    }

    fn leaf(id: PaneId) -> Box<Tree> {
        Box::new(Tree::Leaf(id))
    }

    #[test]
    fn split_builds_the_tree() {
        assert_eq!(
            three(),
            Tree::Split {
                axis: Axis::Row,
                ratio: 0.5,
                first: leaf(1),
                second: Box::new(Tree::Split {
                    axis: Axis::Column,
                    ratio: 0.5,
                    first: leaf(2),
                    second: leaf(3),
                }),
            }
        );
        assert_eq!(three().panes(), vec![1, 2, 3]);
        assert!(three().contains(3) && !three().contains(9));
        assert!(!Tree::Leaf(1).split(9, Axis::Row, 2));
    }

    #[test]
    fn layout_divides_by_ratio() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        assert_eq!(
            three().layout(r(0.0, 0.0, 100.0, 50.0)),
            vec![
                (1, r(0.0, 0.0, 50.0, 50.0)),
                (2, r(50.0, 0.0, 50.0, 25.0)),
                (3, r(50.0, 25.0, 50.0, 25.0)),
            ]
        );
    }

    #[test]
    fn neighbours_by_geometry() {
        let t = three();
        assert_eq!(t.neighbor(1, Dir::Right), Some(2));
        assert_eq!(t.neighbor(3, Dir::Left), Some(1));
        assert_eq!(t.neighbor(3, Dir::Up), Some(2));
        assert_eq!(t.neighbor(2, Dir::Down), Some(3));
        assert_eq!(t.neighbor(1, Dir::Left), None);
        assert_eq!(t.neighbor(2, Dir::Right), None);
        assert_eq!(t.neighbor(9, Dir::Right), None);
    }

    #[test]
    fn removing_a_pane_promotes_its_sibling() {
        let mut t = three();
        assert_eq!(t.remove(2), Some(3));
        assert_eq!(
            t,
            Tree::Split {
                axis: Axis::Row,
                ratio: 0.5,
                first: leaf(1),
                second: leaf(3)
            }
        );
        assert_eq!(t.remove(1), Some(3));
        assert_eq!(t, Tree::Leaf(3));
        assert_eq!(t.remove(3), None);
        assert_eq!(t.remove(9), None);
    }

    #[test]
    fn ratios_are_set_by_path_and_clamped() {
        let mut t = three();
        assert!(t.set_ratio(&[], 0.7));
        assert!(t.set_ratio(&[1], 0.02));
        assert!(!t.set_ratio(&[0], 0.5));
        assert!(!t.set_ratio(&[], f32::NAN));
        let Tree::Split { ratio, second, .. } = &t else {
            panic!()
        };
        assert_eq!(*ratio, 0.7);
        let Tree::Split { ratio, .. } = second.as_ref() else {
            panic!()
        };
        assert_eq!(*ratio, MIN_RATIO);
    }
}
