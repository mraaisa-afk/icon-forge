//! §3.4 confidence score — weighted deductions over the F3/F5 grouping signals.
//!
//! After the refine chain has produced the final group list, this module turns
//! the evidence the chain already collected into the number the UI shows:
//!
//! ```text
//! confidence = clamp01(1 − Σ weightᵢ · signalᵢ)
//! ```
//!
//! with the six §3.4 signals, in spec order:
//!
//! | signal | weight | reads |
//! |---|---|---|
//! | spans multiple cells | 0.30 | every group the F5 lattice check acted on |
//! | high merge ratio | 0.20 | F1 merges that survived the corrections |
//! | size MAD (outlier spread) | 0.15 | MAD/median of each group's `min(w, h)` |
//! | border touch | 0.15 | bbox within the sheet border margin |
//! | weak grid | 0.10 | measured regularity below the F5 grid threshold |
//! | uncertain background | 0.10 | `BackgroundKind` at low consensus |
//!
//! Each `signalᵢ ∈ [0, 1]` is a *share* (how many groups are affected) or a
//! documented ramp between two calibrated endpoints; every endpoint lives in
//! [`ConfidenceParams`] so nothing downstream is a magic constant. The weights
//! are the spec's and are not to be tuned — only the ramps are calibration
//! (W11), and each is exercised by a unit test at both ends.
//!
//! Warnings are the actionable half of the same information: one
//! [`GroupWarning`] per group the overlay must highlight, which is what the
//! §3.4 status line counts (`… · 3 groups need review`). Two kinds exist, and
//! both drive the spans signal — a group that *still* straddles a valley
//! ([`WarningKind::SpansMultipleCells`]) and a group F5 handed back from a merge
//! it proved wrong ([`WarningKind::RestoredFromMerge`], whose bbox did span
//! cells before the correction). A sheet that needed three lattice corrections
//! must not read as 100 % confident.
//!
//! This is the only place a confidence number is produced; `RefineStats`
//! carries the report so callers never recompute it (`W12`/Phase 4 surface it).

use isg_core::{ForegroundMask, IconGroup};

use super::background::{BackgroundKind, BackgroundModel};
use super::grid::{classify, GridFit, GridHint};

/// Version of the scoring rules; bump when a signal or a ramp changes.
pub const CONFIDENCE_VERSION: u32 = 1;

/// Number of §3.4 signals (fixed — the deductions array is aligned to
/// [`SignalKind::ALL`]).
pub const SIGNAL_COUNT: usize = 6;

/// The six §3.4 signals, in spec order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalKind {
    /// A flagged group spans more than one lattice cell (F5).
    SpansMultipleCells,
    /// F1 merged away a large share of the components.
    HighMergeRatio,
    /// Group sizes are spread far from the median size.
    SizeMad,
    /// Groups sit on the sheet border (cropped icons).
    BorderTouch,
    /// Valleys exist but their pitch is irregular (weak lattice).
    WeakGrid,
    /// Background detection fell back to a weak detector.
    UncertainBackground,
}

impl SignalKind {
    /// Every signal, in the order [`ConfidenceReport::deductions`] uses.
    pub const ALL: [Self; SIGNAL_COUNT] = [
        Self::SpansMultipleCells,
        Self::HighMergeRatio,
        Self::SizeMad,
        Self::BorderTouch,
        Self::WeakGrid,
        Self::UncertainBackground,
    ];

    /// Index into [`ConfidenceReport::deductions`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SpansMultipleCells => 0,
            Self::HighMergeRatio => 1,
            Self::SizeMad => 2,
            Self::BorderTouch => 3,
            Self::WeakGrid => 4,
            Self::UncertainBackground => 5,
        }
    }

    /// Human-readable label for the UI/audit trail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SpansMultipleCells => "spans multiple cells",
            Self::HighMergeRatio => "high merge ratio",
            Self::SizeMad => "size spread",
            Self::BorderTouch => "touches the sheet border",
            Self::WeakGrid => "weak grid",
            Self::UncertainBackground => "uncertain background",
        }
    }
}

