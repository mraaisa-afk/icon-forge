//! Snapping: turning a raw drag delta into an aligned one (and the guides that
//! explain it).
//!
//! §2's golden rule puts snapping inside the WASM module — it runs on every
//! pointer move while dragging, so it may not round-trip through the Tauri IPC.
//! It also has to agree with the geometry the editor stores, which is why it
//! lives here and not in the canvas code: the engine already knows every node's
//! placed bounding box.
//!
//! ## What snaps
//!
//! A move snaps the *three* positions of the moving bounding box on each axis
//! (near edge, centre, far edge) against three families of targets:
//!
//! * **canvas** — its outer edges and its centre lines;
//! * **nodes** — every other visible node's edges and centre;
//! * **grid** — the nearest multiple of `grid_step`.
//!
//! The best correction per axis is the smallest one within `tolerance`
//! (in document units — the UI scales its pixel tolerance by the zoom). Two
//! targets at the same distance are broken by what the user is more likely to
//! mean: a node's edge, then the canvas edge, then a node's centre, then the
//! canvas centre, then the grid — so a drag that lands halfway between a
//! neighbour's edge and the canvas centre line follows the neighbour.
//!
//! Guides then report **every alignment that holds at the final position** — the
//! one that produced the correction and any other edge that happens to line up
//! once the box has moved there — because that is what the user is looking at
//! while they drag. One line is drawn per position (its label is the most
//! concrete target aligned there), extents cover the aligned objects, and the
//! whole answer is capped by [`MAX_GUIDES`].
//!
//! Two deliberate limits, both cheap to lift later: only moves snap (a scale or
//! rotate handle does not), and there is no equal-spacing/"distribute" snapping.

use super::doc::{Doc, NodeId};
use super::geom::Point;

/// How close (in document units) a candidate has to be before it wins.
pub const DEFAULT_SNAP_TOLERANCE: f32 = 6.0;

/// Two corrections within this distance are considered the same snap.
const SAME_SNAP_EPSILON: f32 = 1.0e-3;

/// Most guides one snap answer carries (a screenful of lines is already noise).
pub const MAX_GUIDES: usize = 8;

/// Which axis a guide line is perpendicular to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// A vertical line (the correction moved the box horizontally).
    X,
    /// A horizontal line.
    Y,
}

impl Axis {
    /// Raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::X => 0,
            Self::Y => 1,
        }
    }
}

/// Why a guide line exists (the canvas colour-codes these).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideKind {
    /// The canvas edge.
    CanvasEdge,
    /// The canvas centre line.
    CanvasCenter,
    /// Another node's edge.
    NodeEdge,
    /// Another node's centre.
    NodeCenter,
    /// A multiple of the grid step.
    Grid,
}

impl GuideKind {
    /// Raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::CanvasEdge => 0,
            Self::CanvasCenter => 1,
            Self::NodeEdge => 2,
            Self::NodeCenter => 3,
            Self::Grid => 4,
        }
    }
}

/// One alignment guideline, in document coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    /// Which axis the line is perpendicular to.
    pub axis: Axis,
    /// What produced it.
    pub kind: GuideKind,
    /// Position along the axis (the line's x for [`Axis::X`]).
    pub position: f32,
    /// Start of the drawn extent (the other axis).
    pub from: f32,
    /// End of the drawn extent.
    pub to: f32,
}

/// Which target families a move should snap to, and how forgiving it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapOptions {
    /// Maximum correction distance, in document units.
    pub tolerance: f32,
    /// Grid pitch; ignored unless [`SnapOptions::grid`] is set.
    pub grid_step: f32,
    /// Snap to the canvas edges and centre.
    pub canvas: bool,
    /// Snap to other nodes' edges and centres.
    pub nodes: bool,
    /// Snap to the grid.
    pub grid: bool,
}

impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            tolerance: DEFAULT_SNAP_TOLERANCE,
            grid_step: 0.0,
            canvas: true,
            nodes: true,
            grid: false,
        }
    }
}

impl SnapOptions {
    /// True when at least one family is enabled and the tolerance is usable.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.tolerance > 0.0 && (self.canvas || self.nodes || (self.grid && self.grid_step > 0.0))
    }
}

