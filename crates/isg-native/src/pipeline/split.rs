//! §3.4 **F2 — over-merge split** (W9).
//!
//! Runs after F1 on the components F1 could not fully separate: chamfer
//! distance transform → non-max-suppressed peaks as seeds → **Meyer's
//! marker-controlled watershed** on a binary heap → sliver rejection →
//! deterministic fallbacks when seeding disagrees with the bbox.
//!
//! Everything here is integer-ordered on purpose. The distance transform is
//! quantised to thousandths of a pixel (`i32`), so seeds, heap pops and
//! comparisons never depend on float comparison order; every tie breaks on
//! scan index. A split stands only when ≥ 2 regions survive sliver rejection.
//!
//! Only components whose bbox area exceeds `(2.2 × median_h)²` are considered,
//! and a component whose inscribed radius exceeds `0.75 × median_h` is never
//! split — that is one large icon, not a merge (ARCHITECTURE.md §3.4 F2).

use std::collections::{BinaryHeap, HashMap};

use isg_core::{Bbox, ForegroundMask, IconGroup};

use super::group::sort_groups;

/// Bumped whenever split behaviour changes.
pub const SPLIT_VERSION: u32 = 1;

/// Quantisation of the chamfer distance to thousandths of a pixel.
const DT_SCALE: i32 = 1000;

/// 8-neighbourhood, in a fixed order so flooding is reproducible.
const NEIGH8: [(i64, i64); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// Tunables for the W9 split stage. Defaults are the ARCHITECTURE.md §3.4
/// values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitParams {
    /// Stage switch (the master switch is [`super::RefineParams::enabled`]).
    pub enabled: bool,
    /// Size gate: bbox area must exceed `(size_mult × median_h)²`.
    pub size_mult: f32,
    /// Non-maximum-suppression radius, as a fraction of `median_h`.
    pub seed_radius_frac: f32,
    /// Seed peaks must be at least this fraction of `median_h` away from
    /// background.
    pub seed_min_dt_frac: f32,
    /// Fat-component guard: skip when the inscribed radius exceeds this
    /// fraction of `median_h`.
    pub max_dt_frac: f32,
    /// Regions below this fraction of the expected region area are reattached.
    pub sliver_frac: f32,
    /// Hard cap on seeds per component (runaway-seeding backstop).
    pub max_seeds: u32,
    /// Enable the two deterministic fallbacks.
    pub fallback_enabled: bool,
}

impl Default for SplitParams {
    fn default() -> Self {
        Self {
            enabled: true,
            size_mult: 2.2,
            seed_radius_frac: 0.6,
            seed_min_dt_frac: 0.3,
            max_dt_frac: 0.75,
            sliver_frac: 0.10,
            max_seeds: 64,
            fallback_enabled: true,
        }
    }
}

/// Evidence counters for the split stage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SplitStats {
    /// Components that passed the size gate.
    pub candidates: u32,
    /// Components actually replaced by ≥ 2 regions.
    pub split: u32,
    /// Regions produced across all splits.
    pub regions: u32,
    /// Sliver regions reattached to a neighbour.
    pub slivers: u32,
    /// Under-seeded components handled by the profile-cut fallback.
    pub fallback_profile: u32,
    /// Over-seeded components handled by the reseed fallback.
    pub fallback_reseed: u32,
    /// Components left alone by the fat guard.
    pub skipped_fat: u32,
    /// Components that produced no usable structure (kept whole).
    pub skipped_no_structure: u32,
}

/// F2 over all groups. Non-candidates pass through untouched.
pub(crate) fn split_overmerged(
    groups: Vec<IconGroup>,
    mask: &ForegroundMask,
    median_h: f32,
    params: &SplitParams,
    stats: &mut SplitStats,
) -> Vec<IconGroup> {
    if !params.enabled || median_h <= 0.0 {
        return groups;
    }
    let gate_area = f64::from(params.size_mult * median_h).powi(2) as u64;
    let mut out: Vec<IconGroup> = Vec::with_capacity(groups.len());
    for g in groups {
        if g.bbox.area() <= gate_area {
            out.push(g);
            continue;
        }
        stats.candidates += 1;
        match try_split(&g, mask, median_h, params, stats) {
            Some(regions) => {
                stats.split += 1;
                stats.regions += regions.len() as u32;
                out.extend(regions);
            }
            None => {
                stats.skipped_no_structure += 1;
                out.push(g);
            }
        }
    }
    sort_groups(&mut out);
    out
}

