//! Stage 2 (spike): 8-connected two-pass union-find grouping.
//!
//! Deterministic by construction: scan order is fixed, labels are canonical
//! (component minimum), output is sorted by (`bbox.y`, `bbox.x`, `origin`).
//!
//! Memory: pass 1 keeps one `i32` per pixel (union-find parent); pass 2 walks
//! only foreground pixels and aggregates per-root stats in hash maps, so a
//! 4096² sheet peaks around the parent vector instead of per-pixel stat
//! arrays. (The Phase 3 production grouper replaces all of this with RLE-run
//! CCL per ARCHITECTURE.md §3.4.)

use std::collections::HashMap;

use isg_core::{Bbox, ForegroundMasker, GroupingStrategy, IconGroup, RasterView};

/// Two-pass union-find grouper with a speckle (minimum area) filter.
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

impl GroupingStrategy for CclGrouper {
    #[allow(clippy::needless_range_loop)] // pixel-grid indexing is the domain
    fn group_all(&self, raster: &dyn RasterView, mask: &[bool]) -> Vec<IconGroup> {
        let w = raster.width() as i32;
        let h = raster.height() as i32;
        let n = (w * h) as usize;
        debug_assert_eq!(mask.len(), n);

        // ---- pass 1: label with union-find (upper-left 8-neighbourhood) ----
        let mut parent: Vec<i32> = vec![-1; n];

        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if !mask[i] {
                    continue;
                }
                parent[i] = i as i32; // new set
                // neighbours: (x-1,y-1) (x-1,y) (x-1,y+1) (x,y-1)
                for &(dx, dy) in &[(1i32, 1i32), (0, 1), (-1, 1), (1, 0)] {
                    let nx = x - dx;
                    let ny = y - dy;
                    // The `nx >= w` bound matters: without it, the (x+1, y-1)
                    // neighbour of a last-column pixel wraps to column 0 of
                    // row `y` and merges opposite sheet edges.
                    if nx < 0 || nx >= w || ny < 0 {
                        continue;
                    }
                    let ni = (ny * w + nx) as usize;
                    if mask[ni] && parent[ni] != -1 {
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
                if !mask[i] {
                    continue;
                }
                let root = uf_find(&mut parent, i);
                let st = stats
                    .entry(root)
                    .or_insert(CclStats {
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
        groups.sort_by(|a, b| {
            a.bbox
                .y
                .cmp(&b.bbox.y)
                .then_with(|| a.bbox.x.cmp(&b.bbox.x))
                .then_with(|| a.origin.0.cmp(&b.origin.0))
                .then_with(|| a.origin.1.cmp(&b.origin.1))
        });
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_disconnected_blocks_and_filters_noise() {
        let mut mask = vec![false; 32 * 32];
        // 6x6 block at (4,4)
        for y in 4..10 {
            for x in 4..10 {
                mask[y * 32 + x] = true;
            }
        }
        // 8x3 block at (20,20)
        for y in 20..23 {
            for x in 20..28 {
                mask[y * 32 + x] = true;
            }
        }
        // 2x2 noise at (0, 28) — below min_area
        for y in 28..30 {
            for x in 0..2 {
                mask[y * 32 + x] = true;
            }
        }
        // diagonal touch: two 4x4 blocks joined by the single bridge pixel
        // (14,14) — 8-connectivity must merge them into one group.
        for y in 10..14 {
            for x in 10..14 {
                mask[y * 32 + x] = true;
            }
        }
        mask[14 * 32 + 14] = true;
        for y in 15..19 {
            for x in 15..19 {
                mask[y * 32 + x] = true;
            }
        }

        struct R;
        impl RasterView for R {
            fn width(&self) -> u32 {
                32
            }
            fn height(&self) -> u32 {
                32
            }
            fn luma_row(&self, _y: u32) -> &[f32] {
                &[]
            }
        }
        let groups = CclGrouper::default().group_all(&R, &mask);
        assert_eq!(groups.len(), 3, "diagonal touch merges; noise is filtered");
        // deterministic order: (y, x)
        assert_eq!(groups[0].bbox, Bbox::new(4, 4, 6, 6).unwrap());
        assert_eq!(
            groups[1].bbox,
            Bbox::new(10, 10, 9, 9).unwrap(),
            "diagonally touched blocks form one 9x9 group"
        );
        assert_eq!(groups[2].bbox, Bbox::new(20, 20, 8, 3).unwrap());
    }

    #[test]
    fn last_column_does_not_wrap_to_first() {
        // Regression for the CCL bounds bug: a component flush against the
        // right edge must not union with one flush against the left edge of
        // the same rows.
        let w = 24usize;
        let mut mask = vec![false; 24 * 24];
        for y in 10..14 {
            for x in 20..24 {
                mask[y * w + x] = true; // flush right
            }
            for x in 0..4 {
                mask[y * w + x] = true; // flush left
            }
        }
        struct R;
        impl RasterView for R {
            fn width(&self) -> u32 {
                24
            }
            fn height(&self) -> u32 {
                24
            }
            fn luma_row(&self, _y: u32) -> &[f32] {
                &[]
            }
        }
        let groups = CclGrouper { min_area: 1 }.group_all(&R, &mask);
        assert_eq!(groups.len(), 2, "edge columns must not wrap around");
        assert_eq!(groups[0].bbox, Bbox::new(0, 10, 4, 4).unwrap());
        assert_eq!(groups[1].bbox, Bbox::new(20, 10, 4, 4).unwrap());
    }
}
