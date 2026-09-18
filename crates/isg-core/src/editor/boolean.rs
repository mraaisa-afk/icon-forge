//! Path booleans (pathfinder): union, subtract, intersect and exclude.
//!
//! ## How it works
//!
//! Every operation is the same three steps on flattened outlines:
//!
//! 1. **Flatten** each operand's subpaths into closed polygons (cubics sampled
//!    at [`CURVE_STEPS`], the same fixed sampling every other editor query uses,
//!    so the result is deterministic).
//! 2. **Split** every edge of every polygon at each intersection with any other
//!    polygon, rounding the split points to a grid of `QUANT` units first. The
//!    rounding is what makes the construction robust: after it, two edges either
//!    share exactly the same vertex or do not touch at all, so the stitcher
//!    never has to reason about near-misses.
//! 3. **Classify and stitch**: each resulting edge is tested at its midpoint
//!    against *both* operands (winding number), kept when the boolean's
//!    inside-test says so, then chained end-to-end into rings. Because every
//!    kept edge is inside the result, the chain is forced — a vertex can start a
//!    ring but never a branch.
//!
//! ## What it deliberately is not
//!
//! The result is a polygon soup: curves come back as dense polylines, exactly as
//! Inkscape's pathfinder and Illustrator's "Pathfinder" effects produce unless
//! you ask otherwise. Re-fitting curves to the output is a later refinement, not
//! a correctness property, and pretending to fit them would be worse than
//! keeping the geometry the operation actually computed.
//!
//! The kernel is also honest about its limits: a self-intersecting *operand* is
//! not resolved (only crossings *between* operands split edges), which is why
//! [`union_all`] refuses rather than guessing when the stitcher ends up with
//! leftover edges.

use super::geom::{Point, Subpath};

/// Coordinates are rounded to this many units before intersecting, so that
/// coincident points from different edges become the *same* point exactly.
pub const QUANT: f32 = 1024.0;

/// Winding-number parity: the rule the canvas fills with (`fill("evenodd")`),
/// so what the boolean sees is what the user saw.
#[derive(Clone, Debug)]
struct Operand {
    rings: Vec<Vec<Point>>,
}

impl Operand {
    fn from_path(path: &[Subpath]) -> Self {
        Self {
            rings: path
                .iter()
                .filter_map(|sub| {
                    let polygon = sub.polygon();
                    (polygon.len() >= 3).then_some(polygon)
                })
                .collect(),
        }
    }