/// Attempts to split one component; `None` keeps it whole.
fn try_split(
    g: &IconGroup,
    mask: &ForegroundMask,
    median_h: f32,
    p: &SplitParams,
    stats: &mut SplitStats,
) -> Option<Vec<IconGroup>> {
    let cw = g.bbox.w as usize;
    let ch = g.bbox.h as usize;
    if cw == 0 || ch == 0 {
        return None;
    }
    let comp = extract_component(mask, g);
    let area: u32 = comp.iter().filter(|c| **c).count() as u32;
    if area == 0 {
        return None;
    }
    let dtq = distance_transform(cw, ch, &comp);
    let dt_max = comp
        .iter()
        .enumerate()
        .filter(|(_, c)| **c)
        .map(|(i, _)| dtq[i])
        .max()
        .unwrap_or(0);
    // F2 spec: a thick component is one large icon, not a merge.
    if dt_max as f32 > p.max_dt_frac * median_h * DT_SCALE as f32 {
        stats.skipped_fat += 1;
        return None;
    }

    let radius = (p.seed_radius_frac * median_h).max(1.0);
    let min_dtq = (p.seed_min_dt_frac * median_h * DT_SCALE as f32) as i32;
    let seeds = nms_seeds(&comp, &dtq, cw, radius, min_dtq, p.max_seeds);

    // How many median-sized icons fit in this component's bbox — the
    // "expected cell count" that seeding is compared against.
    let expected_cells = {
        let raw = g.bbox.area() as f32 / (median_h * median_h);
        (raw.round() as u32).clamp(1, p.max_seeds)
    };

    let labels = if p.fallback_enabled && seeds.len() < 2 && expected_cells >= 2 {
        // Under-seeded: the bbox implies several icons but the transform found
        // no second peak — cut at the lowest-ink positions of the long axis.
        stats.fallback_profile += 1;
        profile_cut(&comp, cw, ch, expected_cells as usize)
    } else {
        let mut seeds = seeds;
        if p.fallback_enabled && seeds.len() as u32 > expected_cells {
            // Over-seeded: keep the strongest `expected_cells` peaks (the list
            // is already ordered by peak strength, then scan index).
            seeds.truncate(expected_cells as usize);
            stats.fallback_reseed += 1;
        }
        if seeds.len() < 2 {
            return None;
        }
        watershed(&comp, &dtq, cw, ch, &seeds)
    };

    let (labels, slivers) = reattach_slivers(&comp, cw, ch, labels, p.sliver_frac);
    stats.slivers += slivers;
    let regions = regions_to_groups(&comp, &labels, &g.bbox);
    if regions.len() < 2 {
        return None;
    }
    Some(regions)
}

/// The component's own pixels, as a crop-sized bitmap. Extracted by breadth-
/// first traversal from the group origin, so a bbox shared with another
/// component can never leak pixels into the split.
fn extract_component(mask: &ForegroundMask, g: &IconGroup) -> Vec<bool> {
    let cw = g.bbox.w as usize;
    let ch = g.bbox.h as usize;
    let mut comp = vec![false; cw * ch];
    let (mut sx, mut sy) = g.origin;
    if !mask.get(sx, sy) {
        // Defensive: the origin is always a member, but never trust it blindly.
        'find: {
            for y in g.bbox.y..g.bbox.y + g.bbox.h {
                for x in g.bbox.x..g.bbox.x + g.bbox.w {
                    if mask.get(x, y) {
                        sx = x;
                        sy = y;
                        break 'find;
                    }
                }
            }
            return comp;
        }
    }
    let mut stack = vec![(sx, sy)];
    comp[(sy - g.bbox.y) as usize * cw + (sx - g.bbox.x) as usize] = true;
    while let Some((x, y)) = stack.pop() {
        for (dx, dy) in NEIGH8 {
            let nx = x as i64 + dx;
            let ny = y as i64 + dy;
            if nx < g.bbox.x as i64
                || ny < g.bbox.y as i64
                || nx >= (g.bbox.x + g.bbox.w) as i64
                || ny >= (g.bbox.y + g.bbox.h) as i64
            {
                continue;
            }
            let (lx, ly) = (
                (nx - g.bbox.x as i64) as usize,
                (ny - g.bbox.y as i64) as usize,
            );
            if comp[ly * cw + lx] || !mask.get(nx as u32, ny as u32) {
                continue;
            }
            comp[ly * cw + lx] = true;
            stack.push((nx as u32, ny as u32));
        }
    }
    comp
}

