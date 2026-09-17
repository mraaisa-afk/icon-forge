//! §3.4 **F5 — grid drift hint** (W10).
//!
//! Projection profiles of the mask (ink per column / per row, accumulated from
//! the RLE runs — never a per-pixel rescan) give the *valleys*: interior runs
//! of empty columns or rows. The spacing between consecutive valley centres is
//! regular on a sprite sheet and irregular on a scattered layout, so
//!
//! ```text
//! regularity = 1 − MAD(cell widths) / median(cell widths) > 0.75
//! ```
//!
//! marks an axis as a grid axis. An axis needs at least two valleys to be
//! judged at all — with a single valley the width sample is empty and the
//! ratio would trivially read 1.0.
//!
//! The gate is **deliberately permissive**: measured across the whole W10
//! corpus it passes on twelve of thirteen sheets (including the scattered and
//! drifting ones), because the median/MAD of a handful of pitches is not a
//! strong discriminator. That is acceptable precisely because the hint is not
//! authoritative — the *actionable* signal is [`GridFit::SpansMultipleCells`],
//! which flagged 0 of the corpus's 1454 truth icons while catching every
//! wrongly merged component F1 produced. Tightening the gate later only loses
//! hint coverage; it cannot create false splits.
//!
//! **The grid is a hint layer, never authoritative.** Its actionable output is
//! [`GridFit::SpansMultipleCells`]: a component whose bounding box reaches at
//! least `span_tolerance` px past a valley *centre* on both sides cannot be one
//! cell's icon, so [`super::merge`] hands it back to the watershed. The
//! tolerance absorbs the 1-px bbox erosion of stage ③. A component that sits
//! inside one cell is [`GridFit::Cell`]; sheets without a regular grid report
//! [`GridFit::OffGrid`] and nothing is re-split.

use isg_core::{Bbox, ForegroundMask};

/// Bumped whenever the hint's semantics change (audit trail / cache keys).
pub const GRID_VERSION: u32 = 1;

/// Tunables for F5. Defaults are the ARCHITECTURE.md §3.4 values plus the
/// as-built guards documented in the README of this module.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridParams {
    /// Stage switch.
    pub enabled: bool,
    /// An axis is a grid axis when `1 − MAD/median` exceeds this.
    pub regularity_min: f32,
    /// Valleys an axis needs before regularity is even computed.
    pub min_valleys: u8,
    /// Px a bbox may reach past a valley centre on both sides before it counts
    /// as spanning two cells (absorbs the stage-③ 1-px erosion).
    pub span_tolerance: u32,
    /// Re-split gate: a flagged component is only sent back to the watershed
    /// when its box could hold this many median-sized icons.
    pub min_cells_to_resplit: f32,
    /// Upper bound on re-split candidates per sheet (deterministic: the
    /// largest boxes win, ties on scan order).
    pub max_resplit: u32,
}

impl Default for GridParams {
    fn default() -> Self {
        Self {
            enabled: true,
            regularity_min: 0.75,
            min_valleys: 2,
            span_tolerance: 2,
            min_cells_to_resplit: 1.5,
            max_resplit: 64,
        }
    }
}

/// What detection found, for stats, confidence (W11) and the UI (W12).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridHint {
    /// Columns form a regular lattice.
    pub grid_x: bool,
    /// Rows form a regular lattice.
    pub grid_y: bool,
    /// Cells along x (`valleys + 1`) when `grid_x`.
    pub cells_x: u32,
    /// Cells along y when `grid_y`.
    pub cells_y: u32,
    /// `1 − MAD/median` of the column pitch.
    pub regularity_x: f32,
    /// `1 − MAD/median` of the row pitch.
    pub regularity_y: f32,
    /// Column valley centres, ascending.
    pub valley_x: Vec<u32>,
    /// Row valley centres, ascending.
    pub valley_y: Vec<u32>,
}

impl GridHint {
    /// True when at least one axis is a grid axis.
    #[must_use]
    pub fn any(&self) -> bool {
        self.grid_x || self.grid_y
    }
}

