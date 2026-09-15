//! §3.3-⑥ Simplify — the custom geometry pass (not vtracer's):
//!
//! ```text
//! abs-cubics → remove collinear (0.5°) → RDP with CORNERS PINNED
//!            → Visvalingam (area-based) → axis-snap within 0.15 px
//! ```
//!
//! RDP is distance-based (good for straight runs); Visvalingam is area-based
//! (kills curve jitter); corner pinning preserves arrow tips and star points.
//! Anchors adjacent to a cubic are always pinned, so spline geometry only
//! ever undergoes collinear merging + snapping (conservative mode).
//!
//! Input/output are SVG path `d` strings (kurbo parse / own serializer with
//! fixed 2-decimal coordinates — deterministic and diff-friendly).

use isg_core::TraceError;
use kurbo::{BezPath, PathEl, Point};

/// Simplifier tunables (§3.3-⑥). Zero disables a pass.
#[derive(Clone, Copy, Debug)]
pub struct SimplifyParams {
    /// Anchors deviating less than this from straight are merged.
    pub collinear_deg: f64,
    /// Turn angle at or above this pins a corner (never removed).
    pub corner_turn_deg: f64,
    /// RDP distance epsilon in px.
    pub rdp_px: f64,
    /// Visvalingam effective-area threshold in px².
    pub vis_area_px2: f64,
    /// Coordinates within this distance of an integer are snapped.
    pub snap_px: f64,
}

impl Default for SimplifyParams {
    fn default() -> Self {
        Self {
            collinear_deg: 0.5,
            corner_turn_deg: 60.0,
            rdp_px: 0.35,
            vis_area_px2: 0.4,
            snap_px: 0.15,
        }
    }
}

/// One absolute-cubic segment (quads are elevated on parse).
#[derive(Clone, Copy, Debug)]
enum Seg {
    Line { end: Point },
    Cubic { c1: Point, c2: Point, end: Point },
}

impl Seg {
    fn end(self) -> Point {
        match self {
            Seg::Line { end } | Seg::Cubic { end, .. } => end,
        }
    }
}

struct SubPath {
    start: Point,
    segs: Vec<Seg>,
    closed: bool,
}

/// Simplifies one SVG path `d` string. Fails with
/// [`TraceError::UnparseableSvg`] when `d` cannot be parsed.
pub fn simplify_d(d: &str, p: &SimplifyParams) -> Result<String, TraceError> {
    let subs = parse_subpaths(d)?;
    let mut out = String::new();
    for s in subs {
        let s = simplify_subpath(s, p);
        emit(&s, &mut out);
    }
    Ok(out)
}

fn parse_subpaths(d: &str) -> Result<Vec<SubPath>, TraceError> {
    let path = BezPath::from_svg(d).map_err(|_| TraceError::UnparseableSvg)?;
    const T: f64 = 2.0 / 3.0;
    let mut subs = Vec::new();
    let mut cur: Option<SubPath> = None;
    let mut pos = Point::ORIGIN;
    for el in path.iter() {
        match el {
            PathEl::MoveTo(p) => {
                if let Some(s) = cur.take() {
                    subs.push(s);
                }
                cur = Some(SubPath {
                    start: p,
                    segs: Vec::new(),
                    closed: false,
                });
                pos = p;
            }
            PathEl::LineTo(p) => {
                if let Some(s) = cur.as_mut() {
                    s.segs.push(Seg::Line { end: p });
                }
                pos = p;
            }
            PathEl::QuadTo(q, p) => {
                // Degree elevation: quad → cubic with 2/3 control points.
                if let Some(s) = cur.as_mut() {
                    let c1 = Point::new(
                        pos.x + T * (q.x - pos.x),
                        pos.y + T * (q.y - pos.y),
                    );
                    let c2 = Point::new(p.x + T * (q.x - p.x), p.y + T * (q.y - p.y));
                    s.segs.push(Seg::Cubic { c1, c2, end: p });
                }
                pos = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                if let Some(s) = cur.as_mut() {
                    s.segs.push(Seg::Cubic { c1, c2, end: p });
                }
                pos = p;
            }
            PathEl::ClosePath => {
                if let Some(s) = cur.as_mut() {
                    s.closed = true;
                }
            }
        }
    }
    if let Some(s) = cur.take() {
        subs.push(s);
    }
    Ok(subs)
}

