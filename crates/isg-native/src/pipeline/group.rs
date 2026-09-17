//! §3.3 stage ④-prep — connected-component grouping (Phase 3, W7).
//!
//! Two implementations share one contract:
//!
//! * [`RleCclGrouper`] — **production**. It consumes [`ForegroundMask::runs`]
//!   and nothing else, per ARCHITECTURE.md §3.4 ("all subsequent steps run on
//!   runs, not pixels"): a 5 %-ink 4096² sheet works on ~50–200 KB of runs
//!   instead of 64 MB of per-pixel labels, and the sweep is O(runs · α(runs)).
//! * [`CclGrouper`] — the Phase-0 spike's pixel-domain two-pass union-find,
//!   kept as the **equality oracle**. The spike was CI-validated at
//!   1454/1454 icons across the 16 corpus sheets; the tests below assert that
//!   both implementations return *identical* [`IconGroup`] vectors for every
//!   mask they are given, so the W7 rewrite cannot silently change grouping.
//!
//! Both are 8-connected two-pass union-find. Deterministic by construction:
//! scan order is fixed, labels are canonical (component minimum), output is
//! sorted by (`bbox.y`, `bbox.x`, `origin`).

use std::collections::HashMap;

use isg_core::{Bbox, ForegroundMask, GroupingStrategy, IconGroup, RasterView};

/// Two-pass union-find grouper over **pixels** — the equality oracle.
///
/// Production code uses [`RleCclGrouper`]; this implementation stays as the
/// reference the run-based grouper is tested against (and as the readable
/// statement of the 8-connectivity contract).
#[derive(Clone, Copy, Debug)]
pub struct CclGrouper {
    /// Drop components smaller than this many foreground pixels (vtracer's
    /// own speckle default is 4×4 = 16).
    pub min_area: u32,
}

impl Default for CclGrouper {
    fn default() -> Self {
        Self { min_area: 16 }
    }
}

/// Run-length connected-component grouper over **runs** — production path.
///
/// Consumes [`ForegroundMask::runs`] (row-major `{y, x_start, x_end}`) and
/// unions a run with every run of the previous row it touches once both are
/// expanded by one pixel — the ±1 that turns 4-connectivity into 8. No
/// per-pixel labelling array is ever allocated.
#[derive(Clone, Copy, Debug)]
pub struct RleCclGrouper {
    /// Drop components smaller than this many foreground pixels (vtracer's
    /// own speckle default is 4×4 = 16).
    pub min_area: u32,
}

impl Default for RleCclGrouper {
    fn default() -> Self {
        Self { min_area: 16 }
    }
}

/// Root-finding with path halving. `parent[i]` is `i` at a root; unions always
/// attach toward the lower index, so the root is the canonical (minimum)
/// member of the set.
fn uf_find(parent: &mut [i32], mut i: usize) -> usize {
    while parent[i] != i as i32 {
        parent[i] = parent[parent[i] as usize];
        i = parent[i] as usize;
    }
    i
}

fn uf_union(parent: &mut [i32], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra == rb {
        return;
    }
    let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
    parent[hi] = lo as i32;
}

#[derive(Debug, Clone, Copy)]
struct CclStats {
    minx: i32,
    miny: i32,
    maxx: i32,
    maxy: i32,
    area: u32,
}

/// Canonical output order: top-to-bottom, left-to-right, then the scanned
/// member pixel. Shared so the two groupers cannot drift apart.
fn sort_groups(groups: &mut [IconGroup]) {
    groups.sort_by(|a, b| {
        a.bbox
            .y
            .cmp(&b.bbox.y)
            .then_with(|| a.bbox.x.cmp(&b.bbox.x))
            .then_with(|| a.origin.0.cmp(&b.origin.0))
            .then_with(|| a.origin.1.cmp(&b.origin.1))
    });
}

