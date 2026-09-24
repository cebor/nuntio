//! Split layout of a tab: a binary tree of splits with panes as leaves.

use crate::event::PaneId;

/// Orientation of the line dividing a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, divided by a vertical line.
    Vertical,
    /// Stacked, divided by a horizontal line.
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Vertical,
            Direction::Up | Direction::Down => Axis::Horizontal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Split into two parts along `axis`, leaving a `gap` between them.
    fn split(self, axis: Axis, ratio: f32, gap: f32) -> (Rect, Rect) {
        match axis {
            Axis::Vertical => {
                let first = ((self.width - gap) * ratio).round();
                (
                    Rect {
                        width: first,
                        ..self
                    },
                    Rect {
                        x: self.x + first + gap,
                        width: self.width - first - gap,
                        ..self
                    },
                )
            }
            Axis::Horizontal => {
                let first = ((self.height - gap) * ratio).round();
                (
                    Rect {
                        height: first,
                        ..self
                    },
                    Rect {
                        y: self.y + first + gap,
                        height: self.height - first - gap,
                        ..self
                    },
                )
            }
        }
    }
}

/// Smallest share a pane can be resized to.
const MIN_RATIO: f32 = 0.05;

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// Share of the first child, 0..1.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    fn first_leaf(&self) -> PaneId {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { first, .. } => first.first_leaf(),
        }
    }
}

/// A divider between two parts of a split, for drawing and dragging.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    pub rect: Rect,
    pub axis: Axis,
    /// Area of the whole split, to turn a drag position into a ratio.
    split_area: Rect,
    /// Route from the root to the split: `false` = first child.
    path: Vec<bool>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Divider>,
}

