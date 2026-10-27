//! Editor groups: the tree the editor area is split into. A group is a strip of tabs plus the
//! editor and results of its active tab; a split is a row (side by side) or a column (stacked)
//! of children, each a group or another split, with draggable ratios. The tree holds tab ids,
//! never indices: `AppState.tabs` stays the flat owner of every tab, and `AppState.active_tab`
//! is kept as the derived index of the active group's active tab (see `shell::sync_layout`).
//!
//! Invariants after every mutation (`normalize`): every id in the tree names a live group, no
//! group is empty unless it is the only one, no split has fewer than two children, nested
//! splits along one axis are flattened, ratios are positive and sum to one, `active` names a
//! live group, and `maximized` does too or is `None`.

use cobalt_core::TabId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupId(pub u32);

impl std::fmt::Display for GroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `Row`: children left to right (A | B). `Col`: top to bottom (A / B).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Row,
    Col,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    pub fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Row,
            Side::Up | Side::Down => Axis::Col,
        }
    }
    /// The new child goes after the existing one (right of it, below it).
    pub fn after(self) -> bool {
        matches!(self, Side::Right | Side::Down)
    }
    pub fn parse(s: &str) -> Option<Side> {
        match s {
            "left" => Some(Side::Left),
            "right" => Some(Side::Right),
            "up" | "top" => Some(Side::Up),
            "down" | "bottom" => Some(Side::Down),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Group(GroupId),
    Split { axis: Axis, children: Vec<Node>, ratios: Vec<f32> },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub tabs: Vec<TabId>,
    pub active: Option<TabId>,
}

/// Where a tab goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// Into a group's strip at an index (clamped; pinned tabs stay before unpinned ones).
    Strip { group: GroupId, index: usize },
    /// Into a new group split off `group` on `side`.
    Split { group: GroupId, side: Side },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    root: Node,
    groups: BTreeMap<GroupId, Group>,
    active: GroupId,
    maximized: Option<GroupId>,
    next: u32,
    /// Bumped by every mutation; the snapshot writer saves when it changed.
    #[serde(skip)]
    generation: u64,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

const MIN_RATIO: f32 = 0.08;

impl Layout {
    pub fn new() -> Self {
        let mut groups = BTreeMap::new();
        groups.insert(GroupId(1), Group::default());
        Self { root: Node::Group(GroupId(1)), groups, active: GroupId(1), maximized: None, next: 2, generation: 1 }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn bump(&mut self) {
        self.generation += 1;
    }

    pub fn root(&self) -> &Node {
        &self.root
    }
    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.get(&id)
    }
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
    pub fn active_group(&self) -> GroupId {
        self.active
    }
    pub fn maximized(&self) -> Option<GroupId> {
        self.maximized
    }
    /// The active group's active tab.
    pub fn active_tab(&self) -> Option<TabId> {
        self.groups.get(&self.active).and_then(|g| g.active)
    }
    pub fn group_of(&self, tab: TabId) -> Option<GroupId> {
        self.groups.iter().find(|(_, g)| g.tabs.contains(&tab)).map(|(id, _)| *id)
    }
    /// Groups in reading order (depth first, left to right, top to bottom).
    pub fn groups_in_order(&self) -> Vec<GroupId> {
        let mut out = Vec::new();
        fn walk(n: &Node, out: &mut Vec<GroupId>) {
            match n {
                Node::Group(g) => out.push(*g),
                Node::Split { children, .. } => children.iter().for_each(|c| walk(c, out)),
            }
        }
        walk(&self.root, &mut out);
        out
    }
    /// Every tab in reading order of groups, then strip order.
    #[cfg(test)]
    pub fn tabs_in_order(&self) -> Vec<TabId> {
        self.groups_in_order().into_iter().flat_map(|g| self.groups.get(&g).map(|g| g.tabs.clone()).unwrap_or_default()).collect()
    }

    // ----- activation -----

    /// Make `tab` the active tab of its group and that group the active one.
    pub fn set_active_tab(&mut self, tab: TabId) -> bool {
        let Some(gid) = self.group_of(tab) else { return false };
        let g = self.groups.get_mut(&gid).unwrap();
        let changed = g.active != Some(tab) || self.active != gid;
        g.active = Some(tab);
        self.active = gid;
        if changed {
            self.bump();
        }
        changed
    }

    pub fn set_active_group(&mut self, gid: GroupId) -> bool {
        if self.groups.contains_key(&gid) && self.active != gid {
            self.active = gid;
            self.bump();
            true
        } else {
            false
        }
    }

    // ----- membership -----

    /// Reconcile with the app's tab list: ids the tree does not know are added to the active
    /// group (after its active tab, or at the end when `at_end`), ids the list no longer has are
    /// dropped. Returns true when anything changed.
    pub fn sync(&mut self, tabs: &[TabId], at_end: bool) -> bool {
        let mut changed = false;
        for g in self.groups.values_mut() {
            let before = g.tabs.len();
            g.tabs.retain(|t| tabs.contains(t));
            if g.tabs.len() != before {
                changed = true;
                if g.active.map(|a| !g.tabs.contains(&a)).unwrap_or(true) {
                    g.active = g.tabs.last().copied();
                }
            }
        }
        for t in tabs {
            if self.group_of(*t).is_none() {
                let gid = if self.groups.contains_key(&self.active) { self.active } else { *self.groups.keys().next().unwrap() };
                let g = self.groups.get_mut(&gid).unwrap();
                let idx = if at_end { g.tabs.len() } else { g.active.and_then(|a| g.tabs.iter().position(|x| *x == a)).map(|i| i + 1).unwrap_or(g.tabs.len()) };
                g.tabs.insert(idx, *t);
                if g.active.is_none() {
                    g.active = Some(*t);
                }
                changed = true;
            }
        }
        if changed {
            self.normalize();
            self.bump();
        }
        changed
    }

    /// Take a tab out (it closed). Its group's active moves to the tab after it, else before.
    pub fn remove_tab(&mut self, tab: TabId) {
        let Some(gid) = self.group_of(tab) else { return };
        let g = self.groups.get_mut(&gid).unwrap();
        let pos = g.tabs.iter().position(|t| *t == tab).unwrap();
        g.tabs.remove(pos);
        if g.active == Some(tab) {
            g.active = g.tabs.get(pos).or_else(|| g.tabs.get(pos.wrapping_sub(1))).or_else(|| g.tabs.last()).copied();
        }
        self.normalize();
        self.bump();
    }

    /// Move a tab within or between strips, or into a new group split off another. `pinned`
    /// says which tabs are pinned: they stay before the unpinned ones of a strip.
    pub fn move_tab(&mut self, tab: TabId, target: Target, pinned: &dyn Fn(TabId) -> bool) -> bool {
        let Some(from) = self.group_of(tab) else { return false };
        match target {
            Target::Strip { group, index } => {
                if !self.groups.contains_key(&group) {
                    return false;
                }
                // out of the old group (its active moves on if needed)
                {
                    let g = self.groups.get_mut(&from).unwrap();
                    let pos = g.tabs.iter().position(|t| *t == tab).unwrap();
                    g.tabs.remove(pos);
                    if g.active == Some(tab) && from != group {
                        g.active = g.tabs.get(pos).or_else(|| g.tabs.get(pos.wrapping_sub(1))).or_else(|| g.tabs.last()).copied();
                    }
                }
                let g = self.groups.get_mut(&group).unwrap();
                let pinned_count = g.tabs.iter().filter(|t| pinned(**t)).count();
                let index = if pinned(tab) { index.min(pinned_count) } else { index.max(pinned_count) }.min(g.tabs.len());
                g.tabs.insert(index, tab);
                g.active = Some(tab);
                self.active = group;
            }
            Target::Split { group, side } => {
                if !self.groups.contains_key(&group) {
                    return false;
                }
                if from == group && self.groups[&from].tabs.len() == 1 {
                    return false; // a lone tab split off its own group: nothing to do
                }
                let new = GroupId(self.next);
                self.next += 1;
                {
                    let g = self.groups.get_mut(&from).unwrap();
                    let pos = g.tabs.iter().position(|t| *t == tab).unwrap();
                    g.tabs.remove(pos);
                    if g.active == Some(tab) {
                        g.active = g.tabs.get(pos).or_else(|| g.tabs.get(pos.wrapping_sub(1))).or_else(|| g.tabs.last()).copied();
                    }
                }
                self.groups.insert(new, Group { tabs: vec![tab], active: Some(tab) });
                Self::insert_beside(&mut self.root, group, new, side);
                self.active = new;
            }
        }
        self.maximized = None;
        self.normalize();
        self.bump();
        true
    }

    /// Put `new` next to `beside` in the tree, sharing a split along `side`'s axis.
    fn insert_beside(node: &mut Node, beside: GroupId, new: GroupId, side: Side) -> bool {
        match node {
            Node::Group(g) if *g == beside => {
                let old = Node::Group(beside);
                let children = if side.after() { vec![old, Node::Group(new)] } else { vec![Node::Group(new), old] };
                *node = Node::Split { axis: side.axis(), children, ratios: vec![0.5, 0.5] };
                true
            }
            Node::Group(_) => false,
            Node::Split { axis, children, ratios } => {
                if let Some(i) = children.iter().position(|c| matches!(c, Node::Group(g) if *g == beside)) {
                    if *axis == side.axis() {
                        // a sibling in the same row / column, taking half of the neighbour's share
                        let share = ratios[i] / 2.0;
                        ratios[i] = share;
                        let at = if side.after() { i + 1 } else { i };
                        children.insert(at, Node::Group(new));
                        ratios.insert(at, share);
                        return true;
                    }
                }
                children.iter_mut().any(|c| Self::insert_beside(c, beside, new, side))
            }
        }
    }

    /// Close a group: its tabs are returned for the caller to close; the group goes once they
    /// are gone (through `remove_tab` / `sync`).
    pub fn group_tabs(&self, gid: GroupId) -> Vec<TabId> {
        self.groups.get(&gid).map(|g| g.tabs.clone()).unwrap_or_default()
    }

    pub fn toggle_maximize(&mut self, gid: GroupId) {
        if !self.groups.contains_key(&gid) {
            return;
        }
        self.maximized = if self.maximized == Some(gid) { None } else { Some(gid) };
        self.bump();
    }

    /// Give every child of every split the same share.
    pub fn equalize(&mut self) {
        fn eq(n: &mut Node) {
            if let Node::Split { children, ratios, .. } = n {
                let share = 1.0 / children.len() as f32;
                ratios.iter_mut().for_each(|r| *r = share);
                children.iter_mut().for_each(eq);
            }
        }
        eq(&mut self.root);
        self.bump();
    }

    /// The split at `path` (indices into children from the root).
    pub fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let mut n = &mut self.root;
        for i in path {
            match n {
                Node::Split { children, .. } => n = children.get_mut(*i)?,
                Node::Group(_) => return None,
            }
        }
        Some(n)
    }

    /// Drag the gutter after child `i` of the split at `path` by `delta` (a fraction of the
    /// split's length along its axis).
    pub fn drag_gutter(&mut self, path: &[usize], i: usize, delta: f32) {
        if let Some(Node::Split { ratios, .. }) = self.node_at_mut(path) {
            if i + 1 < ratios.len() {
                let pair = ratios[i] + ratios[i + 1];
                let a = (ratios[i] + delta).clamp(MIN_RATIO, pair - MIN_RATIO);
                ratios[i] = a;
                ratios[i + 1] = pair - a;
                self.bump();
            }
        }
    }

    pub fn equalize_split(&mut self, path: &[usize]) {
        if let Some(Node::Split { children, ratios, .. }) = self.node_at_mut(path) {
            let share = 1.0 / children.len() as f32;
            ratios.iter_mut().for_each(|r| *r = share);
            self.bump();
        }
    }

    /// The group next to `gid` on `side`, by the rectangles the last frame drew.
    pub fn neighbour(&self, gid: GroupId, side: Side, rects: &HashMap<GroupId, egui::Rect>) -> Option<GroupId> {
        let me = *rects.get(&gid)?;
        let mut best: Option<(f32, GroupId)> = None;
        for (other, r) in rects {
            if *other == gid {
                continue;
            }
            let (adjacent, overlap) = match side {
                Side::Left => ((me.left() - r.right()).abs() < 12.0, (me.bottom().min(r.bottom()) - me.top().max(r.top())).max(0.0)),
                Side::Right => ((r.left() - me.right()).abs() < 12.0, (me.bottom().min(r.bottom()) - me.top().max(r.top())).max(0.0)),
                Side::Up => ((me.top() - r.bottom()).abs() < 12.0, (me.right().min(r.right()) - me.left().max(r.left())).max(0.0)),
                Side::Down => ((r.top() - me.bottom()).abs() < 12.0, (me.right().min(r.right()) - me.left().max(r.left())).max(0.0)),
            };
            if adjacent && overlap > 0.0 && best.map(|(o, _)| overlap > o).unwrap_or(true) {
                best = Some((overlap, *other));
            }
        }
        best.map(|(_, g)| g)
    }

    // ----- invariants -----

    pub fn normalize(&mut self) {
        // empty groups go, unless nothing would be left
        let empty: Vec<GroupId> = self.groups.iter().filter(|(_, g)| g.tabs.is_empty()).map(|(id, _)| *id).collect();
        for id in empty {
            if self.groups.len() > 1 {
                self.groups.remove(&id);
            }
        }
        let live: Vec<GroupId> = self.groups.keys().copied().collect();
        let mut root = std::mem::replace(&mut self.root, Node::Group(GroupId(0)));
        Self::prune(&mut root, &live);
        // groups the tree lost (should not happen) hang off the root as a row
        let in_tree = {
            let mut v = Vec::new();
            fn walk(n: &Node, v: &mut Vec<GroupId>) {
                match n {
                    Node::Group(g) => v.push(*g),
                    Node::Split { children, .. } => children.iter().for_each(|c| walk(c, v)),
                }
            }
            walk(&root, &mut v);
            v
        };
        let missing: Vec<GroupId> = live.iter().filter(|g| !in_tree.contains(g)).copied().collect();
        if !missing.is_empty() {
            let mut children = if matches!(root, Node::Group(GroupId(0))) { Vec::new() } else { vec![root] };
            children.extend(missing.into_iter().map(Node::Group));
            let n = children.len() as f32;
            root = if children.len() == 1 { children.pop().unwrap() } else { Node::Split { axis: Axis::Row, children, ratios: vec![1.0 / n; n as usize] } };
        }
        if matches!(root, Node::Group(GroupId(0))) {
            root = Node::Group(*live.first().unwrap());
        }
        self.root = root;
        if !self.groups.contains_key(&self.active) {
            self.active = self.groups_in_order().first().copied().unwrap_or(*self.groups.keys().next().unwrap());
        }
        if self.maximized.map(|m| !self.groups.contains_key(&m)).unwrap_or(false) {
            self.maximized = None;
        }
        for g in self.groups.values_mut() {
            if g.active.map(|a| !g.tabs.contains(&a)).unwrap_or(true) {
                g.active = g.tabs.last().copied();
            }
        }
    }

    /// Drop dead groups, collapse single-child splits, flatten same-axis nesting, fix ratios.
    fn prune(node: &mut Node, live: &[GroupId]) {
        if let Node::Split { axis, children, ratios } = node {
            let axis = *axis;
            if ratios.len() != children.len() {
                let n = children.len().max(1) as f32;
                *ratios = vec![1.0 / n; children.len()];
            }
            let mut kept: Vec<(Node, f32)> = Vec::new();
            for (mut c, r) in children.drain(..).zip(ratios.drain(..)) {
                Self::prune(&mut c, live);
                match c {
                    Node::Group(g) if !live.contains(&g) => {}
                    Node::Group(GroupId(0)) => {}
                    Node::Split { axis: a, children: cc, ratios: rr } if a == axis => {
                        // flatten: the child's children take its share
                        for (c2, r2) in cc.into_iter().zip(rr) {
                            kept.push((c2, r * r2));
                        }
                    }
                    other => kept.push((other, r)),
                }
            }
            match kept.len() {
                0 => *node = Node::Group(GroupId(0)),
                1 => *node = kept.pop().unwrap().0,
                _ => {
                    let total: f32 = kept.iter().map(|(_, r)| r.max(MIN_RATIO)).sum();
                    *ratios = kept.iter().map(|(_, r)| r.max(MIN_RATIO) / total).collect();
                    *children = kept.into_iter().map(|(c, _)| c).collect();
                }
            }
        }
    }

    // ----- persistence -----

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// A saved layout, trimmed to the tabs that exist (`known`); tabs the layout does not name
    /// are added by the next `sync`.
    pub fn from_json(json: &str, known: &[TabId]) -> Option<Layout> {
        let mut l: Layout = serde_json::from_str(json).ok()?;
        for g in l.groups.values_mut() {
            g.tabs.retain(|t| known.contains(t));
        }
        if l.groups.is_empty() {
            return None;
        }
        l.next = l.next.max(l.groups.keys().map(|g| g.0 + 1).max().unwrap_or(2));
        l.normalize();
        l.generation = 1;
        Some(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> TabId {
        TabId::parse(&uuid::Uuid::from_u128(n).to_string()).unwrap()
    }
    fn no_pins(_: TabId) -> bool {
        false
    }

    #[test]
    fn sync_adds_after_the_active_tab_and_drops_closed_ones() {
        let mut l = Layout::new();
        assert!(l.sync(&[id(1), id(2)], false));
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(1), id(2)]);
        assert_eq!(l.active_tab(), Some(id(1)));
        l.set_active_tab(id(1));
        l.sync(&[id(1), id(2), id(3)], false);
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(1), id(3), id(2)], "a new tab lands right of the active one");
        l.sync(&[id(1), id(2), id(3), id(4)], true);
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(1), id(3), id(2), id(4)], "or at the end");
        l.sync(&[id(3), id(2)], false);
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(3), id(2)]);
        assert_eq!(l.active_tab(), Some(id(2)), "the closed active tab hands over");
    }

    #[test]
    fn reorder_within_a_strip_respects_pins() {
        let mut l = Layout::new();
        l.sync(&[id(1), id(2), id(3), id(4)], true);
        let pinned = |t: TabId| t == id(1);
        assert!(l.move_tab(id(4), Target::Strip { group: GroupId(1), index: 0 }, &pinned));
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(1), id(4), id(2), id(3)], "an unpinned tab cannot pass the pinned one");
        assert!(l.move_tab(id(1), Target::Strip { group: GroupId(1), index: 3 }, &pinned));
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(1), id(4), id(2), id(3)], "a pinned tab stays in the pinned range");
        assert!(l.move_tab(id(2), Target::Strip { group: GroupId(1), index: 4 }, &pinned));
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(1), id(4), id(3), id(2)]);
        assert_eq!(l.active_tab(), Some(id(2)));
    }

    #[test]
    fn split_right_then_right_again_is_one_row_and_collapses_back() {
        let mut l = Layout::new();
        l.sync(&[id(1), id(2), id(3)], true);
        assert!(l.move_tab(id(3), Target::Split { group: GroupId(1), side: Side::Right }, &no_pins));
        assert_eq!(l.group_count(), 2);
        assert_eq!(l.active_group(), GroupId(2));
        assert_eq!(l.groups_in_order(), vec![GroupId(1), GroupId(2)]);
        assert!(matches!(l.root(), Node::Split { axis: Axis::Row, children, .. } if children.len() == 2));
        assert!(l.move_tab(id(2), Target::Split { group: GroupId(2), side: Side::Right }, &no_pins));
        assert!(matches!(l.root(), Node::Split { axis: Axis::Row, children, ratios } if children.len() == 3 && (ratios.iter().sum::<f32>() - 1.0).abs() < 1e-4), "{:?}", l.root());
        assert_eq!(l.groups_in_order(), vec![GroupId(1), GroupId(2), GroupId(3)]);
        // a column under the middle one: the first group loses its last tab and goes
        assert!(l.move_tab(id(1), Target::Split { group: GroupId(2), side: Side::Down }, &no_pins));
        assert_eq!(l.group_count(), 3);
        assert_eq!(l.tabs_in_order(), vec![id(3), id(1), id(2)]);
        assert!(matches!(l.root(), Node::Split { axis: Axis::Row, children, .. } if children.len() == 2 && matches!(children[0], Node::Split { axis: Axis::Col, .. })), "{:?}", l.root());
        // closing tabs collapses groups and splits
        l.remove_tab(id(1));
        assert_eq!(l.group_count(), 2);
        assert!(matches!(l.root(), Node::Split { axis: Axis::Row, children, .. } if children.len() == 2 && children.iter().all(|c| matches!(c, Node::Group(_)))), "{:?}", l.root());
        l.remove_tab(id(2));
        assert_eq!(l.group_count(), 1);
        assert!(matches!(l.root(), Node::Group(_)));
        l.remove_tab(id(3));
        assert_eq!(l.group_count(), 1);
        assert!(l.group(l.active_group()).unwrap().tabs.is_empty());
    }

    #[test]
    fn a_lone_tab_cannot_split_its_own_group_and_the_last_group_stays() {
        let mut l = Layout::new();
        l.sync(&[id(1)], true);
        assert!(!l.move_tab(id(1), Target::Split { group: GroupId(1), side: Side::Right }, &no_pins));
        l.remove_tab(id(1));
        assert_eq!(l.group_count(), 1);
        assert_eq!(l.active_tab(), None);
    }

    #[test]
    fn move_between_strips_and_close_group() {
        let mut l = Layout::new();
        l.sync(&[id(1), id(2), id(3)], true);
        l.move_tab(id(3), Target::Split { group: GroupId(1), side: Side::Down }, &no_pins);
        assert!(l.move_tab(id(1), Target::Strip { group: GroupId(2), index: 0 }, &no_pins));
        assert_eq!(l.group(GroupId(2)).unwrap().tabs, vec![id(1), id(3)]);
        assert_eq!(l.group(GroupId(1)).unwrap().tabs, vec![id(2)]);
        assert_eq!(l.group(GroupId(1)).unwrap().active, Some(id(2)));
        let victims = l.group_tabs(GroupId(2));
        assert_eq!(victims, vec![id(1), id(3)]);
        for t in victims {
            l.remove_tab(t);
        }
        assert_eq!(l.group_count(), 1);
        assert_eq!(l.active_group(), GroupId(1));
    }

    #[test]
    fn gutters_maximize_and_json_round_trip() {
        let mut l = Layout::new();
        l.sync(&[id(1), id(2)], true);
        l.move_tab(id(2), Target::Split { group: GroupId(1), side: Side::Right }, &no_pins);
        l.drag_gutter(&[], 0, 0.2);
        assert!(matches!(l.root(), Node::Split { ratios, .. } if (ratios[0] - 0.7).abs() < 1e-5));
        l.drag_gutter(&[], 0, 5.0);
        assert!(matches!(l.root(), Node::Split { ratios, .. } if ratios[1] >= MIN_RATIO - 1e-6));
        l.equalize();
        assert!(matches!(l.root(), Node::Split { ratios, .. } if (ratios[0] - 0.5).abs() < 1e-5));
        l.toggle_maximize(GroupId(2));
        assert_eq!(l.maximized(), Some(GroupId(2)));
        let json = l.to_json();
        let back = Layout::from_json(&json, &[id(1), id(2)]).unwrap();
        assert_eq!(back.groups_in_order(), l.groups_in_order());
        assert_eq!(back.maximized(), Some(GroupId(2)));
        // an id that no longer exists is dropped and its empty group goes
        let trimmed = Layout::from_json(&json, &[id(1)]).unwrap();
        assert_eq!(trimmed.group_count(), 1);
        assert!(matches!(trimmed.root(), Node::Group(GroupId(1))));
        assert_eq!(trimmed.maximized(), None);
        // a new tab after the restore joins the active group
        let mut trimmed = trimmed;
        trimmed.sync(&[id(1), id(9)], false);
        assert_eq!(trimmed.group_of(id(9)), Some(GroupId(1)));
    }

    #[test]
    fn neighbours_by_rect() {
        let mut l = Layout::new();
        l.sync(&[id(1), id(2), id(3)], true);
        l.move_tab(id(2), Target::Split { group: GroupId(1), side: Side::Right }, &no_pins);
        l.move_tab(id(3), Target::Split { group: GroupId(1), side: Side::Down }, &no_pins);
        let mut rects = HashMap::new();
        rects.insert(GroupId(1), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0)));
        rects.insert(GroupId(3), egui::Rect::from_min_max(egui::pos2(0.0, 56.0), egui::pos2(100.0, 100.0)));
        rects.insert(GroupId(2), egui::Rect::from_min_max(egui::pos2(106.0, 0.0), egui::pos2(200.0, 100.0)));
        assert_eq!(l.neighbour(GroupId(1), Side::Right, &rects), Some(GroupId(2)));
        assert_eq!(l.neighbour(GroupId(1), Side::Down, &rects), Some(GroupId(3)));
        assert_eq!(l.neighbour(GroupId(2), Side::Left, &rects), Some(GroupId(1)));
        assert_eq!(l.neighbour(GroupId(1), Side::Left, &rects), None);
    }
}
