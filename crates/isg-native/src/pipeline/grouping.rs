//! §3.4 "Group All" session — the stateful grouping service behind the UI (W12).
//!
//! Everything the overlay needs, with no decoding of its own:
//!
//! * **Group All** — [`GroupingSession::group_sheet`]: RLE CCL + the refine
//!   chain + the §3.4 confidence score over a *cached* mask, so a repeat run on
//!   a sheet costs only grouping (`pipeline::mask_cached` fills the cache on the
//!   first call with a real decode + segmentation).
//! * **Sensitivity sliders** — [`GroupingSession::set_refine`] swaps the refine
//!   parameters and re-groups the sheet on screen from the same cached mask
//!   (milliseconds, no decode).
//! * **Split Here** — [`GroupingSession::split_here`]: the group under the
//!   pointer goes through the W9 watershed (`resplit_forced`, i.e. the F2 size
//!   gate is skipped because the user's click *is* the evidence a split is
//!   wanted; the fat / ≥ 2-region / sliver guards still apply) and the result is
//!   spliced back into the list. The edit is timed, so the ≤ 20 ms budget is a
//!   number this module proves rather than claims.
//! * **Group Selected** — [`GroupingSession::group_selected`]: every group the
//!   marquee hits collapses into one icon (union box, summed area, first
//!   member's origin).
//!
//! After a manual edit the list is re-scored, so the status line and the review
//! list always describe what the user is looking at; the hint and the F5
//! restorations the chain measured ([`RefineStats::hint`],
//! [`RefineStats::restored_originals`]) are fed back into that re-score, because
//! an edit must not silently erase the warnings that were on screen.
//!
//! Manual edits are view state, not a new segmentation: toggling Group All again
//! restores the automatic result ([`GroupingSession::reset_manual`]), which is
//! why nothing here is persisted.

use isg_core::{Bbox, GroupingStrategy, IconGroup};

use super::background::SegParams;
use super::confidence::{score_groups, summary_line, ConfidenceReport, ScoreInput};
use super::grid::GridHint;
use super::group::{sort_groups, RleCclGrouper};
use super::maskcache::{mask_key, MaskCache, MaskView};
use super::merge::{refine_groups_with_context, RefineParams, RefineStats};
use super::split::{resplit_forced, SplitStats};

/// Session API version (bump when a report field changes meaning).
pub const GROUPING_VERSION: u32 = 1;

/// The session's parameters: the refinement knobs the sensitivity sliders move,
/// plus the mask-cache capacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupingParams {
    /// Refine-chain parameters — what the UI sliders edit.
    pub refine: RefineParams,
    /// Mask-cache capacity in sheets.
    pub mask_capacity: usize,
}

impl Default for GroupingParams {
    fn default() -> Self {
        Self {
            refine: RefineParams {
                // The session is the UI path: grouping runs live, unlike the
                // batch default which stays opt-in until W12 calibration.
                enabled: true,
                ..RefineParams::default()
            },
            mask_capacity: 8,
        }
    }
}

/// The numeric envelope of [`SensitivityParams`]. A value outside it is a bug,
/// not a preference, so [`SensitivityParams::apply`] rejects rather than clamps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensitivityRanges {
    /// `(min, max)` for the F1 gap fraction of `median_h`.
    pub merge_gap_frac: (f32, f32),
    /// `(min, max)` for rule 5's combined-area ceiling.
    pub merge_area_ratio: (f32, f32),
    /// Largest accepted F4 speckle area.
    pub noise_min_area: u32,
    /// `(min, max)` for the F5 regularity gate.
    pub grid_regularity_min: (f32, f32),
}

/// The four "sensitivity" knobs the overlay exposes. Each maps onto exactly one
/// documented §3.4 parameter; nothing here invents a new one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensitivityParams {
    /// F1: merge distance as a fraction of `median_h` (`0.35` by default).
    pub merge_gap_frac: f32,
    /// Rule 5: combined area ≤ this × `median_area` (`1.75` by default).
    pub merge_area_ratio: f32,
    /// F4: components below this area are speckle (`16` by default).
    pub noise_min_area: u32,
    /// F5: minimum lattice regularity (`0.75` by default).
    pub grid_regularity_min: f32,
}

/// Why a slider value was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SensitivityError {
    /// Human-readable reason (field + value + envelope).
    pub message: String,
}