fn simplify_subpath(s: SubPath, p: &SimplifyParams) -> SubPath {
    let has_curve = s.segs.iter().any(|g| matches!(g, Seg::Cubic { .. }));
    if has_curve {
        return simplify_curved(s, p);
    }
    let pts: Vec<Point> = {
        let mut v = Vec::with_capacity(s.segs.len() + 1);
        v.push(s.start);
        v.extend(s.segs.iter().map(|g| g.end()));
        v
    };
    if pts.len() <= 2 {
        return snap_subpath(s, p.snap_px);
    }
    let pinned = pin_mask(&pts, s.closed, p.corner_turn_deg);

    // Pass 1: collinear merge (never removes pinned anchors).
    let (pts, pinned) = filter_collinear(pts, pinned, p.collinear_deg);
    if pts.len() <= 2 {
        let kept: Vec<Point> = pts;
        return rebuild(kept, s.closed, p.snap_px);
    }

    // Pass 2: RDP between pinned islands (corners survive by construction).
    let mut keep = pinned.clone();
    rdp_pass(&pts, &pinned, p.rdp_px, &mut keep);
    let mut pts2 = Vec::new();
    let mut pinned2 = Vec::new();
    for ((pt, pn), k) in pts.into_iter().zip(pinned).zip(keep) {
        if k {
            pts2.push(pt);
            pinned2.push(pn);
        }
    }
    let pts = pts2;
    let pinned = pinned2;

    // Pass 3: Visvalingam on the RDP survivors (structural pins only).
    let removed = visvalingam_pass(&pts, &pinned, p.vis_area_px2);
    let mut kept = Vec::new();
    for (pt, r) in pts.into_iter().zip(removed) {
        if !r {
            kept.push(pt);
        }
    }
    rebuild(kept, s.closed, p.snap_px)
}

fn rebuild(kept: Vec<Point>, closed: bool, snap: f64) -> SubPath {
    if kept.len() < 2 {
        // Degenerate sliver — emit what there is as a point.
        let start = kept.first().copied().unwrap_or(Point::ORIGIN);
        return SubPath {
            start: snap_pt(start, snap),
            segs: Vec::new(),
            closed: false,
        };
    }
    let start = snap_pt(kept[0], snap);
    let segs: Vec<Seg> = kept
        .windows(2)
        .map(|w| Seg::Line {
            end: snap_pt(w[1], snap),
        })
        .collect();
    SubPath {
        start,
        segs,
        closed,
    }
}

/// Corner + endpoint pinning. `closed` also evaluates the wrap-around
/// corners at both seam anchors.
fn pin_mask(pts: &[Point], closed: bool, corner_deg: f64) -> Vec<bool> {
    let n = pts.len();
    let mut pin = vec![false; n];
    pin[0] = true;
    pin[n - 1] = true;
    for i in 1..n - 1 {
        if turn_deg(pts[i - 1], pts[i], pts[i + 1]) >= corner_deg {
            pin[i] = true;
        }
    }
    if closed && n >= 3 {
        let head = turn_deg(pts[n - 1], pts[0], pts[1]);
        let tail = turn_deg(pts[n - 2], pts[n - 1], pts[0]);
        if head >= corner_deg {
            pin[0] = true;
        }
        if tail >= corner_deg {
            pin[n - 1] = true;
        }
    }
    pin
}

/// Deviation from straight at `b` in degrees (0 = collinear).
fn turn_deg(a: Point, b: Point, c: Point) -> f64 {
    let (v1, v2) = (b - a, c - b);
    let (l1, l2) = (v1.hypot(), v2.hypot());
    if l1 == 0.0 || l2 == 0.0 {
        return 0.0;
    }
    let cos = (v1.dot(v2) / (l1 * l2)).clamp(-1.0, 1.0);
    cos.acos().to_degrees()
}

