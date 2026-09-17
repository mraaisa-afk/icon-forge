//! §3.4 **F4 (noise) + F1 (fragmentation merge)** — the W8 stage that turns
//! raw CCL components into icon-sized groups.
//!
//! Order is mandated by ARCHITECTURE.md §3.4: noise is filtered *before*
//! merging, "or merge cost explodes". Every threshold derives from
//! `median_h` / `median_area` of the surviving components — no magic
//! constants — and the whole stage is deterministic: input order is
//! canonicalised internally, pair acceptance is decided from the original
//! components (never from partially merged output), and the result is
//! re-sorted into the same canonical order the CCL groupers use.
//!
//! ## F4 — noise filter
//!
//! | rule | drop when |
//! |---|---|
//! | speckle | `area < 16` |
//! | sliver | `min(w, h) < 3` |
//! | streak | `max(w, h) / min(w, h) > 60` |
//! | frame / rule line | spans ≥ 90 % of the sheet in **either** axis |
//! | isolated speck | both dims ≤ `0.35 · median_h` |
//!
//! ## F1 — merge (≤ 3 iterations)
//!
//! Candidates come from a uniform spatial hash whose cell is `2 × GAP`, so
//! each pass is `O(n)` with a local neighbourhood, not `O(n²)` — the C1 sheet
//! (1024 icons) stays in the tens of milliseconds.
//!
//! | rule | merge when |
//! |---|---|
//! | 1 | gap ≤ `GAP` and the smaller piece ≤ 0.5 × the larger's height |
//! | 2 | *(refusal)* merged aspect > 4.0 |
//! | 3 | *(refusal)* merged height > 1.9 × `median_h` |
//! | 4 | IoU > 0.25 **or** one bbox contains the other |
//! | 5 | gap ≤ 0.5 × `GAP`, height ratio ∈ [0.6, 1.6], and combined area ≤ `rule5_max_area_ratio × median_area` |
//!
//! Rules 2 and 3 are the documented cascade guards; rule 5's area guard is a
//! W8 addition (documented deviation, see the phase handoff): without it a
//! row of equal-sized icons with 4 px gaps glues itself together, which
//! cannot satisfy the corridor's exact-count gates.
//!
//! `GAP = clamp(0.35 × median_h, 4, 40)`.

use std::collections::HashMap;

use isg_core::{Bbox, ForegroundMask, IconGroup};

use super::background::BackgroundModel;
use super::confidence::{score_groups, ConfidenceParams, ConfidenceReport, ScoreInput};
use super::containment::{build_containment, ContainmentParams, ContainmentStats};
use super::grid::{classify, detect_grid, GridFit, GridHint, GridParams, GridStats};
use super::group::sort_groups;
use super::split::{resplit_forced, split_overmerged, SplitParams, SplitStats};

/// Bumped whenever merge behaviour changes, for the confidence/audit trail
/// and cache keys in later work items.
pub const REFINE_VERSION: u32 = 2;

/// Tunables for the refine chain. Defaults are the ARCHITECTURE.md §3.4
/// values; `enabled` is false until the corpus is calibrated (W12), so the
/// Phase 0–2 gates keep measuring the raw CCL output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefineParams {
    /// Master switch. `false` ⇒ [`refine_groups`] is the identity function.
    pub enabled: bool,
    /// F4 stage switch.
    pub noise_enabled: bool,
    /// F1 stage switch (`max_iterations` still caps the merge rounds).
    pub merge_enabled: bool,
    /// F2 over-merge split (W9).
    pub split: SplitParams,
    /// F5 grid-drift hint (W10).
    pub grid: GridParams,
    /// F3 hole/containment forest (W10).
    pub containment: ContainmentParams,
    /// §3.4 confidence scoring (W11) — reads the F5/F1/F3 evidence above.
    pub confidence: ConfidenceParams,
    /// F4 speckle: drop components with `area < noise_min_area`.
    pub noise_min_area: u32,
    /// F4 sliver: drop components with `min(w, h) < noise_min_dim`.
    pub noise_min_dim: u32,
    /// F4 streak: drop components with `max(w, h) / min(w, h) > noise_max_aspect`.
    pub noise_max_aspect: f32,
    /// F4 frame/rule line: drop components spanning ≥ this fraction of the
    /// sheet in either axis.
    pub frame_min_coverage: f32,
    /// F4b isolated speck: drop components with both dims ≤ this fraction of
    /// `median_h` (applied after the size gates, on the surviving set).
    pub specks_frac: f32,
    /// F1 gap fraction of `median_h`.
    pub gap_frac: f32,
    /// F1 gap floor in pixels.
    pub gap_min: f32,
    /// F1 gap ceiling in pixels.
    pub gap_max: f32,
    /// F1 iteration cap (ARCHITECTURE.md: max 3).
    pub max_iterations: u8,
    /// Rule 1: the smaller piece must be ≤ this fraction of the larger's
    /// height to count as a "small piece near a big piece".
    pub rule1_small_factor: f32,
    /// Rule 4: IoU above this merges outright.
    pub iou_merge: f32,
    /// Rule 5: gap must be ≤ this fraction of `GAP`.
    pub rule5_gap_frac: f32,
    /// Rule 5: lower bound of the accepted height ratio band.
    pub rule5_band_low: f32,
    /// Rule 5: upper bound of the accepted height ratio band.
    pub rule5_band_high: f32,
    /// Rule 5: combined area must be ≤ this multiple of `median_area`.
    pub rule5_max_area_ratio: f32,
    /// Refusal 2: reject a merge whose aspect exceeds this.
    pub max_merged_aspect: f32,
    /// Refusal 3: reject a merge whose height exceeds this × `median_h`.
    pub max_height_mult: f32,
}

impl Default for RefineParams {
    fn default() -> Self {
        Self {
            enabled: false,
            noise_enabled: true,
            merge_enabled: true,
            split: SplitParams::default(),
            grid: GridParams::default(),
            containment: ContainmentParams::default(),
            confidence: ConfidenceParams::default(),
            noise_min_area: 16,
            noise_min_dim: 3,
            noise_max_aspect: 60.0,
            frame_min_coverage: 0.9,
            specks_frac: 0.35,
            gap_frac: 0.35,
            gap_min: 4.0,
            gap_max: 40.0,
            max_iterations: 3,
            rule1_small_factor: 0.5,
            iou_merge: 0.25,
            rule5_gap_frac: 0.5,
            rule5_band_low: 0.6,
            rule5_band_high: 1.6,
            rule5_max_area_ratio: 1.75,
            max_merged_aspect: 4.0,
            max_height_mult: 1.9,
        }
    }
}