    /// Crossing-count parity of a point against this operand.
    ///
    /// Deliberately *strict*: a point exactly on the boundary is not treated as
    /// "covered", because the classifier is only ever asked about points offset
    /// to one side of an edge (see [`SIDE_OFFSET`]). An earlier version returned
    /// `true` for a point on the boundary, which made a shared edge classify as
    /// "inside the other operand" — and a union then dropped the shared run
    /// instead of keeping it, tearing the outline open.
    fn covers(&self, p: Point) -> bool {
        let mut inside = false;
        for ring in &self.rings {
            let n = ring.len();
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let crosses = (a.y > p.y) != (b.y > p.y);
                if crosses {
                    let t = (p.y - a.y) / (b.y - a.y);
                    if a.x + t * (b.x - a.x) > p.x {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }
}

/// How far to each side of an edge the classifier samples, in document units.
///
/// Two quantisation steps: far enough that the sample is not rounded back onto
/// the edge, close enough that it stays inside the region the edge borders.
pub const SIDE_OFFSET: f32 = 2.0 / QUANT;

/// The unit normal of a directed edge, scaled to [`SIDE_OFFSET`].
fn side_offset(a: Point, b: Point) -> Point {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1e-9 {
        return Point::new(0.0, 0.0);
    }
    Point::new(-dy / length * SIDE_OFFSET, dx / length * SIDE_OFFSET)
}

fn quantise(p: Point) -> Point {
    Point::new((p.x * QUANT).round() / QUANT, (p.y * QUANT).round() / QUANT)
}

/// Splits every edge of `edges` at each of its intersections with any edge,
/// in one pass over a snapshot of the input. Returns how many pieces were added.
///
/// The source flag travels with each piece: a split does not change which
/// operand an edge came from, and the classification step needs that.
fn split_pass(edges: &mut Vec<([Point; 2], bool)>) -> usize {
    let snapshot = edges.clone();
    let mut out: Vec<([Point; 2], bool)> = Vec::with_capacity(snapshot.len());
    let mut added = 0;
    for (index, (edge, source)) in snapshot.iter().enumerate() {
        let (a1, a2) = (edge[0], edge[1]);
        let mut cuts: Vec<f32> = Vec::new();
        for (other_index, (other, _)) in snapshot.iter().enumerate() {
            if index == other_index {
                continue;
            }
            if let Some(t) = intersect_t(a1, a2, other[0], other[1]) {
                // A crossing at an end point needs no split: the edge already
                // ends there (after quantisation the points are equal).
                if t > 1e-6 && t < 1.0 - 1e-6 {
                    cuts.push(t);
                }
            }
            // Collinear overlap: the *ends* of the shared run are split points.
            // Without them an edge that partly overlaps another is never cut,
            // and classification then drops the whole thing instead of the
            // shared part — which tears a hole in the outline.
            cuts.extend(collinear_cuts(a1, a2, other[0], other[1]));
        }
        if cuts.is_empty() {
            out.push((*edge, *source));
            continue;
        }
        cuts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        cuts.dedup_by(|x, y| (*x - *y).abs() < 1e-6);
        let mut previous = 0.0;
        for t in cuts.iter().copied().chain(std::iter::once(1.0)) {
            if t - previous > 1e-6 {
                // Quantise the new ends: the two edges that cross must land on
                // *the same* vertex for the chain to close, and they compute
                // that vertex from different parameters.
                out.push((
                    [quantise(lerp(a1, a2, previous)), quantise(lerp(a1, a2, t))],
                    *source,
                ));
            }
            previous = t;
        }
        added += cuts.len();
    }
    *edges = out;
    added
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Parameter along `a1 → a2` where the closed segments cross, or `None`.
///
/// Endpoint touches count (they are how two paths that share a corner meet);
/// parallel and collinear overlaps do not produce a split, because a shared edge
/// is resolved by the classification step instead.
fn intersect_t(a1: Point, a2: Point, b1: Point, b2: Point) -> Option<f32> {
    let d1 = Point::new(a2.x - a1.x, a2.y - a1.y);
    let d2 = Point::new(b2.x - b1.x, b2.y - b1.y);
    let denom = d1.x * d2.y - d1.y * d2.x;
    if denom.abs() < 1e-9 {
        return None;
    }
    let offset = Point::new(b1.x - a1.x, b1.y - a1.y);
    let t = (offset.x * d2.y - offset.y * d2.x) / denom;
    let u = (offset.x * d1.y - offset.y * d1.x) / denom;
    if !(-1e-6..=1.0 + 1e-6).contains(&t) || !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let t = t.clamp(0.0, 1.0);
    // Whether the crossing is inside both segments (an end touch is fine too).
    let p = lerp(a1, a2, t);
    let q = lerp(b1, b2, u.clamp(0.0, 1.0));
    (p.distance(q) <= 1.0 / QUANT).then_some(t)
}

/// Parameters where another collinear segment's end points fall inside this
/// segment, so a partially shared run gets split at both ends.
fn collinear_cuts(a1: Point, a2: Point, b1: Point, b2: Point) -> Vec<f32> {
    let d = Point::new(a2.x - a1.x, a2.y - a1.y);
    let length2 = d.x * d.x + d.y * d.y;
    if length2 < 1e-12 {
        return Vec::new();
    }
    let cross = |p: Point| d.x * (p.y - a1.y) - d.y * (p.x - a1.x);
    let offset = |p: Point| p.distance(a1);
    let parallel = {
        let e = Point::new(b2.x - b1.x, b2.y - b1.y);
        (d.x * e.y - d.y * e.x).abs() < 1e-6
    };
    let length = length2.sqrt();
    if !parallel || cross(b1).abs() / length > 0.5 / QUANT || cross(b2).abs() / length > 0.5 / QUANT
    {
        return Vec::new();
    }
    // Both end points are on this line: project them onto it.
    let project = |p: Point| {
        let dx = p.x - a1.x;
        let dy = p.y - a1.y;
        (dx * d.x + dy * d.y) / length2
    };
    let mut cuts = Vec::new();
    for t in [project(b1), project(b2)] {
        if t > 1e-6 && t < 1.0 - 1e-6 {
            let at = lerp(a1, a2, t);
            // Only a real overlap counts: a collinear segment that merely
            // touches this one at an end point does not need a split.
            if (offset(b1).min(offset(b2))..=offset(b1).max(offset(b2))).contains(&offset(at)) {
                cuts.push(t);
            }
        }
    }
    cuts
}

/// Chains kept edges into closed rings, snapping ends onto shared vertices.
fn stitch(edges: Vec<[Point; 2]>) -> Option<Vec<Vec<Point>>> {
    let mut rings: Vec<Vec<Point>> = Vec::new();
    let mut used = vec![false; edges.len()];
    for start in 0..edges.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut ring = vec![edges[start][0], edges[start][1]];
        let mut current = edges[start][1];
        let first = edges[start][0];
        loop {
            if current == first {
                break;
            }
            let mut next: Option<usize> = None;
            for (index, edge) in edges.iter().enumerate() {
                if used[index] {
                    continue;
                }
                if edge[0] == current {
                    next = Some(index);
                    break;
                }
                if edge[1] == current {
                    // A reversed edge (possible when two operands share an
                    // edge): walk it backwards.
                    next = Some(index);
                    break;
                }
            }
            let index = next?;
            used[index] = true;
            let edge = edges[index];
            if edge[0] == current {
                ring.push(edge[1]);
                current = edge[1];
            } else {
                ring.push(edge[0]);
                current = edge[0];
            }
        }
        if ring.len() >= 4 {
            ring.pop(); // the closing point repeats the first
            rings.push(ring);
        }
    }
    Some(rings)
}

/// Whether a point is in the result, given whether it is in each operand.
type Membership = fn(bool, bool) -> bool;

/// Computes one boolean of two paths.
///
/// An edge survives when the result's membership changes across it — read by
/// sampling a point just off each side, so the classification never has to
/// decide whether a point *on* a boundary counts as inside.
#[must_use]
pub fn boolean(first: &[Subpath], second: &[Subpath], keep: Membership) -> Option<Vec<Subpath>> {
    let lhs = Operand::from_path(first);
    let rhs = Operand::from_path(second);
    if lhs.rings.is_empty() || rhs.rings.is_empty() {
        return None;
    }
    let mut edges: Vec<([Point; 2], bool)> = Vec::new();
    for (operand, is_first) in [(&lhs, true), (&rhs, false)] {
        for ring in &operand.rings {
            for i in 0..ring.len() {
                let a = quantise(ring[i]);
                let b = quantise(ring[(i + 1) % ring.len()]);
                if a != b {
                    edges.push(([a, b], is_first));
                }
            }
        }
    }
    // Settle the split until nothing more can be cut: a new piece can cross an
    // edge another piece crossed (a grid of many overlapping shapes), so this
    // loops to a fixed point rather than assuming one pass suffices.
    loop {
        let added = split_pass(&mut edges);
        if added == 0 {
            break;
        }
        if edges.len() > 200_000 {
            return None; // pathological input; refuse rather than grind
        }
    }
    let mut kept: Vec<[Point; 2]> = Vec::new();
    for ([a, b], _) in edges {
        let mid = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let n = side_offset(a, b);
        // The edge is on the result's boundary exactly when the result's
        // membership differs between its two sides. Sampling *both* sides is
        // what makes edges that coincide with the other operand's boundary come
        // out right: a midpoint test cannot tell "inside" from "on the line",
        // and the answer differs (a shared run is on the union's boundary but
        // interior to the intersection).
        let left = Point::new(mid.x + n.x, mid.y + n.y);
        let right = Point::new(mid.x - n.x, mid.y - n.y);
        let left_in = keep(lhs.covers(left), rhs.covers(left));
        let right_in = keep(lhs.covers(right), rhs.covers(right));
        if left_in != right_in {
            kept.push([a, b]);
        }
    }
    // Coincident pieces: a shared run is kept from both operands, and the ring
    // walk must see it once. Duplicates are dropped by unordered point pair, so
    // it does not matter which copy (or which direction) survives.
    let mut unique: Vec<[Point; 2]> = Vec::with_capacity(kept.len());
    for edge in kept {
        let (key, reverse) = ((edge[0], edge[1]), (edge[1], edge[0]));
        if unique
            .iter()
            .any(|existing| (*existing == [key.0, key.1]) || (*existing == [reverse.0, reverse.1]))
        {
            continue;
        }
        unique.push(edge);
    }
    let kept = unique;
    if kept.is_empty() {
        return Some(Vec::new());
    }
    let rings = stitch(kept)?;
    let mut path = Vec::with_capacity(rings.len());
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let mut sub = Subpath::new(ring[0]);
        for point in &ring[1..] {
            sub.push_line(*point);
        }
        sub.closed = true;
        path.push(sub);
    }
    Some(path)
}

/// Union of two paths.
#[must_use]
pub fn union(first: &[Subpath], second: &[Subpath]) -> Option<Vec<Subpath>> {
    boolean(first, second, |a, b| a || b)
}

/// `first` with `second` cut out of it.
#[must_use]
pub fn subtract(first: &[Subpath], second: &[Subpath]) -> Option<Vec<Subpath>> {
    boolean(first, second, |a, b| a && !b)
}

/// Only what both cover.
#[must_use]
pub fn intersect(first: &[Subpath], second: &[Subpath]) -> Option<Vec<Subpath>> {
    boolean(first, second, |a, b| a && b)
}

/// What exactly one of them covers (`A ∪ B` minus `A ∩ B`).
///
/// Written as that two-step rather than as one pass with `a != b`: the symmetric
/// difference's boundary is *every* edge of both operands, so at a crossing
/// point four kept edges meet and the chain has to guess which pair continues —
/// two unions and an intersection keep the stitcher's invariant that a kept
/// vertex is shared by exactly two edges, which is what makes it total.
#[must_use]
pub fn exclude(first: &[Subpath], second: &[Subpath]) -> Option<Vec<Subpath>> {
    let together = union(first, second)?;
    let overlap = intersect(first, second)?;
    if overlap.is_empty() {
        return Some(together);
    }
    subtract(&together, &overlap)
}

/// Folds a list of paths under one operation, left to right.
///
/// Returns `None` when the stitcher cannot close a result (see the module docs),
/// which the caller reports as a refusal rather than a corrupt node.
#[must_use]
pub fn fold(paths: &[Vec<Subpath>], op: super::command::BooleanOp) -> Option<Vec<Subpath>> {
    let mut iter = paths.iter();
    let mut acc = iter.next()?.clone();
    for next in iter {
        acc = match op {
            super::command::BooleanOp::Union => union(&acc, next)?,
            super::command::BooleanOp::Subtract => subtract(&acc, next)?,
            super::command::BooleanOp::Intersect => intersect(&acc, next)?,
            super::command::BooleanOp::Exclude => exclude(&acc, next)?,
        };
        if acc.is_empty() {
            // Nothing left (e.g. subtracting everything away): an empty result
            // is a legitimate answer, and folding further cannot improve it.
            return Some(Vec::new());
        }
    }
    Some(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned square, as a closed subpath of four lines.
    fn rect(x0: f32, y0: f32, w: f32, h: f32) -> Vec<Subpath> {
        let mut sub = Subpath::new(Point::new(x0, y0));
        sub.push_line(Point::new(x0 + w, y0));
        sub.push_line(Point::new(x0 + w, y0 + h));
        sub.push_line(Point::new(x0, y0 + h));
        sub.closed = true;
        vec![sub]
    }

    /// Area of a path as the canvas fills it: even-odd, so a ring inside
    /// another ring is a hole and subtracts.
    fn area(path: &[Subpath]) -> f32 {
        let rings: Vec<Vec<Point>> = path
            .iter()
            .map(|sub| sub.polygon())
            .filter(|ring| ring.len() >= 3)
            .collect();
        let mut total = 0.0;
        for (index, ring) in rings.iter().enumerate() {
            let mut sum = 0.0;
            for i in 0..ring.len() {
                let a = ring[i];
                let b = ring[(i + 1) % ring.len()];
                sum += a.x * b.y - b.x * a.y;
            }
            let signed = sum / 2.0;
            // Depth = how many *other* rings contain this one's first vertex.
            let probe = ring[0];
            let mut depth = 0;
            for (other_index, other) in rings.iter().enumerate() {
                if other_index == index {
                    continue;
                }
                if ring_covers(other, probe) {
                    depth += 1;
                }
            }
            total += if depth % 2 == 0 {
                signed.abs()
            } else {
                -signed.abs()
            };
        }
        total
    }

    /// Even-odd containment for the area helper.
    fn ring_covers(ring: &[Point], p: Point) -> bool {
        let mut inside = false;
        for i in 0..ring.len() {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            if (a.y > p.y) != (b.y > p.y) {
                let t = (p.y - a.y) / (b.y - a.y);
                if a.x + t * (b.x - a.x) > p.x {
                    inside = !inside;
                }
            }
        }
        inside
    }

    fn bounds(path: &[Subpath]) -> (f32, f32, f32, f32) {
        let mut lo = (f32::INFINITY, f32::INFINITY);
        let mut hi = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for sub in path {
            for point in sub.polygon() {
                lo = (lo.0.min(point.x), lo.1.min(point.y));
                hi = (hi.0.max(point.x), hi.1.max(point.y));
            }
        }
        (lo.0, lo.1, hi.0, hi.1)
    }

    #[test]
    fn union_of_two_overlapping_squares_is_their_combined_area() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let result = union(&a, &b).expect("stitched");
        assert!(
            (area(&result) - 175.0).abs() < 1e-2,
            "area {}",
            area(&result)
        );
        // The union reaches both far corners and no further.
        let (x0, y0, x1, y1) = bounds(&result);
        assert!((x0 - 0.0).abs() < 1e-3 && (y0 - 0.0).abs() < 1e-3);
        assert!((x1 - 15.0).abs() < 1e-3 && (y1 - 15.0).abs() < 1e-3);
    }

    #[test]
    fn intersect_of_two_overlapping_squares_is_the_overlap() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let result = intersect(&a, &b).expect("stitched");
        assert!(
            (area(&result) - 25.0).abs() < 1e-2,
            "area {}",
            area(&result)
        );
        let (x0, y0, x1, y1) = bounds(&result);
        assert!((x0 - 5.0).abs() < 1e-3 && (y0 - 5.0).abs() < 1e-3);
        assert!((x1 - 10.0).abs() < 1e-3 && (y1 - 10.0).abs() < 1e-3);
    }

    #[test]
    fn subtract_leaves_the_first_operand_minus_the_overlap() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let result = subtract(&a, &b).expect("stitched");
        assert!(
            (area(&result) - 75.0).abs() < 1e-2,
            "area {}",
            area(&result)
        );
        // The bite is a staircase: the result keeps (0,0) and loses (6,6).
        assert!(result[0].polygon().iter().any(|p| p.x == 0.0 && p.y == 0.0));
        assert!(!result[0]
            .polygon()
            .iter()
            .any(|p| p.x == 10.0 && p.y == 10.0));
    }

