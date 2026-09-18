//! §3.5 auto-leveling — the arithmetic that makes a sheet look even.
//!
//! An icon scaled to fill its cell is not the same as an icon that *looks* the
//! same size as its neighbours. Three corrections, in the order the spec lists
//! them:
//!
//! ```text
//! target_ink = (cell − 2·padding) · ink_ratio        # e.g. (64−16)·0.80 = 38.4
//! s_fit      = min(target_ink / ink.w, target_ink / ink.h)
//! s_stroke   = clamp((median_stroke / stroke)^0.35, 0.88, 1.12)
//! s_solid    = clamp(1 + 0.06·(median_solidity − solidity), 0.96, 1.04)
//! s          = s_fit · s_stroke · s_solid
//! ```
//!
//! `s_fit` is containment: the *larger* side of the ink lands on `target_ink`,
//! so a wide icon does not touch the top and bottom of its cell. The two
//! corrections then grow a thin outline (which reads as smaller than a solid
//! shape of the same box) and nudge a hollow shape back up. Both are damped and
//! clamped on purpose: naive linear stroke compensation makes thin icons
//! balloon, and a sheet is leveled *relative to its own median*, not against an
//! absolute idea of thickness.
//!
//! Two safeguards stay visible to the caller:
//!
//! * **The overflow guard** — if the corrected scale would push the ink outside
//!   the cell's inner box, the icon backs off to the pure fit and is flagged, so
//!   a very small `ink_ratio` can never do damage silently.
//! * **Every correction is reported** — the median it was measured against, how
//!   many icons hit a clamp, and the resulting `ink_size_cv`, `stroke_cv` and
//!   baseline spread, because "the sheet looks even" has to be explainable to
//!   the designer who disagrees.
//!
//! The whole thing is arithmetic on cached metrics (§3.5: `1000 icons in
//! <20 ms`), so it is pure, allocation-light and identical on every platform.

use super::{GridLayout, IconMetrics, SheetSpec};

/// Damping exponent on the stroke correction.
pub const STROKE_DAMPING: f32 = 0.35;
/// Lower clamp on the stroke correction (a very thick icon shrinks at most 12 %).
pub const STROKE_MIN: f32 = 0.88;
/// Upper clamp on the stroke correction (a very thin icon grows at most 12 %).
pub const STROKE_MAX: f32 = 1.12;
/// How strongly solidity moves the scale.
pub const SOLIDITY_GAIN: f32 = 0.06;
/// Lower clamp on the solidity correction.
pub const SOLIDITY_MIN: f32 = 0.96;
/// Upper clamp on the solidity correction.
pub const SOLIDITY_MAX: f32 = 1.04;

/// The icon backed off to a plain containment fit because the corrected scale
/// would have overflowed its cell's inner box.
pub const FLAG_OVERFLOW_BACKOFF: u32 = 1 << 0;
/// The stroke correction hit one of its clamps.
pub const FLAG_STROKE_CLAMPED: u32 = 1 << 1;
/// The solidity correction hit one of its clamps.
pub const FLAG_SOLIDITY_CLAMPED: u32 = 1 << 2;

/// One icon as leveling sees it: a stable id and its measured ink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconInput {
    /// Caller's id (row number, icon id — the plan only passes it through).
    pub id: u32,
    /// The icon's ink, measured inside its own crop.
    pub metrics: IconMetrics,
}

/// Where one icon ends up on the sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconPlacement {
    /// The id this placement was computed for.
    pub id: u32,
    /// The cell's top-left corner in sheet pixels.
    pub cell: (u32, u32),
    /// The destination of the icon's *ink box* in sheet pixels.
    pub ink: Rect,
    /// Scale from crop pixels to sheet pixels (fit × stroke × solidity).
    pub scale: f32,
    /// The local ink box, kept so an exporter can build the transform without
    /// re-measuring anything.
    pub ink_local: (f32, f32, f32, f32),
    /// [`FLAG_OVERFLOW_BACKOFF`], [`FLAG_STROKE_CLAMPED`], [`FLAG_SOLIDITY_CLAMPED`].
    pub flags: u32,
}