impl std::fmt::Display for SensitivityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl SensitivityParams {
    /// The envelope the sliders may move within.
    pub const RANGES: SensitivityRanges = SensitivityRanges {
        merge_gap_frac: (0.05, 1.0),
        merge_area_ratio: (1.0, 8.0),
        noise_min_area: 512,
        grid_regularity_min: (0.0, 1.0),
    };

    /// Reads the knobs out of a refine parameter set.
    #[must_use]
    pub fn from_refine(p: &RefineParams) -> Self {
        Self {
            merge_gap_frac: p.gap_frac,
            merge_area_ratio: p.rule5_max_area_ratio,
            noise_min_area: p.noise_min_area,
            grid_regularity_min: p.grid.regularity_min,
        }
    }

    /// Writes the knobs into `p`, rejecting anything outside
    /// [`SensitivityParams::RANGES`] (or non-finite).
    pub fn apply(&self, p: &mut RefineParams) -> Result<(), SensitivityError> {
        let r = Self::RANGES;
        check_range("mergeGapFrac", self.merge_gap_frac, r.merge_gap_frac)?;
        check_range("mergeAreaRatio", self.merge_area_ratio, r.merge_area_ratio)?;
        check_range(
            "gridRegularityMin",
            self.grid_regularity_min,
            r.grid_regularity_min,
        )?;
        if self.noise_min_area > r.noise_min_area {
            return Err(SensitivityError {
                message: format!(
                    "noiseMinArea {} outside [0, {}]",
                    self.noise_min_area, r.noise_min_area
                ),
            });
        }
        p.gap_frac = self.merge_gap_frac;
        p.rule5_max_area_ratio = self.merge_area_ratio;
        p.noise_min_area = self.noise_min_area;
        p.grid.regularity_min = self.grid_regularity_min;
        Ok(())
    }
}

/// Range check with the field name in the message.
fn check_range(name: &str, value: f32, (min, max): (f32, f32)) -> Result<(), SensitivityError> {
    if !value.is_finite() || value < min || value > max {
        return Err(SensitivityError {
            message: format!("{name} {value} outside [{min}, {max}]"),
        });
    }
    Ok(())
}

/// One grouping result — exactly what the overlay renders.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupingReport {
    /// Sheet key the mask is cached under (`blake3(bytes) ‖ seg params ‖ v`).
    pub sheet_key: String,
    /// Sheet width in pixels.
    pub width: u32,
    /// Sheet height in pixels.
    pub height: u32,
    /// Final groups, canonical scan order.
    pub groups: Vec<IconGroup>,
    /// The F5 lattice hint measured for this sheet (the overlay's guides).
    pub hint: GridHint,
    /// Confidence score, signals and the review list.
    pub confidence: ConfidenceReport,
    /// Full refine evidence (medians, merge/grid/containment counters).
    pub stats: RefineStats,
    /// The §3.4 status line (`Grouped 84 icons in 0.31 s · confidence 92% · …`).
    pub status_line: String,
    /// Wall-clock for this grouping call, in milliseconds.
    pub elapsed_ms: f32,
    /// True when the mask came from the cache (no decode, no segmentation).
    pub mask_cache_hit: bool,
    /// Manual edits applied on top of the automatic result.
    pub manual_edits: u32,
}

/// What [`GroupingSession::split_here`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitHereReport {
    /// The report after the edit (identical groups when the split was refused).
    pub report: GroupingReport,
    /// True when the component was actually replaced by ≥ 2 regions.
    pub split: bool,
    /// Index of the group the pointer selected, before the edit.
    pub group_index: usize,
    /// Regions the watershed produced (0 when refused).
    pub regions: u32,
    /// Wall-clock for the whole edit (the ≤ 20 ms budget), in milliseconds.
    pub elapsed_ms: f32,
}

/// One mask cache plus the sheet currently on screen.
pub struct GroupingSession {
    params: GroupingParams,
    cache: MaskCache,
    current: Option<Current>,
    /// Encoded sheet previews, most recent last. Two entries cover the sheet on
    /// screen plus the one before it, so stepping back and forth does not
    /// re-encode a multi-megabyte PNG.
    previews: Vec<Preview>,
}

/// An encoded sheet preview — the overlay's backdrop.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewImage {
    /// Normalized sheet width — the coordinate space groups live in.
    pub sheet_width: u32,
    /// Normalized sheet height.
    pub sheet_height: u32,
    /// Preview width in pixels (`≤ max_dim`).
    pub width: u32,
    /// Preview height in pixels.
    pub height: u32,
    /// Encoded PNG bytes.
    pub png: Vec<u8>,
}

