//! §3.4 **F3 — holes / containment forest** (W10).
//!
//! Aggressive merging and splitting can produce a component inside another
//! component's *hole* (a dot in the middle of an “O”), and the UI needs to
//! know a ring is **one** icon rather than two. This stage answers that with a
//! containment forest over the group list:
//!
//! * candidates come from an **x-interval sweep** (`bbox_contains`, smallest
//!   candidate area wins), so the expensive pixel test only runs on pairs whose
//!   boxes nest at all;
//! * every candidate is then **verified against the mask** — the background
//!   around the inner group must be walled off from the outer group's bbox
//!   border, i.e. the inner group really sits in a *hole*, not merely inside a
//!   bounding box (two diagonal neighbours share bbox area all the time);
//! * depth is assigned in area-DESC order, which yields the §3.4 even-odd
//!   parity: `depth 0` = body, the enclosing `hole` level is `2·depth − 1`, and
//!   an inner group at `2·depth` — a ring or an “O” stays **one** icon because
//!   holes are background and never become groups.
//!
//! Determinism: every list is sorted (`area` DESC, then scan order) and every
//! tie breaks on the lower index, so the forest is identical across runs and
//! thread counts.

use isg_core::{Bbox, ForegroundMask, IconGroup};

/// Bumped whenever containment semantics change (confidence/audit trail).
pub const CONTAINMENT_VERSION: u32 = 1;

/// Tunables for F3. Defaults are the ARCHITECTURE.md §3.4 behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContainmentParams {
    /// Stage switch.
    pub enabled: bool,
    /// Defensive nesting bound — deeper chains are clamped, never recursed.
    pub max_depth: u32,
}

impl Default for ContainmentParams {
    fn default() -> Self {
        Self {
            enabled: true,
            max_depth: 8,
        }
    }
}

/// Counters for the evidence log and the confidence score (W11).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ContainmentStats {
    /// Pairs whose boxes nested and reached the pixel test.
    pub pairs_tested: u32,
    /// Pairs verified as true enclosures.
    pub contained: u32,
    /// Pairs whose boxes nested but whose background leaks to the outside
    /// (diagonal neighbours, open rings, touching frames).
    pub rejected_open: u32,
    /// Hole regions found across all groups.
    pub holes: u32,
    /// Deepest nesting reached.
    pub max_depth: u32,
    /// Wall-clock, milliseconds.
    pub elapsed_ms: f32,
}

/// Per-group containment facts, parallel to the group slice handed in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Containment {
    /// Index of the enclosing group (into the same slice), if any.
    pub parent: Option<u32>,
    /// Number of enclosing icons: 0 = top level, 1 = inside one icon's hole.
    /// The group's ink sits at even-odd level `2 · depth`; the hole that
    /// encloses it is level `2 · depth − 1`.
    pub depth: u32,
    /// Background regions fully enclosed by this group's own ink.
    pub holes: u32,
}

/// The F3 result: one entry per input group plus the root count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainmentForest {
    /// Per-group facts, index-aligned with the input slice.
    pub nodes: Vec<Containment>,
    /// Groups at depth 0 (not inside any other group's hole).
    pub roots: u32,
}