impl IconPlacement {
    /// The SVG/PDF transform that maps crop-local coordinates onto the sheet:
    /// `translate(tx, ty) scale(scale)`.
    #[must_use]
    pub fn transform(&self) -> (f32, f32, f32) {
        let (inx, iny, _, _) = self.ink_local;
        (
            self.ink.x - self.scale * inx,
            self.ink.y - self.scale * iny,
            self.scale,
        )
    }

    /// True when the icon was flagged for any reason.
    #[must_use]
    pub fn is_flagged(&self) -> bool {
        self.flags != 0
    }
}

/// A rectangle in sheet pixels (f32 while being computed, rounded on export).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// The right edge.
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// The bottom edge.
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// What leveling did to a sheet — the numbers §3.5 says to report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelReport {
    /// Icons placed.
    pub icons: usize,
    /// CV of the placed ink size (the sheet's headline number, target `< 0.05`).
    pub ink_size_cv: f32,
    /// CV of the icons' stroke weights as measured — how uneven the input was.
    pub stroke_cv: f32,
    /// Spread of the placed ink boxes' bottom edges, in pixels — the *worst
    /// row*, not the whole sheet. With `Placement::Baseline` the ink of every
    /// icon in a row shares a bottom edge, so this is the number that proves
    /// it; across many rows the sheet's own height would swamp it.
    pub baseline_spread: f32,
    /// The stroke weight the sheet was leveled against.
    pub median_stroke: f32,
    /// The solidity the sheet was leveled against.
    pub median_solidity: f32,
    /// Icons that backed off to a plain fit.
    pub overflow_backoffs: usize,
    /// Icons whose stroke correction was clamped.
    pub stroke_clamped: usize,
    /// Icons whose solidity correction was clamped.
    pub solidity_clamped: usize,
}