impl PreviewImage {
    /// `preview pixels / sheet pixels` — the factor the overlay scales by.
    #[must_use]
    pub fn scale(&self) -> f32 {
        if self.sheet_width == 0 {
            1.0
        } else {
            self.width as f32 / self.sheet_width as f32
        }
    }
}

/// A stored preview plus the key/scale it belongs to.
struct Preview {
    key: String,
    max_dim: u32,
    image: PreviewImage,
}

struct Current {
    key: String,
    width: u32,
    height: u32,
    groups: Vec<IconGroup>,
    stats: RefineStats,
    status_line: String,
    manual_edits: u32,
    mask_cache_hit: bool,
}

impl GroupingSession {
    /// A session whose mask cache holds `capacity` sheets.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            params: GroupingParams {
                mask_capacity: capacity,
                ..GroupingParams::default()
            },
            cache: MaskCache::new(capacity),
            current: None,
            previews: Vec::new(),
        }
    }

    /// The active parameters.
    #[must_use]
    pub fn params(&self) -> &GroupingParams {
        &self.params
    }

    /// Mutable view of the parameters — the sliders write here, and the next
    /// [`GroupingSession::set_refine`] / regroup uses them.
    pub fn params_mut(&mut self) -> &mut GroupingParams {
        &mut self.params
    }

    /// The mask cache, so the command layer can fill it through
    /// `pipeline::mask_cached`.
    #[must_use]
    pub fn cache_mut(&mut self) -> &mut MaskCache {
        &mut self.cache
    }

    /// The key a sheet's mask is cached under for one segmentation config.
    #[must_use]
    pub fn key_for(&self, bytes: &[u8], seg: &SegParams) -> String {
        mask_key(bytes, seg)
    }

    /// The key of the sheet currently grouped, if any.
    #[must_use]
    pub fn current_key(&self) -> Option<&str> {
        self.current.as_ref().map(|c| c.key.as_str())
    }

    /// Previews kept encoded (see the `previews` field).
    pub const PREVIEW_CACHE: usize = 2;

    /// The cached preview for a sheet key and scale, if one was stored.
    #[must_use]
    pub fn preview(&self, key: &str, max_dim: u32) -> Option<&PreviewImage> {
        self.previews
            .iter()
            .rev()
            .find(|p| p.key == key && p.max_dim == max_dim)
            .map(|p| &p.image)
    }

    /// Stores an encoded preview (most recent wins; at most
    /// [`GroupingSession::PREVIEW_CACHE`] are kept).
    pub fn store_preview(&mut self, key: &str, max_dim: u32, image: PreviewImage) {
        self.previews
            .retain(|p| !(p.key == key && p.max_dim == max_dim));
        self.previews.push(Preview {
            key: key.to_string(),
            max_dim,
            image,
        });
        while self.previews.len() > Self::PREVIEW_CACHE {
            self.previews.remove(0);
        }
    }

    /// The report for the sheet on screen, if one is grouped — used by the
    /// command layer to answer without re-grouping an unchanged sheet.
    #[must_use]
    pub fn current_report(&self) -> Option<GroupingReport> {
        self.report_from_current()
    }

    /// Group All over a cached mask.
    ///
    /// `mask_cache_hit` records whether that mask came from the cache or from a
    /// fresh segmentation — the caller knows (it is the one that segmented).
    /// Returns `None` when the key is not cached.
    pub fn group_sheet(&mut self, key: &str, mask_cache_hit: bool) -> Option<GroupingReport> {
        let entry = self.cache.get(key)?;
        let mask = entry.mask();
        let raster = MaskView::new(entry.width, entry.height);
        let raw = RleCclGrouper::default().group_all(&raster, &mask);
        let (groups, stats) =
            refine_groups_with_context(raw, &mask, Some(&entry.background), &self.params.refine);
        let line = summary_line(groups.len() as u32, stats.elapsed_ms, &stats.confidence);
        self.current = Some(Current {
            key: key.to_string(),
            width: entry.width,
            height: entry.height,
            groups,
            stats,
            status_line: line,
            manual_edits: 0,
            mask_cache_hit,
        });
        self.report_from_current()
    }

    /// Sensitivity sliders: swap the refine parameters and re-group the sheet on
    /// screen from the same cached mask (never re-decodes).
    pub fn set_refine(&mut self, refine: RefineParams) -> Option<GroupingReport> {
        self.params.refine = refine;
        self.regroup_current()
    }

    /// Drops every manual edit and re-groups from the cached mask.
    pub fn reset_manual(&mut self) -> Option<GroupingReport> {
        self.regroup_current()
    }

    /// Split Here: split the group containing `(x, y)` and splice the result in.
    ///
    /// `None` when no sheet is grouped or nothing is under the pointer; the
    /// `split` flag says whether the watershed actually produced regions (a
    /// refusal leaves the list untouched — a guard is never weakened to make a
    /// click do something).
    pub fn split_here(&mut self, x: u32, y: u32) -> Option<SplitHereReport> {
        let t0 = std::time::Instant::now();
        let key = self.current_key()?.to_string();
        let entry = self.cache.get(&key)?;
        let mask = entry.mask();
        let (index, median_h, split_params) = {
            let current = self.current.as_ref()?;
            let index = current
                .groups
                .iter()
                .position(|g| contains(&g.bbox, x, y))?;
            (index, current.stats.median_h, self.params.refine.split)
        };

        let mut split_stats = SplitStats::default();
        let (regions, split, produced) = resplit_forced(
            vec![self.current.as_ref()?.groups[index]],
            &mask,
            median_h,
            &split_params,
            &mut split_stats,
        );
        let applied = split > 0 && produced >= 2;
        if applied {
            let current = self.current.as_mut()?;
            let mut groups = current.groups.clone();
            groups.splice(index..=index, regions.iter().copied());
            current.groups = groups;
            current.manual_edits += 1;
            self.rescore();
        }
        let report = self.report_from_current()?;
        Some(SplitHereReport {
            report,
            split: applied,
            group_index: index,
            regions: if applied { produced } else { 0 },
            elapsed_ms: t0.elapsed().as_secs_f32() * 1000.0,
        })
    }

    /// Group Selected: collapse every group the marquee boxes hit into one icon.
    ///
    /// A no-op (no edit counted) when fewer than two groups match.
    pub fn group_selected(&mut self, boxes: &[Bbox]) -> Option<GroupingReport> {
        let union = {
            let current = self.current.as_ref()?;
            let hits: Vec<usize> = (0..current.groups.len())
                .filter(|i| {
                    let b = current.groups[*i].bbox;
                    boxes.iter().any(|m| intersects(&b, m))
                })
                .collect();
            if hits.len() < 2 {
                return self.report_from_current();
            }
            let union = hits
                .iter()
                .map(|i| current.groups[*i].bbox)
                .reduce(union_bbox)?;
            let area: u64 = hits
                .iter()
                .map(|i| u64::from(current.groups[*i].area))
                .sum();
            let origin = current.groups[hits[0]].origin;
            (hits, union, area, origin)
        };
        let (hits, union, area, origin) = union;
        let current = self.current.as_mut()?;
        let mut groups = current.groups.clone();
        for i in hits.iter().rev() {
            groups.remove(*i);
        }
        groups.push(IconGroup {
            bbox: union,
            area: area.min(u64::from(u32::MAX)) as u32,
            origin,
        });
        sort_groups(&mut groups);
        current.groups = groups;
        current.manual_edits += 1;
        self.rescore();
        self.report_from_current()
    }

    fn regroup_current(&mut self) -> Option<GroupingReport> {
        let key = self.current_key()?.to_string();
        let hit = self.current.as_ref().is_some_and(|c| c.mask_cache_hit);
        self.group_sheet(&key, hit)
    }

    /// Re-scores the current list after a manual edit, with the evidence the
    /// original chain measured, so warnings survive the edit.
    fn rescore(&mut self) {
        let Some(current) = self.current.as_ref() else {
            return;
        };
        let key = current.key.clone();
        let Some(entry) = self.cache.get(&key) else {
            return;
        };
        let mask = entry.mask();
        let merges = current
            .stats
            .merges
            .saturating_sub(current.stats.grid.restored);
        let report = score_groups(
            ScoreInput {
                groups: &current.groups,
                mask: &mask,
                merges,
                restored: &current.stats.restored_originals,
                hint: &current.stats.hint,
                background: Some(&entry.background),
            },
            &self.params.refine.confidence,
        );
        let line = summary_line(
            current.groups.len() as u32,
            current.stats.elapsed_ms,
            &report,
        );
        let current = self
            .current
            .as_mut()
            .expect("current sheet still present (checked above)");
        current.stats.confidence = report;
        current.status_line = line;
    }

    fn report_from_current(&self) -> Option<GroupingReport> {
        let c = self.current.as_ref()?;
        Some(GroupingReport {
            sheet_key: c.key.clone(),
            width: c.width,
            height: c.height,
            groups: c.groups.clone(),
            hint: c.stats.hint.clone(),
            confidence: c.stats.confidence.clone(),
            stats: c.stats.clone(),
            status_line: c.status_line.clone(),
            elapsed_ms: c.stats.elapsed_ms,
            mask_cache_hit: c.mask_cache_hit,
            manual_edits: c.manual_edits,
        })
    }
}

