//! Editor geometry: points, path segments and the queries the canvas needs.

use super::affine::Affine;

/// How many straight segments a cubic is sampled into for hit-testing,
/// bounds and the pointer-over-outline test.
///
/// A fixed sample count (rather than an adaptive tolerance) keeps every
/// geometric query byte-deterministic, which the editor's undo invariant and
/// the renderer's caches both rely on.
pub const CURVE_STEPS: u32 = 16;

/// A point in document space (y down, origin top-left — the sheet convention).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl Point {
    /// Builds a point.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Euclidean distance to another point.
    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        let (dx, dy) = (self.x - other.x, self.y - other.y);
        (dx * dx + dy * dy).sqrt()
    }

    /// True when both coordinates are finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// One path segment, always starting where the previous one ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    /// A straight line to the given point.
    Line(Point),
    /// A cubic Bézier with two control points and an end point.
    Cubic {
        /// First control point.
        c1: Point,
        /// Second control point.
        c2: Point,
        /// End point.
        to: Point,
    },
}

impl Seg {
    /// The segment's end point.
    #[must_use]
    pub const fn end(self) -> Point {
        match self {
            Seg::Line(p) => p,
            Seg::Cubic { to, .. } => to,
        }
    }

    /// All points that define the segment (start excluded — it is the previous
    /// segment's end).
    #[must_use]
    pub const fn control_points(self) -> [Point; 3] {
        match self {
            Seg::Line(p) => [p, p, p],
            Seg::Cubic { c1, c2, to } => [c1, c2, to],
        }
    }

    /// The segment with every point mapped through `m`.
    #[must_use]
    pub fn transformed(self, m: Affine) -> Self {
        let map = |p: Point| {
            let (x, y) = m.apply(p.x, p.y);
            Point::new(x, y)
        };
        match self {
            Seg::Line(p) => Seg::Line(map(p)),
            Seg::Cubic { c1, c2, to } => Seg::Cubic {
                c1: map(c1),
                c2: map(c2),
                to: map(to),
            },
        }
    }

    /// Samples the segment (start excluded, end included) into `out`.
    fn sample_into(self, start: Point, out: &mut Vec<Point>) {
        match self {
            Seg::Line(p) => out.push(p),
            Seg::Cubic { c1, c2, to } => {
                for i in 1..=CURVE_STEPS {
                    let t = i as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    out.push(Point::new(
                        a * start.x + b * c1.x + c * c2.x + d * to.x,
                        a * start.y + b * c1.y + c * c2.y + d * to.y,
                    ));
                }
            }
        }
    }
}

/// A subpath: a start point, its segments and whether the outline is closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subpath {
    /// Where the subpath begins.
    pub start: Point,
    /// The segments, in order.
    pub segs: Vec<Seg>,
    /// True when the outline is closed (the last point joins the start).
    pub closed: bool,
}

impl Subpath {
    /// A subpath that starts at `start` and has no segments yet.
    #[must_use]
    pub fn new(start: Point) -> Self {
        Self {
            start,
            segs: Vec::new(),
            closed: false,
        }
    }

    /// A polyline through `points` (fewer than two points yields an empty
    /// subpath — an editor node with no extent is never useful and would make
    /// hit-testing ambiguous).
    #[must_use]
    pub fn polyline(points: &[Point], closed: bool) -> Self {
        let mut it = points.iter().copied();
        let Some(start) = it.next() else {
            return Self::default();
        };
        let mut s = Self::new(start);
        for p in it {
            s.segs.push(Seg::Line(p));
        }
        s.closed = closed;
        s
    }

    /// Adds a straight segment.
    pub fn push_line(&mut self, to: Point) {
        self.segs.push(Seg::Line(to));
    }

    /// Adds a cubic segment.
    pub fn push_cubic(&mut self, c1: Point, c2: Point, to: Point) {
        self.segs.push(Seg::Cubic { c1, c2, to });
    }

    /// True when the subpath has no segments (a single point).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// The subpath with every point mapped through `m`.
    #[must_use]
    pub fn transformed(&self, m: Affine) -> Self {
        let (x, y) = m.apply(self.start.x, self.start.y);
        Self {
            start: Point::new(x, y),
            segs: self.segs.iter().map(|s| s.transformed(m)).collect(),
            closed: self.closed,
        }
    }

    /// The subpath sampled into a polyline: the start point first, then every
    /// segment's samples. A closed subpath does **not** repeat the start here —
    /// [`Subpath::polygon`] adds the closing edge.
    #[must_use]
    pub fn flatten(&self) -> Vec<Point> {
        let mut out = Vec::with_capacity(1 + self.segs.len() * CURVE_STEPS as usize);
        out.push(self.start);
        let mut cursor = self.start;
        for seg in &self.segs {
            seg.sample_into(cursor, &mut out);
            cursor = seg.end();
        }
        out
    }