fn filter_collinear(pts: Vec<Point>, pinned: Vec<bool>, deg: f64) -> (Vec<Point>, Vec<bool>) {
    let n = pts.len();
    let mut out_pts = Vec::with_capacity(n);
    let mut out_pin = Vec::with_capacity(n);
    out_pts.push(pts[0]);
    out_pin.push(true);
    for i in 1..n - 1 {
        let prev = *out_pts.last().unwrap();
        if !pinned[i] && turn_deg(prev, pts[i], pts[i + 1]) < deg {
            continue; // merge away
        }
        out_pts.push(pts[i]);
        out_pin.push(pinned[i]);
    }
    out_pts.push(pts[n - 1]);
    out_pin.push(true);
    (out_pts, out_pin)
}

/// Classic RDP over each maximal run between pinned anchors.
fn rdp_pass(pts: &[Point], pinned: &[bool], eps: f64, keep: &mut [bool]) {
    if eps <= 0.0 || pts.len() < 3 {
        return;
    }
    let mut anchors = vec![0usize];
    for i in 1..pts.len() - 1 {
        if pinned[i] {
            anchors.push(i);
        }
    }
    anchors.push(pts.len() - 1);
    for w in anchors.windows(2) {
        rdp_rec(pts, eps, w[0], w[1], keep);
    }
}

fn rdp_rec(pts: &[Point], eps: f64, lo: usize, hi: usize, keep: &mut [bool]) {
    if hi <= lo + 1 {
        return;
    }
    let (mut best_d, mut best_i) = (-1.0f64, None::<usize>);
    for m in lo + 1..hi {
        let d = point_seg_dist(pts[m], pts[lo], pts[hi]);
        if d > best_d {
            best_d = d;
            best_i = Some(m);
        }
    }
    if let Some(m) = best_i {
        if best_d > eps {
            keep[m] = true;
            rdp_rec(pts, eps, lo, m, keep);
            rdp_rec(pts, eps, m, hi, keep);
        }
    }
}

/// Iterative Visvalingam–Whyatt: repeatedly remove the non-pinned interior
/// point with the smallest effective (triangle) area while below `tol`.
fn visvalingam_pass(pts: &[Point], pinned: &[bool], tol: f64) -> Vec<bool> {
    let n = pts.len();
    let mut removed = vec![false; n];
    if tol <= 0.0 || n < 3 {
        return removed;
    }
    let mut prev: Vec<usize> = (0..n).map(|i| i.saturating_sub(1)).collect();
    let mut next: Vec<usize> = (0..n).map(|i| (i + 1).min(n - 1)).collect();
    loop {
        let mut best = (f64::MAX, None::<usize>);
        let mut m = next[0];
        while m < n - 1 {
            if !pinned[m] && !removed[m] {
                let area = tri_area(pts[prev[m]], pts[m], pts[next[m]]);
                if area < best.0 {
                    best = (area, Some(m));
                }
            }
            m = next[m];
        }
        match best.1 {
            Some(m) if best.0 < tol => {
                removed[m] = true;
                next[prev[m]] = next[m];
                prev[next[m]] = prev[m];
            }
            _ => break,
        }
    }
    removed
}

fn tri_area(a: Point, b: Point, c: Point) -> f64 {
    ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() / 2.0
}