/// Distances closer than this count as equal when breaking a tie.
const TIE_EPSILON: f32 = 1.0e-4;

/// Target preference when two candidates are equally far away (lower wins).
fn priority(kind: GuideKind) -> u8 {
    match kind {
        GuideKind::NodeEdge => 0,
        GuideKind::CanvasEdge => 1,
        GuideKind::NodeCenter => 2,
        GuideKind::CanvasCenter => 3,
        GuideKind::Grid => 4,
    }
}

/// One axis' answer.
struct AxisSnap {
    /// Correction to add to the proposed delta.
    delta: f32,
    /// Distance of the winning candidate (`INFINITY` when nothing matched).
    best: f32,
    /// Tie-break rank of the winning candidate.
    rank: u8,
}

impl Default for AxisSnap {
    fn default() -> Self {
        // `f32::INFINITY` rather than `f32::default()`: a derived default would
        // start at 0.0 and every candidate would lose to it.
        Self {
            delta: 0.0,
            best: f32::INFINITY,
            rank: u8::MAX,
        }
    }
}

impl AxisSnap {
    /// Offers a candidate correction; keeps the nearest, then the most concrete.
    fn offer(&mut self, correction: f32, magnitude: f32, kind: GuideKind) {
        let nearer = magnitude < self.best - TIE_EPSILON;
        let tied = (magnitude - self.best).abs() <= TIE_EPSILON;
        if nearer || (tied && priority(kind) < self.rank) {
            self.best = magnitude.min(self.best);
            self.delta = correction;
            self.rank = priority(kind);
        }
    }
}

/// The snapped delta plus the guides that explain it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SnapResult {
    /// The corrected delta (equal to the proposed one when nothing snapped).
    pub dx: f32,
    /// The corrected delta.
    pub dy: f32,
    /// Alignment guides to draw while the drag is live.
    pub guides: Vec<Guide>,
}

/// Collects the candidate positions of one axis for a moving box.
///
/// Each family is asked twice — once to pick the winning correction, once to
/// explain it — instead of materialising every candidate: a sheet with 4096
/// icons has ~25 000 candidate positions, and building that list on every
/// pointer move would be the slowest thing in the drag.
struct Targets<'a> {
    doc: &'a Doc,
    moving: &'a [NodeId],
    options: &'a SnapOptions,
}

/// A candidate target position and the extent it covers on the other axis.
#[derive(Clone, Copy)]
struct Candidate {
    value: f32,
    kind: GuideKind,
    from: f32,
    to: f32,
}

impl Targets<'_> {
    /// Runs `visit` over every enabled candidate on `axis`.
    fn each(&self, axis: Axis, mut visit: impl FnMut(&Candidate)) {
        let mut candidate = Candidate {
            value: 0.0,
            kind: GuideKind::CanvasEdge,
            from: 0.0,
            to: 0.0,
        };
        let (width, height) = (self.doc.width(), self.doc.height());
        if self.options.canvas {
            let (length, other) = match axis {
                Axis::X => (width, height),
                Axis::Y => (height, width),
            };
            for (value, kind) in [
                (0.0, GuideKind::CanvasEdge),
                (length / 2.0, GuideKind::CanvasCenter),
                (length, GuideKind::CanvasEdge),
            ] {
                candidate.value = value;
                candidate.kind = kind;
                candidate.from = 0.0;
                candidate.to = other;
                visit(&candidate);
            }
        }
        if self.options.nodes {
            for node in self.doc.nodes() {
                if !node.visible || self.moving.contains(&node.id) {
                    continue;
                }
                let Some((lo, hi)) = node.bounds() else {
                    continue;
                };
                let (values, other_lo, other_hi) = match axis {
                    Axis::X => ([lo.x, (lo.x + hi.x) / 2.0, hi.x], lo.y, hi.y),
                    Axis::Y => ([lo.y, (lo.y + hi.y) / 2.0, hi.y], lo.x, hi.x),
                };
                for (index, value) in values.into_iter().enumerate() {
                    candidate.value = value;
                    candidate.kind = if index == 1 {
                        GuideKind::NodeCenter
                    } else {
                        GuideKind::NodeEdge
                    };
                    candidate.from = other_lo;
                    candidate.to = other_hi;
                    visit(&candidate);
                }
            }
        }
        if self.options.grid && self.options.grid_step > 0.0 {
            // The grid's extent is the canvas: a grid guide explains itself.
            let step = self.options.grid_step;
            let (length, other) = match axis {
                Axis::X => (width, height),
                Axis::Y => (height, width),
            };
            let first = (0.0 / step).round() * step;
            let count = (length / step).round() as i64;
            for i in 0..=count {
                let value = first + i as f32 * step;
                if value < 0.0 || value > length {
                    continue;
                }
                candidate.value = value;
                candidate.kind = GuideKind::Grid;
                candidate.from = 0.0;
                candidate.to = other;
                visit(&candidate);
            }
        }
    }
}