/// What the F1 pass did, for evidence logs and (later) the confidence score.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RefineStats {
    /// Components handed in.
    pub input: u32,
    /// Components dropped by F4 (size gates).
    pub removed_noise: u32,
    /// Components dropped by F4b (isolated specks).
    pub removed_specks: u32,
    /// Median component height after F4/F4b.
    pub median_h: f32,
    /// Median component area after F4/F4b.
    pub median_area: f32,
    /// Effective merge distance.
    pub gap: f32,
    /// Merge rounds that changed the grouping (≤ `max_iterations`).
    pub iterations: u8,
    /// Components merged away (input_surviving − output).
    pub merges: u32,
    /// Merges accepted by rule 1.
    pub rule1: u32,
    /// Merges accepted by rule 4 (IoU / containment).
    pub rule4: u32,
    /// Merges accepted by rule 5.
    pub rule5: u32,
    /// Pairs refused by the aspect guard (rule 2).
    pub refused_aspect: u32,
    /// Pairs refused by the height guard (rule 3).
    pub refused_height: u32,
    /// F2 split counters (W9).
    pub split: SplitStats,
    /// F5 grid-hint counters (W10).
    pub grid: GridStats,
    /// F3 containment counters (W10).
    pub containment: ContainmentStats,
    /// The measured F5 lattice hint — valleys, regularity and the grid flags.
    /// Kept so the UI can draw the lattice and re-score after manual edits (W12).
    pub hint: GridHint,
    /// The original components F5 handed back from provably wrong merges. Kept
    /// so a manual edit can re-score the edited list without silently dropping
    /// the review items the user was shown (W12).
    pub restored_originals: Vec<IconGroup>,
    /// §3.4 confidence score, its signals and the review list (W11).
    pub confidence: ConfidenceReport,
    /// Wall-clock for F4 + F1 + F2 + F5 + F3 + confidence, in milliseconds.
    pub elapsed_ms: f32,
}

/// Why a candidate pair was merged — kept for tests and the audit trail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeRule {
    /// Small piece near a big piece within `GAP`.
    Rule1,
    /// IoU > threshold or bbox containment.
    Rule4,
    /// Close, similarly sized pieces.
    Rule5,
}

impl MergeRule {
    fn as_str(self) -> &'static str {
        match self {
            MergeRule::Rule1 => "r1",
            MergeRule::Rule4 => "r4",
            MergeRule::Rule5 => "r5",
        }
    }
}

/// Axis-aligned gap between two boxes: `0` when they touch or overlap.
#[must_use]
pub fn bbox_gap(a: &Bbox, b: &Bbox) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let dx = (a.x.max(b.x)) as i64 - (ax2.min(bx2)) as i64;
    let dy = (a.y.max(b.y)) as i64 - (ay2.min(by2)) as i64;
    let dx = dx.max(0) as f32;
    let dy = dy.max(0) as f32;
    // Chebyshev (8-connected) distance between boxes: diagonally adjacent
    // pieces count as touching, matching the CCL connectivity.
    dx.max(dy)
}

/// Intersection-over-union of two boxes.
#[must_use]
pub fn bbox_iou(a: &Bbox, b: &Bbox) -> f32 {
    let ix = (a.x + a.w).min(b.x + b.w).saturating_sub(a.x.max(b.x));
    let iy = (a.y + a.h).min(b.y + b.h).saturating_sub(a.y.max(b.y));
    let inter = ix as u64 * iy as u64;
    if inter == 0 {
        return 0.0;
    }
    let union = a.area() + b.area() - inter;
    if union == 0 {
        return 0.0;
    }
    inter as f32 / union as f32
}

/// True when `outer` fully covers `inner`.
#[must_use]
pub fn bbox_contains(outer: &Bbox, inner: &Bbox) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.x + outer.w >= inner.x + inner.w
        && outer.y + outer.h >= inner.y + inner.h
}

/// The group that results from merging two (or more) components.
#[must_use]
pub fn merged_group(a: &IconGroup, b: &IconGroup) -> IconGroup {
    let x = a.bbox.x.min(b.bbox.x);
    let y = a.bbox.y.min(b.bbox.y);
    let x2 = (a.bbox.x + a.bbox.w).max(b.bbox.x + b.bbox.w);
    let y2 = (a.bbox.y + a.bbox.h).max(b.bbox.y + b.bbox.h);
    IconGroup {
        bbox: Bbox::from_parts(x, y, x2 - x, y2 - y),
        area: a.area + b.area,
        origin: (a.origin.0.min(b.origin.0), a.origin.1.min(b.origin.1)),
    }
}

/// Median of a value list (empty ⇒ 0.0). Even counts average the middle two.
fn median(values: &mut [u32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2] as f32
    } else {
        (values[n / 2 - 1] as f32 + values[n / 2] as f32) / 2.0
    }
}

/// F4 noise filter. Returns the surviving components (input order preserved).
fn noise_filter(
    groups: Vec<IconGroup>,
    sheet_w: u32,
    sheet_h: u32,
    p: &RefineParams,
) -> Vec<IconGroup> {
    let span_x = p.frame_min_coverage * sheet_w as f32;
    let span_y = p.frame_min_coverage * sheet_h as f32;
    groups
        .into_iter()
        .filter(|g| {
            let w = g.bbox.w;
            let h = g.bbox.h;
            // Size gates first — they are the cheap, order-free rules.
            if g.area < p.noise_min_area {
                return false;
            }
            let min_dim = w.min(h);
            let max_dim = w.max(h);
            if min_dim < p.noise_min_dim {
                return false;
            }
            if max_dim as f32 > p.noise_max_aspect * min_dim as f32 {
                return false;
            }
            // Frame / rule line: spans both opposite borders in either axis.
            if w as f32 >= span_x || h as f32 >= span_y {
                return false;
            }
            true
        })
        .collect()
}

/// F4b: drop components that are speck-sized relative to `median_h`.
fn speck_filter(groups: Vec<IconGroup>, median_h: f32, p: &RefineParams) -> (Vec<IconGroup>, u32) {
    if median_h <= 0.0 {
        return (groups, 0);
    }
    let limit = p.specks_frac * median_h;
    let before = groups.len();
    let kept: Vec<IconGroup> = groups
        .into_iter()
        .filter(|g| !(g.bbox.w as f32 <= limit && g.bbox.h as f32 <= limit))
        .collect();
    let removed = (before - kept.len()) as u32;
    (kept, removed)
}