/// How one component sits on the detected lattice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridFit {
    /// Inside a single cell on every grid axis.
    Cell {
        /// Index of the column band, when `grid_x`.
        col: u32,
        /// Index of the row band, when `grid_y`.
        row: u32,
    },
    /// Straddles at least one valley: two icons, or one icon plus a merge.
    SpansMultipleCells {
        /// Bit 0 = x axis, bit 1 = y axis.
        axes: u8,
    },
    /// No grid on either axis — the hint has nothing to say.
    OffGrid,
}

/// Evidence counters for the pipeline log.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridStats {
    /// Valleys found on the x axis.
    pub valleys_x: u32,
    /// Valleys found on the y axis.
    pub valleys_y: u32,
    /// Components flagged `SpansMultipleCells`.
    pub flagged: u32,
    /// Flagged components that passed the size gate.
    pub resplit_candidates: u32,
    /// Proximity merges undone verbatim from merge provenance (the hint proved
    /// the union crossed a valley, so the original components come back).
    pub restored: u32,
    /// Flagged components the watershed actually split.
    pub resplit: u32,
    /// Regions the re-split produced.
    pub regions: u32,
    /// Wall-clock, milliseconds.
    pub elapsed_ms: f32,
}

/// Runs F5 detection over the mask.
#[must_use]
pub fn detect_grid(mask: &ForegroundMask, params: &GridParams, stats: &mut GridStats) -> GridHint {
    let t0 = std::time::Instant::now();
    let (w, h) = (mask.width(), mask.height());
    if !params.enabled || w == 0 || h == 0 {
        stats.elapsed_ms += t0.elapsed().as_secs_f32() * 1000.0;
        return GridHint::default();
    }

    // Column sums from a difference array over the runs (O(runs + w)); row sums
    // are just the run lengths, since a run is exactly one row's ink span.
    let mut diff = vec![0i32; w as usize + 1];
    let mut rows = vec![0u32; h as usize];
    for r in mask.runs() {
        // `RleRun::x_end` is half-open (frozen isg-core contract).
        let len = r.len();
        if r.y < h && len > 0 {
            rows[r.y as usize] += len;
            diff[r.x_start as usize] += 1;
            diff[r.x_end as usize] -= 1;
        }
    }
    let mut cols = vec![0u32; w as usize];
    let mut run = 0i32;
    for x in 0..w as usize {
        run += diff[x];
        cols[x] = run.max(0) as u32;
    }

    let valley_x = valley_centres(&cols);
    let valley_y = valley_centres(&rows);
    let regularity_x = regularity(&valley_x, params.min_valleys);
    let regularity_y = regularity(&valley_y, params.min_valleys);
    let grid_x = regularity_x > params.regularity_min;
    let grid_y = regularity_y > params.regularity_min;
    stats.valleys_x = if grid_x { valley_x.len() as u32 } else { 0 };
    stats.valleys_y = if grid_y { valley_y.len() as u32 } else { 0 };
    stats.elapsed_ms += t0.elapsed().as_secs_f32() * 1000.0;
    GridHint {
        grid_x,
        grid_y,
        cells_x: if grid_x { valley_x.len() as u32 + 1 } else { 0 },
        cells_y: if grid_y { valley_y.len() as u32 + 1 } else { 0 },
        regularity_x: if grid_x { regularity_x } else { 0.0 },
        regularity_y: if grid_y { regularity_y } else { 0.0 },
        valley_x: if grid_x { valley_x } else { Vec::new() },
        valley_y: if grid_y { valley_y } else { Vec::new() },
    }
}

/// Centres of the interior zero-runs of a projection profile, ascending.
///
/// Runs touching the sheet border are margins, not separators, and are skipped.
fn valley_centres(profile: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &v) in profile.iter().enumerate() {
        if v == 0 {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            if s > 0 {
                out.push(((s + i - 1) / 2) as u32);
            }
        }
    }
    // A trailing empty run reaches the sheet border — it is a margin, not a
    // separator, so `start` being set here means "skip".
    let _ = start;
    out
}