/// What is wrong with one group (the overlay's warning layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningKind {
    /// The group's bbox straddles an F5 valley centre and survived — it should
    /// have been split and was not.
    SpansMultipleCells,
    /// The group is an original component F5 restored after proving an F1 merge
    /// wrong (near-touching neighbours).
    RestoredFromMerge,
}

impl WarningKind {
    /// Human-readable label for the UI/audit trail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SpansMultipleCells => "spans multiple cells",
            Self::RestoredFromMerge => "restored from a merge across a valley",
        }
    }
}

/// One group needing review, by **index into the final group list**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupWarning {
    /// Index into the group list the report was scored against.
    pub group: u32,
    /// What is wrong with it.
    pub kind: WarningKind,
}

/// The measured signal values (each `∈ [0, 1]`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ConfidenceSignals {
    /// Share of groups the F5 lattice check acted on (still-straddling ones
    /// plus restorations from provably wrong merges).
    pub spans: f32,
    /// Ramp of the surviving-F1-merge share
    /// `merges / (groups + merges)` (0 below 25 %, 1 at 75 %).
    pub merge_ratio: f32,
    /// Ramp of `MAD / median` of group `min(w, h)` (0 below 0.35, 1 at 0.70).
    pub size_mad: f32,
    /// Share of groups within the border margin.
    pub border_touch: f32,
    /// Share of axes (0/0.5/1) with ≥ 2 valleys and sub-threshold regularity.
    pub weak_grid: f32,
    /// Ramp of the background consensus (0 when no model was supplied).
    pub uncertain_background: f32,
}

/// Scoring configuration: the §3.4 weights plus the calibrated ramp endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfidenceParams {
    /// Master switch (`false` ⇒ a perfect score and no warnings).
    pub enabled: bool,
    /// Valley-straddle tolerance, in pixels (matches `GridParams`).
    pub span_tolerance: u32,
    /// Weight: spans multiple cells (spec).
    pub w_spans: f32,
    /// Weight: high merge ratio (spec).
    pub w_merge: f32,
    /// Weight: size MAD (spec).
    pub w_size_mad: f32,
    /// Weight: border touch (spec).
    pub w_border: f32,
    /// Weight: weak grid (spec).
    pub w_weak_grid: f32,
    /// Weight: uncertain background (spec).
    pub w_background: f32,
    /// Merge ratio at which the deduction starts (calibration).
    pub merge_ratio_lo: f32,
    /// Merge ratio at which the deduction is full (calibration).
    pub merge_ratio_hi: f32,
    /// `MAD / median` at which the size deduction starts (calibration).
    pub size_mad_lo: f32,
    /// `MAD / median` at which the size deduction is full (calibration).
    pub size_mad_hi: f32,
    /// Regularity at which an irregular axis starts counting (calibration).
    pub weak_grid_lo: f32,
    /// Regularity at which an axis is a grid instead of weak — must match
    /// `GridParams::regularity_min` (calibration).
    pub weak_grid_hi: f32,
    /// Border margin as a fraction of the smaller sheet dimension (calibration).
    pub border_margin_frac: f32,
    /// Consensus below which a background detector counts as uncertain
    /// (calibration; the default matches `SegParams::border_share_min`).
    pub background_consensus_min: f32,
}

impl Default for ConfidenceParams {
    fn default() -> Self {
        Self {
            enabled: true,
            span_tolerance: 2,
            w_spans: 0.30,
            w_merge: 0.20,
            w_size_mad: 0.15,
            w_border: 0.15,
            w_weak_grid: 0.10,
            w_background: 0.10,
            merge_ratio_lo: 0.25,
            merge_ratio_hi: 0.75,
            size_mad_lo: 0.35,
            size_mad_hi: 0.70,
            weak_grid_lo: 0.35,
            weak_grid_hi: 0.75,
            border_margin_frac: 0.02,
            background_consensus_min: 0.85,
        }
    }
}