/// Snaps a proposed move of `moving`'s bounding box by `(dx, dy)`.
///
/// Returns `None` when there is nothing to snap (no moving box, or no family
/// enabled), so the caller can pass the raw delta straight through.
#[must_use]
pub fn snap_move(
    doc: &Doc,
    moving: &[NodeId],
    dx: f32,
    dy: f32,
    options: &SnapOptions,
) -> Option<SnapResult> {
    if !options.is_active() {
        return None;
    }
    let (lo, hi) = doc.bounds(moving)?;
    let moved = (
        Point::new(lo.x + dx, lo.y + dy),
        Point::new(hi.x + dx, hi.y + dy),
    );
    let targets = Targets {
        doc,
        moving,
        options,
    };
    let tolerance = options.tolerance;

    let mut x = AxisSnap::default();
    let mut y = AxisSnap::default();
    for axis in [Axis::X, Axis::Y] {
        let (edges, centre) = match axis {
            Axis::X => ([moved.0.x, moved.1.x], (moved.0.x + moved.1.x) / 2.0),
            Axis::Y => ([moved.0.y, moved.1.y], (moved.0.y + moved.1.y) / 2.0),
        };
        let snap = match axis {
            Axis::X => &mut x,
            Axis::Y => &mut y,
        };
        targets.each(axis, |candidate| {
            for edge in [edges[0], centre, edges[1]] {
                let correction = candidate.value - edge;
                let magnitude = correction.abs();
                if magnitude <= tolerance {
                    snap.offer(correction, magnitude, candidate.kind);
                }
            }
        });
    }

    let snapped_dx = dx + x.delta;
    let snapped_dy = dy + y.delta;
    let final_lo = Point::new(lo.x + snapped_dx, lo.y + snapped_dy);
    let final_hi = Point::new(hi.x + snapped_dx, hi.y + snapped_dy);

    let mut guides: Vec<Guide> = Vec::new();
    for axis in [Axis::X, Axis::Y] {
        let snap = match axis {
            Axis::X => &x,
            Axis::Y => &y,
        };
        if !snap.best.is_finite() {
            continue;
        }
        let (edges, centre) = match axis {
            Axis::X => ([final_lo.x, final_hi.x], (final_lo.x + final_hi.x) / 2.0),
            Axis::Y => ([final_lo.y, final_hi.y], (final_lo.y + final_hi.y) / 2.0),
        };
        // The moving box's own extent on the other axis keeps the guide close to
        // the objects it describes.
        let (moving_from, moving_to) = match axis {
            Axis::X => (final_lo.y, final_hi.y),
            Axis::Y => (final_lo.x, final_hi.x),
        };
        targets.each(axis, |candidate| {
            if guides.len() >= MAX_GUIDES {
                return;
            }
            if !edges
                .iter()
                .copied()
                .chain(core::iter::once(centre))
                .any(|edge| (candidate.value - edge).abs() <= SAME_SNAP_EPSILON)
            {
                return;
            }
            // One line per position: the same coordinate reached by several
            // targets is a single guide. Its label is the most concrete target
            // aligned there, and its extent then covers that target plus the
            // moving box (a less concrete target at the same place widens the
            // line instead of relabelling it).
            if let Some(existing) = guides.iter_mut().find(|g| {
                g.axis == axis && (g.position - candidate.value).abs() <= SAME_SNAP_EPSILON
            }) {
                if priority(candidate.kind) < priority(existing.kind) {
                    existing.kind = candidate.kind;
                    existing.from = candidate.from.min(moving_from);
                    existing.to = candidate.to.max(moving_to);
                } else {
                    existing.from = existing.from.min(candidate.from);
                    existing.to = existing.to.max(candidate.to);
                }
                return;
            }
            guides.push(Guide {
                axis,
                kind: candidate.kind,
                position: candidate.value,
                from: candidate.from.min(moving_from),
                to: candidate.to.max(moving_to),
            });
        });
    }

    Some(SnapResult {
        dx: snapped_dx,
        dy: snapped_dy,
        guides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Doc, Point, Seg, Subpath};

    fn square(x: f32, y: f32, size: f32) -> Vec<Subpath> {
        vec![Subpath {
            start: Point::new(x, y),
            segs: vec![
                Seg::Line(Point::new(x + size, y)),
                Seg::Line(Point::new(x + size, y + size)),
                Seg::Line(Point::new(x, y + size)),
            ],
            closed: true,
        }]
    }

    /// A 200×200 canvas with one 20×20 square sitting at (100, 100).
    fn doc() -> (Doc, NodeId) {
        let mut doc = Doc::new(200.0, 200.0);
        let anchor = doc.add(square(100.0, 100.0, 20.0), [1, 2, 3, 255]);
        (doc, anchor)
    }

    fn options(tolerance: f32) -> SnapOptions {
        SnapOptions {
            tolerance,
            ..SnapOptions::default()
        }
    }

    #[test]
    fn an_inactive_option_set_asks_for_no_snapping() {
        let (doc, anchor) = doc();
        for options in [
            SnapOptions {
                tolerance: 0.0,
                ..SnapOptions::default()
            },
            SnapOptions {
                canvas: false,
                nodes: false,
                grid: false,
                ..SnapOptions::default()
            },
            SnapOptions {
                canvas: false,
                nodes: false,
                grid: true,
                grid_step: 0.0,
                tolerance: 6.0,
            },
        ] {
            assert!(!options.is_active());
            assert!(snap_move(&doc, &[anchor], 5.0, 5.0, &options).is_none());
        }
    }

    #[test]
    fn a_node_edge_beats_an_equally_distant_canvas_line() {
        let (doc, anchor) = doc();
        // The anchor's left edge is at x = 100 = the canvas centre line; a
        // second square dragged to x 103 is two units from both.
        let mut doc = doc;
        let dragged = doc.add(square(103.0, 10.0, 20.0), [9, 9, 9, 255]);
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!(snapped.dx, -3.0);
        assert!(snapped
            .guides
            .iter()
            .any(|g| g.kind == GuideKind::NodeEdge && g.position == 100.0));
        assert!(anchor.get() < dragged.get());
    }

    #[test]
    fn the_nearest_target_wins_over_the_more_concrete_one() {
        let (mut doc, _anchor) = doc();
        // A node at x = 150 and the canvas edge at x = 200: the dragged square
        // (at 148) is two units from the node and 32 from the edge.
        doc.add(square(150.0, 10.0, 20.0), [1, 1, 1, 255]);
        let dragged = doc.add(square(148.0, 100.0, 20.0), [2, 2, 2, 255]);
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!(snapped.dx, 2.0);
    }

    #[test]
    fn the_tolerance_is_absolute_in_document_units() {
        let (mut doc, _anchor) = doc();
        let dragged = doc.add(square(96.0, 10.0, 20.0), [5, 5, 5, 255]);
        // Four units short of the canvas centre: inside a 6-unit tolerance, but
        // outside a 2-unit one (which then reports no guides at all).
        let near = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!(near.dx, 4.0);
        let strict = snap_move(&doc, &[dragged], 0.0, 0.0, &options(2.0)).unwrap();
        assert_eq!(strict.dx, 0.0);
        assert!(strict.guides.is_empty());
    }

    #[test]
    fn the_grid_family_only_fires_when_it_is_asked_for() {
        let (mut doc, _anchor) = doc();
        // 8-unit grid: the square at 95 is one unit from 96.
        let dragged = doc.add(square(95.0, 40.0, 10.0), [7, 7, 7, 255]);
        let grid = SnapOptions {
            tolerance: 3.0,
            grid_step: 8.0,
            canvas: false,
            nodes: false,
            grid: true,
        };
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &grid).unwrap();
        assert_eq!(snapped.dx, 1.0);
        assert!(snapped
            .guides
            .iter()
            .any(|g| g.kind == GuideKind::Grid && g.position == 96.0));

        let off = SnapOptions {
            grid: false,
            ..grid
        };
        assert!(
            snap_move(&doc, &[dragged], 0.0, 0.0, &off).is_none(),
            "with every family off there is nothing to snap to, so the caller \
             passes its raw delta through"
        );
    }

    #[test]
    fn guides_span_the_aligned_objects_and_are_capped() {
        let (mut doc, _anchor) = doc();
        // Three squares in a column, all left-aligned at x = 100: dragging a
        // fourth near that line should produce one guide spanning the column,
        // not one guide per neighbour.
        for y in [0.0, 40.0, 80.0] {
            doc.add(square(100.0, y, 20.0), [3, 3, 3, 255]);
        }
        let dragged = doc.add(square(102.0, 140.0, 20.0), [4, 4, 4, 255]);
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!(snapped.dx, -2.0);
        let column: Vec<&Guide> = snapped
            .guides
            .iter()
            .filter(|g| g.axis == Axis::X && (g.position - 100.0).abs() < 1.0e-3)
            .collect();
        assert_eq!(
            column.len(),
            1,
            "one line per aligned position, not per node"
        );
        assert_eq!(column[0].from, 0.0, "the line starts at the topmost object");
        assert_eq!(column[0].to, 160.0, "and ends past the dragged one");
        assert!(snapped.guides.len() <= MAX_GUIDES);
    }

    #[test]
    fn only_the_axis_with_a_target_in_reach_moves() {
        let (mut doc, _anchor) = doc();
        // The anchor spans x 100..120 (and the canvas centre is also x = 100).
        // The dragged square's left edge (98) is two units short of that line,
        // while nothing is within reach on y — so only x moves, and only x gets
        // a guide.
        let dragged = doc.add(square(98.0, 60.0, 10.0), [8, 8, 8, 255]);
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!((snapped.dx, snapped.dy), (2.0, 0.0));
        assert!(
            snapped.guides.iter().all(|g| g.axis == Axis::X),
            "y never came within tolerance, so it contributes no guide"
        );
        // The correction itself, labelled by the node edge that won the tie…
        assert!(snapped
            .guides
            .iter()
            .any(|g| g.position == 100.0 && g.kind == GuideKind::NodeEdge));
        // …and the alignment the move left behind for free: the box's right
        // edge (110) now sits on the anchor's centre line.
        assert!(snapped.guides.iter().any(|g| g.position == 110.0));
    }

    #[test]
    fn an_already_aligned_box_needs_no_correction() {
        let (mut doc, _anchor) = doc();
        // Both centres already sit on the anchor's upper-left corner (100, 100),
        // so the winning corrections are zero: the delta passes through and the
        // guides explain the alignment that is already there.
        let dragged = doc.add(square(95.0, 95.0, 10.0), [8, 8, 8, 255]);
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!((snapped.dx, snapped.dy), (0.0, 0.0));
        assert_eq!(snapped.guides.len(), 2);
        assert!(snapped
            .guides
            .iter()
            .all(|g| g.position == 100.0 && g.kind == GuideKind::NodeEdge));
    }

    #[test]
    fn hidden_nodes_are_not_snap_targets() {
        let (mut doc, anchor) = doc();
        let dragged = doc.add(square(103.0, 10.0, 20.0), [9, 9, 9, 255]);
        let index = doc.index_of(anchor).unwrap();
        let mut hidden = doc.node(anchor).unwrap().clone();
        hidden.visible = false;
        doc.set_at(index, hidden);
        // The only remaining target within reach is the canvas centre at 100,
        // which the dragged box is three units from.
        let snapped = snap_move(&doc, &[dragged], 0.0, 0.0, &options(6.0)).unwrap();
        assert_eq!(snapped.dx, -3.0);
        assert!(snapped
            .guides
            .iter()
            .any(|g| g.kind == GuideKind::CanvasCenter));
        assert!(!snapped.guides.iter().any(|g| g.kind == GuideKind::NodeEdge));
    }
}