/// `(x, y)` inside a half-open box.
fn contains(b: &Bbox, x: u32, y: u32) -> bool {
    x >= b.x && y >= b.y && x < b.x + b.w && y < b.y + b.h
}

/// Two boxes overlap (touching edges do not count).
fn intersects(a: &Bbox, b: &Bbox) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Smallest box containing both.
fn union_bbox(a: Bbox, b: Bbox) -> Bbox {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let x1 = (a.x + a.w).max(b.x + b.w);
    let y1 = (a.y + a.h).max(b.y + b.h);
    Bbox {
        x,
        y,
        w: x1 - x,
        h: y1 - y,
    }
}

#[cfg(test)]
mod tests {
    use super::super::background::{BackgroundKind, BackgroundModel};
    use super::super::confidence::{GroupWarning, WarningKind};
    use super::*;
    use isg_core::ForegroundMask;

    const KEY: &str = "test-sheet";

    fn bg() -> BackgroundModel {
        BackgroundModel {
            kind: BackgroundKind::BorderConsensus,
            rgba: [255, 255, 255, 255],
            consensus: 0.99,
        }
    }

    fn blk(m: &mut ForegroundMask, x0: u32, y0: u32, w: u32, h: u32) {
        for y in 0..h {
            for x in 0..w {
                m.set(x0 + x, y0 + y, true);
            }
        }
    }