impl ConfidenceParams {
    /// Weight of one signal (spec values unless overridden).
    #[must_use]
    pub const fn weight(&self, kind: SignalKind) -> f32 {
        match kind {
            SignalKind::SpansMultipleCells => self.w_spans,
            SignalKind::HighMergeRatio => self.w_merge,
            SignalKind::SizeMad => self.w_size_mad,
            SignalKind::BorderTouch => self.w_border,
            SignalKind::WeakGrid => self.w_weak_grid,
            SignalKind::UncertainBackground => self.w_background,
        }
    }

    /// Signal value for one kind (field access, so `ALL` iteration stays total).
    #[must_use]
    pub const fn signal(&self, signals: &ConfidenceSignals, kind: SignalKind) -> f32 {
        match kind {
            SignalKind::SpansMultipleCells => signals.spans,
            SignalKind::HighMergeRatio => signals.merge_ratio,
            SignalKind::SizeMad => signals.size_mad,
            SignalKind::BorderTouch => signals.border_touch,
            SignalKind::WeakGrid => signals.weak_grid,
            SignalKind::UncertainBackground => signals.uncertain_background,
        }
    }
}

/// The scored result: number, its inputs, and the review list.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfidenceReport {
    /// `1 − Σ weightᵢ · signalᵢ`, clamped to `[0, 1]`.
    pub score: f32,
    /// Measured signal values.
    pub signals: ConfidenceSignals,
    /// `weightᵢ · signalᵢ`, indexed by [`SignalKind::index`].
    pub deductions: [f32; SIGNAL_COUNT],
    /// Groups needing review, ascending by group index.
    pub warnings: Vec<GroupWarning>,
    /// `warnings.len()` (the §3.4 status line's count).
    pub review_groups: u32,
    /// Time spent scoring, in milliseconds.
    pub elapsed_ms: f32,
}

impl Default for ConfidenceReport {
    fn default() -> Self {
        Self {
            score: 1.0,
            signals: ConfidenceSignals::default(),
            deductions: [0.0; SIGNAL_COUNT],
            warnings: Vec::new(),
            review_groups: 0,
            elapsed_ms: 0.0,
        }
    }
}

impl ConfidenceReport {
    /// Deduction contributed by one signal.
    #[must_use]
    pub fn deduction(&self, kind: SignalKind) -> f32 {
        self.deductions[kind.index()]
    }

    /// Whole-percent score for display.
    #[must_use]
    pub fn percent(&self) -> u32 {
        (self.score.clamp(0.0, 1.0) * 100.0).round() as u32
    }
}

/// The §3.4 status line:
/// `Grouped 84 icons in 0.31 s · confidence 92% · 3 groups need review`.
#[must_use]
pub fn summary_line(icons: u32, elapsed_ms: f32, report: &ConfidenceReport) -> String {
    let review = match report.review_groups {
        0 => "no groups need review".to_string(),
        1 => "1 group needs review".to_string(),
        n => format!("{n} groups need review"),
    };
    format!(
        "Grouped {icons} icons in {:.2} s · confidence {}% · {review}",
        elapsed_ms / 1000.0,
        report.percent()
    )
}

/// Everything the score reads — the final grouping plus the evidence the chain
/// collected for it.
#[derive(Clone, Copy, Debug)]
pub struct ScoreInput<'a> {
    /// Final group list.
    pub groups: &'a [IconGroup],
    /// The mask the list was measured against (sheet dimensions).
    pub mask: &'a ForegroundMask,
    /// Components **F1 glued away that survived the corrections**
    /// (`RefineStats::merges − GridStats::restored`); F4 noise/speckle removals
    /// are deliberately not part of it — dropping dust is expected, gluing
    /// icons is not.
    pub merges: u32,
    /// Original components F5 restored out of provably wrong merges (may be
    /// empty). Recognised in the final list by value and reported as
    /// [`WarningKind::RestoredFromMerge`].
    pub restored: &'a [IconGroup],
    /// F5 grid hint (empty ⇒ no lattice was detected).
    pub hint: &'a GridHint,
    /// Background model when the caller has it; `None` leaves the
    /// uncertain-background signal at 0 rather than guessing.
    pub background: Option<&'a BackgroundModel>,
}

