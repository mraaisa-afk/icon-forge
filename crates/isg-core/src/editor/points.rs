//! Point addressing: which vertex or handle a gesture grabbed, and the local
//! edits the node-editing tools make (Phase 4C).
//!
//! A path is stored as [`Subpath`]s — a start point plus segments — which is the
//! right shape for rendering but not for editing: a user grabs *a vertex* or
//! *a handle*, and the engine has to turn that into a mutation of the segment
//! list. Every function here is that translation, and every one of them refuses
//! rather than guesses: an out-of-range vertex, a handle on a straight segment,
//! a split at the wrong parameter, or an edit that would leave a subpath with
//! nothing in it all come back as `false`/`None`.
//!
//! Two conventions are worth stating once:
//!
//! * **Vertex `k`** is the end point of segment `k - 1`; vertex `0` is the
//!   subpath's start point. A closed subpath's last vertex is joined back to
//!   vertex `0` by an *implicit straight edge* (the renderer's `closePath`), so
//!   vertex `0` has no incoming handle and the last vertex has no outgoing one.
//! * **Moving an anchor moves its handles with it** — that is what every vector
//!   editor does, and it is what keeps the curve around the anchor the same
//!   shape as the user drags it. Moving a *handle* moves only that handle.

use super::geom::{Point, Seg, Subpath};

/// A vertex of a subpath, addressed by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexRef {
    /// Which subpath (document order).
    pub subpath: usize,
    /// Which vertex: `0` is the start, `k` the end of segment `k - 1`.
    pub vertex: usize,
}

impl VertexRef {
    /// Builds a reference.
    #[must_use]
    pub const fn new(subpath: usize, vertex: usize) -> Self {
        Self { subpath, vertex }
    }
}

/// A segment of a subpath, addressed by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentRef {
    /// Which subpath (document order).
    pub subpath: usize,
    /// Which segment, `0`-based.
    pub segment: usize,
}

impl SegmentRef {
    /// Builds a reference.
    #[must_use]
    pub const fn new(subpath: usize, segment: usize) -> Self {
        Self { subpath, segment }
    }
}

/// Which of a cubic's two control handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// The handle leaving the segment's start point.
    C1,
    /// The handle arriving at the segment's end point.
    C2,
}

/// A control handle of a cubic segment, addressed by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleRef {
    /// Which subpath (document order).
    pub subpath: usize,
    /// Which segment, `0`-based.
    pub segment: usize,
    /// Which of the two handles.
    pub handle: Handle,
}

impl HandleRef {
    /// Builds a reference.
    #[must_use]
    pub const fn new(subpath: usize, segment: usize, handle: Handle) -> Self {
        Self {
            subpath,
            segment,
            handle,
        }
    }
}

/// What a segment should be, for the line ↔ cubic conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    /// A straight line.
    Line,
    /// A cubic Bézier.
    Cubic,
}

impl SegKind {
    /// The kind of an existing segment.
    #[must_use]
    pub const fn of(seg: &Seg) -> Self {
        match seg {
            Seg::Line(_) => Self::Line,
            Seg::Cubic { .. } => Self::Cubic,
        }
    }

    /// Raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Line => 0,
            Self::Cubic => 1,
        }
    }

    /// Decodes a wire value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Line),
            1 => Some(Self::Cubic),
            _ => None,
        }
    }
}

/// How many vertices a subpath has (its start plus one per segment).
#[must_use]
pub fn vertex_count(sub: &Subpath) -> usize {
    sub.segs.len() + 1
}

/// The subpath's point at `vertex`, if the index is in range.
#[must_use]
pub fn vertex_point(sub: &Subpath, vertex: usize) -> Option<Point> {
    if vertex == 0 {
        return Some(sub.start);
    }
    sub.segs.get(vertex - 1).map(|seg| seg.end())
}