    #[test]
    fn exclude_drops_the_overlap_from_both_sides() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let result = exclude(&a, &b).expect("stitched");
        assert!(
            (area(&result) - 150.0).abs() < 1e-2,
            "area {}",
            area(&result)
        );
    }

    #[test]
    fn disjoint_operands_behave_the_obvious_way() {
        let a = rect(0.0, 0.0, 4.0, 4.0);
        let b = rect(10.0, 10.0, 4.0, 4.0);
        assert!((area(&union(&a, &b).unwrap()) - 32.0).abs() < 1e-2);
        assert!(intersect(&a, &b).unwrap().is_empty());
        assert!((area(&subtract(&a, &b).unwrap()) - 16.0).abs() < 1e-2);
        assert!((area(&exclude(&a, &b).unwrap()) - 32.0).abs() < 1e-2);
    }

    #[test]
    fn one_inside_the_other() {
        let outer = rect(0.0, 0.0, 20.0, 20.0);
        let inner = rect(5.0, 5.0, 5.0, 5.0);
        // A hole: the outer ring plus the inner ring (even-odd fill makes it a
        // hole on screen, which is what the canvas draws).
        let cut = subtract(&outer, &inner).unwrap();
        assert!((area(&cut) - 375.0).abs() < 1e-2, "area {}", area(&cut));
        assert_eq!(cut.len(), 2);
        // Union is just the outer, intersection just the inner.
        assert!((area(&union(&outer, &inner).unwrap()) - 400.0).abs() < 1e-2);
        assert!((area(&intersect(&outer, &inner).unwrap()) - 25.0).abs() < 1e-2);
    }

    #[test]
    fn shared_edges_and_corner_touches_stitch_cleanly() {
        // Two squares sharing a full edge: the union is one rectangle.
        let a = rect(0.0, 0.0, 5.0, 5.0);
        let b = rect(5.0, 0.0, 5.0, 5.0);
        let joined = union(&a, &b).expect("stitched");
        assert!(
            (area(&joined) - 50.0).abs() < 1e-2,
            "area {}",
            area(&joined)
        );
        // Touching at one corner only.
        let c = rect(5.0, 5.0, 5.0, 5.0);
        let touch = union(&a, &c).expect("stitched");
        assert!((area(&touch) - 50.0).abs() < 1e-2, "area {}", area(&touch));
        // …and their intersection is nothing (measure-zero contact).
        let nothing = intersect(&a, &c).expect("stitched");
        assert!(area(&nothing) < 1e-2, "area {}", area(&nothing));
    }

    #[test]
    fn curves_are_flattened_and_still_boolean() {
        // A circle-ish blob (a cubic ring) minus a square.
        let r = 10.0;
        let k = 0.5523 * r;
        let mut blob = Subpath::new(Point::new(r, 0.0));
        blob.push_cubic(
            Point::new(r + k, 0.0),
            Point::new(2.0 * r, r - k),
            Point::new(2.0 * r, r),
        );
        blob.push_cubic(
            Point::new(2.0 * r, r + k),
            Point::new(r + k, 2.0 * r),
            Point::new(r, 2.0 * r),
        );
        blob.push_cubic(
            Point::new(r - k, 2.0 * r),
            Point::new(0.0, r + k),
            Point::new(0.0, r),
        );
        blob.push_cubic(
            Point::new(0.0, r - k),
            Point::new(r - k, 0.0),
            Point::new(r, 0.0),
        );
        blob.closed = true;
        let disc = vec![blob];
        let disc_area = area(&disc);
        assert!(
            (disc_area - std::f32::consts::PI * r * r).abs() < 1.0,
            "{disc_area}"
        );

        let bite = rect(10.0, 0.0, 10.0, 20.0);
        let cut = subtract(&disc, &bite).expect("stitched");
        let half = disc_area / 2.0;
        assert!(
            (area(&cut) - half).abs() < 1.0,
            "area {} vs {half}",
            area(&cut)
        );
    }

    #[test]
    fn fold_combines_a_whole_selection() {
        // Three squares spanning x 0..10, 5..15 and 8..18.
        let paths = vec![
            rect(0.0, 0.0, 10.0, 10.0),
            rect(5.0, 0.0, 10.0, 10.0),
            rect(8.0, 0.0, 10.0, 10.0),
        ];
        let all = fold(&paths, super::super::command::BooleanOp::Union).unwrap();
        assert!((area(&all) - 180.0).abs() < 1e-2, "area {}", area(&all));
        // What all three share is x 8..10: two units wide, ten tall.
        let shared = fold(&paths, super::super::command::BooleanOp::Intersect).unwrap();
        assert!(
            (area(&shared) - 20.0).abs() < 1e-2,
            "area {}",
            area(&shared)
        );
        // Subtract folds the rest out of the first, leaving x 0..5.
        let left = fold(&paths, super::super::command::BooleanOp::Subtract).unwrap();
        assert!((area(&left) - 50.0).abs() < 1e-2, "area {}", area(&left));
    }

    #[test]
    fn an_operand_with_no_rings_is_refused_not_guessed() {
        assert!(union(&[], &rect(0.0, 0.0, 1.0, 1.0)).is_none());
        assert!(union(&rect(0.0, 0.0, 1.0, 1.0), &[]).is_none());
        assert!(fold(&[], super::super::command::BooleanOp::Union).is_none());
    }
}
