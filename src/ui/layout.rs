//! Layout primitives: a recursive split tree placed into terminal rects.
//!
//! A *view* is a tree of nested splits referencing panels by id. Each [`Node`]
//! is either a leaf (a single panel) or a split arranging its children in a row
//! (side by side, splitting width) or a column (stacked, splitting height),
//! sized proportional to per-child weights.
//!
//! [`compute_layout`] prunes panels that aren't available (e.g. the requests
//! panel without a log source), reflows the remaining tree, and returns the
//! screen [`Rect`] for each visible panel.

use std::collections::{HashMap, HashSet};

pub const MIN_COLS: i32 = 62;
pub const MIN_LINES: i32 = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub y: i32,
    pub x: i32,
    pub h: i32,
    pub w: i32,
}

impl Rect {
    pub fn new(y: i32, x: i32, h: i32, w: i32) -> Self {
        Self { y, x, h, w }
    }
}

/// A node in a view's layout tree.
///
/// A leaf carries `panel` (its panel id) and no children. A split carries
/// `children` and `vertical` (true = stack into a column, splitting height;
/// false = lay out side by side in a row, splitting width). `weight` sizes this
/// node against its siblings.
#[derive(Clone, Debug)]
pub struct Node {
    pub weight: i32,
    pub panel: Option<&'static str>,
    pub vertical: bool,
    pub children: Vec<Node>,
}

pub fn leaf(panel: &'static str, weight: i32) -> Node {
    Node {
        weight,
        panel: Some(panel),
        vertical: false,
        children: Vec::new(),
    }
}

/// Arrange children side by side (split the available width).
pub fn row(children: Vec<Node>, weight: i32) -> Node {
    Node {
        weight,
        panel: None,
        vertical: false,
        children,
    }
}

/// Stack children top to bottom (split the available height).
pub fn col(children: Vec<Node>, weight: i32) -> Node {
    Node {
        weight,
        panel: None,
        vertical: true,
        children,
    }
}

#[derive(Default)]
pub struct Layout {
    pub panels: HashMap<&'static str, Rect>,
    pub too_small: bool,
}

/// Split `total` into segments proportional to `weights`.
///
/// The last segment absorbs the rounding remainder so the segments exactly tile
/// `[start, start + total)`.
fn split_weighted(start: i32, total: i32, weights: &[i32]) -> Vec<(i32, i32)> {
    let tw = weights.iter().sum::<i32>().max(1);
    let mut out = Vec::with_capacity(weights.len());
    let mut pos = start;
    let mut used = 0;
    for (i, &wt) in weights.iter().enumerate() {
        let seg = if i == weights.len() - 1 {
            total - used
        } else {
            total * wt / tw
        };
        out.push((pos, seg));
        pos += seg;
        used += seg;
    }
    out
}

/// Drop leaves whose panel isn't available and collapse empty splits.
///
/// A split with a single surviving child collapses to that child (keeping the
/// parent's weight, so the survivor inherits the parent's slot). Returns `None`
/// when nothing in the subtree is available.
pub fn prune(node: &Node, available: &HashSet<&str>) -> Option<Node> {
    if let Some(p) = node.panel {
        return if available.contains(p) {
            Some(node.clone())
        } else {
            None
        };
    }
    let kids: Vec<Node> = node
        .children
        .iter()
        .filter_map(|c| prune(c, available))
        .collect();
    if kids.is_empty() {
        return None;
    }
    if kids.len() == 1 {
        let mut k = kids.into_iter().next().unwrap();
        k.weight = node.weight;
        return Some(k);
    }
    Some(Node {
        children: kids,
        weight: node.weight,
        panel: None,
        vertical: node.vertical,
    })
}

