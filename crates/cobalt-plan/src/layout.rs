//! Tidy layered tree layout for the plan canvas.
//!
//! SSMS/ADS convention: the root operator sits at the LEFT (`x = 0`), children are placed
//! one column to the right (`x = depth * (node_w + h_gap)`), siblings stack vertically in
//! child order, and every subtree occupies contiguous vertical space. Leaves take the next
//! free `y`; a parent is centered on its first-to-last child span. The result is
//! deterministic and overlap-free by construction.

use crate::model::Statement;
use serde::{Deserialize, Serialize};

/// Node size and spacing, in canvas units (the UI scales by zoom).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayoutOptions {
    pub node_w: f32,
    pub node_h: f32,
    /// Horizontal gap between columns (edge length).
    pub h_gap: f32,
    /// Vertical gap between stacked nodes.
    pub v_gap: f32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self { node_w: 150.0, node_h: 72.0, h_gap: 60.0, v_gap: 20.0 }
    }
}

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }
    /// True if the two rectangles share interior area (touching edges do not count).
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

/// A parent → child edge. Drawn from the parent's right edge to the child's left edge;
/// `rows` drives line thickness.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    /// Rows flowing along the edge: actual rows of the child when present, else estimated.
    pub rows: f64,
    pub est_rows: f64,
    pub actual_rows: Option<f64>,
    /// Start point (parent's right-center).
    pub from_pt: (f32, f32),
    /// End point (child's left-center).
    pub to_pt: (f32, f32),
}

/// Result of [`layout`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// `(node index, rect)` for every node reachable from the root, in pre-order.
    pub rects: Vec<(usize, Rect)>,
    pub edges: Vec<Edge>,
    /// Total canvas extent `(width, height)`.
    pub size: (f32, f32),
}

impl Layout {
    /// Rect of a node by index.
    pub fn rect_of(&self, node: usize) -> Option<Rect> {
        self.rects.iter().find(|(n, _)| *n == node).map(|(_, r)| *r)
    }

    /// The node under a canvas point, if any.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        self.rects.iter().find(|(_, r)| r.contains(x, y)).map(|(n, _)| *n)
    }
}