/// Builds the containment forest for `groups` (usually the post-split list).
///
/// Runs in `O(n log n)` for the sweep plus one bounded flood fill per nested
/// candidate. `stats` is additive, so callers may accumulate across sheets.
#[must_use]
pub fn build_containment(
    groups: &[IconGroup],
    mask: &ForegroundMask,
    params: &ContainmentParams,
    stats: &mut ContainmentStats,
) -> ContainmentForest {
    let t0 = std::time::Instant::now();
    let n = groups.len();
    let mut nodes = vec![Containment::default(); n];
    if !params.enabled || n == 0 {
        stats.elapsed_ms += t0.elapsed().as_secs_f32() * 1000.0;
        return ContainmentForest {
            nodes,
            roots: n as u32,
        };
    }

    // Area-DESC, then scan order — parents are always seen before children.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        groups[b]
            .area
            .cmp(&groups[a].area)
            .then_with(|| groups[a].bbox.y.cmp(&groups[b].bbox.y))
            .then_with(|| groups[a].bbox.x.cmp(&groups[b].bbox.x))
            .then_with(|| groups[a].origin.cmp(&groups[b].origin))
            .then_with(|| a.cmp(&b))
    });

    // ---- x-interval sweep: collect nesting candidates -------------------
    // Queries walk the sheet left-to-right; `active` holds the groups whose
    // x-range can still cover a query starting here, pruned by `x_end`.
    let mut by_x: Vec<usize> = (0..n).collect();
    by_x.sort_by(|&a, &b| {
        groups[a]
            .bbox
            .x
            .cmp(&groups[b].bbox.x)
            .then_with(|| a.cmp(&b))
    });
    let mut candidates: Vec<Option<usize>> = vec![None; n];
    let mut candidate_area: Vec<u64> = vec![u64::MAX; n];
    let mut active: Vec<usize> = Vec::new();
    for &q in &by_x {
        active.retain(|&a| groups[a].bbox.x + groups[a].bbox.w > groups[q].bbox.x);
        let qb = groups[q].bbox;
        for &a in &active {
            if groups[a].area <= groups[q].area {
                continue; // must enclose strictly more ink
            }
            if !strictly_contains(groups[a].bbox, qb) {
                continue;
            }
            let key = u64::from(groups[a].area);
            if key < candidate_area[q] {
                candidate_area[q] = key;
                candidates[q] = Some(a);
            }
        }
        active.push(q);
        active.sort_unstable();
    }

    // ---- pixel verification + hole counting -----------------------------
    for q in 0..n {
        let Some(a) = candidates[q] else { continue };
        stats.pairs_tested += 1;
        let inner_px = extract_component(mask, groups[q].bbox, groups[q].origin);
        if verify_enclosure(groups[a].bbox, &groups[q], &inner_px, mask) {
            stats.contained += 1;
            nodes[q].parent = Some(a as u32);
        } else {
            stats.rejected_open += 1;
        }
    }

    // ---- depth by area-DESC (parents resolve first) ---------------------
    for &i in &order {
        let parent = nodes[i].parent;
        let depth = match parent {
            Some(p) => (nodes[p as usize].depth + 1).min(params.max_depth),
            None => 0,
        };
        nodes[i].depth = depth;
        stats.max_depth = stats.max_depth.max(depth);
    }
    // Holes per group: only for groups that lost nobody to the border, i.e.
    // genuinely closed outlines (a broken ring reports zero).
    for i in 0..n {
        let holes = count_holes(&groups[i], mask);
        nodes[i].holes = holes;
        stats.holes += holes;
    }
    let roots = nodes.iter().filter(|c| c.depth == 0).count() as u32;
    stats.elapsed_ms += t0.elapsed().as_secs_f32() * 1000.0;
    ContainmentForest { nodes, roots }
}

/// Strict bbox containment: `inner` inside `outer` with at least one side
/// strictly larger, so identical boxes never nest.
fn strictly_contains(outer: Bbox, inner: Bbox) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.x + outer.w >= inner.x + inner.w
        && outer.y + outer.h >= inner.y + inner.h
        && (outer.w > inner.w || outer.h > inner.h)
}

/// Crop of `outer`'s bbox holding the mask, plus the reachable background.
struct Crop {
    cw: usize,
    ch: usize,
    ink: Vec<bool>,
    reachable: Vec<bool>,
}

impl Crop {
    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.cw + x
    }
}

/// Builds `outer`'s crop and floods the background reachable from its border
/// (4-connected, the dual of the mask's 8-connected ink).
fn crop_with_reachable(outer: Bbox, mask: &ForegroundMask) -> Crop {
    let cw = outer.w as usize;
    let ch = outer.h as usize;
    let mut crop = Crop {
        cw,
        ch,
        ink: vec![false; cw * ch],
        reachable: vec![false; cw * ch],
    };
    for y in 0..ch {
        for x in 0..cw {
            let i = crop.idx(x, y);
            crop.ink[i] = mask.get(outer.x + x as u32, outer.y + y as u32);
        }
    }
    let mut stack: Vec<usize> = Vec::new();
    let push = |crop: &mut Crop, stack: &mut Vec<usize>, x: usize, y: usize| {
        let i = crop.idx(x, y);
        if !crop.ink[i] && !crop.reachable[i] {
            crop.reachable[i] = true;
            stack.push(i);
        }
    };
    for x in 0..cw {
        push(&mut crop, &mut stack, x, 0);
        push(&mut crop, &mut stack, x, ch - 1);
    }
    for y in 0..ch {
        push(&mut crop, &mut stack, 0, y);
        push(&mut crop, &mut stack, cw - 1, y);
    }
    while let Some(i) = stack.pop() {
        let x = i % cw;
        let y = i / cw;
        if x > 0 {
            push(&mut crop, &mut stack, x - 1, y);
        }
        if x + 1 < cw {
            push(&mut crop, &mut stack, x + 1, y);
        }
        if y > 0 {
            push(&mut crop, &mut stack, x, y - 1);
        }
        if y + 1 < ch {
            push(&mut crop, &mut stack, x, y + 1);
        }
    }
    crop
}