/// Scores a final group list (see [`ScoreInput`] for the evidence it reads).
#[must_use]
pub fn score_groups(input: ScoreInput<'_>, params: &ConfidenceParams) -> ConfidenceReport {
    let ScoreInput {
        groups,
        mask,
        merges,
        restored,
        hint,
        background,
    } = input;
    let t0 = std::time::Instant::now();
    let mut report = ConfidenceReport::default();
    if !params.enabled {
        report.elapsed_ms = t0.elapsed().as_secs_f32() * 1000.0;
        return report;
    }

    // Warnings: surviving valley straddles + F5 restorations. Both were
    // lattice flags; both are what the overlay must draw and what the spans
    // deduction counts.
    let restored_keys: Vec<(u32, u32, u32, u32)> = restored.iter().map(key_of).collect();
    let mut straddles = 0usize;
    for (i, g) in groups.iter().enumerate() {
        if hint.any()
            && matches!(
                classify(&g.bbox, hint, params.span_tolerance),
                GridFit::SpansMultipleCells { .. }
            )
        {
            straddles += 1;
            report.warnings.push(GroupWarning {
                group: i as u32,
                kind: WarningKind::SpansMultipleCells,
            });
        } else if restored_keys.contains(&key_of(g)) {
            report.warnings.push(GroupWarning {
                group: i as u32,
                kind: WarningKind::RestoredFromMerge,
            });
        }
    }
    report.review_groups = report.warnings.len() as u32;

    let signals = ConfidenceSignals {
        spans: share(straddles + report.warnings.len() - straddles, groups.len()),
        merge_ratio: ramp(
            share(merges as usize, groups.len() + merges as usize),
            params.merge_ratio_lo,
            params.merge_ratio_hi,
        ),
        size_mad: size_spread(groups, params),
        border_touch: border_share(groups, mask, params),
        weak_grid: weak_grid_share(hint, params),
        uncertain_background: background.map_or(0.0, |b| {
            ramp(
                params.background_consensus_min - b.consensus,
                0.0,
                params.background_consensus_min,
            )
            .max(match b.kind {
                // A weak detector is uncertain by construction, whatever its
                // self-reported share says.
                BackgroundKind::BorderConsensus | BackgroundKind::Alpha => 0.0,
                BackgroundKind::KMeans | BackgroundKind::Otsu => 1.0,
            })
        }),
    };

    let mut deductions = [0.0f32; SIGNAL_COUNT];
    let mut total = 0.0f32;
    for kind in SignalKind::ALL {
        let d = params.weight(kind) * params.signal(&signals, kind).clamp(0.0, 1.0);
        deductions[kind.index()] = d;
        total += d;
    }

    report.signals = signals;
    report.deductions = deductions;
    report.score = (1.0 - total).clamp(0.0, 1.0);
    report.elapsed_ms = t0.elapsed().as_secs_f32() * 1000.0;
    report
}

/// Identity of a group for restoration matching (its bbox; the F5 restoration
/// hands back the original components byte-for-byte, so the box identifies
/// them uniquely in a list that cannot contain two groups for one icon).
fn key_of(g: &IconGroup) -> (u32, u32, u32, u32) {
    (g.bbox.x, g.bbox.y, g.bbox.w, g.bbox.h)
}

/// `count / total` as a share; 0 when `total == 0`.
fn share(count: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        count as f32 / total as f32
    }
}