fn point_seg_dist(p: Point, a: Point, b: Point) -> f64 {
    let (abx, aby) = (b.x - a.x, b.y - a.y);
    let len2 = abx * abx + aby * aby;
    if len2 == 0.0 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = (((p.x - a.x) * abx + (p.y - a.y) * aby) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (a.x + t * abx, a.y + t * aby);
    ((p.x - cx) * (p.x - cx) + (p.y - cy) * (p.y - cy)).sqrt()
}

/// Conservative pass for subpaths containing cubics: merge strictly
/// line-line collinear anchors, leave every curve untouched.
fn simplify_curved(s: SubPath, p: &SimplifyParams) -> SubPath {
    let pts: Vec<Point> = {
        let mut v = Vec::with_capacity(s.segs.len() + 1);
        v.push(s.start);
        v.extend(s.segs.iter().map(|g| g.end()));
        v
    };
    let n = pts.len();
    if n <= 2 {
        return snap_subpath(s, p.snap_px);
    }
    let is_curve: Vec<bool> = {
        // anchor i is curve-adjacent when seg i-1 or seg i is a cubic
        let mut v = vec![false; n];
        for (i, g) in s.segs.iter().enumerate() {
            if matches!(g, Seg::Cubic { .. }) {
                v[i] = true;
                v[i + 1] = true;
            }
        }
        v
    };
    let mut drop = vec![false; n];
    for i in 1..n - 1 {
        if is_curve[i] {
            continue;
        }
        let dev = turn_deg(pts[i - 1], pts[i], pts[i + 1]);
        if dev < p.collinear_deg && dev < p.corner_turn_deg {
            drop[i] = true;
        }
    }
    let keep_idx: Vec<usize> = (0..n).filter(|&i| !drop[i]).collect();
    let mut segs: Vec<Seg> = Vec::new();
    for w in keep_idx.windows(2) {
        let (a, b) = (w[0], w[1]);
        match s.segs[a] {
            Seg::Line { .. } => segs.push(Seg::Line { end: pts[b] }),
            Seg::Cubic { c1, c2, .. } => segs.push(Seg::Cubic {
                c1,
                c2,
                end: pts[b],
            }),
        }
    }
    SubPath {
        start: snap_pt(s.start, p.snap_px),
        segs: segs
            .into_iter()
            .map(|g| match g {
                Seg::Line { end } => Seg::Line {
                    end: snap_pt(end, p.snap_px),
                },
                Seg::Cubic { c1, c2, end } => Seg::Cubic {
                    c1: snap_pt(c1, p.snap_px),
                    c2: snap_pt(c2, p.snap_px),
                    end: snap_pt(end, p.snap_px),
                },
            })
            .collect(),
        closed: s.closed,
    }
}

fn snap_subpath(s: SubPath, snap: f64) -> SubPath {
    SubPath {
        start: snap_pt(s.start, snap),
        segs: s
            .segs
            .into_iter()
            .map(|g| match g {
                Seg::Line { end } => Seg::Line {
                    end: snap_pt(end, snap),
                },
                Seg::Cubic { c1, c2, end } => Seg::Cubic {
                    c1: snap_pt(c1, snap),
                    c2: snap_pt(c2, snap),
                    end: snap_pt(end, snap),
                },
            })
            .collect(),
        closed: s.closed,
    }
}

fn snap_pt(v: Point, snap: f64) -> Point {
    if snap <= 0.0 {
        return v;
    }
    Point::new(snap_coord(v.x, snap), snap_coord(v.y, snap))
}

fn snap_coord(v: f64, snap: f64) -> f64 {
    let r = v.round();
    if (v - r).abs() <= snap {
        r
    } else {
        v
    }
}

fn f2(v: f64) -> String {
    let v = if v.abs() < 0.005 { 0.0 } else { v };
    format!("{:.2}", v)
}

fn emit(s: &SubPath, out: &mut String) {
    if s.segs.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str(&format!("M{},{}", f2(s.start.x), f2(s.start.y)));
    for g in &s.segs {
        match *g {
            Seg::Line { end } => {
                out.push_str(&format!(" L{},{}", f2(end.x), f2(end.y)));
            }
            Seg::Cubic { c1, c2, end } => {
                out.push_str(&format!(
                    " C{},{} {},{} {},{}",
                    f2(c1.x),
                    f2(c1.y),
                    f2(c2.x),
                    f2(c2.y),
                    f2(end.x),
                    f2(end.y)
                ));
            }
        }
    }
    if s.closed {
        out.push('Z');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collinear_anchor_is_merged() {
        let d = "M0,0 L10,0 L20,0 L20,10 Z";
        let out = simplify_d(d, &SimplifyParams::default()).unwrap();
        assert_eq!(out, "M0.00,0.00 L20.00,0.00 L20.00,10.00 Z");
    }

    #[test]
    fn corner_survives_rdp_even_with_huge_epsilon() {
        let d = "M0,0 L10,0 L10,10 Z";
        let p = SimplifyParams {
            rdp_px: 1000.0,
            vis_area_px2: 1000.0,
            ..SimplifyParams::default()
        };
        assert_eq!(simplify_d(d, &p).unwrap(), "M0.00,0.00 L10.00,0.00 L10.00,10.00 Z");
    }

    #[test]
    fn rdp_drops_low_detail_midpoint() {
        let d = "M0,0 L5,0.10 L10,0 L10,10 Z";
        let out = simplify_d(d, &SimplifyParams::default()).unwrap();
        assert_eq!(out, "M0.00,0.00 L10.00,0.00 L10.00,10.00 Z");
    }

    #[test]
    fn visvalingam_kills_jitter_rdp_cannot_see() {
        // All deviations are 0.05 px (below RDP eps 0.35) but the wiggle
        // chain has tiny triangle areas — Visvalingam removes them.
        let d = "M0,0 L4,0 L5,0.05 L6,0 L10,0 L10,10 Z";
        let p = SimplifyParams {
            collinear_deg: 0.0,
            rdp_px: 0.0,
            vis_area_px2: 0.4,
            snap_px: 0.0,
            ..SimplifyParams::default()
        };
        assert_eq!(simplify_d(d, &p).unwrap(), "M0.00,0.00 L10.00,0.00 L10.00,10.00 Z");
    }

    #[test]
    fn axis_snap_within_tolerance() {
        let d = "M0.05,3.96 L10,10";
        let out = simplify_d(d, &SimplifyParams::default()).unwrap();
        assert_eq!(out, "M0.00,4.00 L10.00,10.00");
    }

    #[test]
    fn disabled_passes_roundtrip_exactly() {
        let d = "M2.00,3.00 L8.00,3.00 L8.00,9.00 Z";
        let p = SimplifyParams {
            collinear_deg: 0.0,
            rdp_px: 0.0,
            vis_area_px2: 0.0,
            snap_px: 0.0,
            ..SimplifyParams::default()
        };
        assert_eq!(simplify_d(d, &p).unwrap(), d);
    }

    #[test]
    fn quad_is_elevated_to_cubic() {
        let out = simplify_d("M0,0 Q5,5 10,0", &SimplifyParams::default()).unwrap();
        // Control points: p0 + 2/3(pq - p0) = (3.33, 3.33), p2 + 2/3(pq - p2).
        assert_eq!(out, "M0.00,0.00 C3.33,3.33 6.67,3.33 10.00,0.00");
    }

    #[test]
    fn curve_anchors_are_never_removed() {
        let d = "M0,0 C1,1 2,1 3,0 L3,3 Z";
        let p = SimplifyParams {
            rdp_px: 1000.0,
            vis_area_px2: 1000.0,
            ..SimplifyParams::default()
        };
        assert_eq!(
            simplify_d(d, &p).unwrap(),
            "M0.00,0.00 C1.00,1.00 2.00,1.00 3.00,0.00 L3.00,3.00 Z"
        );
    }

    #[test]
    fn relative_command_input_is_resolved() {
        // vtracer (optimize >= 1) emits relative commands; the 90° corner
        // at (15,10) is pinned and survives.
        let out = simplify_d("M10,10 l5,0 l0,5 Z", &SimplifyParams::default()).unwrap();
        assert_eq!(out, "M10.00,10.00 L15.00,10.00 L15.00,15.00 Z");
    }

    #[test]
    fn unparseable_d_is_rejected() {
        let err = simplify_d("M not numbers", &SimplifyParams::default()).unwrap_err();
        assert_eq!(err, TraceError::UnparseableSvg);
    }
}