/// Two-pass chamfer distance to the nearest non-component pixel, quantised.
///
/// The crop is the component's *tight* bbox, so everything outside it is
/// background by definition: the transform runs on a 1-pixel zero border and
/// the inner region is returned. Without that border, edge pixels would
/// measure their distance against in-crop air only and report values far
/// larger than the truth (which is 1), tripping the fat-component guard.
fn distance_transform(cw: usize, ch: usize, comp: &[bool]) -> Vec<i32> {
    const INF: i32 = i32::MAX / 4;
    const ORTH: i32 = DT_SCALE;
    const DIAG: i32 = 1414; // √2 ≈ 1.414
    let pw = cw + 2;
    let ph = ch + 2;
    let mut padded = vec![0i32; pw * ph];
    for (i, c) in comp.iter().enumerate() {
        if *c {
            padded[(i / cw + 1) * pw + (i % cw + 1)] = INF;
        }
    }
    {
        let dt = &mut padded;
        let at = |x: usize, y: usize| y * pw + x;
        for y in 0..ph {
            for x in 0..pw {
                let i = at(x, y);
                if dt[i] == 0 {
                    continue;
                }
                let mut best = dt[i];
                if y > 0 {
                    best = best.min(dt[at(x, y - 1)].saturating_add(ORTH));
                    if x > 0 {
                        best = best.min(dt[at(x - 1, y - 1)].saturating_add(DIAG));
                    }
                    if x + 1 < pw {
                        best = best.min(dt[at(x + 1, y - 1)].saturating_add(DIAG));
                    }
                }
                if x > 0 {
                    best = best.min(dt[at(x - 1, y)].saturating_add(ORTH));
                }
                dt[i] = best;
            }
        }
        for y in (0..ph).rev() {
            for x in (0..pw).rev() {
                let i = at(x, y);
                if dt[i] == 0 {
                    continue;
                }
                let mut best = dt[i];
                if y + 1 < ph {
                    best = best.min(dt[at(x, y + 1)].saturating_add(ORTH));
                    if x > 0 {
                        best = best.min(dt[at(x - 1, y + 1)].saturating_add(DIAG));
                    }
                    if x + 1 < pw {
                        best = best.min(dt[at(x + 1, y + 1)].saturating_add(DIAG));
                    }
                }
                if x + 1 < pw {
                    best = best.min(dt[at(x + 1, y)].saturating_add(ORTH));
                }
                dt[i] = best;
            }
        }
    }
    let mut out = vec![0i32; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            out[y * cw + x] = padded[(y + 1) * pw + (x + 1)];
        }
    }
    out
}

/// Greedy non-maximum suppression over peak strength: strongest first, ties by
/// scan index, accepting only peaks at least `radius` away from every accepted
/// seed.
fn nms_seeds(
    comp: &[bool],
    dtq: &[i32],
    cw: usize,
    radius: f32,
    min_dtq: i32,
    max_seeds: u32,
) -> Vec<u32> {
    let mut cand: Vec<(i32, u32)> = comp
        .iter()
        .enumerate()
        .filter(|(i, c)| **c && dtq[*i] >= min_dtq)
        .map(|(i, _)| (dtq[i], i as u32))
        .collect();
    cand.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let r2 = radius * radius;
    let mut seeds: Vec<u32> = Vec::new();
    let mut taken: Vec<(i64, i64)> = Vec::new();
    for (_, idx) in cand {
        let (x, y) = ((idx as usize % cw) as i64, (idx as usize / cw) as i64);
        let clear = taken.iter().all(|&(px, py)| {
            let dx = x - px;
            let dy = y - py;
            (dx * dx + dy * dy) as f32 > r2
        });
        if clear {
            seeds.push(idx);
            taken.push((x, y));
            if seeds.len() as u32 >= max_seeds {
                break;
            }
        }
    }
    seeds
}

