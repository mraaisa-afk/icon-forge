//! The editor document: z-ordered nodes with a local path and a transform.

use super::affine::Affine;
use super::geom::{Point, Subpath};

/// Default pointer tolerance used when a click lands just outside an outline,
/// in document units. The UI passes the canvas-space value scaled by its zoom.
pub const DEFAULT_PICK_TOLERANCE: f32 = 2.0;

/// Stable identity of a node inside one document.
///
/// Ids are never reused within a document, so a command that captured an id
/// (a drag, a selection) can still be inverted after other nodes were added or
/// removed — index-based references would silently retarget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// Wraps a raw id (the WASM wire format and tests use raw numbers).
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Stable identity of an editor **group** (4B).
///
/// A group is what makes several nodes behave like one object: picking any
/// member selects all of them, and a transform applies to the whole set. Groups
/// are deliberately **flat** in 4B (a group cannot contain another group) and
/// live on the nodes rather than in a separate table, so the document stays a
/// flat z-ordered list and every existing z operation keeps working unchanged.
/// Like [`NodeId`], a group id is never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(u32);

impl GroupId {
    /// Wraps a raw id (the WASM wire format and tests use raw numbers).
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One editable layer: geometry in local coordinates plus its placement.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Stable identity (see [`NodeId`]).
    pub id: NodeId,
    /// Geometry in the node's local space.
    pub path: Vec<Subpath>,
    /// Local → document transform (identity for a freshly traced icon).
    pub transform: Affine,
    /// Fill colour, RGBA (traced icons are flat-filled).
    pub fill: [u8; 4],
    /// Hidden nodes still exist and can be re-shown with an exact undo.
    pub visible: bool,
    /// The group this node belongs to, if any (see [`GroupId`]).
    pub group: Option<GroupId>,
}

impl Node {
    /// A node with the identity transform and an opaque fill.
    #[must_use]
    pub fn new(id: NodeId, path: Vec<Subpath>, fill: [u8; 4]) -> Self {
        Self {
            id,
            path,
            transform: Affine::IDENTITY,
            fill,
            visible: true,
            group: None,
        }
    }

    /// Total number of path segments (the cost unit the editor reports).
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.path.iter().map(|s| s.segs.len()).sum()
    }

    /// True when the node has no geometry at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.path.iter().all(Subpath::is_empty)
    }

    /// The node's path in **document** space (what the canvas draws).
    #[must_use]
    pub fn placed_path(&self) -> Vec<Subpath> {
        self.path
            .iter()
            .map(|s| s.transformed(self.transform))
            .collect()
    }

    /// The node's bounding box in document space, as `(min, max)`.
    ///
    /// Uses the *control hull* of each subpath, so a cubic can never stick out
    /// of the reported box.
    #[must_use]
    pub fn bounds(&self) -> Option<(Point, Point)> {
        self.bounds_with(self.transform)
    }

    /// The bounding box the node would have under `transform` instead of its
    /// stored one — what a live drag preview asks for.
    #[must_use]
    pub fn bounds_with(&self, transform: Affine) -> Option<(Point, Point)> {
        let mut min = Point::new(f32::INFINITY, f32::INFINITY);
        let mut max = Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut any = false;
        for sub in &self.path {
            let Some((lo, hi)) = sub.bounds() else {
                continue;
            };
            for p in [lo, Point::new(hi.x, lo.y), hi, Point::new(lo.x, hi.y)] {
                let (x, y) = transform.apply(p.x, p.y);
                min.x = min.x.min(x);
                min.y = min.y.min(y);
                max.x = max.x.max(x);
                max.y = max.y.max(y);
                any = true;
            }
        }
        any.then_some((min, max))
    }

    /// True when `(x, y)` (document space) is inside the node's filled area, or
    /// within `tolerance` of its outline (thin strokes are still clickable).
    ///
    /// The tolerance is given in document units and divided down into local
    /// units by the transform's uniform scale, so picking behaves the same at
    /// every zoom level and after every scale command.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32, tolerance: f32) -> bool {
        self.contains_with(self.transform, x, y, tolerance)
    }

    /// [`Node::contains`] under an overriding transform (the live preview).
    #[must_use]
    pub fn contains_with(&self, transform: Affine, x: f32, y: f32, tolerance: f32) -> bool {
        let inv = match transform.invert() {
            Some(inv) => inv,
            None => return false,
        };
        let (lx, ly) = inv.apply(x, y);
        let local = Point::new(lx, ly);
        if !local.is_finite() {
            return false;
        }
        if self.path.iter().any(|sub| sub.contains(local)) {
            return true;
        }
        if tolerance <= 0.0 {
            return false;
        }
        // `inv` maps document distances into local units; sqrt(|det inv|) is
        // its average linear magnification, so this keeps picking identical at
        // every zoom level and after every scale command.
        let local_tolerance = tolerance * inv.det().abs().sqrt();
        local_tolerance.is_finite()
            && self
                .path
                .iter()
                .any(|sub| sub.distance_to(local) <= local_tolerance)
    }
}