    /// Five irregular icons — no lattice, so every stage is exercised without
    /// the F5 hint flagging anything.
    fn scattered() -> ForegroundMask {
        let mut m = ForegroundMask::new(200, 140);
        blk(&mut m, 12, 12, 24, 24);
        blk(&mut m, 60, 16, 17, 21);
        blk(&mut m, 116, 10, 31, 27);
        blk(&mut m, 24, 78, 22, 22);
        blk(&mut m, 92, 88, 26, 19);
        m
    }

    /// Two icons joined by an 8 px bar: one component the watershed can split.
    fn dumbbell() -> ForegroundMask {
        let mut m = ForegroundMask::new(200, 140);
        blk(&mut m, 30, 30, 22, 22);
        blk(&mut m, 80, 30, 22, 22);
        blk(&mut m, 52, 37, 28, 8);
        m
    }

    /// Two 20×20 blocks 2 px apart plus two irregular distractors: nothing is
    /// merged at the default `rule5_max_area_ratio`, and the pair is exactly
    /// what the sensitivity slider is for.
    fn pair() -> ForegroundMask {
        let mut m = ForegroundMask::new(200, 140);
        blk(&mut m, 30, 30, 20, 20);
        blk(&mut m, 52, 30, 20, 20);
        blk(&mut m, 120, 20, 24, 24);
        blk(&mut m, 40, 90, 17, 21);
        m
    }

    /// A 4×4 lattice plus a small fragment F1 glues to its neighbour: F5 then
    /// proves the merge wrong and hands both originals back, so the review list
    /// is non-empty.
    fn lattice_fragment() -> ForegroundMask {
        let mut m = ForegroundMask::new(300, 300);
        for row in 0..4u32 {
            for col in 0..4u32 {
                blk(&mut m, 32 + col * 64, 32 + row * 64, 24, 24);
            }
        }
        blk(&mut m, 58, 32, 10, 12);
        m
    }

    fn session_with(mask: &ForegroundMask) -> GroupingSession {
        let mut s = GroupingSession::new(2);
        s.cache_mut().put(KEY, mask, &bg());
        s
    }

    fn boxes(groups: &[IconGroup]) -> Vec<(u32, u32, u32, u32)> {
        groups
            .iter()
            .map(|g| (g.bbox.x, g.bbox.y, g.bbox.w, g.bbox.h))
            .collect()
    }