/// Replaces the anchor at `vertex`, dragging that anchor's handles with it.
///
/// The attached handles (the incoming segment's `c2` and the outgoing segment's
/// `c1`) move by the same delta, which preserves the local shape of the curve —
/// the behaviour a direct-manipulation tool has to have. Returns `false` for an
/// out-of-range vertex or a non-finite target.
pub fn move_vertex(sub: &mut Subpath, vertex: usize, to: Point) -> bool {
    if !to.is_finite() || vertex >= vertex_count(sub) {
        return false;
    }
    let Some(from) = vertex_point(sub, vertex) else {
        return false;
    };
    let d = Point::new(to.x - from.x, to.y - from.y);
    let drag = |p: &mut Point| {
        p.x += d.x;
        p.y += d.y;
    };
    // The anchor itself…
    if vertex == 0 {
        sub.start = to;
    } else if let Some(seg) = sub.segs.get_mut(vertex - 1) {
        match seg {
            Seg::Line(p) => *p = to,
            Seg::Cubic { to: end, .. } => *end = to,
        }
    }
    // …then its handles, once each. The incoming one belongs to the segment
    // that ends here — except at vertex 0, whose incoming edge is the implicit
    // straight close.
    if vertex > 0 {
        if let Some(Seg::Cubic { c2, .. }) = sub.segs.get_mut(vertex - 1) {
            drag(c2);
        }
    }
    // …and the outgoing one, which for the last vertex of a closed subpath is
    // the implicit close edge (straight, so nothing to drag).
    if vertex < sub.segs.len() {
        if let Some(Seg::Cubic { c1, .. }) = sub.segs.get_mut(vertex) {
            drag(c1);
        }
    }
    true
}

/// Moves one control handle of a cubic segment. Returns `false` when the
/// segment is a line (it has no handles) or the target is not finite.
pub fn move_handle(sub: &mut Subpath, segment: usize, handle: Handle, to: Point) -> bool {
    if !to.is_finite() {
        return false;
    }
    match sub.segs.get_mut(segment) {
        Some(Seg::Cubic { c1, c2, .. }) => {
            match handle {
                Handle::C1 => *c1 = to,
                Handle::C2 => *c2 = to,
            }
            true
        }
        _ => false,
    }
}

/// Converts a segment between a line and a cubic.
///
/// A line becomes the cubic that *is* that line (handles at the thirds), so the
/// conversion is shape-preserving and reversible in appearance; a cubic becomes
/// the straight line to its end point (the handles are dropped, which is the
/// point of the conversion). Returns `false` for an out-of-range segment or a
/// conversion that would change nothing.
pub fn set_segment_kind(sub: &mut Subpath, segment: usize, to: SegKind) -> bool {
    let Some(current) = sub.segs.get(segment).copied() else {
        return false;
    };
    if SegKind::of(&current) == to {
        return false;
    }
    let replacement = match (current, to) {
        (Seg::Line(end), SegKind::Cubic) => {
            let start = segment_start(sub, segment);
            Seg::Cubic {
                c1: Point::new(
                    start.x + (end.x - start.x) / 3.0,
                    start.y + (end.y - start.y) / 3.0,
                ),
                c2: Point::new(
                    start.x + 2.0 * (end.x - start.x) / 3.0,
                    start.y + 2.0 * (end.y - start.y) / 3.0,
                ),
                to: end,
            }
        }
        (Seg::Cubic { to: end, .. }, SegKind::Line) => Seg::Line(end),
        // The two same-kind cases returned above; this arm keeps the match
        // total without a second early return.
        _ => return false,
    };
    sub.segs[segment] = replacement;
    true
}

/// Where a segment starts (the previous segment's end, or the subpath start).
#[must_use]
pub fn segment_start(sub: &Subpath, segment: usize) -> Point {
    if segment == 0 {
        sub.start
    } else {
        // Total rather than panicking: callers validate their addresses, and
        // this keeps a bad one from turning into a panic on the wasm side.
        sub.segs.get(segment - 1).map_or(sub.start, |seg| seg.end())
    }
}