/// A flat, z-ordered editor document (index 0 is the bottom layer).
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    width: f32,
    height: f32,
    nodes: Vec<Node>,
    next_id: u32,
    next_group: u32,
}

impl Doc {
    /// An empty canvas of the given size.
    #[must_use]
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            nodes: Vec::new(),
            next_id: 1,
            next_group: 1,
        }
    }

    /// Canvas width in document units.
    #[must_use]
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Canvas height in document units.
    #[must_use]
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Every node, bottom first.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Node count.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// True when the document has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Every node id, bottom first.
    #[must_use]
    pub fn ids(&self) -> Vec<NodeId> {
        self.nodes.iter().map(|n| n.id).collect()
    }

    /// Total segment count across all nodes (the editor's cost unit).
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.nodes.iter().map(Node::segment_count).sum()
    }

    /// Looks a node up by id.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The z-order index of a node.
    #[must_use]
    pub fn index_of(&self, id: NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// The id the next [`Doc::add`] will hand out.
    #[must_use]
    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    /// Restores the id counter (used by the undo of an insert).
    pub fn set_next_id(&mut self, next: u32) {
        self.next_id = next;
    }

    /// The group a node belongs to.
    #[must_use]
    pub fn group_of(&self, id: NodeId) -> Option<GroupId> {
        self.node(id).and_then(|n| n.group)
    }

    /// Every member of a group, in z order (bottom first).
    #[must_use]
    pub fn members(&self, group: GroupId) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|n| n.group == Some(group))
            .map(|n| n.id)
            .collect()
    }

    /// How many distinct groups the document contains.
    #[must_use]
    pub fn group_count(&self) -> usize {
        let mut groups: Vec<GroupId> = self.nodes.iter().filter_map(|n| n.group).collect();
        groups.sort_unstable();
        groups.dedup();
        groups.len()
    }

    /// The id the next group will be minted with (see [`Doc::note_group`]).
    #[must_use]
    pub fn next_group(&self) -> u32 {
        self.next_group
    }

    /// Mints a fresh group id.
    pub fn alloc_group(&mut self) -> GroupId {
        let id = GroupId::new(self.next_group);
        self.note_group(id);
        id
    }

    /// Raises the group counter so `group` can never be minted twice — the
    /// [`Doc::insert_at`] rule for ids, applied to groups.
    pub fn note_group(&mut self, group: GroupId) {
        self.next_group = self.next_group.max(group.get().saturating_add(1));
    }

    /// The ids a click on `id` should select: its whole group, or just itself.
    #[must_use]
    pub fn selection_target(&self, id: NodeId) -> Vec<NodeId> {
        match self.group_of(id) {
            Some(group) => self.members(group),
            None => vec![id],
        }
    }

    /// Appends a node on top of the stack.
    pub fn add(&mut self, path: Vec<Subpath>, fill: [u8; 4]) -> NodeId {
        let id = NodeId::new(self.next_id);
        self.next_id += 1;
        self.nodes.push(Node::new(id, path, fill));
        id
    }

    /// Inserts a fully specified node at `index` (clamped), keeping its id.
    ///
    /// This is what the undo of a delete calls; it also advances the id counter
    /// so a restored id can never be handed out twice.
    pub fn insert_at(&mut self, index: usize, node: Node) {
        let index = index.min(self.nodes.len());
        self.next_id = self.next_id.max(node.id.get() + 1);
        self.nodes.insert(index, node);
    }

    /// Removes and returns the node at `index`.
    pub fn remove_at(&mut self, index: usize) -> Node {
        self.nodes.remove(index)
    }

    /// Replaces the node at `index`.
    pub fn set_at(&mut self, index: usize, node: Node) {
        self.nodes[index] = node;
    }

    /// Moves the node at `from` to index `to` (both clamped).
    pub fn move_at(&mut self, from: usize, to: usize) {
        if from >= self.nodes.len() {
            return;
        }
        let node = self.nodes.remove(from);
        let to = to.min(self.nodes.len());
        self.nodes.insert(to, node);
    }

    /// The union bounding box of `ids`, as `(min, max)`; `None` when every id
    /// is unknown or empty.
    #[must_use]
    pub fn bounds(&self, ids: &[NodeId]) -> Option<(Point, Point)> {
        let mut min = Point::new(f32::INFINITY, f32::INFINITY);
        let mut max = Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut any = false;
        for id in ids {
            let Some((lo, hi)) = self.node(*id).and_then(Node::bounds) else {
                continue;
            };
            min.x = min.x.min(lo.x);
            min.y = min.y.min(lo.y);
            max.x = max.x.max(hi.x);
            max.y = max.y.max(hi.y);
            any = true;
        }
        any.then_some((min, max))
    }

    /// The topmost **visible** node under `(x, y)`.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32, tolerance: f32) -> Option<NodeId> {
        self.nodes
            .iter()
            .rev()
            .find(|n| n.visible && n.contains(x, y, tolerance))
            .map(|n| n.id)
    }

    /// Every visible node whose bounding box intersects the rectangle
    /// `(x0, y0)–(x1, y1)` (document space, corners in any order).
    #[must_use]
    pub fn marquee(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<NodeId> {
        let (rx0, rx1) = (x0.min(x1), x0.max(x1));
        let (ry0, ry1) = (y0.min(y1), y0.max(y1));
        self.nodes
            .iter()
            .filter(|n| {
                n.visible
                    && n.bounds().is_some_and(|(lo, hi)| {
                        hi.x >= rx0 && lo.x <= rx1 && hi.y >= ry0 && lo.y <= ry1
                    })
            })
            .map(|n| n.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_sub(x0: f32, y0: f32, x1: f32, y1: f32) -> Subpath {
        Subpath {
            start: Point::new(x0, y0),
            segs: vec![
                super::super::geom::Seg::Line(Point::new(x1, y0)),
                super::super::geom::Seg::Line(Point::new(x1, y1)),
                super::super::geom::Seg::Line(Point::new(x0, y1)),
            ],
            closed: true,
        }
    }

    fn doc_with_squares() -> (Doc, NodeId, NodeId) {
        let mut doc = Doc::new(100.0, 100.0);
        let a = doc.add(vec![square_sub(0.0, 0.0, 10.0, 10.0)], [10, 20, 30, 255]);
        let b = doc.add(vec![square_sub(5.0, 5.0, 15.0, 15.0)], [40, 50, 60, 255]);
        (doc, a, b)
    }

    #[test]
    fn hit_test_prefers_the_topmost_node() {
        let (doc, _a, b) = doc_with_squares();
        assert_eq!(doc.hit_test(7.0, 7.0, DEFAULT_PICK_TOLERANCE), Some(b));
        assert!(doc.hit_test(1.0, 1.0, DEFAULT_PICK_TOLERANCE).is_some());
        assert_eq!(doc.hit_test(90.0, 90.0, DEFAULT_PICK_TOLERANCE), None);
    }

    #[test]
    fn hidden_nodes_are_neither_picked_nor_marqueed() {
        let (mut doc, a, _b) = doc_with_squares();
        doc.set_at(0, {
            let mut n = doc.node(a).unwrap().clone();
            n.visible = false;
            n
        });
        assert_eq!(doc.hit_test(1.0, 1.0, DEFAULT_PICK_TOLERANCE), None);
        assert!(doc.hit_test(7.0, 7.0, DEFAULT_PICK_TOLERANCE).is_some());
        let marquee = doc.marquee(-1.0, -1.0, 20.0, 20.0);
        assert_eq!(marquee.len(), 1);
    }

    #[test]
    fn bounds_union_covers_placed_transforms() {
        let (mut doc, a, _b) = doc_with_squares();
        let placed = {
            let mut n = doc.node(a).unwrap().clone();
            n.transform = Affine::translate(50.0, 0.0);
            n
        };
        doc.set_at(0, placed);
        let (lo, hi) = doc.bounds(&[a]).expect("bounds");
        assert_eq!((lo.x, hi.x), (50.0, 60.0));
    }

    #[test]
    fn insert_at_keeps_ids_and_raises_the_counter() {
        let (mut doc, a, b) = doc_with_squares();
        let removed = doc.remove_at(0);
        assert_eq!(removed.id, a);
        doc.insert_at(0, removed);
        assert_eq!(doc.ids(), vec![a, b]);
        assert!(doc.next_id() > b.get());
    }

    #[test]
    fn marquee_uses_bounding_boxes_not_fill_rules() {
        let (doc, a, b) = doc_with_squares();
        assert_eq!(doc.marquee(-5.0, -5.0, 3.0, 3.0), vec![a]);
        assert_eq!(doc.marquee(-5.0, -5.0, 20.0, 20.0), vec![a, b]);
        assert!(doc.marquee(30.0, 30.0, 40.0, 40.0).is_empty());
    }
}