/// Lay out a statement's operator tree. A statement without a plan yields an empty layout.
pub fn layout(stmt: &Statement, opts: &LayoutOptions) -> Layout {
    let Some(root) = stmt.root else {
        return Layout::default();
    };
    if root >= stmt.nodes.len() {
        return Layout::default();
    }
    let mut ys: Vec<Option<f32>> = vec![None; stmt.nodes.len()];
    let mut next_free = 0.0f32;
    let mut max_depth = 0usize;
    // Iterative post-order to avoid recursion on pathological depth.
    let mut order: Vec<(usize, usize)> = Vec::new(); // (node, depth)
    let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
    let mut visited = vec![false; stmt.nodes.len()];
    while let Some((n, d)) = stack.pop() {
        if visited[n] {
            continue;
        }
        visited[n] = true;
        order.push((n, d));
        max_depth = max_depth.max(d);
        for &c in stmt.nodes[n].children.iter().rev() {
            if c < stmt.nodes.len() && !visited[c] {
                stack.push((c, d + 1));
            }
        }
    }
    // `order` is pre-order; process in reverse so children are placed before parents.
    let mut depth_of = vec![0usize; stmt.nodes.len()];
    for &(n, d) in &order {
        depth_of[n] = d;
    }
    for &(n, _) in order.iter().rev() {
        let kids: Vec<usize> = stmt.nodes[n]
            .children
            .iter()
            .copied()
            .filter(|&c| c < stmt.nodes.len() && ys[c].is_some())
            .collect();
        let y = if kids.is_empty() {
            let y = next_free;
            next_free += opts.node_h + opts.v_gap;
            y
        } else {
            let first = ys[kids[0]].unwrap_or(0.0);
            let last = ys[*kids.last().unwrap_or(&kids[0])].unwrap_or(first);
            (first + last) / 2.0
        };
        ys[n] = Some(y);
    }

    let mut rects = Vec::with_capacity(order.len());
    for &(n, d) in &order {
        let r = Rect {
            x: d as f32 * (opts.node_w + opts.h_gap),
            y: ys[n].unwrap_or(0.0),
            w: opts.node_w,
            h: opts.node_h,
        };
        rects.push((n, r));
    }
    let rect_of = |n: usize| -> Rect {
        Rect {
            x: depth_of[n] as f32 * (opts.node_w + opts.h_gap),
            y: ys[n].unwrap_or(0.0),
            w: opts.node_w,
            h: opts.node_h,
        }
    };
    let mut edges = Vec::new();
    for &(n, _) in &order {
        let pr = rect_of(n);
        for &c in &stmt.nodes[n].children {
            if c >= stmt.nodes.len() || ys[c].is_none() {
                continue;
            }
            let cr = rect_of(c);
            let child = &stmt.nodes[c];
            let est = child.est_rows_all_executions.unwrap_or(child.est_rows);
            let actual = child.actual.as_ref().map(|a| a.rows as f64);
            edges.push(Edge {
                from: n,
                to: c,
                rows: actual.unwrap_or(est),
                est_rows: est,
                actual_rows: actual,
                from_pt: (pr.right(), pr.center().1),
                to_pt: (cr.x, cr.center().1),
            });
        }
    }
    let width = (max_depth as f32 + 1.0) * opts.node_w + max_depth as f32 * opts.h_gap;
    let height = (next_free - opts.v_gap).max(opts.node_h);
    Layout { rects, edges, size: (width, height) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Node;

    fn chain(n: usize) -> Statement {
        let mut s = Statement::default();
        for i in 0..n {
            s.nodes.push(Node {
                index: i,
                parent: if i == 0 { None } else { Some(i - 1) },
                children: if i + 1 < n { vec![i + 1] } else { vec![] },
                est_rows: 10.0,
                ..Default::default()
            });
        }
        s.root = Some(0);
        s
    }

    #[test]
    fn empty_statement_gives_empty_layout() {
        let l = layout(&Statement::default(), &LayoutOptions::default());
        assert!(l.rects.is_empty() && l.edges.is_empty());
        assert_eq!(l.size, (0.0, 0.0));
    }

    #[test]
    fn chain_goes_left_to_right_on_one_row() {
        let o = LayoutOptions { node_w: 100.0, node_h: 50.0, h_gap: 20.0, v_gap: 10.0 };
        let l = layout(&chain(3), &o);
        assert_eq!(l.rects.len(), 3);
        assert_eq!(l.rect_of(0).unwrap().x, 0.0);
        assert_eq!(l.rect_of(1).unwrap().x, 120.0);
        assert_eq!(l.rect_of(2).unwrap().x, 240.0);
        assert!(l.rects.iter().all(|(_, r)| r.y == 0.0));
        assert_eq!(l.size, (340.0, 50.0));
        assert_eq!(l.edges.len(), 2);
        assert_eq!(l.edges[0].from_pt, (100.0, 25.0));
        assert_eq!(l.edges[0].to_pt, (120.0, 25.0));
        assert_eq!(l.edges[0].rows, 10.0);
    }

    #[test]
    fn parent_is_centered_on_children_and_siblings_stack() {
        // root -> [a, b]; a -> [a1, a2]
        let mut s = Statement::default();
        let mk = |i, p, c: Vec<usize>| Node { index: i, parent: p, children: c, ..Default::default() };
        s.nodes.push(mk(0, None, vec![1, 2]));
        s.nodes.push(mk(1, Some(0), vec![3, 4]));
        s.nodes.push(mk(2, Some(0), vec![]));
        s.nodes.push(mk(3, Some(1), vec![]));
        s.nodes.push(mk(4, Some(1), vec![]));
        s.root = Some(0);
        let o = LayoutOptions { node_w: 100.0, node_h: 50.0, h_gap: 20.0, v_gap: 10.0 };
        let l = layout(&s, &o);
        let r = |i| l.rect_of(i).unwrap();
        assert_eq!(r(3).y, 0.0);
        assert_eq!(r(4).y, 60.0);
        assert_eq!(r(1).y, 30.0);
        assert_eq!(r(2).y, 120.0);
        assert_eq!(r(0).y, 75.0);
        assert_eq!(l.size.1, 170.0);
        for (i, (_, a)) in l.rects.iter().enumerate() {
            for (_, b) in &l.rects[i + 1..] {
                assert!(!a.intersects(b));
            }
        }
        assert_eq!(l.hit_test(5.0, 80.0), Some(0));
        assert_eq!(l.hit_test(500.0, 500.0), None);
    }

    #[test]
    fn rect_helpers() {
        let a = Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let b = Rect { x: 10.0, y: 0.0, w: 10.0, h: 10.0 };
        let c = Rect { x: 5.0, y: 5.0, w: 10.0, h: 10.0 };
        assert!(!a.intersects(&b), "touching edges are not an overlap");
        assert!(a.intersects(&c));
        assert_eq!(a.center(), (5.0, 5.0));
        assert!(a.contains(10.0, 10.0));
    }
}