/// The group's own pixels, as a crop-sized bitmap — breadth-first from the
/// group origin, so a bbox shared with a neighbour can never leak ink in.
fn extract_component(mask: &ForegroundMask, bbox: Bbox, origin: (u32, u32)) -> Vec<bool> {
    let cw = bbox.w as usize;
    let ch = bbox.h as usize;
    let mut comp = vec![false; cw * ch];
    if !mask.get(origin.0, origin.1) {
        return comp;
    }
    let mut stack = vec![(origin.0, origin.1)];
    comp[(origin.1 - bbox.y) as usize * cw + (origin.0 - bbox.x) as usize] = true;
    while let Some((x, y)) = stack.pop() {
        let visit = |nx: u32, ny: u32, comp: &mut Vec<bool>, stack: &mut Vec<(u32, u32)>| {
            if nx < bbox.x || ny < bbox.y || nx >= bbox.x + bbox.w || ny >= bbox.y + bbox.h {
                return;
            }
            let i = (ny - bbox.y) as usize * cw + (nx - bbox.x) as usize;
            if !comp[i] && mask.get(nx, ny) {
                comp[i] = true;
                stack.push((nx, ny));
            }
        };
        if x > 0 {
            visit(x - 1, y, &mut comp, &mut stack);
        }
        if x + 1 < mask.width() {
            visit(x + 1, y, &mut comp, &mut stack);
        }
        if y > 0 {
            visit(x, y - 1, &mut comp, &mut stack);
        }
        if y + 1 < mask.height() {
            visit(x, y + 1, &mut comp, &mut stack);
        }
    }
    comp
}

/// True when every ink pixel of `inner` is separated from `outer`'s border by
/// ink — i.e. `inner` sits in a hole of `outer`.
fn verify_enclosure(
    outer: Bbox,
    inner: &IconGroup,
    inner_px: &[bool],
    mask: &ForegroundMask,
) -> bool {
    let crop = crop_with_reachable(outer, mask);
    let (x0, y0) = (
        (inner.bbox.x - outer.x) as usize,
        (inner.bbox.y - outer.y) as usize,
    );
    let inner_cw = inner.bbox.w as usize;
    let inner_ch = inner.bbox.h as usize;
    if x0 + inner_cw > crop.cw || y0 + inner_ch > crop.ch {
        return false;
    }
    let mut touches_outside = false;
    for dy in 0..inner_ch {
        for dx in 0..inner_cw {
            if !inner_px[dy * inner_cw + dx] {
                continue;
            }
            let x = x0 + dx;
            let y = y0 + dy;
            let mut open_neighbour = false;
            if x > 0 && crop.reachable[crop.idx(x - 1, y)] {
                open_neighbour = true;
            }
            if x + 1 < crop.cw && crop.reachable[crop.idx(x + 1, y)] {
                open_neighbour = true;
            }
            if y > 0 && crop.reachable[crop.idx(x, y - 1)] {
                open_neighbour = true;
            }
            if y + 1 < crop.ch && crop.reachable[crop.idx(x, y + 1)] {
                open_neighbour = true;
            }
            if open_neighbour {
                touches_outside = true;
                break;
            }
        }
        if touches_outside {
            break;
        }
    }
    !touches_outside
}