/// Meyer's marker-controlled watershed by priority flooding: immerge from the
/// seeds, always expanding the pixel with the largest distance value; ties pop
/// the lowest scan index first, so the result is fully reproducible.
fn watershed(comp: &[bool], dtq: &[i32], cw: usize, ch: usize, seeds: &[u32]) -> Vec<i32> {
    let n = cw * ch;
    let mut labels = vec![-1i32; n];
    let mut heap: BinaryHeap<(i32, std::cmp::Reverse<u32>)> = BinaryHeap::new();
    for (id, &s) in seeds.iter().enumerate() {
        labels[s as usize] = id as i32;
        heap.push((dtq[s as usize], std::cmp::Reverse(s)));
    }
    while let Some((_, std::cmp::Reverse(idx))) = heap.pop() {
        let (x, y) = ((idx as usize % cw) as i64, (idx as usize / cw) as i64);
        for (dx, dy) in NEIGH8 {
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= cw as i64 || ny >= ch as i64 {
                continue;
            }
            let ni = ny as usize * cw + nx as usize;
            if !comp[ni] || labels[ni] != -1 {
                continue;
            }
            labels[ni] = labels[idx as usize];
            heap.push((dtq[ni], std::cmp::Reverse(ni as u32)));
        }
    }
    labels
}

/// Reattaches sliver regions (< `sliver_frac` of the expected region area) to
/// the neighbour they share the longest boundary with; ties go to the lowest
/// label. Repeats until stable, so a sliver can be absorbed into another
/// sliver and together clear the threshold.
fn reattach_slivers(
    comp: &[bool],
    cw: usize,
    ch: usize,
    mut labels: Vec<i32>,
    sliver_frac: f32,
) -> (Vec<i32>, u32) {
    let total: u32 = comp.iter().filter(|c| **c).count() as u32;
    let mut reattached = 0u32;
    for _ in 0..8 {
        let mut areas: HashMap<i32, u32> = HashMap::new();
        for (i, c) in comp.iter().enumerate() {
            if *c && labels[i] >= 0 {
                *areas.entry(labels[i]).or_insert(0) += 1;
            }
        }
        if areas.len() < 2 {
            break;
        }
        let limit = sliver_frac * (total as f32 / areas.len() as f32);
        let mut slivers: Vec<i32> = areas
            .iter()
            .filter(|(_, &a)| (a as f32) < limit)
            .map(|(&l, _)| l)
            .collect();
        slivers.sort_unstable();
        if slivers.is_empty() {
            break;
        }
        for lab in slivers {
            // Count the shared boundary with every neighbouring region.
            let mut contacts: HashMap<i32, u32> = HashMap::new();
            for i in 0..cw * ch {
                if !comp[i] || labels[i] != lab {
                    continue;
                }
                let (x, y) = ((i % cw) as i64, (i / cw) as i64);
                for (dx, dy) in NEIGH8 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= cw as i64 || ny >= ch as i64 {
                        continue;
                    }
                    let nl = labels[ny as usize * cw + nx as usize];
                    if nl >= 0 && nl != lab {
                        *contacts.entry(nl).or_insert(0) += 1;
                    }
                }
            }
            // Longest border wins; ties break on the lowest label.
            let target = contacts
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(&l, _)| l);
            if let Some(target) = target {
                for i in 0..cw * ch {
                    if comp[i] && labels[i] == lab {
                        labels[i] = target;
                    }
                }
                reattached += 1;
            }
        }
    }
    (labels, reattached)
}

/// Under-seeded fallback: cut the component into `k` bands along its long
/// axis, placing each cut at the lowest-ink column/row of the admissible
/// range (ties → lowest coordinate), with a minimum band width of 2 px.
fn profile_cut(comp: &[bool], cw: usize, ch: usize, k: usize) -> Vec<i32> {
    let n = cw * ch;
    let mut labels = vec![-1i32; n];
    if k < 2 {
        return labels;
    }
    let horizontal = cw >= ch;
    let len = if horizontal { cw } else { ch };
    let mut profile = vec![0u32; len];
    for y in 0..ch {
        for x in 0..cw {
            if comp[y * cw + x] {
                profile[if horizontal { x } else { y }] += 1;
            }
        }
    }
    let min_band = 2usize;
    let mut cuts: Vec<usize> = Vec::with_capacity(k - 1);
    for i in 0..k - 1 {
        let lo = cuts.last().map(|c| c + min_band).unwrap_or(min_band);
        let hi = len.saturating_sub((k - 1 - i) * min_band);
        if lo >= hi {
            break;
        }
        let mut best = lo;
        let mut best_val = profile[lo];
        for (off, &v) in profile[lo..hi].iter().enumerate() {
            if v < best_val {
                best_val = v;
                best = lo + off;
            }
        }
        cuts.push(best);
    }
    let bands = cuts.len() + 1;
    for y in 0..ch {
        for x in 0..cw {
            if !comp[y * cw + x] {
                continue;
            }
            let coord = if horizontal { x } else { y };
            let band = cuts.iter().filter(|&&c| c <= coord).count();
            labels[y * cw + x] = band.min(bands - 1) as i32;
        }
    }
    labels
}