/// Splits a segment at parameter `t` (`0 < t < 1`), inserting a vertex.
///
/// A line splits into two lines through the interpolated point; a cubic splits
/// by de Casteljau's construction, which produces two cubics whose union is the
/// original curve (to floating-point accuracy) — that is why inserting a vertex
/// does not visibly change a path. Returns the new vertex index
/// (`segment + 1`), or `None` when the segment is out of range or `t` is not
/// strictly inside the segment.
pub fn insert_vertex(sub: &mut Subpath, segment: usize, t: f32) -> Option<usize> {
    if !t.is_finite() || t <= 0.0 || t >= 1.0 {
        return None;
    }
    let seg = *sub.segs.get(segment)?;
    let start = segment_start(sub, segment);
    let lerp = |a: Point, b: Point| Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    let (first, second) = match seg {
        Seg::Line(end) => {
            let mid = lerp(start, end);
            (Seg::Line(mid), Seg::Line(end))
        }
        Seg::Cubic { c1, c2, to } => {
            let q0 = lerp(start, c1);
            let q1 = lerp(c1, c2);
            let q2 = lerp(c2, to);
            let r0 = lerp(q0, q1);
            let r1 = lerp(q1, q2);
            let mid = lerp(r0, r1);
            (
                Seg::Cubic {
                    c1: q0,
                    c2: r0,
                    to: mid,
                },
                Seg::Cubic { c1: r1, c2: q2, to },
            )
        }
    };
    sub.segs[segment] = first;
    sub.segs.insert(segment + 1, second);
    Some(segment + 1)
}

/// Deletes a vertex, joining its two neighbours.
///
/// The join is a straight line, except when *both* sides were cubics — then the
/// outer handles are kept, which preserves the curve the user drew through the
/// deleted point. At an open subpath's end there is no join to make, so the
/// segment that ended at the removed vertex goes with it.
///
/// Returns `false` for an out-of-range vertex, or when the deletion would leave
/// the subpath with no segments at all (delete the node instead — a path of one
/// point has no meaning, and silently dropping the node would be worse than
/// refusing).
pub fn delete_vertex(sub: &mut Subpath, vertex: usize) -> bool {
    let count = vertex_count(sub);
    if vertex >= count || sub.segs.is_empty() {
        return false;
    }
    let segments = sub.segs.len();
    if segments <= 1 {
        // Every deletion removes at least one segment, so a single-segment
        // subpath would be left as a bare point. Delete the node instead.
        return false;
    }
    if sub.closed {
        match vertex {
            // Deleting the start: the path starts at the old vertex 1 and the
            // implicit close edge (a straight line) joins the last vertex to it.
            0 => {
                let new_start = sub.segs.remove(0).end();
                sub.start = new_start;
            }
            // Deleting the last vertex: dropping its segment leaves the implicit
            // close edge to join the previous vertex to the start.
            v if v == segments => {
                sub.segs.pop();
            }
            v => {
                join(sub, v - 1);
            }
        }
        return true;
    }
    match vertex {
        0 => {
            let new_start = sub.segs.remove(0).end();
            sub.start = new_start;
        }
        v if v == segments => {
            sub.segs.pop();
        }
        v => {
            join(sub, v - 1);
        }
    }
    true
}

/// Replaces segments `at` and `at + 1` with one segment across them.
fn join(sub: &mut Subpath, at: usize) {
    let first = sub.segs[at];
    let second = sub.segs[at + 1];
    let merged = match (first, second) {
        (Seg::Cubic { c1, .. }, Seg::Cubic { c2, to, .. }) => Seg::Cubic { c1, c2, to },
        _ => Seg::Line(second.end()),
    };
    sub.segs[at] = merged;
    sub.segs.remove(at + 1);
}

/// Squared distance from `p` to the segment `a → b`, with the projection
/// parameter, used by the pointer-over-a-vertex test.
fn segment_projection(p: Point, a: Point, b: Point) -> (f32, f32) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length2 = dx * dx + dy * dy;
    if length2 <= f32::EPSILON {
        return (0.0, p.distance(a));
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2;
    let t_clamped = t.clamp(0.0, 1.0);
    let closest = Point::new(a.x + dx * t_clamped, a.y + dy * t_clamped);
    (t, p.distance(closest))
}