impl GroupingStrategy for CclGrouper {
    #[allow(clippy::needless_range_loop)] // pixel-grid indexing is the domain
    fn group_all(&self, raster: &dyn RasterView, mask: &ForegroundMask) -> Vec<IconGroup> {
        let w = raster.width() as i32;
        let h = raster.height() as i32;
        let n = (w * h) as usize;

        // ---- pass 1: label with union-find (upper-left 8-neighbourhood) ----
        let mut parent: Vec<i32> = vec![-1; n];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if !mask.get(x as u32, y as u32) {
                    continue;
                }
                parent[i] = i as i32; // new set
                                      // Neighbours: (x-1,y-1) (x-1,y) (x-1,y+1) (x,y-1). The
                                      // `nx >= w` bound matters: without it, the (x+1, y-1)
                                      // neighbour of a last-column pixel wraps to column 0 of row
                                      // `y` and merges opposite sheet edges.
                for &(dx, dy) in &[(1i32, 1i32), (0, 1), (-1, 1), (1, 0)] {
                    let nx = x - dx;
                    let ny = y - dy;
                    if nx < 0 || nx >= w || ny < 0 {
                        continue;
                    }
                    let ni = (ny * w + nx) as usize;
                    if mask.get(nx as u32, ny as u32) && parent[ni] != -1 {
                        uf_union(&mut parent, i, ni);
                    }
                }
            }
        }

        // ---- pass 2: aggregate per canonical root (foreground only) ----
        let mut stats: HashMap<usize, CclStats> = HashMap::new();
        let mut origins: HashMap<usize, (u32, u32)> = HashMap::new();
        for y in 0..h {
            let row = (y * w) as usize;
            for x in 0..w {
                let i = row + x as usize;
                if !mask.get(x as u32, y as u32) {
                    continue;
                }
                let root = uf_find(&mut parent, i);
                let st = stats.entry(root).or_insert(CclStats {
                    minx: x,
                    miny: y,
                    maxx: x,
                    maxy: y,
                    area: 0,
                });
                st.minx = st.minx.min(x);
                st.miny = st.miny.min(y);
                st.maxx = st.maxx.max(x);
                st.maxy = st.maxy.max(y);
                st.area += 1;
                origins.entry(root).or_insert((x as u32, y as u32));
            }
        }

        let mut groups = Vec::with_capacity(stats.len());
        for (root, s) in &stats {
            if s.area < self.min_area {
                continue;
            }
            let o = origins[root];
            groups.push(IconGroup {
                bbox: Bbox::from_parts(
                    s.minx as u32,
                    s.miny as u32,
                    (s.maxx - s.minx + 1) as u32,
                    (s.maxy - s.miny + 1) as u32,
                ),
                area: s.area,
                origin: o,
            });
        }
        sort_groups(&mut groups);
        groups
    }
}