    /// The flattened outline as a closed polygon (for the even-odd fill test).
    /// Open subpaths are returned as-is — they have no inside.
    #[must_use]
    pub fn polygon(&self) -> Vec<Point> {
        let mut pts = self.flatten();
        if self.closed && pts.len() >= 3 {
            pts.push(self.start);
        }
        pts
    }

    /// The subpath's bounding box over its control points (conservative for
    /// cubics — the curve never leaves its control hull) as `(min, max)`.
    #[must_use]
    pub fn bounds(&self) -> Option<(Point, Point)> {
        if self.segs.is_empty() {
            return None;
        }
        let mut min = self.start;
        let mut max = self.start;
        let mut take = |p: Point| {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        };
        for seg in &self.segs {
            for p in seg.control_points() {
                take(p);
            }
        }
        Some((min, max))
    }

    /// Distance from `p` to the outline (the flattened polyline, closing edge
    /// included for closed subpaths).
    #[must_use]
    pub fn distance_to(&self, p: Point) -> f32 {
        let pts = self.polygon();
        if pts.len() < 2 {
            return pts.first().map_or(f32::INFINITY, |q| p.distance(*q));
        }
        let mut best = f32::INFINITY;
        for w in pts.windows(2) {
            best = best.min(segment_distance(p, w[0], w[1]));
        }
        best
    }

    /// Even-odd fill test over the flattened polygon. Open subpaths never
    /// contain a point (they are strokes; use [`Subpath::distance_to`]).
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        if !self.closed {
            return false;
        }
        let pts = self.polygon();
        if pts.len() < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[j]);
            if (a.y > p.y) != (b.y > p.y) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if p.x < x {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }
}

/// Distance from `p` to the segment `a`–`b`.
#[must_use]
pub fn segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let (vx, vy) = (b.x - a.x, b.y - a.y);
    let len2 = vx * vx + vy * vy;
    if len2 <= f32::MIN_POSITIVE {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * vx + (p.y - a.y) * vy) / len2).clamp(0.0, 1.0);
    p.distance(Point::new(a.x + t * vx, a.y + t * vy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f32, y0: f32, x1: f32, y1: f32) -> Subpath {
        Subpath {
            start: Point::new(x0, y0),
            segs: vec![
                Seg::Line(Point::new(x1, y0)),
                Seg::Line(Point::new(x1, y1)),
                Seg::Line(Point::new(x0, y1)),
            ],
            closed: true,
        }
    }

    #[test]
    fn flatten_hits_the_end_point_exactly() {
        let s = Subpath {
            start: Point::new(0.0, 0.0),
            segs: vec![Seg::Cubic {
                c1: Point::new(0.0, 10.0),
                c2: Point::new(10.0, 10.0),
                to: Point::new(10.0, 0.0),
            }],
            closed: false,
        };
        let pts = s.flatten();
        assert_eq!(pts.len(), 1 + CURVE_STEPS as usize);
        assert_eq!(pts[0], Point::new(0.0, 0.0));
        assert_eq!(*pts.last().unwrap(), Point::new(10.0, 0.0));
    }

    #[test]
    fn contains_follows_even_odd_on_closed_outlines_only() {
        let sq = square(0.0, 0.0, 10.0, 10.0);
        assert!(sq.contains(Point::new(5.0, 5.0)));
        assert!(!sq.contains(Point::new(15.0, 5.0)));
        let mut open = sq.clone();
        open.closed = false;
        assert!(!open.contains(Point::new(5.0, 5.0)));
    }

    #[test]
    fn bounds_cover_the_control_hull_and_reject_empty_paths() {
        let (min, max) = square(2.0, 3.0, 12.0, 9.0).bounds().expect("bounds");
        assert_eq!((min.x, min.y, max.x, max.y), (2.0, 3.0, 12.0, 9.0));
        assert!(Subpath::new(Point::new(1.0, 1.0)).bounds().is_none());
    }

    #[test]
    fn distance_measures_the_closing_edge_too() {
        let sq = square(0.0, 0.0, 10.0, 10.0);
        // The closing edge runs from (0,10) back to (0,0).
        assert!((sq.distance_to(Point::new(-2.0, 5.0)) - 2.0).abs() < 1e-4);
        assert!((sq.distance_to(Point::new(5.0, 5.0)) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn transform_maps_start_controls_and_closed_flag() {
        let m = Affine::translate(5.0, 0.0).then(Affine::scale(2.0, 2.0));
        let t = square(0.0, 0.0, 10.0, 10.0).transformed(m);
        assert_eq!(t.start, Point::new(10.0, 0.0));
        assert_eq!(t.segs[0].end(), Point::new(30.0, 0.0));
        assert!(t.closed);
        assert!(t.contains(Point::new(20.0, 10.0)));
    }
}