/// `clamp01((value − lo) / (hi − lo))`; a `hi <= lo` ramp is a step at `lo`.
fn ramp(value: f32, lo: f32, hi: f32) -> f32 {
    if hi <= lo {
        return if value >= lo { 1.0 } else { 0.0 };
    }
    ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// `MAD / median` of `min(w, h)` ramped between the calibrated endpoints.
fn size_spread(groups: &[IconGroup], params: &ConfidenceParams) -> f32 {
    if groups.is_empty() {
        return 0.0;
    }
    let mut sizes: Vec<f32> = groups
        .iter()
        .map(|g| g.bbox.w.min(g.bbox.h) as f32)
        .collect();
    let median = median_f32(&mut sizes);
    if median <= 0.0 {
        return 0.0;
    }
    let mut dev: Vec<f32> = sizes.iter().map(|s| (s - median).abs()).collect();
    let mad = median_f32(&mut dev);
    ramp(mad / median, params.size_mad_lo, params.size_mad_hi)
}

/// Median of a float slice, same convention as the merge chain's integer
/// `median` (odd ⇒ middle value, even ⇒ mean of the middle pair).
fn median_f32(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Share of groups whose bbox lies within the border margin.
fn border_share(groups: &[IconGroup], mask: &ForegroundMask, params: &ConfidenceParams) -> f32 {
    if groups.is_empty() {
        return 0.0;
    }
    let w = f64::from(mask.width());
    let h = f64::from(mask.height());
    let margin = (f64::from(params.border_margin_frac) * w.min(h)).max(1.0);
    let touched = groups
        .iter()
        .filter(|g| {
            let b = g.bbox;
            f64::from(b.x) <= margin
                || f64::from(b.y) <= margin
                || f64::from(b.x) + f64::from(b.w) >= w - margin
                || f64::from(b.y) + f64::from(b.h) >= h - margin
        })
        .count();
    share(touched, groups.len())
}

/// Share of axes that have valleys but did not clear the grid threshold.
fn weak_grid_share(hint: &GridHint, params: &ConfidenceParams) -> f32 {
    let axes = [
        (hint.valley_x.len(), hint.regularity_x, hint.grid_x),
        (hint.valley_y.len(), hint.regularity_y, hint.grid_y),
    ];
    let weak = axes
        .iter()
        .filter(|(valleys, reg, is_grid)| {
            !*is_grid && *valleys >= 2 && *reg >= params.weak_grid_lo && *reg <= params.weak_grid_hi
        })
        .count();
    weak as f32 / axes.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use isg_core::Bbox;

    fn mask_of(w: u32, h: u32) -> ForegroundMask {
        ForegroundMask::new(w, h)
    }

    fn group(x: u32, y: u32, w: u32, h: u32) -> IconGroup {
        IconGroup {
            bbox: Bbox::new(x, y, w, h).unwrap(),
            area: w * h,
            origin: (x, y),
        }
    }

    /// A 4×4 lattice of 40×40 icons, pitch 100 — F5 detects it, nothing spans.
    fn lattice_groups() -> Vec<IconGroup> {
        let mut out = Vec::new();
        for row in 0..4u32 {
            for col in 0..4u32 {
                out.push(group(20 + col * 100, 20 + row * 100, 40, 40));
            }
        }
        out
    }

    fn lattice_mask() -> ForegroundMask {
        let mut m = ForegroundMask::new(440, 440);
        for row in 0..4u32 {
            for col in 0..4u32 {
                for y in 0..40 {
                    for x in 0..40 {
                        m.set(20 + col * 100 + x, 20 + row * 100 + y, true);
                    }
                }
            }
        }
        m
    }

    fn lattice_hint(mask: &ForegroundMask) -> GridHint {
        let mut stats = super::super::grid::GridStats::default();
        super::super::grid::detect_grid(
            mask,
            &super::super::grid::GridParams::default(),
            &mut stats,
        )
    }

    fn params() -> ConfidenceParams {
        ConfidenceParams::default()
    }

    #[test]
    fn a_clean_lattice_scores_exactly_one() {
        let mask = lattice_mask();
        let groups = lattice_groups();
        let hint = lattice_hint(&mask);
        assert!(hint.any(), "the lattice must be detected: {hint:?}");
        let report = score_groups(
            ScoreInput {
                groups: &groups,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert_eq!(report.score, 1.0, "{report:?}");
        assert_eq!(report.review_groups, 0);
        assert!(report.warnings.is_empty());
        assert_eq!(report.percent(), 100);
        for kind in SignalKind::ALL {
            assert_eq!(report.deduction(kind), 0.0, "{:?}", kind.label());
        }
    }

    #[test]
    fn a_group_spanning_two_cells_deducts_its_share_of_thirty_points() {
        let mask = lattice_mask();
        let hint = lattice_hint(&mask);
        // Replace two in-row icons with one wide group spanning the valley.
        let mut groups = lattice_groups();
        groups.remove(1);
        groups[0] = group(20, 20, 140, 40);
        let report = score_groups(
            ScoreInput {
                groups: &groups,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert_eq!(report.review_groups, 1);
        assert_eq!(report.warnings[0].kind, WarningKind::SpansMultipleCells);
        assert_eq!(report.warnings[0].group, 0);
        let expected = 0.30 * (1.0 / groups.len() as f32);
        assert!(
            (report.deduction(SignalKind::SpansMultipleCells) - expected).abs() < 1e-6,
            "{report:?}"
        );
        assert!((report.score - (1.0 - expected)).abs() < 1e-6, "{report:?}");
    }

    #[test]
    fn merge_ratio_ramps_between_its_endpoints() {
        let mask = lattice_mask();
        let hint = GridHint::default();
        let groups = lattice_groups(); // 16 groups
        let score = |merges: u32| {
            score_groups(
                ScoreInput {
                    groups: &groups,
                    mask: &mask,
                    merges,
                    restored: &[],
                    hint: &hint,
                    background: None,
                },
                &params(),
            )
            .signals
            .merge_ratio
        };
        assert_eq!(score(0), 0.0, "no merges");
        assert_eq!(score(5), 0.0, "5/21 = 24 % is below the ramp");
        assert!((score(16) - 0.5).abs() < 1e-6, "16/32 = 50 %");
        assert_eq!(score(48), 1.0, "48/64 = 75 % is full deduction");
        let r = score_groups(
            ScoreInput {
                groups: &groups,
                mask: &mask,
                merges: 16,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert!(
            (r.deduction(SignalKind::HighMergeRatio) - 0.10).abs() < 1e-6,
            "{r:?}"
        );
    }

    #[test]
    fn size_spread_needs_real_outliers() {
        let mask = mask_of(400, 400);
        let hint = GridHint::default();
        let uniform: Vec<IconGroup> = (0..8).map(|i| group(i * 40, 50, 30, 30)).collect();
        let r = score_groups(
            ScoreInput {
                groups: &uniform,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert_eq!(r.signals.size_mad, 0.0, "identical sizes have zero MAD");

        // MAD is outlier-robust by design: one small group among identical ones
        // does not move the median deviation, so it must NOT deduct.
        let mut lone = uniform.clone();
        lone.push(group(0, 100, 8, 8));
        let r = score_groups(
            ScoreInput {
                groups: &lone,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert_eq!(r.signals.size_mad, 0.0, "one outlier is invisible to MAD");

        // A genuine spread does move it.
        let spread: Vec<IconGroup> = [8u32, 16, 24, 32, 40, 48]
            .iter()
            .enumerate()
            .map(|(i, s)| group(i as u32 * 40, 150, *s, *s))
            .collect();
        let r = score_groups(
            ScoreInput {
                groups: &spread,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert!(r.signals.size_mad > 0.0, "{:?}", r.signals);
        let expected = params().w_size_mad * r.signals.size_mad;
        assert!(
            (r.deduction(SignalKind::SizeMad) - expected).abs() < 1e-6,
            "{r:?}"
        );
    }

    #[test]
    fn border_touch_counts_only_groups_in_the_margin() {
        let mask = mask_of(400, 400);
        let hint = GridHint::default();
        let inside: Vec<IconGroup> = (0..4).map(|i| group(50 + i * 60, 50, 30, 30)).collect();
        assert_eq!(
            score_groups(
                ScoreInput {
                    groups: &inside,
                    mask: &mask,
                    merges: 0,
                    restored: &[],
                    hint: &hint,
                    background: None,
                },
                &params(),
            )
            .signals
            .border_touch,
            0.0
        );
        let mut edge = inside.clone();
        edge.push(group(0, 100, 30, 30));
        edge.push(group(370, 100, 30, 30));
        let r = score_groups(
            ScoreInput {
                groups: &edge,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert!(
            (r.signals.border_touch - 2.0 / 6.0).abs() < 1e-6,
            "{:?}",
            r.signals
        );
    }

    #[test]
    fn weak_grid_fires_only_for_measured_irregular_valleys() {
        let mask = lattice_mask();
        let hint = GridHint {
            // x: measured but below `regularity_min` ⇒ not a grid, yet ≥ 2 valleys.
            regularity_x: 0.60,
            valley_x: vec![60, 170, 280],
            // y: a real grid, so it must not count as weak.
            grid_y: true,
            regularity_y: 0.98,
            valley_y: vec![70, 170, 270],
            ..GridHint::default()
        };
        let r = score_groups(
            ScoreInput {
                groups: &lattice_groups(),
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert_eq!(r.signals.spans, 0.0, "row valleys do not straddle: {r:?}");
        assert_eq!(r.signals.weak_grid, 0.5, "one of two axes is weak");
        assert!(
            (r.deduction(SignalKind::WeakGrid) - 0.05).abs() < 1e-6,
            "{r:?}"
        );
    }

    #[test]
    fn uncertain_background_reads_the_detector() {
        let mask = mask_of(200, 200);
        let hint = GridHint::default();
        let groups = vec![group(50, 50, 30, 30)];
        let bg = |kind, consensus| BackgroundModel {
            kind,
            rgba: [255, 255, 255, 255],
            consensus,
        };
        let signal = |kind, consensus| {
            score_groups(
                ScoreInput {
                    groups: &groups,
                    mask: &mask,
                    merges: 0,
                    restored: &[],
                    hint: &hint,
                    background: Some(&bg(kind, consensus)),
                },
                &params(),
            )
            .signals
            .uncertain_background
        };
        assert_eq!(signal(BackgroundKind::BorderConsensus, 0.97), 0.0);
        assert!(signal(BackgroundKind::BorderConsensus, 0.70) > 0.0);
        assert_eq!(signal(BackgroundKind::KMeans, 0.99), 1.0);
        assert_eq!(signal(BackgroundKind::Otsu, 0.99), 1.0);
        assert_eq!(
            score_groups(
                ScoreInput {
                    groups: &groups,
                    mask: &mask,
                    merges: 0,
                    restored: &[],
                    hint: &hint,
                    background: None,
                },
                &params(),
            )
            .signals
            .uncertain_background,
            0.0,
            "no model ⇒ no guess"
        );
    }

    #[test]
    fn all_six_signals_at_full_strength_floor_the_score() {
        let mask = ForegroundMask::new(100, 100);
        // 2 groups, one on the border, wildly different sizes, spanning cells.
        let groups = vec![group(0, 0, 90, 90), group(0, 0, 6, 6)];
        let hint = GridHint {
            // x: a real grid the wide group straddles; y: measured but too
            // irregular to be one — that axis is the weak-grid signal.
            grid_x: true,
            cells_x: 3,
            regularity_x: 0.99,
            valley_x: vec![5, 15],
            regularity_y: 0.60,
            valley_y: vec![5, 15],
            ..GridHint::default()
        };
        let background = BackgroundModel {
            kind: BackgroundKind::Otsu,
            rgba: [0, 0, 0, 0],
            consensus: 0.2,
        };
        let report = score_groups(
            ScoreInput {
                groups: &groups,
                mask: &mask,
                merges: 20,
                restored: &[],
                hint: &hint,
                background: Some(&background),
            },
            &params(),
        );
        for kind in SignalKind::ALL {
            assert!(
                report.deduction(kind) > 0.0,
                "{:?} should contribute: {report:?}",
                kind.label()
            );
        }
        assert!(report.score < 0.3, "{report:?}");
        assert_eq!(
            report.score,
            (1.0 - report.deductions.iter().sum::<f32>()).max(0.0)
        );
    }

    #[test]
    fn disabled_reports_a_perfect_score_without_measuring() {
        let mask = ForegroundMask::new(100, 100);
        let groups = vec![group(0, 0, 90, 90)];
        let hint = GridHint::default();
        let report = score_groups(
            ScoreInput {
                groups: &groups,
                mask: &mask,
                merges: 0,
                restored: &[],
                hint: &hint,
                background: None,
            },
            &ConfidenceParams {
                enabled: false,
                ..params()
            },
        );
        assert_eq!(report.score, 1.0);
        assert_eq!(report.review_groups, 0);
        assert_eq!(report.deductions, [0.0; SIGNAL_COUNT]);
    }

    #[test]
    fn restored_merge_groups_warn_and_deduct_as_lattice_flags() {
        // A near-touching pair F5 handed back: neither group straddles a valley
        // any more (the lattice hint below has no valleys through them), yet
        // both must appear as review items with no score penalty.
        let mask = lattice_mask();
        let hint = lattice_hint(&mask);
        let groups = lattice_groups();
        let restored = vec![groups[5], groups[6]];
        let report = score_groups(
            ScoreInput {
                groups: &groups,
                mask: &mask,
                // 8/24 merges ⇒ above the 25 % ramp, below full.
                merges: 8,
                restored: &restored,
                hint: &hint,
                background: None,
            },
            &params(),
        );
        assert_eq!(report.review_groups, 2, "{report:?}");
        assert!(report
            .warnings
            .iter()
            .all(|w| w.kind == WarningKind::RestoredFromMerge));
        assert_eq!(report.warnings[0].group, 5);
        assert_eq!(report.warnings[1].group, 6);
        assert_eq!(
            report.signals.spans,
            2.0 / 16.0,
            "both restorations count as lattice flags: {report:?}"
        );
        assert!(report.signals.merge_ratio > 0.0, "their merges did happen");
        assert!(
            (report.deduction(SignalKind::SpansMultipleCells) - 0.30 * 2.0 / 16.0).abs() < 1e-6,
            "{report:?}"
        );
    }

    #[test]
    fn scoring_is_deterministic_for_sorted_inputs() {
        let mask = lattice_mask();
        let groups = lattice_groups();
        let hint = lattice_hint(&mask);
        let input = ScoreInput {
            groups: &groups,
            mask: &mask,
            merges: 4,
            restored: &[],
            hint: &hint,
            background: None,
        };
        let a = score_groups(input, &params());
        let b = score_groups(input, &params());
        assert_eq!(a.signals, b.signals);
        assert_eq!(a.deductions, b.deductions);
        assert_eq!(a.score, b.score);
        assert_eq!(a.warnings, b.warnings);
    }

    #[test]
    fn summary_line_matches_the_spec_example() {
        let report = ConfidenceReport {
            score: 0.92,
            review_groups: 3,
            ..ConfidenceReport::default()
        };
        assert_eq!(
            summary_line(84, 310.0, &report),
            "Grouped 84 icons in 0.31 s · confidence 92% · 3 groups need review"
        );

        let clean = ConfidenceReport {
            score: 1.0,
            review_groups: 0,
            ..ConfidenceReport::default()
        };
        assert_eq!(
            summary_line(16, 45.0, &clean),
            "Grouped 16 icons in 0.05 s · confidence 100% · no groups need review"
        );

        let one = ConfidenceReport {
            score: 0.7,
            review_groups: 1,
            ..ConfidenceReport::default()
        };
        assert!(summary_line(20, 120.0, &one).ends_with("· 1 group needs review"));
    }
}