impl GroupingStrategy for RleCclGrouper {
    fn group_all(&self, _raster: &dyn RasterView, mask: &ForegroundMask) -> Vec<IconGroup> {
        let runs = mask.runs();
        if runs.is_empty() {
            return Vec::new();
        }
        let mut parent: Vec<i32> = (0..runs.len() as i32).collect();

        // Row spans: `runs()` is row-major, so each row is one contiguous
        // slice of the vector.
        let mut rows: Vec<(usize, usize)> = Vec::new();
        let mut i = 0usize;
        while i < runs.len() {
            let y = runs[i].y;
            let mut j = i + 1;
            while j < runs.len() && runs[j].y == y {
                j += 1;
            }
            rows.push((i, j));
            i = j;
        }

        // Union against the previous row only — 8-connectivity needs no other
        // neighbour. Two runs touch when `cur.x_start <= prev.x_end` and
        // `prev.x_start <= cur.x_end`: `x_end` is exclusive, so a run ending
        // at column 3 and one starting at column 3 are diagonally adjacent.
        // Both row lists are sorted by `x_start` and non-overlapping within a
        // row, so one forward pointer per row walks each pair once.
        for pair in rows.windows(2) {
            let (ps, pe) = pair[0];
            let (cs, ce) = pair[1];
            if runs[cs].y != runs[ps].y + 1 {
                continue; // a skipped row cannot connect anything
            }
            let mut k = ps;
            for (off, cur) in runs[cs..ce].iter().enumerate() {
                while k < pe && runs[k].x_end < cur.x_start {
                    k += 1;
                }
                let mut p = k;
                while p < pe {
                    if runs[p].x_start > cur.x_end {
                        break;
                    }
                    uf_union(&mut parent, cs + off, p);
                    p += 1;
                }
            }
        }

        // Aggregate per canonical root, then filter and order exactly like the
        // pixel-domain oracle. The first run of a component in scan order is
        // its top-left-most member pixel, so it is also the group `origin`.
        let mut stats: HashMap<usize, CclStats> = HashMap::new();
        let mut origins: HashMap<usize, (u32, u32)> = HashMap::new();
        for (i, run) in runs.iter().enumerate() {
            let root = uf_find(&mut parent, i);
            let last = run.x_end.saturating_sub(1);
            let st = stats.entry(root).or_insert(CclStats {
                minx: run.x_start as i32,
                miny: run.y as i32,
                maxx: last as i32,
                maxy: run.y as i32,
                area: 0,
            });
            st.minx = st.minx.min(run.x_start as i32);
            st.miny = st.miny.min(run.y as i32);
            st.maxx = st.maxx.max(last as i32);
            st.maxy = st.maxy.max(run.y as i32);
            st.area += run.len();
            origins.entry(root).or_insert((run.x_start, run.y));
        }

        let mut groups = Vec::with_capacity(stats.len());
        for (root, s) in &stats {
            if s.area < self.min_area {
                continue;
            }
            let o = origins[root];
            groups.push(IconGroup {
                bbox: Bbox::from_parts(
                    s.minx as u32,
                    s.miny as u32,
                    (s.maxx - s.minx + 1) as u32,
                    (s.maxy - s.miny + 1) as u32,
                ),
                area: s.area,
                origin: o,
            });
        }
        sort_groups(&mut groups);
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal [`RasterView`] for tests — grouping never reads luma.
    struct RV {
        w: u32,
        h: u32,
    }

    impl RV {
        fn new(w: u32, h: u32) -> Self {
            Self { w, h }
        }
    }

    impl RasterView for RV {
        fn width(&self) -> u32 {
            self.w
        }
        fn height(&self) -> u32 {
            self.h
        }
        fn luma_row(&self, _y: u32) -> &[f32] {
            &[]
        }
    }

    /// Deterministic LCG (the corpus generator's constants) so the oracle
    /// comparison sees identical masks on every machine.
    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn below(&mut self, bound: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as u32) % bound
        }
    }

    fn random_mask(w: u32, h: u32, permille: u32, seed: u64) -> ForegroundMask {
        let mut mask = ForegroundMask::new(w, h);
        let mut rng = Lcg::new(seed);
        for y in 0..h {
            for x in 0..w {
                if rng.below(1000) < permille {
                    mask.set(x, y, true);
                }
            }
        }
        mask
    }

    /// The W7 invariant: run-based grouping must equal pixel-domain grouping.
    fn assert_oracles_agree(mask: &ForegroundMask, min_area: u32, what: &str) {
        let raster = RV::new(mask.width(), mask.height());
        let pixel = CclGrouper { min_area }.group_all(&raster, mask);
        let rle = RleCclGrouper { min_area }.group_all(&raster, mask);
        assert_eq!(rle, pixel, "{what} (min_area {min_area})");
    }

    #[test]
    fn groups_disconnected_blocks_and_filters_noise() {
        let r = RV::new(32, 32);
        let mut mask = ForegroundMask::new(32, 32);
        // 6x6 block at (4,4)
        for y in 4..10 {
            for x in 4..10 {
                mask.set(x, y, true);
            }
        }
        // 8x3 block at (20,20)
        for y in 20..23 {
            for x in 20..28 {
                mask.set(x, y, true);
            }
        }
        // 2x2 noise at (0, 28) — below min_area
        for y in 28..30 {
            for x in 0..2 {
                mask.set(x, y, true);
            }
        }
        // Diagonal touch: two 3x3 blocks joined ONLY by the single bridge
        // pixel (19,9) — 8-connectivity must merge them into one group.
        for y in 6..9 {
            for x in 16..19 {
                mask.set(x, y, true);
            }
        }
        mask.set(19, 9, true);
        for y in 10..13 {
            for x in 20..23 {
                mask.set(x, y, true);
            }
        }

        let groups = CclGrouper::default().group_all(&r, &mask);
        assert_eq!(groups.len(), 3, "diagonal touch merges; noise is filtered");
        // Deterministic order: (y, x).
        assert_eq!(groups[0].bbox, Bbox::new(4, 4, 6, 6).unwrap());
        assert_eq!(
            groups[1].bbox,
            Bbox::new(16, 6, 7, 7).unwrap(),
            "diagonally touched blocks form one 7x7 group"
        );
        assert_eq!(groups[2].bbox, Bbox::new(20, 20, 8, 3).unwrap());

        // The run-based grouper returns the same three groups, same order.
        assert_oracles_agree(&mask, 16, "diagonal touch + speckle");
        assert_eq!(
            RleCclGrouper::default().group_all(&r, &mask),
            groups,
            "RLE grouper output must match the oracle exactly"
        );
    }

    #[test]
    fn last_column_does_not_wrap_to_first() {
        // Regression for the CCL bounds bug: a component flush against the
        // right edge must not union with one flush against the left edge of
        // the same rows.
        let r = RV::new(24, 24);
        let mut mask = ForegroundMask::new(24, 24);
        for y in 10..14 {
            for x in 20..24 {
                mask.set(x, y, true); // flush right
            }
            for x in 0..4 {
                mask.set(x, y, true); // flush left
            }
        }
        let groups = CclGrouper { min_area: 1 }.group_all(&r, &mask);
        assert_eq!(groups.len(), 2, "edge columns must not wrap around");
        assert_eq!(groups[0].bbox, Bbox::new(0, 10, 4, 4).unwrap());
        assert_eq!(groups[1].bbox, Bbox::new(20, 10, 4, 4).unwrap());

        // The run sweep is immune by construction: columns never wrap because
        // run coordinates are per-row. Prove it on the same mask.
        let rle = RleCclGrouper { min_area: 1 }.group_all(&r, &mask);
        assert_eq!(rle, groups);
    }

    #[test]
    fn rle_matches_pixel_oracle_on_random_masks() {
        for seed in 0..8u64 {
            for permille in [80u32, 250, 500, 800] {
                let mask = random_mask(97, 61, permille, seed * 16 + permille as u64);
                for min_area in [1u32, 4, 16, 64] {
                    assert_oracles_agree(&mask, min_area, "random mask");
                }
            }
        }
    }

    #[test]
    fn rle_matches_pixel_oracle_on_structured_patterns() {
        // Diagonal 1-px chain: only 8-connectivity keeps it one component.
        let mut diag = ForegroundMask::new(64, 64);
        for i in 0..30 {
            diag.set(i, i, true);
        }
        assert_oracles_agree(&diag, 1, "diagonal chain");
        assert_eq!(
            RleCclGrouper { min_area: 1 }
                .group_all(&RV::new(64, 64), &diag)
                .len(),
            1
        );

        // 2×2 checkerboard: under 8-connectivity the whole block is one
        // component (every background pixel is a diagonal-only gap).
        let mut checker = ForegroundMask::new(32, 32);
        for y in 0..8 {
            for x in 0..8 {
                if (x + y) % 2 == 0 {
                    checker.set(x, y, true);
                }
            }
        }
        assert_oracles_agree(&checker, 1, "checkerboard");
        assert_eq!(
            RleCclGrouper { min_area: 1 }
                .group_all(&RV::new(32, 32), &checker)
                .len(),
            1
        );

        // Two blocks with a 2-column gap: separated until two explicitly set
        // bridge pixels on the last shared row join them into one component.
        let mut bridge = ForegroundMask::new(48, 24);
        for y in 4..8 {
            for x in 4..12 {
                bridge.set(x, y, true);
            }
            for x in 14..22 {
                bridge.set(x, y, true);
            }
        }
        let r = RV::new(48, 24);
        assert_eq!(
            RleCclGrouper { min_area: 1 }.group_all(&r, &bridge).len(),
            2
        );
        bridge.set(12, 7, true);
        bridge.set(13, 7, true);
        assert_oracles_agree(&bridge, 1, "bridged blocks");
        let joined = RleCclGrouper { min_area: 1 }.group_all(&r, &bridge);
        assert_eq!(joined.len(), 1, "bridge pixel must join both blocks");
        assert_eq!(joined[0].bbox, Bbox::new(4, 4, 18, 4).unwrap());
        assert_eq!(joined[0].origin, (4, 4));

        // Ink flush against all four borders: one frame component.
        let mut edge = ForegroundMask::new(40, 30);
        for x in 0..40 {
            edge.set(x, 0, true);
            edge.set(x, 29, true);
        }
        for y in 0..30 {
            edge.set(0, y, true);
            edge.set(39, y, true);
        }
        assert_oracles_agree(&edge, 1, "border frame");
        let frame = RleCclGrouper { min_area: 1 }.group_all(&RV::new(40, 30), &edge);
        assert_eq!(frame.len(), 1);
        assert_eq!(frame[0].bbox, Bbox::new(0, 0, 40, 30).unwrap());
    }

    #[test]
    fn rle_handles_empty_single_and_full_masks() {
        let r = RV::new(24, 24);
        let empty = ForegroundMask::new(24, 24);
        assert!(RleCclGrouper::default().group_all(&r, &empty).is_empty());
        assert!(CclGrouper::default().group_all(&r, &empty).is_empty());

        let mut one = ForegroundMask::new(24, 24);
        one.set(7, 19, true);
        let groups = RleCclGrouper { min_area: 1 }.group_all(&r, &one);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].bbox, Bbox::new(7, 19, 1, 1).unwrap());
        assert_eq!(groups[0].area, 1);
        assert_eq!(groups[0].origin, (7, 19));
        // The same pixel is a speckle below min_area.
        assert!(RleCclGrouper { min_area: 2 }.group_all(&r, &one).is_empty());

        let mut full = ForegroundMask::new(40, 30);
        for y in 0..30 {
            for x in 0..40 {
                full.set(x, y, true);
            }
        }
        let rf = RV::new(40, 30);
        assert_oracles_agree(&full, 16, "full mask");
        let solid = RleCclGrouper::default().group_all(&rf, &full);
        assert_eq!(solid.len(), 1);
        assert_eq!(solid[0].bbox, Bbox::new(0, 0, 40, 30).unwrap());
        assert_eq!(solid[0].area, 40 * 30);
        assert_eq!(solid[0].origin, (0, 0));
    }

    #[test]
    fn rle_is_deterministic_across_threads() {
        let masks: Vec<ForegroundMask> = (0..4)
            .map(|i| random_mask(128, 96, 200 + i * 150, 0xC0FFEE + i as u64))
            .collect();
        let expected: Vec<Vec<IconGroup>> = masks
            .iter()
            .map(|m| RleCclGrouper::default().group_all(&RV::new(m.width(), m.height()), m))
            .collect();

        // Same input, eight concurrent workers — identical output vectors.
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        masks
                            .iter()
                            .map(|m| {
                                RleCclGrouper::default()
                                    .group_all(&RV::new(m.width(), m.height()), m)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for h in handles {
                assert_eq!(h.join().expect("worker thread"), expected);
            }
        });
    }

    #[test]
    fn rle_matches_pixel_oracle_at_sheet_scale() {
        // 1024² sheet: 32×32 blocks of 24×24 in 32-px cells — the largest
        // mask the pixel oracle can be compared against cheaply.
        let mut mask = ForegroundMask::new(1024, 1024);
        for gy in 0..32u32 {
            for gx in 0..32u32 {
                let x0 = gx * 32 + 4;
                let y0 = gy * 32 + 4;
                for y in y0..y0 + 24 {
                    for x in x0..x0 + 24 {
                        mask.set(x, y, true);
                    }
                }
            }
        }
        let r = RV::new(1024, 1024);
        let t0 = std::time::Instant::now();
        let pixel = CclGrouper { min_area: 16 }.group_all(&r, &mask);
        let pixel_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t0 = std::time::Instant::now();
        let rle = RleCclGrouper { min_area: 16 }.group_all(&r, &mask);
        let rle_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(rle, pixel, "1024² sheet: RLE must equal the oracle");
        assert_eq!(rle.len(), 1024, "32×32 blocks = 1024 groups");
        assert_eq!(rle[0].bbox, Bbox::new(4, 4, 24, 24).unwrap());
        assert!(
            rle_ms < 500.0,
            "RLE grouping must stay far under 0.5 s: {rle_ms:.1} ms"
        );
        eprintln!(
            "W7 evidence: group_all on a 1024²/1024-block sheet — pixel oracle {pixel_ms:.1} ms, RLE {rle_ms:.1} ms"
        );
    }

    #[test]
    fn rle_matches_pixel_oracle_at_keystone_scale() {
        // 4096² sheet with 100 blocks — the C2 sheet's shape and size, so the
        // run-based path is exercised at the keystone scale inside `cargo test`.
        let mut mask = ForegroundMask::new(4096, 4096);
        for gy in 0..10u32 {
            for gx in 0..10u32 {
                let x0 = gx * 400 + 40;
                let y0 = gy * 400 + 40;
                for y in y0..y0 + 200 {
                    for x in x0..x0 + 200 {
                        mask.set(x, y, true);
                    }
                }
            }
        }
        let r = RV::new(4096, 4096);
        let t0 = std::time::Instant::now();
        let rle = RleCclGrouper { min_area: 16 }.group_all(&r, &mask);
        let rle_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(rle.len(), 100, "10×10 blocks = 100 groups");
        assert_eq!(rle[0].bbox, Bbox::new(40, 40, 200, 200).unwrap());
        assert!(
            rle_ms < 2000.0,
            "keystone-scale grouping must beat the 2 s budget: {rle_ms:.1} ms"
        );
        eprintln!(
            "W7 evidence: group_all on a 4096²/100-block sheet — RLE {rle_ms:.1} ms (C2 budget 2000 ms)"
        );

        let t0 = std::time::Instant::now();
        let pixel = CclGrouper { min_area: 16 }.group_all(&r, &mask);
        let pixel_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            rle, pixel,
            "4096² keystone shape: RLE must equal the oracle"
        );
        eprintln!(
            "W7 evidence: 4096²/100-block sheet — pixel oracle {pixel_ms:.1} ms, RLE {rle_ms:.1} ms"
        );
    }
}