/// `1 − MAD/median` of the pitch between consecutive valley centres; 0.0 when
/// the axis has too few valleys to judge.
fn regularity(valleys: &[u32], min_valleys: u8) -> f32 {
    if valleys.len() < usize::from(min_valleys).max(2) {
        return 0.0;
    }
    let widths: Vec<f32> = valleys.windows(2).map(|w| (w[1] - w[0]) as f32).collect();
    let mut sorted = widths.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    if median <= 0.0 {
        return 0.0;
    }
    let mut dev: Vec<f32> = widths.iter().map(|w| (w - median).abs()).collect();
    dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mad = dev[dev.len() / 2];
    1.0 - mad / median
}

/// Classifies one bbox against the hint.
#[must_use]
pub fn classify(bbox: &Bbox, hint: &GridHint, span_tolerance: u32) -> GridFit {
    if bbox.w == 0 || bbox.h == 0 {
        return GridFit::OffGrid;
    }
    let (a0, a1) = (i64::from(bbox.x), i64::from(bbox.x) + i64::from(bbox.w) - 1);
    let (b0, b1) = (i64::from(bbox.y), i64::from(bbox.y) + i64::from(bbox.h) - 1);
    let tol = i64::from(span_tolerance);
    let mut axes = 0u8;
    let col = band_index(a0, a1, &hint.valley_x, tol, hint.grid_x);
    if col.is_none() && hint.grid_x {
        axes |= 1;
    }
    let row = band_index(b0, b1, &hint.valley_y, tol, hint.grid_y);
    if row.is_none() && hint.grid_y {
        axes |= 2;
    }
    if axes != 0 {
        GridFit::SpansMultipleCells { axes }
    } else if hint.any() {
        GridFit::Cell {
            col: col.unwrap_or(0),
            row: row.unwrap_or(0),
        }
    } else {
        GridFit::OffGrid
    }
}