/// Holes of `own_px` inside `crop`: 4-connected background regions the border
/// flood never reached **and** that touch this group's own ink — a background
/// pocket bounded by a *child* group (nested rings) belongs to the child, not
/// to us, even though it sits inside our box.
fn count_own_holes(
    crop: &Crop,
    own_px: &[bool],
    own_off: (usize, usize),
    own_dims: (usize, usize),
) -> u32 {
    let mut seen = vec![false; crop.cw * crop.ch];
    let mut stack: Vec<usize> = Vec::new();
    let mut region: Vec<usize> = Vec::new();
    let mut holes = 0u32;
    let own_at = |x: usize, y: usize| -> bool {
        x >= own_off.0
            && y >= own_off.1
            && x < own_off.0 + own_dims.0
            && y < own_off.1 + own_dims.1
            && own_px[(y - own_off.1) * own_dims.0 + (x - own_off.0)]
    };
    for start in 0..crop.cw * crop.ch {
        if crop.ink[start] || crop.reachable[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        region.clear();
        while let Some(i) = stack.pop() {
            region.push(i);
            let x = i % crop.cw;
            let y = i / crop.cw;
            let visit = |x: usize, y: usize, seen: &mut Vec<bool>, stack: &mut Vec<usize>| {
                let j = y * crop.cw + x;
                if !crop.ink[j] && !crop.reachable[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(x - 1, y, &mut seen, &mut stack);
            }
            if x + 1 < crop.cw {
                visit(x + 1, y, &mut seen, &mut stack);
            }
            if y > 0 {
                visit(x, y - 1, &mut seen, &mut stack);
            }
            if y + 1 < crop.ch {
                visit(x, y + 1, &mut seen, &mut stack);
            }
        }
        let touches_own = region.iter().any(|&i| {
            let x = i % crop.cw;
            let y = i / crop.cw;
            (x > 0 && own_at(x - 1, y))
                || (x + 1 < crop.cw && own_at(x + 1, y))
                || (y > 0 && own_at(x, y - 1))
                || (y + 1 < crop.ch && own_at(x, y + 1))
        });
        if touches_own {
            holes += 1;
        }
    }
    holes
}

/// Holes of one group: background the border flood cannot reach *and* that is
/// bounded by this group's own ink.
fn count_holes(g: &IconGroup, mask: &ForegroundMask) -> u32 {
    if g.bbox.w == 0 || g.bbox.h == 0 {
        return 0;
    }
    let crop = crop_with_reachable(g.bbox, mask);
    let own = extract_component(mask, g.bbox, g.origin);
    let own_off = (
        (g.origin.0 - g.bbox.x) as usize,
        (g.origin.1 - g.bbox.y) as usize,
    );
    count_own_holes(&crop, &own, own_off, (g.bbox.w as usize, g.bbox.h as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ring: 24×24 outline with a 12×12 hole, `t` px thick.
    fn ring_mask(w: u32, h: u32, x: u32, y: u32, size: u32, t: u32) -> ForegroundMask {
        let mut m = ForegroundMask::new(w, h);
        for dy in 0..size {
            for dx in 0..size {
                let border = dx < t || dy < t || dx + t >= size || dy + t >= size;
                if border {
                    m.set(x + dx, y + dy, true);
                }
            }
        }
        m
    }

    fn group(x: u32, y: u32, w: u32, h: u32, area: u32) -> IconGroup {
        IconGroup {
            bbox: Bbox { x, y, w, h },
            area,
            origin: (x, y),
        }
    }

    fn stats() -> ContainmentStats {
        ContainmentStats::default()
    }

    #[test]
    fn ring_is_one_icon_with_one_hole() {
        let mask = ring_mask(64, 64, 10, 10, 40, 6);
        let groups = vec![group(10, 10, 40, 40, 40 * 40 - 28 * 28)];
        let mut st = stats();
        let forest = build_containment(&groups, &mask, &ContainmentParams::default(), &mut st);
        assert_eq!(forest.nodes.len(), 1);
        assert_eq!(forest.nodes[0].depth, 0, "a ring is a body, never a hole");
        assert_eq!(forest.nodes[0].holes, 1, "one enclosed background region");
        assert!(forest.nodes.iter().all(|c| c.parent.is_none()));
        assert_eq!(st.holes, 1);
        assert_eq!(st.rejected_open, 0);
    }

    #[test]
    fn dot_inside_a_ring_sits_at_depth_one() {
        let mut mask = ring_mask(64, 64, 10, 10, 40, 6);
        // 8×8 dot in the middle of the ring's hole.
        for dy in 0..8 {
            for dx in 0..8 {
                mask.set(26 + dx, 26 + dy, true);
            }
        }
        let ring = group(10, 10, 40, 40, 40 * 40 - 28 * 28);
        let dot = group(26, 26, 8, 8, 64);
        let mut st = stats();
        let forest = build_containment(&[ring, dot], &mask, &ContainmentParams::default(), &mut st);
        assert_eq!(forest.nodes[1].parent, Some(0));
        assert_eq!(
            forest.nodes[1].depth, 1,
            "inner icon = depth 1 (even-odd level 2)"
        );
        assert_eq!(forest.nodes[0].depth, 0);
        assert_eq!(forest.roots, 1);
        assert_eq!(st.contained, 1);
    }

    #[test]
    fn nested_rings_stack_depths() {
        // Ring inside ring: outer hole holds an inner ring, whose own hole
        // holds a dot — depths 0 / 1 / 2.
        let mut mask = ring_mask(96, 96, 4, 4, 88, 5);
        for dy in 0..60 {
            for dx in 0..60 {
                let border = dx < 5 || dy < 5 || dx + 5 >= 60 || dy + 5 >= 60;
                if border {
                    mask.set(18 + dx, 18 + dy, true);
                }
            }
        }
        for dy in 0..20 {
            for dx in 0..20 {
                mask.set(38 + dx, 38 + dy, true);
            }
        }
        let outer = group(4, 4, 88, 88, 88 * 88 - 78 * 78);
        let middle = group(18, 18, 60, 60, 60 * 60 - 50 * 50);
        let dot = group(38, 38, 20, 20, 400);
        let mut st = stats();
        let forest = build_containment(
            &[outer, middle, dot],
            &mask,
            &ContainmentParams::default(),
            &mut st,
        );
        assert_eq!(forest.nodes[0].depth, 0);
        assert_eq!(forest.nodes[1].depth, 1);
        assert_eq!(forest.nodes[2].depth, 2);
        assert_eq!(forest.roots, 1);
        assert_eq!(st.max_depth, 2);
        assert_eq!(forest.nodes[0].holes, 1);
        assert_eq!(forest.nodes[1].holes, 1);
    }

    #[test]
    fn nested_boxes_without_enclosure_are_rejected() {
        // Two diagonal icons whose boxes nest: the inner one's surroundings
        // reach the outer box's border, so it is NOT inside a hole.
        let mut mask = ForegroundMask::new(64, 64);
        for dy in 0..40 {
            for dx in 0..40 {
                if dx < 6 || dy < 6 {
                    mask.set(2 + dx, 2 + dy, true); // an L, open to the SE
                }
            }
        }
        for dy in 0..10 {
            for dx in 0..10 {
                mask.set(30 + dx, 30 + dy, true); // sits in the L's mouth
            }
        }
        let outer = group(2, 2, 40, 40, 40 * 40 - 34 * 34);
        let inner = group(30, 30, 10, 10, 100);
        let mut st = stats();
        let forest = build_containment(
            &[outer, inner],
            &mask,
            &ContainmentParams::default(),
            &mut st,
        );
        assert_eq!(
            forest.nodes[1].parent, None,
            "box nesting is not containment"
        );
        assert_eq!(st.pairs_tested, 1);
        assert_eq!(st.rejected_open, 1);
        assert_eq!(forest.nodes[1].depth, 0);
    }

    #[test]
    fn broken_ring_has_no_hole() {
        // Ring with a 4 px gap in its top edge: the interior drains out.
        let mut mask = ring_mask(64, 64, 10, 10, 40, 6);
        for dx in 20..24 {
            for dy in 0..6 {
                mask.set(10 + dx, 10 + dy, false);
            }
        }
        let groups = vec![group(10, 10, 40, 40, 40 * 40 - 28 * 28 - 24)];
        let mut st = stats();
        let forest = build_containment(&groups, &mask, &ContainmentParams::default(), &mut st);
        assert_eq!(forest.nodes[0].holes, 0, "an open outline encloses nothing");
    }

    #[test]
    fn equal_boxes_never_nest_and_disabled_is_identity() {
        let mask = ring_mask(64, 64, 10, 10, 40, 6);
        let a = group(10, 10, 40, 40, 1000);
        let b = group(10, 10, 40, 40, 900);
        let mut st = stats();
        let forest = build_containment(&[a, b], &mask, &ContainmentParams::default(), &mut st);
        assert!(forest.nodes.iter().all(|c| c.parent.is_none()));
        let off = ContainmentParams {
            enabled: false,
            ..Default::default()
        };
        let mut st2 = stats();
        let forest2 = build_containment(&[a, b], &mask, &off, &mut st2);
        assert_eq!(forest2.roots, 2);
        assert_eq!(st2.pairs_tested, 0);
    }

    #[test]
    fn containment_is_permutation_invariant() {
        let mut mask = ring_mask(64, 64, 10, 10, 40, 6);
        for dy in 0..8 {
            for dx in 0..8 {
                mask.set(26 + dx, 26 + dy, true);
            }
        }
        let ring = group(10, 10, 40, 40, 40 * 40 - 28 * 28);
        let dot = group(26, 26, 8, 8, 64);
        let mut st_a = stats();
        let a = build_containment(
            &[ring, dot],
            &mask,
            &ContainmentParams::default(),
            &mut st_a,
        );
        let mut st_b = stats();
        let b = build_containment(
            &[dot, ring],
            &mask,
            &ContainmentParams::default(),
            &mut st_b,
        );
        // Same facts, index-shifted: [ring, dot] vs [dot, ring].
        assert_eq!(a.nodes[0].depth, 0);
        assert_eq!(b.nodes[1].depth, 0);
        assert_eq!(a.nodes[1].depth, b.nodes[0].depth);
        assert_eq!(a.nodes[1].parent, Some(0));
        assert_eq!(b.nodes[0].parent, Some(1));
        assert_eq!(a.nodes[0].holes, b.nodes[1].holes);
    }
}