fn place(node: &Node, rect: Rect, out: &mut HashMap<&'static str, Rect>) {
    if let Some(p) = node.panel {
        out.insert(p, rect);
        return;
    }
    let weights: Vec<i32> = node.children.iter().map(|c| c.weight).collect();
    if node.vertical {
        let segs = split_weighted(rect.y, rect.h, &weights);
        for (child, (ry, rh)) in node.children.iter().zip(segs.iter()) {
            place(child, Rect::new(*ry, rect.x, *rh, rect.w), out);
        }
    } else {
        let segs = split_weighted(rect.x, rect.w, &weights);
        for (child, (rx, rw)) in node.children.iter().zip(segs.iter()) {
            place(child, Rect::new(rect.y, *rx, rect.h, *rw), out);
        }
    }
}

/// Place `view`'s available panels into the terminal rect.
///
/// Panels start at the top of the screen (no header/footer bar). Panels not in
/// `available` are pruned and the rest reflow to fill the freed space.
pub fn compute_layout(lines: i32, cols: i32, view: &Node, available: &HashSet<&str>) -> Layout {
    if cols < MIN_COLS || lines < MIN_LINES {
        return Layout {
            panels: HashMap::new(),
            too_small: true,
        };
    }
    let Some(pruned) = prune(view, available) else {
        return Layout {
            panels: HashMap::new(),
            too_small: false,
        };
    };
    let mut out = HashMap::new();
    place(&pruned, Rect::new(0, 0, lines, cols), &mut out);
    Layout {
        panels: out,
        too_small: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ui::{registry::REGISTRY, views::VIEWS};

    fn all_panels() -> HashSet<&'static str> {
        REGISTRY.iter().map(|p| p.id).collect()
    }

    #[test]
    fn row_splits_width_col_splits_height() {
        let tree = col(
            vec![leaf("a", 1), row(vec![leaf("b", 1), leaf("c", 1)], 1)],
            1,
        );
        let avail: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let lay = compute_layout(40, 100, &tree, &avail);
        let a = lay.panels["a"];
        let b = lay.panels["b"];
        let c = lay.panels["c"];
        assert_eq!(a, Rect::new(0, 0, 20, 100));
        assert_eq!(b.y, 20);
        assert_eq!(c.y, 20);
        assert_eq!(b.h, 20);
        assert_eq!(c.h, 20);
        assert_eq!(b.x, 0);
        assert_eq!(c.x, b.w);
        assert_eq!(b.w + c.w, 100);
    }

    #[test]
    fn weights_are_proportional() {
        let tree = col(vec![leaf("a", 2), leaf("b", 3)], 1);
        let avail: HashSet<&str> = ["a", "b"].into_iter().collect();
        let lay = compute_layout(50, 80, &tree, &avail);
        assert_eq!(lay.panels["a"].h, 20);
        assert_eq!(lay.panels["b"].h, 30);
    }

    #[test]
    fn prune_drops_unavailable_and_collapses() {
        let tree = row(vec![leaf("perf", 1), leaf("requests", 1)], 1);
        let avail: HashSet<&str> = ["requests"].into_iter().collect();
        let pruned = prune(&tree, &avail).unwrap();
        assert_eq!(pruned.panel, Some("requests"));
    }

    #[test]
    fn unavailable_panel_reflows_to_fill() {
        let mut avail = all_panels();
        avail.remove("perf");
        let lay = compute_layout(40, 120, &VIEWS[0].root, &avail);
        assert!(!lay.panels.contains_key("perf"));
        let ys = lay.panels.values().map(|r| r.y).min().unwrap();
        assert_eq!(ys, 0);
    }

    #[test]
    fn too_small_flags() {
        let lay = compute_layout(MIN_LINES - 1, MIN_COLS - 1, &VIEWS[0].root, &all_panels());
        assert!(lay.too_small);
        assert!(lay.panels.is_empty());
    }

    #[test]
    fn views_only_reference_registered_panels() {
        fn panels_in(node: &Node) -> HashSet<&'static str> {
            match node.panel {
                Some(p) => {
                    let mut s = HashSet::new();
                    s.insert(p);
                    s
                }
                None => node.children.iter().flat_map(panels_in).collect(),
            }
        }
        let all = all_panels();
        for view in VIEWS.iter() {
            assert!(panels_in(&view.root).is_subset(&all), "{}", view.name);
        }
    }
}