/// Band index of a `[lo, hi]` span, or `None` when it straddles a valley
/// centre by at least `tol` px on both sides.
fn band_index(lo: i64, hi: i64, valleys: &[u32], tol: i64, on_grid: bool) -> Option<u32> {
    if !on_grid {
        return None;
    }
    let mut band = 0u32;
    for &c in valleys {
        let c = i64::from(c);
        if lo <= c - tol && hi >= c + tol {
            return None;
        }
        if hi < c {
            break;
        }
        band += 1;
    }
    Some(band)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a grid mask: `n × n` solid squares of `size` px, pitch `pitch`,
    /// origin `(ox, oy)`.
    fn grid_mask(sheet: u32, n: u32, pitch: u32, size: u32, ox: u32, oy: u32) -> ForegroundMask {
        let mut m = ForegroundMask::new(sheet, sheet);
        for r in 0..n {
            for c in 0..n {
                let (x0, y0) = (ox + c * pitch, oy + r * pitch);
                for y in y0..(y0 + size).min(sheet) {
                    for x in x0..(x0 + size).min(sheet) {
                        m.set(x, y, true);
                    }
                }
            }
        }
        m
    }

    fn bbox(x: u32, y: u32, w: u32, h: u32) -> Bbox {
        Bbox { x, y, w, h }
    }

    #[test]
    fn detects_a_regular_grid_on_both_axes() {
        let mask = grid_mask(256, 4, 64, 40, 8, 8);
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        assert!(hint.grid_x && hint.grid_y, "{hint:?}");
        assert_eq!(hint.cells_x, 4);
        assert_eq!(hint.cells_y, 4);
        assert!(hint.regularity_x >= 1.0 - 1e-6, "{}", hint.regularity_x);
        assert_eq!(st.valleys_x, 3);
    }

    #[test]
    fn one_lone_blob_is_not_a_grid() {
        // No valleys at all: the hint has nothing to say and nothing is
        // re-split, whatever the component's size.
        let mut mask = ForegroundMask::new(128, 128);
        for y in 20..100 {
            for x in 20..100 {
                mask.set(x, y, true);
            }
        }
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        assert!(!hint.any(), "{hint:?}");
        assert_eq!(classify(&bbox(20, 20, 80, 80), &hint, 2), GridFit::OffGrid);
    }

    #[test]
    fn scattered_icons_are_never_flagged() {
        // Safety property behind the C5 gate: whether or not a scattered
        // layout passes the (permissive) regularity gate, no single icon may
        // ever be reported as spanning two cells.
        let mut mask = ForegroundMask::new(256, 256);
        let spots = [(20u32, 20u32), (100, 30), (40, 120), (150, 150), (200, 60)];
        for (x0, y0) in spots {
            for y in y0..y0 + 24 {
                for x in x0..x0 + 24 {
                    mask.set(x, y, true);
                }
            }
        }
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        for (x0, y0) in spots {
            assert!(
                !matches!(
                    classify(&bbox(x0, y0, 24, 24), &hint, 2),
                    GridFit::SpansMultipleCells { .. }
                ),
                "single icon at ({x0},{y0}) must not be flagged"
            );
        }
    }

    #[test]
    fn a_single_valley_axis_is_never_a_grid_axis() {
        // Two blocks only: one interior valley, no width sample.
        let mut mask = ForegroundMask::new(128, 128);
        for y in 20..100 {
            for x in 10..50 {
                mask.set(x, y, true);
            }
            for x in 70..110 {
                mask.set(x, y, true);
            }
        }
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        assert!(!hint.grid_x && !hint.grid_y, "{hint:?}");
    }

    #[test]
    fn classify_flags_a_merged_pair_and_spares_single_icons() {
        let mask = grid_mask(256, 4, 64, 40, 8, 8);
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        // Single icon: 8..48 inside the first band.
        assert!(matches!(
            classify(&bbox(8, 8, 40, 40), &hint, 2),
            GridFit::Cell { col: 0, row: 0 }
        ));
        // Merged pair across the first valley (centre 64): 10..118.
        assert!(matches!(
            classify(&bbox(10, 8, 108, 40), &hint, 2),
            GridFit::SpansMultipleCells { axes: 1 }
        ));
        // Two icons merged vertically.
        assert!(matches!(
            classify(&bbox(8, 10, 40, 108), &hint, 2),
            GridFit::SpansMultipleCells { axes: 2 }
        ));
    }

    #[test]
    fn classify_tolerates_two_pixels_of_drift() {
        let mask = grid_mask(256, 4, 64, 40, 8, 8);
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        // A box that pokes 1 px past the valley centre on both sides is still
        // one cell (within the 2-px tolerance).
        assert!(matches!(
            classify(&bbox(63, 8, 3, 40), &hint, 2),
            GridFit::Cell { .. }
        ));
    }

    #[test]
    fn off_grid_when_no_axis_qualifies() {
        let mut mask = ForegroundMask::new(64, 64);
        for y in 4..12 {
            for x in 4..12 {
                mask.set(x, y, true);
            }
        }
        let mut st = GridStats::default();
        let hint = detect_grid(&mask, &GridParams::default(), &mut st);
        assert!(!hint.any());
        assert_eq!(classify(&bbox(4, 4, 8, 8), &hint, 2), GridFit::OffGrid);
    }

    #[test]
    fn detection_is_deterministic_and_profiles_use_runs() {
        let mask = grid_mask(256, 4, 64, 40, 8, 8);
        let mut a = GridStats::default();
        let mut b = GridStats::default();
        let ha = detect_grid(&mask, &GridParams::default(), &mut a);
        let hb = detect_grid(&mask, &GridParams::default(), &mut b);
        assert_eq!(ha, hb);
        // Timing is the only field allowed to differ between runs.
        assert_eq!(
            (a.flagged, a.resplit, a.regions),
            (b.flagged, b.resplit, b.regions)
        );
        assert_eq!(ha.valley_x, ha.valley_y, "square lattice is symmetric");
    }
}