/// Places every icon and reports what the leveling did.
#[must_use]
pub fn place(
    icons: &[IconInput],
    spec: &SheetSpec,
    layout: &GridLayout,
) -> (Vec<IconPlacement>, LevelReport) {
    let median_stroke = median(icons.iter().map(|i| i.metrics.stroke));
    let median_solidity = median(icons.iter().map(|i| i.metrics.solidity));
    let inner = (spec.cell as f32 - 2.0 * spec.padding as f32).max(0.0);
    let target_ink = inner * spec.ink_ratio;

    let mut placements = Vec::with_capacity(icons.len());
    let mut sizes = Vec::with_capacity(icons.len());
    let mut strokes = Vec::with_capacity(icons.len());
    // Per-row bottom edges, so the baseline spread is a property of a row (see
    // `LevelReport::baseline_spread`).
    let rows = layout.rows as usize;
    let mut row_low = vec![f32::INFINITY; rows];
    let mut row_high = vec![f32::NEG_INFINITY; rows];
    let mut stroke_clamped = 0usize;
    let mut solidity_clamped = 0usize;
    let mut overflow_backoffs = 0usize;

    for (index, icon) in icons.iter().enumerate() {
        let m = icon.metrics;
        let mut flags = 0u32;
        let (cell_x, cell_y) = layout.cell_origin(index, spec);

        // A degenerate ink box would divide by zero; treat it as a point and
        // give it the pure fit of the other axis rather than an infinity.
        let ink_w = m.ink_w.max(f32::MIN_POSITIVE);
        let ink_h = m.ink_h.max(f32::MIN_POSITIVE);
        let s_fit = (target_ink / ink_w).min(target_ink / ink_h).max(0.0);

        let (s_stroke, stroke_hit_clamp) = if median_stroke > 0.0 && m.stroke > 0.0 {
            let raw = (median_stroke / m.stroke).powf(STROKE_DAMPING);
            let clamped = raw.clamp(STROKE_MIN, STROKE_MAX);
            (clamped, (clamped - raw).abs() > f32::EPSILON)
        } else {
            (1.0, false)
        };
        let s_solid = {
            let raw = 1.0 + SOLIDITY_GAIN * (median_solidity - m.solidity);
            let clamped = raw.clamp(SOLIDITY_MIN, SOLIDITY_MAX);
            (clamped, (clamped - raw).abs() > f32::EPSILON)
        };
        if stroke_hit_clamp {
            flags |= FLAG_STROKE_CLAMPED;
            stroke_clamped += 1;
        }
        if s_solid.1 {
            flags |= FLAG_SOLIDITY_CLAMPED;
            solidity_clamped += 1;
        }

        let mut scale = s_fit * s_stroke * s_solid.0;
        let placed_w = scale * m.ink_w;
        let placed_h = scale * m.ink_h;
        if placed_w > inner || placed_h > inner {
            // The corrections would push the ink out of its cell: keep the fit,
            // drop the corrections, and say so.
            scale = if inner > 0.0 {
                let pure = (inner / ink_w).min(inner / ink_h).max(0.0);
                pure.min(s_fit)
            } else {
                s_fit
            };
            flags |= FLAG_OVERFLOW_BACKOFF;
            overflow_backoffs += 1;
        }

        let placed_w = scale * m.ink_w;
        let placed_h = scale * m.ink_h;
        let (shift_x, shift_y) = match spec.placement {
            super::Placement::Center => (0.0, 0.0),
            super::Placement::OpticalCenter => {
                // Half the distance from the centre of mass to the box centre:
                // a top-heavy arrow centred by its box *reads* as low, so the
                // icon moves until the weight is nearer the middle — halved, so
                // a lopsided icon cannot drift out of its cell either.
                let (cx, cy) = m.ink_center();
                (0.5 * (cx - m.centroid_x), 0.5 * (cy - m.centroid_y))
            }
            super::Placement::Baseline => (0.0, 0.0), // handled by the vertical anchor below
        };
        let free_x = (inner - placed_w).max(0.0) * 0.5;
        let free_y = (inner - placed_h).max(0.0) * 0.5;
        let ink_x = cell_x as f32 + spec.padding as f32 + free_x + shift_x;
        let ink_y = match spec.placement {
            super::Placement::Baseline => {
                // The ink sits *on* the inner box's baseline, so a row of icons
                // shares a bottom edge whatever their heights.
                cell_y as f32 + spec.padding as f32 + (inner - placed_h).max(0.0)
            }
            _ => cell_y as f32 + spec.padding as f32 + free_y + shift_y,
        };

        sizes.push(scale * m.ink_long_side());
        strokes.push(m.stroke);
        if layout.columns > 0 && rows > 0 {
            let row = index / layout.columns as usize;
            if let (Some(low), Some(high)) = (row_low.get_mut(row), row_high.get_mut(row)) {
                *low = low.min(ink_y + placed_h);
                *high = high.max(ink_y + placed_h);
            }
        }
        placements.push(IconPlacement {
            id: icon.id,
            cell: (cell_x, cell_y),
            ink: Rect {
                x: ink_x,
                y: ink_y,
                w: placed_w,
                h: placed_h,
            },
            scale,
            ink_local: (m.ink_x, m.ink_y, m.ink_w, m.ink_h),
            flags,
        });
    }

    let report = LevelReport {
        icons: icons.len(),
        ink_size_cv: cv(&sizes),
        stroke_cv: cv(&strokes),
        baseline_spread: row_low
            .iter()
            .zip(row_high.iter())
            .filter(|(low, high)| low.is_finite() && high.is_finite())
            .map(|(low, high)| high - low)
            .fold(0.0f32, f32::max),
        median_stroke,
        median_solidity,
        overflow_backoffs,
        stroke_clamped,
        solidity_clamped,
    };
    (placements, report)
}

/// The median of a series (`0.0` when empty), computed on a sorted copy so the
/// result never depends on the order the icons arrived in.
#[must_use]
pub fn median(values: impl Iterator<Item = f32>) -> f32 {
    let mut values: Vec<f32> = values.collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) * 0.5
    }
}

/// Coefficient of variation (`σ/µ`), the number §3.5 gates on.
#[must_use]
pub fn cv(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let n = values.len() as f32;
    let mean = values.iter().sum::<f32>() / n;
    if mean <= 0.0 {
        return 0.0;
    }
    let var = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
    var.sqrt() / mean
}