    #[test]
    fn group_all_reports_groups_confidence_and_the_status_line() {
        let mut s = session_with(&scattered());
        let report = s.group_sheet(KEY, false).expect("grouped");
        assert_eq!(
            boxes(&report.groups),
            vec![
                (116, 10, 31, 27),
                (12, 12, 24, 24),
                (60, 16, 17, 21),
                (24, 78, 22, 22),
                (92, 88, 26, 19)
            ],
            "canonical scan order"
        );
        assert_eq!((report.width, report.height), (200, 140));
        assert_eq!(report.sheet_key, KEY);
        assert_eq!(
            report.status_line,
            "Grouped 5 icons in 0.00 s · confidence 100% · no groups need review"
        );
        assert_eq!(
            report.confidence.score, 1.0,
            "{:?}",
            report.confidence.signals
        );
        assert!(report.confidence.warnings.is_empty());
        assert_eq!(report.manual_edits, 0);
        assert!(!report.mask_cache_hit);
        assert_eq!(report.stats.median_h, 22.0);
    }

    #[test]
    fn grouping_is_a_pure_function_of_the_mask_and_the_params() {
        let mut a = session_with(&lattice_fragment());
        let mut b = session_with(&lattice_fragment());
        let ra = a.group_sheet(KEY, false).unwrap();
        let rb = b.group_sheet(KEY, false).unwrap();
        assert_eq!(ra.groups, rb.groups);
        assert_eq!(ra.hint, rb.hint);
        // `GridStats` carries an elapsed_ms — compare the measured evidence.
        assert_eq!(ra.stats.grid.flagged, rb.stats.grid.flagged);
        assert_eq!(ra.stats.grid.restored, rb.stats.grid.restored);
        assert_eq!(ra.stats.grid.valleys_x, rb.stats.grid.valleys_x);
        assert_eq!(ra.stats.grid.valleys_y, rb.stats.grid.valleys_y);
        assert_eq!(ra.confidence.score, rb.confidence.score);
        assert_eq!(ra.confidence.signals, rb.confidence.signals);
        assert_eq!(ra.confidence.warnings, rb.confidence.warnings);
        assert_eq!(ra.status_line, rb.status_line);
    }

    #[test]
    fn unknown_keys_group_to_nothing() {
        let mut s = GroupingSession::new(1);
        assert!(s.group_sheet("absent", false).is_none());
        assert!(s.current_key().is_none());
        assert!(s.split_here(10, 10).is_none());
        assert!(s
            .group_selected(&[Bbox::new(0, 0, 10, 10).unwrap()])
            .is_none());
        assert!(s.set_refine(RefineParams::default()).is_none());
        assert!(s.reset_manual().is_none());
    }

    #[test]
    fn split_here_splits_the_component_under_the_pointer() {
        let mut s = session_with(&dumbbell());
        let before = s.group_sheet(KEY, false).unwrap();
        assert_eq!(
            before.groups.len(),
            1,
            "the bar glue makes it one component"
        );
        let b = before.groups[0].bbox;
        assert_eq!((b.x, b.y, b.w, b.h), (30, 30, 72, 22));

        let out = s.split_here(b.x + b.w / 2, b.y + b.h / 2).expect("hit");
        assert_eq!(out.group_index, 0);
        assert!(out.split, "watershed must find two lobes: {out:?}");
        assert_eq!(out.regions, 2);
        assert_eq!(
            boxes(&out.report.groups),
            vec![(30, 30, 49, 22), (79, 30, 23, 22)],
            "the two icons, with the bar shared by the watershed"
        );
        assert_eq!(out.report.manual_edits, 1);
        assert!(
            out.elapsed_ms < 20.0,
            "Split Here budget: {:.3} ms",
            out.elapsed_ms
        );
        // The edit is view state: Group All again restores the automatic result.
        let reset = s.reset_manual().unwrap();
        assert_eq!(reset.groups, before.groups);
        assert_eq!(reset.manual_edits, 0);
    }

    #[test]
    fn split_here_refuses_when_the_guards_say_no() {
        // A solid, near-square icon: no watershed structure to find.
        let mut s = session_with(&scattered());
        let before = s.group_sheet(KEY, false).unwrap();
        let solid = before
            .groups
            .iter()
            .position(|g| g.bbox == Bbox::new(12, 12, 24, 24).unwrap())
            .expect("the solid block");
        let out = s.split_here(13, 13).expect("hit");
        assert_eq!(out.group_index, solid);
        assert!(!out.split, "a solid icon is never split: {out:?}");
        assert_eq!(out.regions, 0);
        assert_eq!(out.report.groups, before.groups);
        assert_eq!(out.report.manual_edits, 0);
        assert!(out.elapsed_ms < 20.0, "{:.3} ms", out.elapsed_ms);
    }