impl Layout {
    pub fn rect(&self, id: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == id).map(|(_, r)| *r)
    }

    pub fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(id, _)| *id)
    }

    /// Divider under a point, with `slop` pixels of extra grab area.
    pub fn divider_at(&self, x: f32, y: f32, slop: f32) -> Option<&Divider> {
        self.dividers.iter().find(|d| {
            let r = d.rect;
            let grab = Rect {
                x: r.x - slop,
                y: r.y - slop,
                width: r.width + 2.0 * slop,
                height: r.height + 2.0 * slop,
            };
            grab.contains(x, y)
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneTree {
    root: Node,
    zoomed: Option<PaneId>,
}

impl PaneTree {
    pub fn new(id: PaneId) -> Self {
        Self {
            root: Node::Leaf(id),
            zoomed: None,
        }
    }

    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.root.leaves(&mut out);
        out
    }

    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    /// Show `id` alone, or return to the split layout.
    pub fn toggle_zoom(&mut self, id: PaneId) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None if self.len() > 1 && self.root.contains(id) => Some(id),
            None => None,
        };
    }

    /// Split `target`, putting `new` right of or below it.
    pub fn split(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        fn walk(node: &mut Node, target: PaneId, new: PaneId, axis: Axis) -> bool {
            match node {
                Node::Leaf(id) if *id == target => {
                    *node = Node::Split {
                        axis,
                        ratio: 0.5,
                        first: Box::new(Node::Leaf(target)),
                        second: Box::new(Node::Leaf(new)),
                    };
                    true
                }
                Node::Leaf(_) => false,
                Node::Split { first, second, .. } => {
                    walk(first, target, new, axis) || walk(second, target, new, axis)
                }
            }
        }
        self.zoomed = None;
        walk(&mut self.root, target, new, axis)
    }

    /// Remove a pane; its sibling takes the space. Returns the pane that
    /// should get focus next, or `None` if `id` was the last pane (the tree
    /// is left unchanged then) or isn't in the tree.
    pub fn remove(&mut self, id: PaneId) -> Option<PaneId> {
        fn walk(node: &mut Node, id: PaneId) -> Option<PaneId> {
            let Node::Split { first, second, .. } = node else {
                return None;
            };
            let sibling = if matches!(**first, Node::Leaf(l) if l == id) {
                Some(std::mem::replace(&mut **second, Node::Leaf(id)))
            } else if matches!(**second, Node::Leaf(l) if l == id) {
                Some(std::mem::replace(&mut **first, Node::Leaf(id)))
            } else {
                None
            };
            if let Some(sibling) = sibling {
                let next = sibling.first_leaf();
                *node = sibling;
                return Some(next);
            }
            walk(first, id).or_else(|| walk(second, id))
        }
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
        walk(&mut self.root, id)
    }

    pub fn layout(&self, area: Rect, gap: f32) -> Layout {
        fn walk(node: &Node, area: Rect, gap: f32, path: &mut Vec<bool>, out: &mut Layout) {
            match node {
                Node::Leaf(id) => out.panes.push((*id, area)),
                Node::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    let (a, b) = area.split(*axis, *ratio, gap);
                    let rect = match axis {
                        Axis::Vertical => Rect {
                            x: a.x + a.width,
                            width: gap,
                            ..area
                        },
                        Axis::Horizontal => Rect {
                            y: a.y + a.height,
                            height: gap,
                            ..area
                        },
                    };
                    out.dividers.push(Divider {
                        rect,
                        axis: *axis,
                        split_area: area,
                        path: path.clone(),
                    });
                    path.push(false);
                    walk(first, a, gap, path, out);
                    path.pop();
                    path.push(true);
                    walk(second, b, gap, path, out);
                    path.pop();
                }
            }
        }
        let mut layout = Layout::default();
        match self.zoomed {
            Some(id) => layout.panes.push((id, area)),
            None => walk(&self.root, area, gap, &mut Vec::new(), &mut layout),
        }
        layout
    }

    /// Move a divider so it sits at pixel position `pos` (x for vertical,
    /// y for horizontal dividers).
    pub fn drag_divider(&mut self, divider: &Divider, pos: f32) {
        // Inverse of `Rect::split`: the gap isn't part of either child.
        let (area, gap) = (divider.split_area, divider.rect);
        let ratio = match divider.axis {
            Axis::Vertical => (pos - area.x) / (area.width - gap.width),
            Axis::Horizontal => (pos - area.y) / (area.height - gap.height),
        };
        let mut node = &mut self.root;
        for &second in &divider.path {
            let Node::Split {
                first, second: s, ..
            } = node
            else {
                return;
            };
            node = if second { s } else { first };
        }
        if let Node::Split { ratio: r, .. } = node {
            *r = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        }
    }

    /// Grow or shrink `id` towards `direction` by `pixels`, moving the
    /// nearest divider on that side.
    pub fn resize(&mut self, id: PaneId, direction: Direction, pixels: f32, layout: &Layout) {
        let Some(pane) = layout.rect(id) else {
            return;
        };
        // The divider on that side of the pane that spans the pane.
        let divider = layout.dividers.iter().find(|d| {
            d.axis == direction.axis()
                && match direction {
                    Direction::Left => (d.rect.x + d.rect.width - pane.x).abs() < 1.0,
                    Direction::Right => (d.rect.x - (pane.x + pane.width)).abs() < 1.0,
                    Direction::Up => (d.rect.y + d.rect.height - pane.y).abs() < 1.0,
                    Direction::Down => (d.rect.y - (pane.y + pane.height)).abs() < 1.0,
                }
                && overlaps(d.rect, pane, direction.axis())
        });
        let Some(divider) = divider.cloned() else {
            return;
        };
        let pos = match direction {
            Direction::Left => divider.rect.x - pixels,
            Direction::Right => divider.rect.x + pixels,
            Direction::Up => divider.rect.y - pixels,
            Direction::Down => divider.rect.y + pixels,
        };
        self.drag_divider(&divider, pos);
    }

    /// The pane next to `id` in `direction`, preferring the one that
    /// overlaps it the most.
    pub fn neighbor(id: PaneId, direction: Direction, layout: &Layout) -> Option<PaneId> {
        let from = layout.rect(id)?;
        let adjacent = |r: &Rect| match direction {
            Direction::Left => r.x + r.width <= from.x,
            Direction::Right => r.x >= from.x + from.width,
            Direction::Up => r.y + r.height <= from.y,
            Direction::Down => r.y >= from.y + from.height,
        };
        let distance = |r: &Rect| match direction {
            Direction::Left => from.x - (r.x + r.width),
            Direction::Right => r.x - (from.x + from.width),
            Direction::Up => from.y - (r.y + r.height),
            Direction::Down => r.y - (from.y + from.height),
        };
        let overlap = |r: &Rect| match direction.axis() {
            Axis::Vertical => span_overlap(r.y, r.height, from.y, from.height),
            Axis::Horizontal => span_overlap(r.x, r.width, from.x, from.width),
        };
        layout
            .panes
            .iter()
            .filter(|(p, r)| *p != id && adjacent(r) && overlap(r) > 0.0)
            .min_by(|(_, a), (_, b)| {
                distance(a)
                    .total_cmp(&distance(b))
                    .then(overlap(b).total_cmp(&overlap(a)))
            })
            .map(|(p, _)| *p)
    }
}

fn span_overlap(a: f32, a_len: f32, b: f32, b_len: f32) -> f32 {
    ((a + a_len).min(b + b_len) - a.max(b)).max(0.0)
}