/// `max − min` (the spread the report quotes for baselines).
#[must_use]
pub fn spread(values: &[f32]) -> f32 {
    let mut it = values.iter().copied();
    let Some(first) = it.next() else {
        return 0.0;
    };
    let (mut lo, mut hi) = (first, first);
    for v in it {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    hi - lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{GridLayout, Placement, SheetSpec};

    fn spec() -> SheetSpec {
        SheetSpec {
            cell: 64,
            padding: 8,
            gap: 8,
            margin: 8,
            columns: 4,
            ink_ratio: 0.8,
            placement: Placement::Center,
        }
    }

    /// An icon whose ink fills its 20×20 crop, `stroke` px thick.
    fn icon(id: u32, side: f32, stroke: f32, solidity: f32) -> IconInput {
        IconInput {
            id,
            metrics: IconMetrics {
                ink_x: 0.0,
                ink_y: 0.0,
                ink_w: side,
                ink_h: side,
                ink_area: (side * side * solidity) as u32,
                centroid_x: side * 0.5,
                centroid_y: side * 0.5,
                stroke,
                solidity,
            },
        }
    }

    #[test]
    fn a_uniform_sheet_levels_to_one_size() {
        let icons: Vec<IconInput> = (0..8).map(|i| icon(i, 20.0, 4.0, 0.5)).collect();
        let layout = GridLayout::solve(icons.len(), &spec());
        let (placements, report) = place(&icons, &spec(), &layout);
        assert_eq!(placements.len(), 8);
        // target_ink = (64 − 16) · 0.8 = 38.4; the ink is already square, so the
        // scale is exactly 38.4/20 and every icon lands on the same size.
        for p in &placements {
            assert!((p.scale - 1.92).abs() < 1e-5, "scale {}", p.scale);
            assert!((p.ink.w - 38.4).abs() < 1e-4);
            assert!((p.ink.h - 38.4).abs() < 1e-4);
            assert!(!p.is_flagged());
        }
        assert!(report.ink_size_cv < 1e-6, "cv {}", report.ink_size_cv);
        assert_eq!(report.overflow_backoffs, 0);
        // Centred in its cell: (64 − 38.4)/2 = 12.8 from each side.
        assert!((placements[0].ink.x - (8.0 + 12.8)).abs() < 1e-4);
        assert!((placements[0].ink.y - (8.0 + 12.8)).abs() < 1e-4);
    }

    #[test]
    fn the_fit_uses_the_larger_side_so_a_wide_icon_stays_inside_its_cell() {
        let mut wide = icon(0, 20.0, 4.0, 0.5);
        wide.metrics.ink_w = 40.0;
        wide.metrics.ink_h = 10.0;
        let icons = [wide];
        let layout = GridLayout::solve(1, &spec());
        let (p, _) = place(&icons, &spec(), &layout);
        // The long side is normalized to target_ink; the short side follows the
        // aspect ratio, so the ink never touches the cell's top or bottom.
        assert!((p[0].ink.w - 38.4).abs() < 1e-4);
        assert!((p[0].ink.h - 9.6).abs() < 1e-4);
        let (x, _y) = p[0].cell;
        assert!(x as f32 + p[0].ink.x - x as f32 >= 0.0);
    }

    #[test]
    fn thin_strokes_grow_and_thick_ones_shrink_relatively() {
        // Strokes 8 and 14, median 11 — both corrections inside their clamps.
        let icons = [icon(0, 20.0, 8.0, 0.5), icon(1, 20.0, 14.0, 0.5)];
        let layout = GridLayout::solve(2, &spec());
        let (p, report) = place(&icons, &spec(), &layout);
        assert!(p[0].scale > p[1].scale, "{} vs {}", p[0].scale, p[1].scale);
        assert_eq!(report.median_stroke, 11.0);
        assert!(!p[0].is_flagged() && !p[1].is_flagged(), "{p:?}");
        // The correction is the damped ratio, exactly: the 1.75× stroke gap
        // becomes a 1.22× size gap, so the thin icon *still* ends up larger
        // (that is the point) but by far less than a linear rule would.
        let ratio = p[0].ink.w / p[1].ink.w;
        let expected = (14.0f32 / 8.0).powf(STROKE_DAMPING);
        assert!((ratio - expected).abs() < 1e-4, "{ratio} vs {expected}");
        assert!(ratio < 14.0 / 8.0);
        // The input was very uneven, and the report says so.
        assert!(report.stroke_cv > 0.2, "{}", report.stroke_cv);
    }

    #[test]
    fn the_stroke_correction_is_damped_and_clamped() {
        // The median is the thin icon, so the thick one is scaled down hard —
        // and stops at the clamp instead of collapsing.
        let icons = [icon(0, 20.0, 1.0, 0.5), icon(1, 20.0, 100.0, 0.5)];
        let layout = GridLayout::solve(2, &spec());
        let (p, report) = place(&icons, &spec(), &layout);
        let base = 38.4 / 20.0;
        assert!(
            (p[1].scale / base - STROKE_MIN).abs() < 1e-4,
            "{}",
            p[1].scale / base
        );
        // Both ends of the correction are clamped: the very thin icon grows at
        // most 12 %, the very thick one shrinks at most 12 %.
        assert!(p[1].flags & FLAG_STROKE_CLAMPED != 0);
        assert_eq!(report.stroke_clamped, 2);
        // The other side of the clamp: a very thin icon grows at most 12 %.
        let icons = [icon(0, 20.0, 100.0, 0.5), icon(1, 20.0, 1.0, 0.5)];
        let (p, _) = place(&icons, &spec(), &layout);
        assert!(
            (p[1].scale / base - STROKE_MAX).abs() < 1e-4,
            "{}",
            p[1].scale / base
        );
    }

    #[test]
    fn solidity_nudges_hollow_shapes_up_and_solid_ones_down() {
        // Medians: stroke 4 (no stroke correction), solidity 0.5.
        let icons = [
            icon(0, 20.0, 4.0, 0.5), // at the median: untouched
            icon(1, 20.0, 4.0, 0.0), // hollow: pushed up by 3 %
            icon(2, 20.0, 4.0, 1.0), // solid: pushed down by 3 %
        ];
        let layout = GridLayout::solve(3, &spec());
        let (p, report) = place(&icons, &spec(), &layout);
        let base = 38.4 / 20.0;
        assert!(p[1].scale > p[0].scale && p[0].scale > p[2].scale);
        assert!(
            (p[1].scale / base - 1.03).abs() < 1e-4,
            "{}",
            p[1].scale / base
        );
        assert!((p[2].scale / base - 0.97).abs() < 1e-4);
        assert!(!p[0].is_flagged() && !p[1].is_flagged() && !p[2].is_flagged());
        assert_eq!(report.solidity_clamped, 0);
        assert!((report.median_solidity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_solidity_correction_stops_at_its_clamp() {
        // Median solidity 1.0 against a fully hollow icon: the raw correction
        // would be +6 %, and the clamp holds it at +4 %.
        let icons = [
            icon(0, 20.0, 4.0, 1.0),
            icon(1, 20.0, 4.0, 1.0),
            icon(2, 20.0, 4.0, 1.0),
            icon(3, 20.0, 4.0, 0.0),
        ];
        let layout = GridLayout::solve(4, &spec());
        let (p, report) = place(&icons, &spec(), &layout);
        let base = 38.4 / 20.0;
        assert!((report.median_solidity - 1.0).abs() < 1e-6);
        assert!((p[3].scale / base - SOLIDITY_MAX).abs() < 1e-4);
        assert!(p[3].flags & FLAG_SOLIDITY_CLAMPED != 0);
        assert_eq!(report.solidity_clamped, 1);
        // The three solid icons are at the median, so they are untouched.
        assert!((p[0].scale / base - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_overflow_guard_backs_off_and_flags() {
        // An icon whose ink is meant to fill its cell completely, on a sheet
        // where the other icons are much thicker: the stroke correction would
        // grow it past the cell, so the guard drops the corrections.
        let thin = icon(0, 20.0, 1.0, 0.5);
        let thick = [icon(1, 20.0, 40.0, 0.5), icon(2, 20.0, 40.0, 0.5)];
        let icons = [thin, thick[0], thick[1]];
        let tight = SheetSpec {
            padding: 0,
            ink_ratio: 1.0,
            ..spec()
        };
        let layout = GridLayout::solve(3, &tight);
        let (p, report) = place(&icons, &tight, &layout);
        assert!(p[0].flags & FLAG_OVERFLOW_BACKOFF != 0, "{:?}", p[0]);
        assert_eq!(report.overflow_backoffs, 1);
        // Backed off to the cell, not to the corrected scale.
        assert!((p[0].scale - 64.0 / 20.0).abs() < 1e-4, "{}", p[0].scale);
        assert!(p[0].ink.w <= 64.0 + 1e-4 && p[0].ink.h <= 64.0 + 1e-4);
        // Nothing hangs off the sheet or into a neighbouring cell.
        assert!(p[0].ink.x >= 0.0 && p[0].ink.right() <= layout.width as f32 + 1e-4);
        assert!(p[0].ink.bottom() <= layout.height as f32 + 1e-4);
        // The two thick icons sit at the median stroke, so they are untouched —
        // the guard only ever drops a correction, never invents one.
        assert!((p[1].scale - 64.0 / 20.0).abs() < 1e-4, "{}", p[1].scale);
        assert!(!p[1].is_flagged());
    }

    #[test]
    fn baseline_placement_puts_every_icon_on_one_line() {
        // Two icons of the same box whose strokes put them at different scales
        // (median 11: one grows, one shrinks), so the shared baseline is a real
        // property and not a coincidence of two equal sizes.
        let icons = [icon(0, 20.0, 8.0, 0.5), icon(1, 20.0, 14.0, 0.5)];
        let s = SheetSpec {
            placement: Placement::Baseline,
            ..spec()
        };
        let layout = GridLayout::solve(2, &s);
        let (p, report) = place(&icons, &s, &layout);
        assert!(p[0].ink.h > p[1].ink.h, "{p:?}");
        assert!(
            (p[0].ink.bottom() - p[1].ink.bottom()).abs() < 1e-4,
            "{p:?}"
        );
        assert_eq!(report.baseline_spread, 0.0);
        // Both still sit inside their cells.
        assert!(p[0].ink.y >= 8.0 - 1e-4 && p[0].ink.bottom() <= 8.0 + 64.0 + 1e-4);
        // Both icons are centred across their own cell — by box centre, since
        // the fit normalizes the long side and so makes the tall icon narrower
        // — and both sit on the one baseline.
        for p in &p {
            let centre = p.ink.x + p.ink.w * 0.5;
            let cell_centre = p.cell.0 as f32 + s.cell as f32 * 0.5;
            assert!((centre - cell_centre).abs() < 1e-4, "{p:?}");
        }
        // …and both are square, so the shared baseline is the same both ways.
        assert!((p[0].ink.h - p[0].ink.w).abs() < 1e-4);
    }

    #[test]
    fn the_baseline_spread_is_the_worst_row_not_the_whole_sheet() {
        // Eight icons over two columns, alternating scale: without baseline
        // placement each row's bottoms disagree; with it they agree exactly,
        // and in neither case does the sheet's own height leak into the number.
        let icons: Vec<IconInput> = (0..8)
            .map(|i| {
                if i % 2 == 0 {
                    icon(i, 20.0, 8.0, 0.5)
                } else {
                    icon(i, 20.0, 14.0, 0.5)
                }
            })
            .collect();
        let two_columns = SheetSpec {
            columns: 2,
            ..spec()
        };
        let layout = GridLayout::solve(icons.len(), &two_columns);
        let (_, centred) = place(&icons, &two_columns, &layout);
        assert!(centred.baseline_spread > 0.0);
        assert!(
            centred.baseline_spread < layout.height as f32 / 2.0,
            "the spread is a row's, not the sheet's: {}",
            centred.baseline_spread
        );
        let baseline = SheetSpec {
            placement: Placement::Baseline,
            ..two_columns
        };
        let (_, aligned) = place(&icons, &baseline, &layout);
        assert_eq!(aligned.baseline_spread, 0.0);
    }

    #[test]
    fn optical_center_shifts_toward_the_mass() {
        let mut heavy = icon(0, 20.0, 4.0, 0.5);
        // Mass concentrated at the top-left: the optical centre pulls the icon
        // *down-right* so the visual weight lands in the middle of the cell.
        heavy.metrics.centroid_x = 2.0;
        heavy.metrics.centroid_y = 2.0;
        let icons = [heavy];
        let s = SheetSpec {
            placement: Placement::OpticalCenter,
            ..spec()
        };
        let layout = GridLayout::solve(1, &s);
        let (optical, _) = place(&icons, &s, &layout);
        let (plain, _) = place(&icons, &spec(), &layout);
        assert!(optical[0].ink.x > plain[0].ink.x);
        assert!(optical[0].ink.y > plain[0].ink.y);
        // The shift is half the centroid-to-centre delta, exactly.
        assert!((optical[0].ink.x - plain[0].ink.x - 0.5 * (10.0 - 2.0)).abs() < 1e-4);
        // …and the mass ends up nearer the middle of the cell than it started.
        let opted = optical[0].ink.x + 10.0 * optical[0].scale; // box centre of mass
        let plain_mass = plain[0].ink.x + 2.0 * plain[0].scale;
        let middle = optical[0].cell.0 as f32 + 32.0;
        assert!((opted - middle).abs() < (plain_mass - middle).abs());
    }

    #[test]
    fn the_transform_maps_the_ink_box_onto_the_placed_box() {
        let mut offset = icon(0, 20.0, 4.0, 0.5);
        offset.metrics.ink_x = 3.0;
        offset.metrics.ink_y = 5.0;
        let icons = [offset];
        let layout = GridLayout::solve(1, &spec());
        let (p, _) = place(&icons, &spec(), &layout);
        let (tx, ty, s) = p[0].transform();
        // The ink box's top-left corner lands where the placement said.
        assert!((tx + s * 3.0 - p[0].ink.x).abs() < 1e-4);
        assert!((ty + s * 5.0 - p[0].ink.y).abs() < 1e-4);
        // …and the far corner lands on the far corner.
        assert!((tx + s * 23.0 - p[0].ink.right()).abs() < 1e-4);
        assert!((ty + s * 25.0 - p[0].ink.bottom()).abs() < 1e-4);
    }

    #[test]
    fn an_empty_sheet_reports_zeros_rather_than_nan() {
        let layout = GridLayout::solve(0, &spec());
        let (p, report) = place(&[], &spec(), &layout);
        assert!(p.is_empty());
        assert_eq!(report.ink_size_cv, 0.0);
        assert_eq!(report.stroke_cv, 0.0);
        assert_eq!(report.baseline_spread, 0.0);
    }

    #[test]
    fn median_and_cv_are_order_independent() {
        assert_eq!(median([3.0, 1.0, 2.0].into_iter()), 2.0);
        assert_eq!(median([4.0, 1.0, 3.0, 2.0].into_iter()), 2.5);
        assert_eq!(median(std::iter::empty()), 0.0);
        assert!((cv(&[2.0, 2.0, 2.0]) - 0.0).abs() < 1e-9);
        assert!((cv(&[1.0, 3.0]) - 0.5).abs() < 1e-6);
        assert_eq!(spread(&[5.0, 1.0, 9.0]), 8.0);
        assert_eq!(spread(&[]), 0.0);
    }

    #[test]
    fn placement_is_deterministic() {
        let icons: Vec<IconInput> = (0..40)
            .map(|i| {
                icon(
                    i,
                    10.0 + (i % 7) as f32,
                    1.0 + (i % 5) as f32,
                    0.2 + 0.02 * (i % 9) as f32,
                )
            })
            .collect();
        let layout = GridLayout::solve(icons.len(), &spec());
        let a = place(&icons, &spec(), &layout);
        let b = place(&icons, &spec(), &layout);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
    }
}