    #[test]
    fn split_here_misses_outside_every_group() {
        let mut s = session_with(&scattered());
        s.group_sheet(KEY, false).unwrap();
        assert!(s.split_here(3, 3).is_none(), "empty corner");
        assert!(s.split_here(199, 139).is_none(), "empty corner");
    }

    #[test]
    fn group_selected_merges_the_marquee_hits_only() {
        let mut s = session_with(&scattered());
        let report = s.group_sheet(KEY, false).unwrap();
        // The marquee covers the (12,12) and (60,16) icons, nothing else.
        let marquee = Bbox::new(0, 0, 100, 60).unwrap();
        let out = s.group_selected(&[marquee]).expect("grouped");
        assert_eq!(out.manual_edits, 1);
        assert_eq!(
            boxes(&out.groups),
            vec![
                (116, 10, 31, 27),
                (12, 12, 65, 25),
                (24, 78, 22, 22),
                (92, 88, 26, 19)
            ]
        );
        let merged = &out.groups[1];
        assert_eq!(merged.area, 576 + 357, "areas add up");
        assert_eq!(merged.origin, (12, 12), "first member keeps the origin");
        // And the edit is re-scored: a 65×25 union across the sheet's valleys is
        // exactly what the review layer must flag.
        assert_eq!(
            out.confidence.warnings,
            vec![GroupWarning {
                group: 1,
                kind: WarningKind::SpansMultipleCells
            }]
        );
        assert!(out.confidence.score < report.confidence.score);
        assert!(out.status_line.contains("1 group needs review"));
    }

    #[test]
    fn group_selected_needs_two_hits_and_reset_restores_the_auto_result() {
        let mut s = session_with(&scattered());
        let auto = s.group_sheet(KEY, false).unwrap();
        let one = Bbox::new(0, 0, 40, 40).unwrap();
        let same = s.group_selected(&[one]).unwrap();
        assert_eq!(same.groups, auto.groups, "one hit is a no-op");
        assert_eq!(same.manual_edits, 0);
        assert!(s.group_selected(&[]).unwrap().groups == auto.groups);

        let two = Bbox::new(0, 0, 100, 60).unwrap();
        assert_eq!(s.group_selected(&[two]).unwrap().groups.len(), 4);
        let reset = s.reset_manual().unwrap();
        assert_eq!(reset.groups, auto.groups);
        assert_eq!(reset.manual_edits, 0);
    }

    #[test]
    fn sensitivity_sliders_regroup_from_the_same_cached_mask() {
        let mut s = session_with(&pair());
        let auto = s.group_sheet(KEY, true).unwrap();
        assert!(auto.mask_cache_hit);
        assert_eq!(auto.groups.len(), 4, "the 2 px pair stays apart at 1.75×");

        // Loosen rule 5's area ceiling: the pair now merges.
        let mut refine = s.params().refine;
        refine.rule5_max_area_ratio = 3.0;
        let loose = s.set_refine(refine).expect("regrouped");
        assert!(loose.mask_cache_hit, "sliders never re-segment");
        assert_eq!(
            boxes(&loose.groups),
            vec![(120, 20, 24, 24), (30, 30, 42, 20), (40, 90, 17, 21)]
        );
        assert_eq!(loose.manual_edits, 0, "sliders are not manual edits");

        let back = s.set_refine(GroupingParams::default().refine).unwrap();
        assert_eq!(back.groups, auto.groups);
    }

    #[test]
    fn warnings_survive_a_manual_edit() {
        let mut s = session_with(&lattice_fragment());
        let auto = s.group_sheet(KEY, false).unwrap();
        let restored: Vec<GroupWarning> = auto
            .confidence
            .warnings
            .iter()
            .filter(|w| w.kind == WarningKind::RestoredFromMerge)
            .copied()
            .collect();
        assert_eq!(restored.len(), 2, "{:?}", auto.stats.grid);
        assert_eq!(auto.confidence.review_groups, 2);

        // Group the whole bottom row: the union must be flagged, and the two
        // restoration warnings must still be there afterwards.
        let marquee = Bbox::new(20, 200, 260, 60).unwrap();
        let edited = s.group_selected(&[marquee]).unwrap();
        assert_eq!(edited.manual_edits, 1);
        for w in &restored {
            assert!(
                edited.confidence.warnings.contains(w),
                "review item {w:?} was erased by the edit: {:?}",
                edited.confidence.warnings
            );
        }
        assert_eq!(
            edited.confidence.warnings.len(),
            3,
            "the marquee union straddles cells too: {:?}",
            edited.confidence.warnings
        );
        assert!(edited.status_line.contains("3 groups need review"));
    }