/// A divider spans a pane if they overlap along the divider's length.
fn overlaps(divider: Rect, pane: Rect, axis: Axis) -> bool {
    match axis {
        Axis::Vertical => span_overlap(divider.y, divider.height, pane.y, pane.height) > 0.0,
        Axis::Horizontal => span_overlap(divider.x, divider.width, pane.x, pane.width) > 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 201.0,
        height: 101.0,
    };

    fn p(n: u64) -> PaneId {
        PaneId(n)
    }

    /// ┌───┬───┐
    /// │ 0 │ 1 │
    /// │   ├───┤
    /// │   │ 2 │
    /// └───┴───┘
    fn three() -> PaneTree {
        let mut t = PaneTree::new(p(0));
        t.split(p(0), p(1), Axis::Vertical);
        t.split(p(1), p(2), Axis::Horizontal);
        t
    }

    #[test]
    fn layout_splits_with_gaps() {
        let t = three();
        let l = t.layout(AREA, 1.0);
        assert_eq!(
            l.rect(p(0)),
            Some(Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 101.0
            })
        );
        assert_eq!(
            l.rect(p(1)),
            Some(Rect {
                x: 101.0,
                y: 0.0,
                width: 100.0,
                height: 50.0
            })
        );
        assert_eq!(
            l.rect(p(2)),
            Some(Rect {
                x: 101.0,
                y: 51.0,
                width: 100.0,
                height: 50.0
            })
        );
        assert_eq!(l.dividers.len(), 2);
        assert_eq!(
            l.dividers[0].rect,
            Rect {
                x: 100.0,
                y: 0.0,
                width: 1.0,
                height: 101.0
            }
        );
        assert_eq!(t.leaves(), [p(0), p(1), p(2)]);
    }

    #[test]
    fn remove_collapses_and_picks_the_sibling() {
        let mut t = three();
        assert_eq!(t.remove(p(1)), Some(p(2)));
        assert_eq!(t.leaves(), [p(0), p(2)]);
        let l = t.layout(AREA, 1.0);
        assert_eq!(
            l.rect(p(2)).unwrap().height,
            101.0,
            "sibling takes the space"
        );

        assert_eq!(t.remove(p(0)), Some(p(2)));
        assert_eq!(t.leaves(), [p(2)]);
        assert_eq!(t.remove(p(2)), None, "the last pane stays");
        assert_eq!(t.remove(p(9)), None);
    }

    #[test]
    fn neighbors() {
        let t = three();
        let l = t.layout(AREA, 1.0);
        assert_eq!(PaneTree::neighbor(p(0), Direction::Right, &l), Some(p(1)));
        assert_eq!(PaneTree::neighbor(p(2), Direction::Left, &l), Some(p(0)));
        assert_eq!(PaneTree::neighbor(p(1), Direction::Down, &l), Some(p(2)));
        assert_eq!(PaneTree::neighbor(p(2), Direction::Up, &l), Some(p(1)));
        assert_eq!(PaneTree::neighbor(p(0), Direction::Left, &l), None);
        assert_eq!(PaneTree::neighbor(p(1), Direction::Right, &l), None);
    }

    #[test]
    fn resize_moves_the_adjacent_divider() {
        let mut t = three();
        let l = t.layout(AREA, 1.0);
        t.resize(p(0), Direction::Right, 20.0, &l);
        let l = t.layout(AREA, 1.0);
        assert_eq!(l.rect(p(0)).unwrap().width, 120.0);

        // Pane 2 grows up: the horizontal divider moves up.
        t.resize(p(2), Direction::Up, 10.0, &l);
        let l = t.layout(AREA, 1.0);
        assert_eq!(l.rect(p(1)).unwrap().height, 40.0);

        // No divider on the outer edge: nothing happens.
        let before = t.clone();
        t.resize(p(0), Direction::Left, 10.0, &l);
        assert_eq!(t, before);
    }

    #[test]
    fn dragging_is_clamped() {
        let mut t = three();
        let l = t.layout(AREA, 1.0);
        let divider = l.dividers[0].clone();
        t.drag_divider(&divider, -50.0);
        let l = t.layout(AREA, 1.0);
        assert_eq!(l.rect(p(0)).unwrap().width, 10.0, "5% minimum");
        assert!(l.divider_at(10.5, 50.0, 3.0).is_some());
    }

    #[test]
    fn zoom_shows_one_pane() {
        let mut t = three();
        t.toggle_zoom(p(1));
        let l = t.layout(AREA, 1.0);
        assert_eq!(l.panes, [(p(1), AREA)]);
        assert!(l.dividers.is_empty());
        t.toggle_zoom(p(1));
        assert_eq!(t.layout(AREA, 1.0).panes.len(), 3);

        // Removing the zoomed pane ends zoom.
        t.toggle_zoom(p(2));
        t.remove(p(2));
        assert_eq!(t.layout(AREA, 1.0).panes.len(), 2);

        let mut single = PaneTree::new(p(0));
        single.toggle_zoom(p(0));
        single.split(p(0), p(1), Axis::Vertical);
        assert_eq!(
            single.layout(AREA, 1.0).panes.len(),
            2,
            "nothing to zoom with one pane"
        );
    }
}