/// Uniform spatial hash over the components; cells are `2 × GAP` wide so a
/// radius-`GAP` query touches at most a 2 × 2 block of cells per component.
struct SpatialHash {
    cell: f32,
    cells: HashMap<(i64, i64), Vec<u32>>,
}

impl SpatialHash {
    fn build(groups: &[IconGroup], cell: f32) -> Self {
        let cell = cell.max(1.0);
        let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        for (i, g) in groups.iter().enumerate() {
            for key in Self::keys_for(&g.bbox, cell) {
                cells.entry(key).or_default().push(i as u32);
            }
        }
        Self { cell, cells }
    }

    fn keys_for(b: &Bbox, cell: f32) -> Vec<(i64, i64)> {
        let cx0 = (b.x as f32 / cell).floor() as i64;
        let cy0 = (b.y as f32 / cell).floor() as i64;
        let cx1 = ((b.x + b.w) as f32 / cell).floor() as i64;
        let cy1 = ((b.y + b.h) as f32 / cell).floor() as i64;
        let mut out = Vec::new();
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                out.push((cx, cy));
            }
        }
        out
    }

    /// Candidate indices within `radius` of `bbox`, sorted and deduplicated.
    fn candidates(&self, bbox: &Bbox, radius: f32) -> Vec<u32> {
        let cx0 = ((bbox.x as f32 - radius) / self.cell).floor() as i64;
        let cy0 = ((bbox.y as f32 - radius) / self.cell).floor() as i64;
        let cx1 = (((bbox.x + bbox.w) as f32 + radius) / self.cell).floor() as i64;
        let cy1 = (((bbox.y + bbox.h) as f32 + radius) / self.cell).floor() as i64;
        let mut out: Vec<u32> = Vec::new();
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                if let Some(list) = self.cells.get(&(cx, cy)) {
                    out.extend_from_slice(list);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Decide whether a candidate pair merges, and under which rule.
///
/// `stats` is only used to count refusals; acceptance is decided purely from
/// the two *original* components, which is what makes the pass order-free.
fn merge_rule(
    a: &IconGroup,
    b: &IconGroup,
    stats: &mut RefineStats,
    p: &RefineParams,
    medians: (f32, f32),
    gap: f32,
) -> Option<MergeRule> {
    let (median_h, median_area) = medians;
    let combined = merged_group(a, b);

    // Rule 4 first: containment / overlap is not subject to the size guards —
    // nested fragments are inside an existing bbox, so they cannot grow it.
    if bbox_contains(&a.bbox, &b.bbox) || bbox_contains(&b.bbox, &a.bbox) {
        return Some(MergeRule::Rule4);
    }
    let iou = bbox_iou(&a.bbox, &b.bbox);
    if iou > p.iou_merge {
        return Some(MergeRule::Rule4);
    }

    let d = bbox_gap(&a.bbox, &b.bbox);
    if d > gap {
        return None;
    }

    // Refusal 2: aspect. Refusal 3: height explosion.
    let min_dim = combined.bbox.w.min(combined.bbox.h).max(1) as f32;
    let max_dim = combined.bbox.w.max(combined.bbox.h) as f32;
    if max_dim / min_dim > p.max_merged_aspect {
        stats.refused_aspect += 1;
        return None;
    }
    if median_h > 0.0 && combined.bbox.h as f32 > p.max_height_mult * median_h {
        stats.refused_height += 1;
        return None;
    }

    // Rule 1: small piece near a big piece.
    let (lo, hi) = if a.bbox.h <= b.bbox.h {
        (a.bbox.h, b.bbox.h)
    } else {
        (b.bbox.h, a.bbox.h)
    };
    if (lo as f32) <= p.rule1_small_factor * hi as f32 {
        return Some(MergeRule::Rule1);
    }

    // Rule 5: close, similarly sized — with the area guard.
    let ratio = if hi == 0 { 1.0 } else { lo as f32 / hi as f32 };
    let combined_area = combined.area as f32;
    if d <= p.rule5_gap_frac * gap
        && ratio >= p.rule5_band_low
        && ratio <= p.rule5_band_high
        && median_area > 0.0
        && combined_area <= p.rule5_max_area_ratio * median_area
    {
        return Some(MergeRule::Rule5);
    }

    None
}

/// One merge pass: every accepted pair is decided from the input snapshot,
/// then applied with union-find anchored on the lowest index.
fn merge_pass(
    groups: &mut Vec<IconGroup>,
    members: &mut Vec<Vec<IconGroup>>,
    gap: f32,
    stats: &mut RefineStats,
    p: &RefineParams,
    medians: (f32, f32),
) -> bool {
    let hash = SpatialHash::build(groups, 2.0 * gap);
    let n = groups.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();

    fn find(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            parent[i as usize] = parent[parent[i as usize] as usize];
            i = parent[i as usize];
        }
        i
    }

    let mut merged = false;
    for i in 0..n {
        let bbox = groups[i].bbox;
        for j in hash.candidates(&bbox, gap) {
            let j = j as usize;
            if j <= i {
                continue;
            }
            if let Some(rule) = merge_rule(&groups[i], &groups[j], stats, p, medians, gap) {
                let (ri, rj) = (find(&mut parent, i as u32), find(&mut parent, j as u32));
                if ri != rj {
                    let (lo, hi) = if ri < rj { (ri, rj) } else { (rj, ri) };
                    parent[hi as usize] = lo;
                    merged = true;
                    stats.merges += 1;
                    match rule {
                        MergeRule::Rule1 => stats.rule1 += 1,
                        MergeRule::Rule4 => stats.rule4 += 1,
                        MergeRule::Rule5 => stats.rule5 += 1,
                    }
                }
            }
        }
    }

    if !merged {
        return false;
    }

    // Provenance travels with the merge: F5 may later learn (from the grid)
    // that a proximity merge was wrong, and undoing it needs the original
    // components — re-splitting a *disjoint* union through the watershed is
    // impossible, because the splitter only sees the connected component it
    // starts from.
    let mut acc: HashMap<u32, (IconGroup, Vec<IconGroup>)> = HashMap::new();
    for (i, g) in groups.iter().enumerate() {
        let root = find(&mut parent, i as u32);
        let mem = if i < members.len() {
            members[i].clone()
        } else {
            vec![*g]
        };
        match acc.get_mut(&root) {
            Some((existing, list)) => {
                *existing = merged_group(existing, g);
                list.extend(mem);
            }
            None => {
                acc.insert(root, (*g, mem));
            }
        }
    }
    let mut entries: Vec<(IconGroup, Vec<IconGroup>)> = acc.into_values().collect();
    // Sort by the group's own scan key: for a union that equals its first
    // member's key, so `members` stays index-aligned with `groups`.
    entries.sort_by(|a, b| {
        a.0.bbox
            .y
            .cmp(&b.0.bbox.y)
            .then_with(|| a.0.bbox.x.cmp(&b.0.bbox.x))
            .then_with(|| a.0.origin.0.cmp(&b.0.origin.0))
            .then_with(|| a.0.origin.1.cmp(&b.0.origin.1))
    });
    *groups = entries.iter().map(|(g, _)| *g).collect();
    *members = entries.into_iter().map(|(_, m)| m).collect();
    true
}

/// §3.4 F4 + F1 + F2 over the raw CCL components. The mask provides the sheet
/// dimensions (frame/rule-line gate) and the ink pixels the splitter needs.
///
/// With `params.enabled == false` the input vector is returned unchanged.
#[must_use]
pub fn refine_groups(
    groups: Vec<IconGroup>,
    mask: &ForegroundMask,
    params: &RefineParams,
) -> Vec<IconGroup> {
    refine_groups_with_stats(groups, mask, params).0
}

/// [`refine_groups`] plus the evidence counters, without the background model —
/// the §3.4 uncertain-background signal stays at 0 (see
/// [`refine_groups_with_context`] for the full call the app makes).
#[must_use]
pub fn refine_groups_with_stats(
    groups: Vec<IconGroup>,
    mask: &ForegroundMask,
    params: &RefineParams,
) -> (Vec<IconGroup>, RefineStats) {
    refine_groups_with_context(groups, mask, None, params)
}

/// [`refine_groups_with_stats`] with the detected background model, so the
/// confidence score can see which detector won and with what agreement.
#[must_use]
pub fn refine_groups_with_context(
    groups: Vec<IconGroup>,
    mask: &ForegroundMask,
    background: Option<&BackgroundModel>,
    params: &RefineParams,
) -> (Vec<IconGroup>, RefineStats) {
    let sheet_w = mask.width();
    let sheet_h = mask.height();
    let t0 = std::time::Instant::now();
    let mut stats = RefineStats {
        input: groups.len() as u32,
        ..RefineStats::default()
    };
    if !params.enabled {
        stats.elapsed_ms = t0.elapsed().as_secs_f32() * 1000.0;
        return (groups, stats);
    }

    let after_noise = if params.noise_enabled {
        noise_filter(groups, sheet_w, sheet_h, params)
    } else {
        groups
    };
    stats.removed_noise = stats.input - after_noise.len() as u32;

    // `median_h` for the F4b speck gate comes from the size-filtered set;
    // everything downstream recomputes medians on the speck-filtered set.
    let mut heights: Vec<u32> = after_noise.iter().map(|g| g.bbox.h).collect();
    let pre_median_h = median(&mut heights);
    let (mut kept, removed_specks) = speck_filter(after_noise, pre_median_h, params);
    stats.removed_specks = removed_specks;
    sort_groups(&mut kept);

    let mut heights: Vec<u32> = kept.iter().map(|g| g.bbox.h).collect();
    let median_h = median(&mut heights);
    let mut areas: Vec<u32> = kept.iter().map(|g| g.area).collect();
    let median_area = median(&mut areas);
    stats.median_h = median_h;
    stats.median_area = median_area;

    let gap = (params.gap_frac * median_h).clamp(params.gap_min, params.gap_max);
    stats.gap = gap;

    let mut current = kept;
    // Provenance for the F5 back-edge: `members[i]` are the pre-merge
    // components that make up `current[i]`.
    let mut members: Vec<Vec<IconGroup>> = current.iter().map(|g| vec![*g]).collect();
    if params.merge_enabled && !current.is_empty() && median_h > 0.0 {
        for _ in 0..params.max_iterations {
            let merged = merge_pass(
                &mut current,
                &mut members,
                gap,
                &mut stats,
                params,
                (median_h, median_area),
            );
            if !merged {
                break;
            }
            stats.iterations += 1;
        }
    }
    if params.split.enabled && !current.is_empty() && median_h > 0.0 {
        current = split_overmerged(current, mask, median_h, &params.split, &mut stats.split);
    }

    // F5 (W10): a regular lattice turns F1's merges that crossed a valley into
    // provable merges — they are handed back to the watershed with the size
    // gate skipped, because the hint *is* the evidence the gate stood in for.
    // The hint is measured whenever either consumer needs it; F5 acts on it and
    // the confidence score reports it (a disabled F5 must not blind the score).
    // Originals F5 handed back from provably wrong merges — reported as review
    // items (they are where the grouping wanted to glue) without a deduction.
    let mut restored_originals: Vec<IconGroup> = Vec::new();
    let hint = if (params.grid.enabled || params.confidence.enabled)
        && !current.is_empty()
        && median_h > 0.0
    {
        detect_grid(mask, &params.grid, &mut stats.grid)
    } else {
        GridHint::default()
    };
    if params.grid.enabled && hint.any() {
        let min_area =
            f64::from(params.grid.min_cells_to_resplit * median_h * median_h).round() as u64;
        let mut eligible: Vec<bool> = vec![false; current.len()];
        for (i, g) in current.iter().enumerate() {
            if matches!(
                classify(&g.bbox, &hint, params.grid.span_tolerance),
                GridFit::SpansMultipleCells { .. }
            ) {
                stats.grid.flagged += 1;
                if g.bbox.area() >= min_area {
                    eligible[i] = true;
                }
            }
        }
        let mut keep: Vec<IconGroup> = Vec::with_capacity(current.len());
        let mut restored: Vec<IconGroup> = Vec::new();
        let mut glue: Vec<IconGroup> = Vec::new();
        for (i, g) in current.iter().enumerate() {
            if !eligible[i] {
                keep.push(*g);
                continue;
            }
            if members.get(i).is_some_and(|m| m.len() >= 2) {
                // A proximity merge: the hint proves it crossed a valley, so
                // the original components come back verbatim.
                stats.grid.restored += 1;
                restored_originals.extend(members[i].iter().copied());
                restored.extend(members[i].iter().copied());
            } else if glue.len() < params.grid.max_resplit as usize {
                glue.push(*g);
            }
        }
        if !restored.is_empty() || !glue.is_empty() {
            stats.grid.resplit_candidates = glue.len() as u32 + stats.grid.restored;
            let (rest, split, regions) =
                resplit_forced(glue, mask, median_h, &params.split, &mut stats.split);
            keep.extend(restored);
            keep.extend(rest);
            sort_groups(&mut keep);
            current = keep;
            stats.grid.resplit = split;
            stats.grid.regions = regions;
        }
    }

    sort_groups(&mut current);

    // F3 (W10): containment forest + hole counts for confidence (W11) and the
    // evenodd subpaths at trace time. Analysis only — the group list is
    // unchanged, which is the point: a hole is background, never an icon.
    if params.containment.enabled && !current.is_empty() {
        let _forest =
            build_containment(&current, mask, &params.containment, &mut stats.containment);
    }

    // W11: §3.4 confidence over everything the chain above measured.
    if params.confidence.enabled && !current.is_empty() {
        // F1 merges that survived F5's provenance undo: those are the ones the
        // user is actually looking at.
        let surviving_merges = stats.merges.saturating_sub(stats.grid.restored);
        stats.confidence = score_groups(
            ScoreInput {
                groups: &current,
                mask,
                merges: surviving_merges,
                restored: &restored_originals,
                hint: &hint,
                background,
            },
            &params.confidence,
        );
    }
    stats.hint = hint;
    stats.restored_originals = restored_originals;
    stats.elapsed_ms = t0.elapsed().as_secs_f32() * 1000.0;
    (current, stats)
}

/// Convenience: the rule label for the merge audit trail.
#[must_use]
pub fn rule_label(rule: MergeRule) -> &'static str {
    rule.as_str()
}