/// What a pointer is hovering over, in local path coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitPoint {
    /// A vertex, at its position.
    Vertex(VertexRef, Point),
    /// A cubic segment's handle, at its position.
    Handle(HandleRef, Point),
    /// Somewhere along a segment: the split a click there would insert.
    Segment(SegmentRef, f32, Point),
}

/// Finds the closest editable point of `path` to `p`, within `tolerance`.
///
/// Handles win over vertices only when they are strictly closer; a segment hit
/// is the fallback that makes a double-click insert a vertex where the user
/// clicked. The search order is fixed (subpaths, then segments, then within a
/// segment: start, c1, c2, end) so the answer is deterministic for equal
/// distances — the tie-break matters because a cubic's handle can sit exactly on
/// its anchor.
#[must_use]
pub fn hit_point(path: &[Subpath], p: Point, tolerance: f32) -> Option<HitPoint> {
    // Two tiers, not one: a vertex or handle within reach always wins over a
    // segment, however close the segment's outline happens to be. A segment is
    // only "grabbed" when the pointer is not on a point — that is the rule that
    // makes dragging a corner feel attached to the corner.
    let reach = tolerance.max(0.0);
    let mut best_point: Option<HitPoint> = None;
    let mut best_point_distance = reach;
    let mut best_segment: Option<HitPoint> = None;
    let mut best_segment_distance = reach;
    let mut consider = |candidate: HitPoint, distance: f32| match candidate {
        HitPoint::Segment(..) => {
            if distance < best_segment_distance {
                best_segment_distance = distance;
                best_segment = Some(candidate);
            }
        }
        _ => {
            if distance < best_point_distance {
                best_point_distance = distance;
                best_point = Some(candidate);
            }
        }
    };
    for (si, sub) in path.iter().enumerate() {
        if let Some(start) = vertex_point(sub, 0) {
            let d = start.distance(p);
            consider(HitPoint::Vertex(VertexRef::new(si, 0), start), d);
        }
        for (gi, seg) in sub.segs.iter().enumerate() {
            let start = segment_start(sub, gi);
            let end = seg.end();
            if let Seg::Cubic { c1, c2, .. } = seg {
                consider(
                    HitPoint::Handle(HandleRef::new(si, gi, Handle::C1), *c1),
                    c1.distance(p),
                );
                consider(
                    HitPoint::Handle(HandleRef::new(si, gi, Handle::C2), *c2),
                    c2.distance(p),
                );
            }
            consider(
                HitPoint::Vertex(VertexRef::new(si, gi + 1), end),
                end.distance(p),
            );
            let (t, distance) = segment_projection(p, start, end);
            // A click on the segment (rather than on a point) inserts a vertex
            // there; the parameter is clamped away from the ends so the insert
            // is always a real split.
            let t = t.clamp(0.05, 0.95);
            let on = Point::new(
                start.x + (end.x - start.x) * t,
                start.y + (end.y - start.y) * t,
            );
            // Straight-line distance is a poor guide next to a curve, so a
            // cubic uses the curve's own sampled distance to the outline.
            let distance = if matches!(seg, Seg::Cubic { .. }) {
                sub.distance_to(p)
            } else {
                distance
            };
            consider(HitPoint::Segment(SegmentRef::new(si, gi), t, on), distance);
        }
    }
    best_point.or(best_segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f32) -> Subpath {
        let mut sub = Subpath::new(Point::new(0.0, 0.0));
        sub.push_line(Point::new(size, 0.0));
        sub.push_line(Point::new(size, size));
        sub.push_line(Point::new(0.0, size));
        sub.closed = true;
        sub
    }

    fn curve() -> Subpath {
        let mut sub = Subpath::new(Point::new(0.0, 0.0));
        sub.push_line(Point::new(10.0, 0.0));
        sub.push_cubic(
            Point::new(20.0, 0.0),
            Point::new(20.0, 10.0),
            Point::new(10.0, 10.0),
        );
        sub
    }

    #[test]
    fn vertices_are_addressed_from_the_start() {
        let sub = curve();
        assert_eq!(vertex_count(&sub), 3);
        assert_eq!(vertex_point(&sub, 0), Some(Point::new(0.0, 0.0)));
        assert_eq!(vertex_point(&sub, 1), Some(Point::new(10.0, 0.0)));
        assert_eq!(vertex_point(&sub, 2), Some(Point::new(10.0, 10.0)));
        assert_eq!(vertex_point(&sub, 3), None);
        assert_eq!(segment_start(&sub, 0), Point::new(0.0, 0.0));
        assert_eq!(segment_start(&sub, 1), Point::new(10.0, 0.0));
    }

    #[test]
    fn moving_an_anchor_drags_its_handles() {
        let mut sub = curve();
        assert!(move_vertex(&mut sub, 1, Point::new(10.0, 2.0)));
        // The anchor moved by (0, 2): the incoming line has no handle, and the
        // outgoing cubic's c1 came along.
        assert_eq!(vertex_point(&sub, 1), Some(Point::new(10.0, 2.0)));
        match sub.segs[1] {
            Seg::Cubic { c1, c2, to } => {
                assert_eq!(c1, Point::new(20.0, 2.0));
                assert_eq!(c2, Point::new(20.0, 10.0));
                assert_eq!(to, Point::new(10.0, 10.0));
            }
            Seg::Line(_) => panic!("the cubic became a line"),
        }

        // Moving the end anchor drags the incoming c2 only.
        assert!(move_vertex(&mut sub, 2, Point::new(12.0, 10.0)));
        match sub.segs[1] {
            Seg::Cubic { c1, c2, to } => {
                assert_eq!(c1, Point::new(20.0, 2.0));
                assert_eq!(c2, Point::new(22.0, 10.0));
                assert_eq!(to, Point::new(12.0, 10.0));
            }
            Seg::Line(_) => panic!("the cubic became a line"),
        }

        // …and the start anchor drags the first segment's c1.
        let mut sub = curve();
        assert!(move_vertex(&mut sub, 0, Point::new(-1.0, -1.0)));
        assert_eq!(sub.start, Point::new(-1.0, -1.0));
        assert_eq!(sub.segs[0], Seg::Line(Point::new(10.0, 0.0)));

        assert!(!move_vertex(&mut sub, 9, Point::new(0.0, 0.0)));
        assert!(!move_vertex(&mut sub, 0, Point::new(f32::NAN, 0.0)));
    }

    #[test]
    fn the_start_of_a_closed_subpath_has_no_incoming_handle() {
        let mut sub = Subpath::new(Point::new(0.0, 0.0));
        sub.push_cubic(
            Point::new(4.0, 0.0),
            Point::new(4.0, 4.0),
            Point::new(0.0, 4.0),
        );
        sub.closed = true;
        assert!(move_vertex(&mut sub, 0, Point::new(-1.0, 0.0)));
        match sub.segs[0] {
            Seg::Cubic { c1, c2, to } => {
                assert_eq!(c1, Point::new(3.0, 0.0)); // dragged with the anchor
                assert_eq!(c2, Point::new(4.0, 4.0)); // untouched
                assert_eq!(to, Point::new(0.0, 4.0));
            }
            Seg::Line(_) => panic!("the cubic became a line"),
        }
        // The last vertex's outgoing edge is the implicit close, so only its
        // incoming handle comes along (by the +0.5 in x).
        let last = vertex_count(&sub) - 1;
        assert!(move_vertex(&mut sub, last, Point::new(0.5, 4.0)));
        match sub.segs[0] {
            Seg::Cubic { c2, to, .. } => {
                assert_eq!(c2, Point::new(4.5, 4.0));
                assert_eq!(to, Point::new(0.5, 4.0));
            }
            Seg::Line(_) => panic!("the cubic became a line"),
        }
    }

    #[test]
    fn handles_move_on_their_own_and_only_on_cubics() {
        let mut sub = curve();
        assert!(move_handle(&mut sub, 1, Handle::C1, Point::new(30.0, -5.0)));
        match sub.segs[1] {
            Seg::Cubic { c1, .. } => assert_eq!(c1, Point::new(30.0, -5.0)),
            Seg::Line(_) => panic!("the cubic became a line"),
        }
        assert!(!move_handle(&mut sub, 0, Handle::C1, Point::new(1.0, 1.0)));
        assert!(!move_handle(&mut sub, 5, Handle::C1, Point::new(1.0, 1.0)));
    }

    #[test]
    fn a_line_converts_to_the_cubic_that_is_that_line() {
        let mut sub = square(10.0);
        assert!(!set_segment_kind(&mut sub, 0, SegKind::Line));
        assert!(set_segment_kind(&mut sub, 0, SegKind::Cubic));
        assert_eq!(SegKind::of(&sub.segs[0]), SegKind::Cubic);
        match sub.segs[0] {
            Seg::Cubic { c1, c2, to } => {
                assert_eq!(c1, Point::new(10.0 / 3.0, 0.0));
                assert_eq!(c2, Point::new(20.0 / 3.0, 0.0));
                assert_eq!(to, Point::new(10.0, 0.0));
            }
            Seg::Line(_) => panic!("still a line"),
        }
        assert!(set_segment_kind(&mut sub, 0, SegKind::Line));
        assert_eq!(sub.segs[0], Seg::Line(Point::new(10.0, 0.0)));
        assert!(!set_segment_kind(&mut sub, 9, SegKind::Cubic));
    }

    #[test]
    fn inserting_a_vertex_on_a_line_splits_it_exactly() {
        let mut sub = square(10.0);
        assert_eq!(insert_vertex(&mut sub, 0, 0.25), Some(1));
        assert_eq!(sub.segs[0], Seg::Line(Point::new(2.5, 0.0)));
        assert_eq!(sub.segs[1], Seg::Line(Point::new(10.0, 0.0)));
        assert_eq!(vertex_count(&sub), 5);
        // Only parameters strictly inside the segment are a split.
        assert_eq!(insert_vertex(&mut sub, 0, 0.0), None);
        assert_eq!(insert_vertex(&mut sub, 0, 1.0), None);
        assert_eq!(insert_vertex(&mut sub, 0, f32::NAN), None);
        assert_eq!(insert_vertex(&mut sub, 99, 0.5), None);
    }

    #[test]
    fn inserting_a_vertex_on_a_cubic_keeps_the_curve() {
        let mut sub = curve();
        let before = sub.flatten();
        assert_eq!(insert_vertex(&mut sub, 1, 0.5), Some(2));
        assert_eq!(vertex_count(&sub), 4);
        // Both halves are cubics, and the curve is where it was: every sampled
        // point of the split path is within a hair of the original.
        assert!(matches!(sub.segs[1], Seg::Cubic { .. }));
        assert!(matches!(sub.segs[2], Seg::Cubic { .. }));
        // Every point of the split path lies on the original curve — that is
        // what "the insert did not move the shape" means.
        let original = curve();
        let after = sub.flatten();
        for (index, point) in after.iter().enumerate() {
            assert!(
                original.distance_to(*point) < 0.05,
                "point {index} drifted off the curve: {point:?} (distance {})",
                original.distance_to(*point)
            );
        }
        assert_eq!(before.len(), curve().flatten().len());
    }

    #[test]
    fn deleting_an_inner_vertex_joins_with_a_line() {
        let mut sub = Subpath::new(Point::new(0.0, 0.0));
        sub.push_line(Point::new(10.0, 0.0));
        sub.push_line(Point::new(10.0, 10.0));
        sub.push_line(Point::new(0.0, 10.0));
        assert!(delete_vertex(&mut sub, 2));
        assert_eq!(sub.segs.len(), 2);
        assert_eq!(sub.segs[0], Seg::Line(Point::new(10.0, 0.0)));
        assert_eq!(sub.segs[1], Seg::Line(Point::new(0.0, 10.0)));
        assert_eq!(vertex_count(&sub), 3);
    }

    #[test]
    fn deleting_between_two_cubics_keeps_the_outer_handles() {
        let mut sub = Subpath::new(Point::new(0.0, 0.0));
        sub.push_cubic(
            Point::new(3.0, 0.0),
            Point::new(7.0, 0.0),
            Point::new(10.0, 0.0),
        );
        sub.push_cubic(
            Point::new(13.0, 0.0),
            Point::new(17.0, 0.0),
            Point::new(20.0, 0.0),
        );
        assert!(delete_vertex(&mut sub, 1));
        assert_eq!(sub.segs.len(), 1);
        assert_eq!(
            sub.segs[0],
            Seg::Cubic {
                c1: Point::new(3.0, 0.0),
                c2: Point::new(17.0, 0.0),
                to: Point::new(20.0, 0.0),
            }
        );
    }

    #[test]
    fn deleting_a_closed_subpaths_start_or_end_keeps_it_closed() {
        let mut sub = square(10.0);
        assert!(delete_vertex(&mut sub, 0));
        assert!(sub.closed);
        assert_eq!(sub.start, Point::new(10.0, 0.0));
        assert_eq!(vertex_count(&sub), 3);
        assert_eq!(sub.segs.len(), 2);

        let mut sub = square(10.0);
        let last = vertex_count(&sub) - 1;
        assert!(delete_vertex(&mut sub, last));
        assert_eq!(sub.segs.len(), 2);
        assert!(sub.closed);
    }

    #[test]
    fn deleting_an_open_subpaths_endpoint_drops_its_segment() {
        let mut sub = Subpath::new(Point::new(0.0, 0.0));
        sub.push_line(Point::new(10.0, 0.0));
        sub.push_line(Point::new(10.0, 10.0));
        sub.push_line(Point::new(0.0, 10.0));
        // The start goes and the segment that left it goes with it.
        assert!(delete_vertex(&mut sub, 0));
        assert_eq!(sub.start, Point::new(10.0, 0.0));
        assert_eq!(sub.segs.len(), 2);
        // The end goes the same way.
        let last = vertex_count(&sub) - 1;
        assert!(delete_vertex(&mut sub, last));
        assert_eq!(sub.segs.len(), 1);
        assert_eq!(sub.segs[0], Seg::Line(Point::new(10.0, 10.0)));
        // A one-segment subpath cannot give an endpoint up: that would leave a
        // bare point behind. (Delete the node instead.)
        assert!(!delete_vertex(&mut sub, 0));
        assert!(!delete_vertex(&mut sub, 1));
    }

    #[test]
    fn editing_refuses_out_of_range_addresses() {
        let mut sub = square(10.0);
        assert!(!delete_vertex(&mut sub, 99));
        assert_eq!(insert_vertex(&mut sub, 99, 0.5), None);
        assert!(!set_segment_kind(&mut sub, 99, SegKind::Cubic));
        assert!(!move_handle(&mut sub, 99, Handle::C2, Point::new(0.0, 0.0)));
    }

    #[test]
    fn hit_testing_prefers_the_closest_point_and_falls_back_to_the_segment() {
        let path = vec![square(10.0)];
        // On a vertex.
        match hit_point(&path, Point::new(10.2, 0.1), 1.0) {
            Some(HitPoint::Vertex(v, _)) => assert_eq!(v, VertexRef::new(0, 1)),
            other => panic!("expected the vertex, got {other:?}"),
        }
        // On a handle, which wins when it is strictly closer.
        let path = vec![curve()];
        match hit_point(&path, Point::new(20.1, 0.0), 1.0) {
            Some(HitPoint::Handle(h, _)) => {
                assert_eq!(h, HandleRef::new(0, 1, Handle::C1));
            }
            other => panic!("expected the handle, got {other:?}"),
        }
        // Mid-segment: an insert.
        match hit_point(&path, Point::new(0.0, 0.0), 0.0) {
            None => {}
            other => panic!("expected no hit at zero tolerance, got {other:?}"),
        }
        let square_path = vec![square(10.0)];
        match hit_point(&square_path, Point::new(5.0, 0.1), 1.0) {
            Some(HitPoint::Segment(s, t, _)) => {
                assert_eq!(s, SegmentRef::new(0, 0));
                assert!((t - 0.5).abs() < 1e-3);
            }
            other => panic!("expected a segment hit, got {other:?}"),
        }
        // Nothing in reach.
        assert!(hit_point(&square_path, Point::new(100.0, 100.0), 1.0).is_none());
    }
}