/// Turns a label map into groups (tight bbox, ink area, scan-order origin).
fn regions_to_groups(comp: &[bool], labels: &[i32], off: &Bbox) -> Vec<IconGroup> {
    struct Acc {
        minx: u32,
        miny: u32,
        maxx: u32,
        maxy: u32,
        area: u32,
        origin: (u32, u32),
    }
    let cw = off.w as usize;
    let mut acc: HashMap<i32, Acc> = HashMap::new();
    for (i, c) in comp.iter().enumerate() {
        if !*c || labels[i] < 0 {
            continue;
        }
        let x = off.x + (i % cw) as u32;
        let y = off.y + (i / cw) as u32;
        match acc.get_mut(&labels[i]) {
            Some(a) => {
                a.minx = a.minx.min(x);
                a.miny = a.miny.min(y);
                a.maxx = a.maxx.max(x);
                a.maxy = a.maxy.max(y);
                a.area += 1;
            }
            None => {
                acc.insert(
                    labels[i],
                    Acc {
                        minx: x,
                        miny: y,
                        maxx: x,
                        maxy: y,
                        area: 1,
                        origin: (x, y),
                    },
                );
            }
        }
    }
    let mut out: Vec<IconGroup> = acc
        .into_values()
        .map(|a| IconGroup {
            bbox: Bbox::from_parts(a.minx, a.miny, a.maxx - a.minx + 1, a.maxy - a.miny + 1),
            area: a.area,
            origin: a.origin,
        })
        .collect();
    sort_groups(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_on() -> SplitParams {
        SplitParams::default()
    }

    fn params_off() -> SplitParams {
        SplitParams {
            enabled: false,
            ..SplitParams::default()
        }
    }

    /// Fills a rectangle on the mask and returns the matching group (as CCL
    /// would produce it).
    fn rect(mask: &mut ForegroundMask, x: u32, y: u32, w: u32, h: u32) -> IconGroup {
        for yy in y..y + h {
            for xx in x..x + w {
                mask.set(xx, yy, true);
            }
        }
        IconGroup {
            bbox: Bbox::from_parts(x, y, w, h),
            area: w * h,
            origin: (x, y),
        }
    }

    fn split_of(
        groups: Vec<IconGroup>,
        mask: &ForegroundMask,
        median_h: f32,
    ) -> (Vec<IconGroup>, SplitStats) {
        let mut stats = SplitStats::default();
        let out = split_overmerged(groups, mask, median_h, &params_on(), &mut stats);
        (out, stats)
    }

    #[test]
    fn disabled_is_identity() {
        let mut mask = ForegroundMask::new(256, 256);
        let g = rect(&mut mask, 0, 0, 86, 86);
        let mut stats = SplitStats::default();
        let out = split_overmerged(vec![g], &mask, 20.0, &params_off(), &mut stats);
        assert_eq!(out, vec![g]);
        assert_eq!(stats, SplitStats::default(), "no counters move when off");
    }

    #[test]
    fn split_candidates_are_size_gated() {
        let mut mask = ForegroundMask::new(256, 256);
        // 44×44 = 1936 = exactly (2.2 × 20)² → not "exceeds", so not a
        // candidate, and the fat guard would reject it anyway.
        let g = rect(&mut mask, 0, 0, 44, 44);
        let (out, stats) = split_of(vec![g], &mask, 20.0);
        assert_eq!(stats.candidates, 0, "bbox area must *exceed* the gate");
        assert_eq!(out, vec![g]);

        // 4×4 cells of 20×20 joined by 2-px bridges: bbox 86×86 = 7396 > 1936.
        let mut mask = ForegroundMask::new(256, 256);
        let g = glued_grid(&mut mask, 0, 0, 4, 4, 20, 2);
        let (out, stats) = split_of(vec![g], &mask, 20.0);
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.split, 1);
        assert_eq!(out.len(), 16, "one region per glued cell");
    }

    /// Builds a `cols × rows` grid of `cell × cell` squares joined by `bridge`
    /// px-wide connectors, as one CCL component.
    fn glued_grid(
        mask: &mut ForegroundMask,
        ox: u32,
        oy: u32,
        cols: u32,
        rows: u32,
        cell: u32,
        bridge: u32,
    ) -> IconGroup {
        for r in 0..rows {
            for c in 0..cols {
                let x = ox + c * (cell + bridge);
                let y = oy + r * (cell + bridge);
                for yy in y..y + cell {
                    for xx in x..x + cell {
                        mask.set(xx, yy, true);
                    }
                }
                if c + 1 < cols {
                    for yy in y + cell / 3..y + 2 * cell / 3 {
                        for xx in x + cell..x + cell + bridge {
                            mask.set(xx, yy, true);
                        }
                    }
                }
                if r + 1 < rows {
                    for xx in x + cell / 3..x + 2 * cell / 3 {
                        for yy in y + cell..y + cell + bridge {
                            mask.set(xx, yy, true);
                        }
                    }
                }
            }
        }
        let w = cols * (cell + bridge) - bridge;
        let h = rows * (cell + bridge) - bridge;
        IconGroup {
            bbox: Bbox::from_parts(ox, oy, w, h),
            area: 0, // not used by the splitter
            origin: (ox, oy),
        }
    }

    #[test]
    fn splits_a_glued_grid_into_cells() {
        let mut mask = ForegroundMask::new(256, 256);
        let g = glued_grid(&mut mask, 0, 0, 4, 4, 20, 2);
        let (out, stats) = split_of(vec![g], &mask, 20.0);
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.split, 1);
        assert_eq!(stats.fallback_profile, 0, "NMS found the structure");
        assert_eq!(stats.fallback_reseed, 0, "16 seeds ≤ 18 expected cells");
        assert_eq!(out.len(), 16, "one region per cell: {out:?}");
        // Every region is cell-sized (bridges may add a pixel or two).
        for g in &out {
            assert!(
                (19..=22).contains(&g.bbox.w) && (19..=22).contains(&g.bbox.h),
                "cell-sized region expected, got {:?}",
                g.bbox
            );
            assert!((380..=460).contains(&g.area), "area {}", g.area);
        }
        // Ink is conserved: regions partition the component.
        let total: u32 = out.iter().map(|g| g.area).sum();
        let component: u32 = mask.runs().iter().map(|r| r.len()).sum();
        assert_eq!(total, component, "split must conserve ink");
        // Deterministic: the same input yields the identical vector.
        let mut mask2 = ForegroundMask::new(256, 256);
        let g2 = glued_grid(&mut mask2, 0, 0, 4, 4, 20, 2);
        let (again, _) = split_of(vec![g2], &mask2, 20.0);
        assert_eq!(again, out);
    }

    #[test]
    fn fat_component_is_never_split() {
        // A solid 60×60 block: bbox 3600 > 1936, but its inscribed radius is
        // 30 > 0.75 × 20 — one large icon, not a merge.
        let mut mask = ForegroundMask::new(256, 256);
        let g = rect(&mut mask, 0, 0, 60, 60);
        let (out, stats) = split_of(vec![g], &mask, 20.0);
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.skipped_fat, 1);
        assert_eq!(stats.split, 0);
        assert_eq!(out, vec![g], "fat component passes through unchanged");
    }

    #[test]
    fn under_seeding_falls_back_to_profile_cuts() {
        // A 20×20 lobe (dt_max 10 ≤ 15, plateau ~9×9 so NMS radius 12 admits
        // exactly one seed) plus an 80×6 tail (dt_max 3 < the 0.3 × 20 seed
        // floor ⇒ no seed). bbox 100×20 = 2000 > (2.2 × 20)² = 1936, so it is
        // a candidate whose seeding disagrees with its extent: fewer than two
        // seeds while round(2000 / 400) = 5 median icons fit in the bbox.
        let mut mask = ForegroundMask::new(256, 256);
        let mut g = rect(&mut mask, 0, 0, 20, 20);
        let tail = rect(&mut mask, 20, 7, 80, 6);
        assert_eq!(tail.bbox.area(), 480);
        g.bbox = Bbox::from_parts(0, 0, 100, 20);
        g.area = 0;
        let (out, stats) = split_of(vec![g], &mask, 20.0);
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.fallback_profile, 1, "under-seeded ⇒ profile cuts");
        assert_eq!(stats.fallback_reseed, 0);
        // The thin bands the cut creates are then reattached by the sliver
        // rule (6 px of ink per band vs a 176 px mean), so the guarantee here
        // is: ≥ 2 regions, ink conserved, and the lobe survives intact.
        assert!(out.len() >= 2, "fallback must still split: {out:?}");
        let total: u32 = out.iter().map(|x| x.area).sum();
        let mask_ink: u32 = mask.runs().iter().map(|r| r.len()).sum();
        assert_eq!(mask_ink, 20 * 20 + 80 * 6);
        assert_eq!(total, mask_ink, "ink conserved");
        assert!(
            out.iter().any(|r| r.area >= 400 && r.bbox.h == 20),
            "the lobe keeps its own region: {out:?}"
        );
        assert_eq!(stats.slivers, 3, "the three thin bands are reattached");
    }

    #[test]
    fn profile_cut_places_bands_at_the_lowest_ink_valleys() {
        // 8 px tall row with 4 px wide, 1 px deep valleys starting at x = 20
        // and x = 40: cutting into 3 bands must put the cuts on the valleys'
        // lowest-ink columns, ties resolving to the lowest coordinate.
        let cw = 60;
        let ch = 8;
        let mut comp = vec![false; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                let in_valley = (20..24).contains(&x) || (40..44).contains(&x);
                comp[y * cw + x] = if in_valley { y == 3 } else { true };
            }
        }
        let labels = profile_cut(&comp, cw, ch, 3);
        // Read the component's own row (the valley columns are ink at y = 3).
        let row = 3 * cw;
        assert_eq!(labels[row + 19], 0, "x = 19 is in band 0");
        assert_eq!(labels[row + 20], 1, "first cut lands at x = 20");
        assert_eq!(labels[row + 21], 1);
        assert_eq!(labels[row + 22], 2, "second cut lands at x = 22");
        assert_eq!(labels[row + 59], 2);
        // Background pixels stay unlabelled, so nothing leaks into a region.
        assert_eq!(labels[20], -1, "non-component pixels keep label -1");
        let mut seen: Vec<i32> = labels
            .iter()
            .enumerate()
            .filter(|(i, _)| comp[*i])
            .map(|(_, l)| *l)
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, vec![0, 1, 2], "three non-empty bands");
    }

    #[test]
    fn over_seeding_falls_back_to_reseeding() {
        // Six 14×14 lobes (each a legitimate peak) in a 58×36 bbox joined by
        // 3-px bridges: 6 seeds > round(2088 / 400) = 5 expected cells, so the
        // reseed fallback trims to the 5 strongest.
        let mut mask = ForegroundMask::new(256, 256);
        let cells: [(u32, u32); 6] = [(0, 0), (22, 0), (44, 0), (0, 22), (22, 22), (44, 22)];
        for (x, y) in cells {
            for yy in y..y + 14 {
                for xx in x..x + 14 {
                    mask.set(xx, yy, true);
                }
            }
        }
        // Bridges: horizontal along y = 4..7 and y = 26..29, vertical at x = 4..7
        // and x = 26..29 and x = 48..51.
        for x in 14..22 {
            for y in 4..7 {
                mask.set(x, y, true);
                mask.set(x, y + 22, true);
            }
        }
        for x in 36..44 {
            for y in 4..7 {
                mask.set(x, y, true);
                mask.set(x, y + 22, true);
            }
        }
        for y in 14..22 {
            for x in 4..7 {
                mask.set(x, y, true);
                mask.set(x + 44, y, true);
            }
        }
        let g = IconGroup {
            bbox: Bbox::from_parts(0, 0, 58, 36),
            area: 0,
            origin: (0, 0),
        };
        let (out, stats) = split_of(vec![g], &mask, 20.0);
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.fallback_reseed, 1, "over-seeded ⇒ reseed fallback");
        assert_eq!(stats.fallback_profile, 0);
        assert_eq!(out.len(), 5, "trimmed to expected_cells: {out:?}");
        let total: u32 = out.iter().map(|x| x.area).sum();
        let component: u32 = mask.runs().iter().map(|r| r.len()).sum();
        // Every mask pixel belongs to the ring of six lobes + bridges, so this
        // also proves the extraction did not leak the stray component.
        assert_eq!(component, 6 * 14 * 14 + 3 * 8 * 3 * 2, "mask ink");
        assert_eq!(total, component, "ink conserved");
    }

    #[test]
    fn slivers_are_reattached_to_the_longest_border() {
        // White-box: a 10×10 component split into region 0 (x < 5 → 50 px),
        // region 1 (x 5..9, plus x = 9 below row 3 → 47 px) and a 3 px sliver
        // (x = 9, rows 0..3) whose only neighbour is region 1.
        // Mean region area is 100/3 = 33.3 ⇒ sliver limit 3.33 > 3.
        let cw = 10;
        let ch = 10;
        let comp = vec![true; cw * ch];
        let mut labels = vec![-1i32; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                labels[y * cw + x] = if x < 5 {
                    0
                } else if x == 9 && y < 3 {
                    2
                } else {
                    1
                };
            }
        }
        let (out, reattached) = reattach_slivers(&comp, cw, ch, labels, 0.10);
        assert_eq!(reattached, 1, "exactly one sliver is absorbed");
        let mut areas: HashMap<i32, u32> = HashMap::new();
        for (i, c) in comp.iter().enumerate() {
            if *c && out[i] >= 0 {
                *areas.entry(out[i]).or_insert(0) += 1;
            }
        }
        assert_eq!(areas.len(), 2, "the sliver label is gone: {areas:?}");
        assert_eq!(areas.get(&0), Some(&50), "region 0 untouched");
        assert_eq!(areas.get(&1), Some(&50), "region 1 absorbed the sliver");
        assert_eq!(
            areas.values().sum::<u32>(),
            100,
            "every pixel keeps a label"
        );
    }

    #[test]
    fn split_is_deterministic_across_threads() {
        let masks: Vec<(ForegroundMask, IconGroup)> = (0..4)
            .map(|i| {
                let mut m = ForegroundMask::new(256, 256);
                let g = glued_grid(&mut m, 0, 0, 3 + i, 3, 20, 2);
                (m, g)
            })
            .collect();
        let expected: Vec<Vec<IconGroup>> = masks
            .iter()
            .map(|(m, g)| split_of(vec![*g], m, 20.0).0)
            .collect();
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        masks
                            .iter()
                            .map(|(m, g)| split_of(vec![*g], m, 20.0).0)
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for h in handles {
                assert_eq!(h.join().expect("worker"), expected);
            }
        });
    }

    #[test]
    fn split_stays_in_budget_at_c1_scale() {
        // C1-shaped sheet: 1024 icons on a 128 px pitch, of which 16 are 3×3
        // glued blobs (bbox 76×76 = 5776 > (2.2 × 24)² = 2787) and the rest
        // are clean 24×24 icons. Exactly every 64th cell is a blob.
        let mut mask = ForegroundMask::new(4096, 4096);
        let mut groups = Vec::new();
        let mut blobs = 0u32;
        for idx in 0..1024u32 {
            let gx = idx % 32;
            let gy = idx / 32;
            let x = gx * 128;
            let y = gy * 128;
            if idx % 64 == 0 {
                groups.push(glued_grid(&mut mask, x, y, 3, 3, 24, 2));
                blobs += 1;
            } else {
                groups.push(rect(&mut mask, x, y, 24, 24));
            }
        }
        assert_eq!(blobs, 16);
        let t0 = std::time::Instant::now();
        let (out, stats) = split_of(groups, &mask, 24.0);
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        assert_eq!(stats.candidates, 16, "only the glued blobs are candidates");
        assert_eq!(stats.split, 16);
        assert_eq!(stats.regions, 16 * 9);
        assert_eq!(out.len(), 1008 + 16 * 9, "each blob yields 9 regions");
        let total: u32 = out.iter().map(|g| g.area).sum();
        let mask_ink: u32 = mask.runs().iter().map(|r| r.len()).sum();
        assert_eq!(total, mask_ink, "ink conserved at sheet scale");
        assert!(ms < 250.0, "split pass must stay cheap: {ms:.1} ms");
        eprintln!(
            "W9 evidence: split_overmerged on a 4096²/1040-component sheet — {ms:.1} ms, candidates={}, split={}, regions={}, slivers={}, median_h=24",
            stats.candidates, stats.split, stats.regions, stats.slivers
        );
    }
}