#[cfg(test)]
mod tests {
    use super::super::group::RleCclGrouper;
    use super::*;
    use isg_core::GroupingStrategy;

    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % bound
        }
    }

    fn group(x: u32, y: u32, w: u32, h: u32, area: u32) -> IconGroup {
        IconGroup {
            bbox: Bbox::from_parts(x, y, w, h),
            area,
            origin: (x, y),
        }
    }

    fn box_body(x: u32, y: u32, w: u32, h: u32) -> IconGroup {
        group(x, y, w, h, w * h)
    }

    /// F4 + F1 on, F2 off — the W8 stage tests isolate their own stage the
    /// same way `max_iterations: 0` isolates F4 from F1.
    fn enabled() -> RefineParams {
        RefineParams {
            enabled: true,
            split: SplitParams {
                enabled: false,
                ..SplitParams::default()
            },
            ..RefineParams::default()
        }
    }

    /// A mask with the right dimensions and no ink: F4/F1 never read pixels.
    fn blank(w: u32, h: u32) -> ForegroundMask {
        ForegroundMask::new(w, h)
    }

    #[test]
    fn disabled_is_identity() {
        let groups = vec![
            box_body(10, 10, 30, 30),
            group(60, 10, 2, 2, 4),       // speckle
            group(100, 100, 300, 1, 300), // rule line
        ];
        let out = refine_groups(groups.clone(), &blank(512, 512), &RefineParams::default());
        assert_eq!(out, groups, "disabled refine must not touch the input");
    }

    #[test]
    fn noise_filter_drops_speckles_slivers_streaks_and_frames() {
        let p = enabled();
        let mut groups = vec![
            box_body(10, 10, 40, 40),
            box_body(100, 10, 40, 40),
            box_body(190, 10, 40, 40),
            box_body(280, 10, 40, 40),
        ];
        // F4 gates:
        groups.push(group(10, 100, 3, 3, 9)); // area 9  < 16  → speckle
        groups.push(group(30, 100, 20, 2, 40)); // min dim 2 < 3 → sliver
        groups.push(group(60, 100, 200, 2, 400)); // aspect 100 > 60 → streak
                                                  // F4b (both dims ≤ 0.35 × median_h = 14): 10×10 survives the gates…
        groups.push(box_body(120, 100, 10, 10));
        // …but an 18×18 does not (18 > 14).
        groups.push(box_body(150, 100, 18, 18));
        // Frame line: spans the full sheet width.
        groups.push(group(0, 200, 512, 15, 512 * 15));

        let (out, stats) = refine_groups_with_stats(groups, &blank(512, 512), &p);
        assert_eq!(stats.input, 10);
        assert_eq!(
            stats.removed_noise, 4,
            "speckle + sliver + streak + full-width frame line"
        );
        assert_eq!(stats.removed_specks, 1, "10×10 is speck-sized");
        assert_eq!(stats.median_h, 40.0);
        // Bodies + the 18×18 keeper, nothing merged (gaps of 50 px).
        assert_eq!(out.len(), 5);
        assert!(
            out.iter().all(|g| g.bbox.w == g.bbox.h || g.bbox.w == 18),
            "only the 18×18 non-square survives: {out:?}"
        );
        assert!(out
            .iter()
            .any(|g| g.bbox == Bbox::new(150, 100, 18, 18).unwrap()));
    }

    #[test]
    fn frame_line_spanning_two_opposite_borders_is_dropped() {
        let p = enabled();
        // A hollow 500×500 frame on a 512² sheet: its area (8k px) passes
        // every size gate — only the border-span rule can catch it. The two
        // bodies sit inside the frame's bbox, so if the frame survived they
        // would merge into it by rule 4 (containment).
        let frame = group(6, 6, 500, 500, 8_000);
        let inside = vec![
            frame,
            box_body(300, 300, 40, 40),
            box_body(360, 300, 40, 40),
        ];
        let (out, stats) = refine_groups_with_stats(inside, &blank(512, 512), &p);
        assert_eq!(stats.removed_noise, 1, "frame dropped before merging");
        assert_eq!(out.len(), 2);

        // On a 1024² sheet the same component spans nothing, so it survives
        // — and with bodies outside its bbox nothing merges at all.
        let mut outside = vec![
            frame,
            box_body(600, 300, 40, 40),
            box_body(660, 300, 40, 40),
        ];
        let (out_big, stats_big) =
            refine_groups_with_stats(outside.clone(), &blank(1024, 1024), &p);
        assert_eq!(
            stats_big.removed_noise, 0,
            "no border span on a larger sheet"
        );
        assert_eq!(out_big.len(), 3, "frame + two separate bodies");

        // …and with the bodies *inside* it, containment merges them in.
        outside[1] = box_body(300, 300, 40, 40);
        outside[2] = box_body(360, 300, 40, 40);
        let (merged, stats_merged) = refine_groups_with_stats(outside, &blank(1024, 1024), &p);
        assert_eq!(stats_merged.rule4, 2, "both bodies are contained");
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].bbox, Bbox::new(6, 6, 500, 500).unwrap());
    }

    #[test]
    fn merge_joins_a_small_fragment_within_gap() {
        let p = enabled();
        // Body 30×30 at (10,10); fragment 12×12 at (46,24) → gap 6.
        // heights [30, 12] ⇒ median_h 21 ⇒ GAP = clamp(7.35, 4, 40) = 7.35 ≥ 6 ✓
        // rule 1: smaller height 12 ≤ 0.5 × 30 ✓
        // merged box (10,10,48,30): aspect 1.6 < 4 ✓, height 30 ≤ 1.9 × 21 ✓
        let groups = vec![box_body(10, 10, 30, 30), box_body(46, 24, 12, 12)];
        let (out, stats) = refine_groups_with_stats(groups, &blank(256, 256), &p);
        assert_eq!(out.len(), 1, "fragment must join the body: {out:?}");
        assert_eq!(out[0].bbox, Bbox::new(10, 10, 48, 30).unwrap());
        assert_eq!(out[0].area, 30 * 30 + 12 * 12);
        assert_eq!(stats.merges, 1);
        assert_eq!(stats.rule1, 1);
        assert_eq!(stats.gap, 7.35);
        assert_eq!(stats.iterations, 1, "one pass suffices");
    }

    #[test]
    fn merge_refuses_two_full_size_icons_at_a_four_pixel_gap() {
        let p = enabled();
        // 24×24 icons with 4 px gaps: GAP = 8.4, so rule 5's gap test passes
        // (4 ≤ 4.2) and the height ratio is 1.0 — only the area guard
        // (2 × 576 > 1.75 × 576) and rule 1's size factor stop the row glue.
        let groups: Vec<IconGroup> = (0..12).map(|i| box_body(10 + i * 28, 10, 24, 24)).collect();
        let (out, stats) = refine_groups_with_stats(groups, &blank(512, 512), &p);
        assert_eq!(out.len(), 12, "uniform icons must never glue: {out:?}");
        assert_eq!(stats.merges, 0);
    }

    #[test]
    fn merge_allows_body_plus_near_arm_but_not_two_bodies() {
        let p = enabled();
        // Body 30×30 + a 24×14 arm 4 px to its right: combined 58×30, area
        // 900 + 336 = 1236 ≤ 1.75 × 900 = 1575 ✓, ratio 14/30 = 0.47 (rule 1
        // fires first because 14 ≤ 15).
        let body = box_body(10, 10, 30, 30);
        let arm = box_body(44, 18, 14, 24);
        let filler: Vec<IconGroup> = (0..4).map(|i| box_body(200 + i * 60, 10, 30, 30)).collect();
        let mut groups = vec![body, arm];
        groups.extend(filler.iter().copied());
        let (out, stats) = refine_groups_with_stats(groups, &blank(512, 512), &p);
        // Body y 10..40, arm y 18..42 ⇒ merged box (10,10,48,32).
        assert_eq!(out.len(), 5, "one merge: 6 → 5");
        assert_eq!(stats.merges, 1);
        assert_eq!(
            stats.rule5, 1,
            "rule 5 (close + similarly sized + area guard)"
        );
        assert!(out
            .iter()
            .any(|g| g.bbox == Bbox::new(10, 10, 48, 32).unwrap() && g.area == 1_236));
    }

    #[test]
    fn containment_and_overlap_merge_by_rule_4() {
        let p = enabled();
        // A speck-sized component *inside* a large bbox is still removed by
        // F4b, so use a nested pair that survives the size gates.
        let outer = group(10, 10, 60, 60, 2_000); // hollow ring, sparse area
        let inner = group(30, 30, 20, 20, 400); // inside the ring's box
        let filler: Vec<IconGroup> = (0..4)
            .map(|i| box_body(200 + i * 60, 200, 30, 30))
            .collect();
        let mut groups = vec![outer, inner];
        groups.extend(filler.iter().copied());
        let (out, stats) = refine_groups_with_stats(groups, &blank(512, 512), &p);
        assert_eq!(stats.rule4, 1, "containment merges by rule 4");
        assert!(out
            .iter()
            .any(|g| g.bbox == Bbox::new(10, 10, 60, 60).unwrap() && g.area == 2_400));
        assert_eq!(out.len(), 5);

        // Overlap (IoU > 0.25) also merges even when there is no containment.
        let filler2: Vec<IconGroup> = (0..4)
            .map(|i| box_body(300 + i * 60, 300, 30, 30))
            .collect();
        let mut overlapping = vec![
            box_body(10, 400, 40, 40),
            box_body(28, 400, 40, 40), // overlap 12×40 = 480, IoU 0.43
        ];
        overlapping.extend(filler2.iter().copied());
        let (out2, stats2) = refine_groups_with_stats(overlapping, &blank(512, 512), &p);
        assert_eq!(stats2.rule4, 1);
        assert!(out2
            .iter()
            .any(|g| g.bbox == Bbox::new(10, 400, 58, 40).unwrap()));
    }

    #[test]
    fn guards_refuse_aspect_and_height_explosions() {
        let p = enabled();

        // Height guard: two stacked bodies with only a 6 px gap; every size
        // rule would accept (gap ≤ GAP 7, smaller height 12 ≤ 0.5 × 40), but
        // the merged 30×58 box exceeds 1.9 × median_h (20) = 38.
        let mut st = RefineStats::default();
        let a = box_body(10, 10, 30, 40);
        let b = box_body(10, 56, 30, 12);
        assert_eq!(bbox_gap(&a.bbox, &b.bbox), 6.0);
        assert_eq!(merge_rule(&a, &b, &mut st, &p, (20.0, 400.0), 7.0), None);
        assert_eq!(st.refused_height, 1, "height guard fires");
        assert_eq!(st.refused_aspect, 0);

        // Aspect guard: three merged 6×6 squares form a 24×6 row; adding a
        // fourth would exceed the 4.0 aspect ceiling.
        let row = merged_group(
            &merged_group(&box_body(10, 10, 6, 6), &box_body(19, 10, 6, 6)),
            &box_body(28, 10, 6, 6),
        );
        assert_eq!(row.bbox, Bbox::new(10, 10, 24, 6).unwrap());
        let mut st2 = RefineStats::default();
        let next = box_body(40, 10, 6, 6);
        assert_eq!(bbox_gap(&row.bbox, &next.bbox), 6.0);
        assert_eq!(
            merge_rule(&row, &next, &mut st2, &p, (20.0, 400.0), 20.0),
            None
        );
        assert_eq!(st2.refused_aspect, 1, "aspect guard fires");
        assert_eq!(st2.refused_height, 0);

        // Containment is exempt from the guards: a nested fragment cannot
        // grow the bbox, so rule 4 still merges it.
        let mut st3 = RefineStats::default();
        let big = box_body(10, 10, 60, 60); // x 10..70, y 10..70
        let nested = group(30, 30, 8, 38, 200); // x 30..38, y 30..68 — inside
        assert_eq!(
            merge_rule(&nested, &big, &mut st3, &p, (20.0, 400.0), 4.0),
            Some(MergeRule::Rule4),
            "containment merges regardless of size guards"
        );
    }

    #[test]
    fn fragments_merge_but_bodies_stay_separate() {
        let p = enabled();
        // 12 bodies 30×30 on a 60 px pitch, each with a 10×10 fragment 6 px
        // to its right: F1 must attach every fragment and merge nothing else.
        let mut groups = Vec::new();
        for i in 0..12u32 {
            let x = 10 + i * 60;
            groups.push(box_body(x, 10, 30, 30));
            groups.push(box_body(x + 36, 20, 10, 10));
        }
        let (out, stats) = refine_groups_with_stats(groups, &blank(1024, 1024), &p);
        assert_eq!(stats.input, 24);
        assert_eq!(out.len(), 12, "each body absorbs exactly one fragment");
        assert_eq!(stats.merges, 12);
        assert_eq!(stats.rule1, 12, "10 ≤ 0.5 × 30 ⇒ rule 1");
        // Height set is 12 × 30 and 12 × 10 ⇒ median_h 20 ⇒ GAP 7; the
        // fragments are 10 px tall (> 0.35 × 20 = 7), so no speck filter.
        assert_eq!(stats.median_h, 20.0);
        assert_eq!(stats.gap, 7.0);
        assert!(out
            .iter()
            .all(|g| g.bbox == Bbox::new(g.bbox.x, 10, 46, 30).unwrap() && g.area == 1_000));
        // Nothing may have grown past the pitch — bodies never glue.
        assert!(out.windows(2).all(|w| w[0].bbox.x + 60 == w[1].bbox.x));
    }

    #[test]
    fn merge_is_permutation_invariant_and_deterministic() {
        let p = enabled();
        let mut groups = vec![
            box_body(10, 10, 30, 30),
            box_body(10, 46, 12, 30), // merges with the body
            box_body(100, 10, 30, 30),
            box_body(104, 12, 20, 20), // merges by IoU
            box_body(200, 200, 30, 30),
            group(204, 204, 18, 18, 324),
            box_body(300, 300, 40, 40),
        ];
        let expected = refine_groups(groups.clone(), &blank(512, 512), &p);
        let mut rng = Lcg::new(42);
        for _ in 0..25 {
            for i in (1..groups.len()).rev() {
                let j = rng.next(i + 1);
                groups.swap(i, j);
            }
            let shuffled = refine_groups(groups.clone(), &blank(512, 512), &p);
            assert_eq!(shuffled, expected, "refine must not depend on input order");
        }
        // Same input twice ⇒ identical output.
        let again = refine_groups(expected.clone(), &blank(512, 512), &p);
        assert_eq!(again, expected, "second application must be stable");
    }

    #[test]
    fn iteration_cap_limits_the_merge_chain() {
        let mut p = enabled();
        // A chain whose second link only becomes reachable once the first
        // fragment has grown the body's bbox:
        //   body 40×40 at (10,10) — fragment 16×16 at (56,44) [gap 6] ⇒ merge
        //   ⇒ body' spans x 10..72, and the second fragment at (78,10) [gap 6
        //     from body', 28 px from the original body] merges on pass 2.
        let body = box_body(10, 10, 40, 40);
        let frag1 = box_body(56, 44, 16, 16);
        let frag2 = box_body(78, 10, 16, 16);
        let fillers = [box_body(200, 200, 40, 40), box_body(320, 320, 40, 40)];
        let mut groups = vec![body, frag1, frag2];
        groups.extend(fillers.iter().copied());
        assert_eq!(bbox_gap(&body.bbox, &frag1.bbox), 6.0);
        assert_eq!(
            bbox_gap(&body.bbox, &frag2.bbox),
            28.0,
            "out of reach at pass 1"
        );

        p.max_iterations = 0;
        let (none, stats0) = refine_groups_with_stats(groups.clone(), &blank(512, 512), &p);
        assert_eq!(stats0.iterations, 0, "cap 0 ⇒ F4 only, no F1");
        assert_eq!(none.len(), 5);

        p.max_iterations = 1;
        let (one_pass, stats1) = refine_groups_with_stats(groups.clone(), &blank(512, 512), &p);
        assert_eq!(stats1.iterations, 1);
        assert_eq!(one_pass.len(), 4, "only the first link merges");

        p.max_iterations = 3;
        let (chained, stats3) = refine_groups_with_stats(groups, &blank(512, 512), &p);
        assert_eq!(stats3.iterations, 2, "the chain needs exactly two passes");
        assert_eq!(chained.len(), 3, "body + both fragments");
        assert!(chained
            .iter()
            .any(|g| g.bbox == Bbox::new(10, 10, 84, 50).unwrap() && g.area == 2_112));
    }

    /// A minimal `RasterView` so the real CCL grouper can run on a test mask.
    struct MaskView(ForegroundMask);

    impl isg_core::RasterView for MaskView {
        fn width(&self) -> u32 {
            self.0.width()
        }
        fn height(&self) -> u32 {
            self.0.height()
        }
        fn luma_row(&self, _y: u32) -> &[f32] {
            &[]
        }
    }

    #[test]
    fn full_stage_chain_splits_a_glued_blob_from_real_ccl_groups() {
        use isg_core::GroupingStrategy;
        // Twelve clean 24×24 icons on a 60 px pitch plus one 3×3 glued blob
        // (24×24 cells, 2 px bridges) — the F1 cascade's signature failure.
        // Real CCL gives 13 components; F4 leaves them alone; F1 merges
        // nothing (gaps far exceed GAP = 8.4); F2 splits the blob into 9.
        let mut mask = ForegroundMask::new(1024, 1024);
        for i in 0..12u32 {
            let x = 40 + i * 60;
            for y in 40..64 {
                for xx in x..x + 24 {
                    mask.set(xx, y, true);
                }
            }
        }
        for r in 0..3u32 {
            for c in 0..3u32 {
                let x = 700 + c * 26;
                let y = 700 + r * 26;
                for yyy in y..y + 24 {
                    for xxx in x..x + 24 {
                        mask.set(xxx, yyy, true);
                    }
                }
                if c + 1 < 3 {
                    for yyy in y + 8..y + 16 {
                        for xxx in x + 24..x + 26 {
                            mask.set(xxx, yyy, true);
                        }
                    }
                }
                if r + 1 < 3 {
                    for xxx in x + 8..x + 16 {
                        for yyy in y + 24..y + 26 {
                            mask.set(xxx, yyy, true);
                        }
                    }
                }
            }
        }

        let view = MaskView(mask.clone());
        let raw = super::super::group::RleCclGrouper { min_area: 16 }.group_all(&view, &mask);
        assert_eq!(raw.len(), 13, "12 icons + 1 glued blob");

        let p = RefineParams {
            enabled: true,
            ..RefineParams::default()
        };
        let (out, stats) = refine_groups_with_stats(raw, &mask, &p);
        assert_eq!(stats.removed_noise, 0);
        assert_eq!(stats.merges, 0, "nothing is close enough to merge");
        assert_eq!(stats.split.candidates, 1, "only the blob is oversized");
        assert_eq!(stats.split.split, 1);
        assert_eq!(out.len(), 12 + 9, "blob becomes 9 cell-sized groups");
        let ink: u32 = out.iter().map(|g| g.area).sum();
        let mask_ink: u32 = mask.runs().iter().map(|r| r.len()).sum();
        assert_eq!(ink, mask_ink, "the whole chain conserves ink");
    }

    /// Hollow square outline (`ring`/`frame` corpus shape): `size` px box with
    /// a `t` px stroke.
    fn ring(mask: &mut ForegroundMask, x0: u32, y0: u32, size: u32, t: u32) {
        for dy in 0..size {
            for dx in 0..size {
                if dx < t || dy < t || dx + t >= size || dy + t >= size {
                    mask.set(x0 + dx, y0 + dy, true);
                }
            }
        }
    }

    #[test]
    fn grid_hint_restores_merges_that_crossed_a_valley() {
        // 4×4 lattice of rings, pitch 56 (8 px gaps, inside rule 5's window).
        // One cell holds a smaller ring so rule 5's area guard has the size
        // asymmetry it needs — two equal rings are refused, which is the W8
        // anchor test's whole point.
        let mut mask = blank(300, 300);
        for row in 0..4u32 {
            for col in 0..4u32 {
                let small = row == 2 && col == 2;
                let size = if small { 30 } else { 48 };
                ring(&mut mask, 10 + col * 56, 10 + row * 56, size, 4);
            }
        }
        let raster = crate::pipeline::raster::SheetRaster::from_rgba(1, 1, vec![0, 0, 0, 0]);
        let groups = RleCclGrouper::default().group_all(&raster, &mask);
        assert_eq!(groups.len(), 16, "16 separate rings before merging");
        let params = enabled();
        let (out, stats) = refine_groups_with_stats(groups, &mask, &params);
        assert!(
            stats.merges >= 1,
            "the lattice must provoke a rule-5 merge: {stats:?}"
        );
        assert!(
            stats.grid.flagged >= 1 && stats.grid.restored >= 1,
            "the grid hint must undo what it proves wrong: {stats:?}"
        );
        assert_eq!(out.len(), 16, "count is back to the raw CCL count");
        // Invariant after the whole chain: on a detected lattice no surviving
        // group may straddle a valley.
        let gp = GridParams::default();
        let mut gstats = GridStats::default();
        let hint = detect_grid(&mask, &gp, &mut gstats);
        assert!(hint.any(), "lattice detected: {hint:?}");
        for g in &out {
            assert!(
                !matches!(
                    classify(&g.bbox, &hint, gp.span_tolerance),
                    GridFit::SpansMultipleCells { .. }
                ),
                "group {:?} still spans cells",
                g.bbox
            );
        }
    }

    #[test]
    fn containment_closes_the_chain() {
        // A ring plus a dot in its hole. With merging off the two stay
        // separate and F3 must report: one hole on the ring, depth 1 on the
        // dot, one root. With merging on, rule 4 (containment) glues the dot
        // into the ring — the §3.4 behaviour — and the ring still reports its
        // hole afterwards.
        let mut mask = blank(64, 64);
        ring(&mut mask, 8, 8, 48, 6);
        // 16×16: big enough to survive F4b's speck gate (dims > 0.35·median_h).
        for dy in 0..16 {
            for dx in 0..16 {
                mask.set(24 + dx, 24 + dy, true);
            }
        }
        let raster = crate::pipeline::raster::SheetRaster::from_rgba(1, 1, vec![0, 0, 0, 0]);
        let groups = RleCclGrouper::default().group_all(&raster, &mask);
        assert_eq!(groups.len(), 2);

        let split_params = RefineParams {
            enabled: true,
            merge_enabled: false,
            ..enabled()
        };
        let (out, stats) = refine_groups_with_stats(groups.clone(), &mask, &split_params);
        assert_eq!(out.len(), 2, "ring and dot stay two groups");
        assert_eq!(stats.containment.holes, 1, "the ring encloses one hole");
        let mut cstats = ContainmentStats::default();
        let forest = build_containment(&out, &mask, &split_params.containment, &mut cstats);
        let depths: Vec<u32> = forest.nodes.iter().map(|n| n.depth).collect();
        assert!(depths.contains(&0) && depths.contains(&1), "{depths:?}");
        assert_eq!(forest.roots, 1, "the dot hangs inside the ring's hole");

        let (merged, mstats) = refine_groups_with_stats(groups, &mask, &enabled());
        assert_eq!(merged.len(), 1, "rule 4 merges a contained dot");
        assert_eq!(
            mstats.containment.holes, 1,
            "the hole survives the containment merge"
        );
    }

    #[test]
    fn merge_stays_in_budget_at_c1_scale() {
        let p = enabled();
        // C1's shape: 1024 icons in a 32×32 grid, 8 px gutters.
        let groups: Vec<IconGroup> = (0..32u32)
            .flat_map(|gy| (0..32u32).map(move |gx| box_body(gx * 32 + 4, gy * 32 + 4, 24, 24)))
            .collect();
        let t0 = std::time::Instant::now();
        let (out, stats) = refine_groups_with_stats(groups, &blank(4096, 4096), &p);
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        // 8 px gutters: rule 5's half-GAP gate (4.2) and rule 1's size factor
        // (equal heights) both refuse, so a neat grid never glues.
        assert_eq!(out.len(), 1024, "grid icons must not merge");
        assert_eq!(stats.gap, 8.4);
        assert!(ms < 250.0, "1024-icon refine must be cheap: {ms:.1} ms");
        eprintln!(
            "W8 evidence: refine on a 1024-icon 4096² sheet — {ms:.1} ms, merges={}, iterations={}, median_h={}, GAP={}",
            stats.merges, stats.iterations, stats.median_h, stats.gap
        );
    }
}