    #[test]
    fn sensitivity_defaults_mirror_the_documented_parameters() {
        let s = SensitivityParams::from_refine(&RefineParams::default());
        assert_eq!(s.merge_gap_frac, 0.35);
        assert_eq!(s.merge_area_ratio, 1.75);
        assert_eq!(s.noise_min_area, 16);
        assert_eq!(s.grid_regularity_min, 0.75);
    }

    #[test]
    fn sensitivity_apply_writes_and_rejects_out_of_envelope() {
        let mut p = RefineParams::default();
        let knobs = SensitivityParams {
            merge_gap_frac: 0.5,
            merge_area_ratio: 3.0,
            noise_min_area: 32,
            grid_regularity_min: 0.6,
        };
        knobs.apply(&mut p).expect("inside the envelope");
        assert_eq!(p.gap_frac, 0.5);
        assert_eq!(p.rule5_max_area_ratio, 3.0);
        assert_eq!(p.noise_min_area, 32);
        assert_eq!(p.grid.regularity_min, 0.6);
        assert_eq!(SensitivityParams::from_refine(&p), knobs);

        // Out of envelope: refused, and the target is left untouched.
        let before = p;
        for (bad, field) in [
            (
                SensitivityParams {
                    merge_gap_frac: 1.5,
                    ..knobs
                },
                "mergeGapFrac",
            ),
            (
                SensitivityParams {
                    merge_gap_frac: f32::NAN,
                    ..knobs
                },
                "mergeGapFrac",
            ),
            (
                SensitivityParams {
                    merge_area_ratio: 20.0,
                    ..knobs
                },
                "mergeAreaRatio",
            ),
            (
                SensitivityParams {
                    grid_regularity_min: -0.1,
                    ..knobs
                },
                "gridRegularityMin",
            ),
            (
                SensitivityParams {
                    noise_min_area: 4096,
                    ..knobs
                },
                "noiseMinArea",
            ),
        ] {
            let err = bad.apply(&mut p).expect_err("outside the envelope");
            assert!(err.message.contains(field), "{}", err.message);
            assert_eq!(p, before, "a refused slider leaves the parameters alone");
        }
    }

    #[test]
    fn previews_are_cached_per_sheet_and_scale() {
        fn img(sheet: u32, width: u32, png: Vec<u8>) -> PreviewImage {
            PreviewImage {
                sheet_width: sheet,
                sheet_height: sheet,
                width,
                height: width,
                png,
            }
        }
        let mut s = GroupingSession::new(1);
        assert!(s.preview("a", 1024).is_none());
        s.store_preview("a", 1024, img(1024, 512, vec![1, 2, 3]));
        s.store_preview("a", 256, img(1024, 128, vec![4]));
        assert_eq!(
            s.preview("a", 1024).map(|p| p.png.clone()),
            Some(vec![1, 2, 3])
        );
        assert_eq!(s.preview("a", 1024).map(PreviewImage::scale), Some(0.5));
        assert_eq!(s.preview("a", 256).map(|p| p.width), Some(128));
        assert!(s.preview("b", 1024).is_none());

        // Re-storing the same sheet+scale replaces, it does not duplicate.
        s.store_preview("a", 1024, img(1024, 512, vec![9]));
        assert_eq!(s.preview("a", 1024).map(|p| p.png.clone()), Some(vec![9]));
        // At most `PREVIEW_CACHE` entries survive, oldest evicted.
        s.store_preview("b", 1024, img(2048, 1024, vec![7]));
        assert!(s.preview("a", 256).is_none(), "oldest evicted");
        assert_eq!(s.preview("b", 1024).map(|p| p.sheet_width), Some(2048));
        assert_eq!(GroupingSession::PREVIEW_CACHE, 2);
    }

    #[test]
    fn params_and_keys_are_stable() {
        let s = GroupingSession::new(3);
        assert_eq!(s.params().mask_capacity, 3);
        assert!(s.params().refine.enabled, "the UI session groups live");
        let key = s.key_for(b"bytes", &SegParams::default());
        assert!(key.contains("-v"), "{key}");
        assert_eq!(key, s.key_for(b"bytes", &SegParams::default()));
        assert_ne!(key, s.key_for(b"other", &SegParams::default()));
    }

    #[test]
    fn api_version_is_exported() {
        assert_eq!(GROUPING_VERSION, 1);
    }
}
