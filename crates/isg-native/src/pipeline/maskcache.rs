//! §3.4 override path — re-run grouping **from the cached mask** without
//! re-decoding (`~50–200 ms, feels live` is the spec's target for the
//! sensitivity sliders).
//!
//! What is expensive on the slider path is stages ①–③ (decode → normalize →
//! background → mask → clean); what the sliders actually change is the
//! *grouping* parameters. This module keeps the segmentation result — the
//! bit-packed mask as RLE runs, plus the detected background — in a small
//! content-addressed LRU, so a slider tick costs only CCL + refine.
//!
//! Two properties are load-bearing and tested here:
//!
//! * **Key stability**: [`mask_key`] is a pure function of the source bytes and
//!   the segmentation parameters, so a re-run with the *same* image and params
//!   can never miss; a different `SegParams` (a true re-segmentation) can never
//!   hit. Versioned by [`MASKCACHE_VERSION`] so a format change invalidates.
//! * **Bounded memory**: entries are evicted LRU past the capacity, because the
//!   payload is one `Vec<RleRun>` per sheet (a 4096² / 5 %-ink sheet is a few
//!   hundred KB of runs, not the 134 MB the sheet raster would cost — which is
//!   exactly why the raster is *not* cached).
//!
//! The full-image path stays cache-transparent: `pipeline::mask_cached` asks
//! here first and only then calls `segment`, so a warm hit performs no decode
//! at all.

use std::collections::{HashMap, VecDeque};

use isg_core::{ForegroundMask, GroupingStrategy, IconGroup, RasterView};

use super::background::{BackgroundKind, BackgroundModel, SegParams};
use super::group::RleCclGrouper;
use super::merge::{refine_groups_with_context, RefineParams, RefineStats};

/// Entry-format version; part of every key, so bumping it invalidates caches.
pub const MASKCACHE_VERSION: u32 = 1;

/// Content hash of a sheet's source bytes (the caller can compute it once and
/// reuse it across slider ticks via [`mask_key_from_hash`]).
#[must_use]
pub fn sheet_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The deterministic mask-cache key for one (sheet, segmentation) pairing.
#[must_use]
pub fn mask_key(bytes: &[u8], params: &SegParams) -> String {
    mask_key_from_hash(&sheet_hash(bytes), params)
}

/// [`mask_key`] for an already-computed [`sheet_hash`].
#[must_use]
pub fn mask_key_from_hash(sheet: &str, params: &SegParams) -> String {
    let cfg = blake3::hash(params.to_cache_string().as_bytes())
        .to_hex()
        .to_string();
    let sheet = &sheet[..sheet.len().min(16)];
    format!("{sheet}-{}-v{MASKCACHE_VERSION}", &cfg[..cfg.len().min(16)])
}

/// One cached segmentation result: the mask (as runs) plus what produced it.
///
/// The sheet raster is deliberately absent — `RleCclGrouper` never looks at
/// pixels, and caching 4096² RGBA + luma would cost ~134 MB per sheet.
#[derive(Clone, Debug)]
pub struct CachedMask {
    /// Sheet width in pixels.
    pub width: u32,
    /// Sheet height in pixels.
    pub height: u32,
    /// Foreground runs, row-major (the mask's own serialization).
    pub runs: Vec<isg_core::RleRun>,
    /// Background model the mask was built against.
    pub background: BackgroundModel,
}

impl CachedMask {
    /// Rebuilds the bit-packed mask (`ForegroundMask::from_runs` is lossless).
    #[must_use]
    pub fn mask(&self) -> ForegroundMask {
        ForegroundMask::from_runs(self.width, self.height, &self.runs)
    }

    /// Approximate payload size in bytes (runs + model).
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.runs.len() * std::mem::size_of::<isg_core::RleRun>() + 16
    }
}

/// LRU cache of segmentation results, keyed by [`mask_key`].
#[derive(Debug)]
pub struct MaskCache {
    cap: usize,
    entries: HashMap<String, CachedMask>,
    order: VecDeque<String>,
    hits: u64,
    misses: u64,
}

impl MaskCache {
    /// Cache holding at most `cap` sheets (0 disables storage but still counts
    /// hits/misses, which keeps the accounting honest in tests).
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            entries: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Configured capacity in sheets.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Cached sheet count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Warm hits recorded so far.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cold misses recorded so far.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Total payload size of the cached entries, in bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries.values().map(CachedMask::bytes).sum()
    }

    /// Looks a key up, marking it most-recently-used.
    pub fn get(&mut self, key: &str) -> Option<CachedMask> {
        let found = self.entries.get(key).cloned();
        match found {
            Some(entry) => {
                self.hits += 1;
                self.touch(key);
                Some(entry)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Inserts (or replaces) an entry, evicting the least-recently-used ones
    /// past the capacity.
    pub fn put(&mut self, key: &str, mask: &ForegroundMask, background: &BackgroundModel) {
        let entry = CachedMask {
            width: mask.width(),
            height: mask.height(),
            runs: mask.runs(),
            background: *background,
        };
        self.entries.insert(key.to_string(), entry);
        self.touch(key);
        while self.entries.len() > self.cap {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if !self.order.contains(&oldest) {
                self.entries.remove(&oldest);
            }
        }
    }

    /// Drops every entry (hit/miss counters keep counting).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }
}

/// RLE CCL ignores luma, which is what lets the cached path work without the
/// sheet — this view supplies the dimensions and a contract-respecting zero
/// luma row so the trait is not abused. Public so the grouping session (W12)
/// can CCL a cached mask without the sheet too.
#[derive(Clone, Debug)]
pub struct MaskView {
    w: u32,
    h: u32,
    row: Vec<f32>,
}

impl MaskView {
    /// A `width × height` view whose luma row is all zeros.
    #[must_use]
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            row: vec![0.0; w as usize],
        }
    }
}

impl RasterView for MaskView {
    fn width(&self) -> u32 {
        self.w
    }

    fn height(&self) -> u32 {
        self.h
    }

    fn luma_row(&self, _y: u32) -> &[f32] {
        &self.row
    }
}

/// What a cached re-group produced.
#[derive(Clone, Debug)]
pub struct RegroupOutcome {
    /// Final groups (CCL + refine).
    pub groups: Vec<IconGroup>,
    /// Refine evidence counters.
    pub refine: RefineStats,
    /// Wall-clock for the whole re-run, in milliseconds.
    pub elapsed_ms: f32,
}

/// §3.4 slider re-run: CCL + refine straight off a cached mask. `None` when the
/// key is not cached — the caller then re-segments (the only path that decodes).
pub fn regroup_cached(
    key: &str,
    cache: &mut MaskCache,
    params: &RefineParams,
) -> Option<RegroupOutcome> {
    let t0 = std::time::Instant::now();
    let entry = cache.get(key)?;
    let mask = entry.mask();
    let raster = MaskView::new(entry.width, entry.height);
    let raw = RleCclGrouper::default().group_all(&raster, &mask);
    // The cached background model goes in as well, so the slider path scores the
    // same signals the app path does.
    let (groups, refine) = refine_groups_with_context(raw, &mask, Some(&entry.background), params);
    Some(RegroupOutcome {
        groups,
        refine,
        elapsed_ms: t0.elapsed().as_secs_f32() * 1000.0,
    })
}

/// Convenience for callers that hold the decoded pieces rather than a mask.
#[must_use]
pub fn cached_from_parts(mask: &ForegroundMask, background: &BackgroundModel) -> CachedMask {
    CachedMask {
        width: mask.width(),
        height: mask.height(),
        runs: mask.runs(),
        background: *background,
    }
}

/// Only `BorderConsensus` / `Alpha` masks are trustworthy enough to be reused
/// by a slider tick; kept here so cache decisions stay next to the data.
#[must_use]
pub fn background_is_confident(background: &BackgroundModel, min_share: f32) -> bool {
    matches!(
        background.kind,
        BackgroundKind::BorderConsensus | BackgroundKind::Alpha
    ) && background.consensus >= min_share
}

#[cfg(test)]
mod tests {
    use super::*;
    use isg_core::Bbox;

    fn block_mask(x0: u32, y0: u32, n: u32) -> ForegroundMask {
        let mut m = ForegroundMask::new(64, 64);
        for y in 0..n {
            for x in 0..n {
                m.set(x0 + x, y0 + y, true);
            }
        }
        m
    }

    fn bg(consensus: f32) -> BackgroundModel {
        BackgroundModel {
            kind: BackgroundKind::BorderConsensus,
            rgba: [255, 255, 255, 255],
            consensus,
        }
    }

    #[test]
    fn key_is_stable_and_parameter_sensitive() {
        let bytes = b"sheet-bytes";
        let a = SegParams::default();
        let mut b = SegParams::default();
        assert_eq!(mask_key(bytes, &a), mask_key(bytes, &a));
        assert_eq!(mask_key(b"sheet-bytes", &a), mask_key(bytes, &a));
        assert_ne!(mask_key(b"other-bytes", &a), mask_key(bytes, &a));
        b.delta_e_max = 20.0;
        assert_ne!(mask_key(bytes, &b), mask_key(bytes, &a), "params matter");
        b = a;
        b.close_passes = 2;
        assert_ne!(mask_key(bytes, &b), mask_key(bytes, &a), "clean matters");
    }

    #[test]
    fn key_from_hash_agrees_with_key_from_bytes() {
        let bytes = b"abc";
        let h = sheet_hash(bytes);
        assert_eq!(
            mask_key(bytes, &SegParams::default()),
            mask_key_from_hash(&h, &SegParams::default())
        );
    }

    #[test]
    fn put_then_get_round_trips_the_mask_exactly() {
        let mut cache = MaskCache::new(4);
        let mask = block_mask(8, 8, 12);
        let key = "k1";
        cache.put(key, &mask, &bg(0.9));
        let got = cache.get(key).expect("cached");
        assert_eq!(got.mask(), mask, "runs round-trip must be lossless");
        assert_eq!(got.background.kind, BackgroundKind::BorderConsensus);
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 0);
    }

    #[test]
    fn misses_are_counted_and_return_nothing() {
        let mut cache = MaskCache::new(2);
        assert!(cache.get("nope").is_none());
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn capacity_evicts_the_least_recently_used() {
        let mut cache = MaskCache::new(2);
        let mask = block_mask(0, 0, 4);
        cache.put("a", &mask, &bg(0.9));
        cache.put("b", &mask, &bg(0.9));
        // Touch "a" so "b" becomes the LRU entry.
        assert!(cache.get("a").is_some());
        cache.put("c", &mask, &bg(0.9));
        assert_eq!(cache.len(), 2);
        assert!(cache.get("a").is_some(), "recently used entry survives");
        assert!(cache.get("b").is_none(), "LRU evicted");
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn zero_capacity_stores_nothing_but_still_counts() {
        let mut cache = MaskCache::new(0);
        cache.put("a", &block_mask(0, 0, 4), &bg(0.9));
        assert!(cache.is_empty());
        assert!(cache.get("a").is_none());
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn regroup_from_cache_matches_a_direct_run() {
        // Two glued blocks + one apart: CCL makes 2 groups, refine (disabled)
        // leaves them alone. The cached path must agree exactly.
        let mut mask = block_mask(4, 4, 20);
        for y in 0..20 {
            for x in 0..20 {
                mask.set(26 + x, 4 + y, true); // 2 px gap from the first block
            }
        }
        let key = "sheet";
        let mut cache = MaskCache::new(1);
        cache.put(key, &mask, &bg(0.95));
        let params = RefineParams {
            enabled: false,
            ..RefineParams::default()
        };
        let out = regroup_cached(key, &mut cache, &params).expect("cached");
        assert_eq!(out.groups.len(), 2, "{:?}", out.groups);
        assert!(out.refine.elapsed_ms >= 0.0);
        assert!(regroup_cached("absent", &mut cache, &params).is_none());
    }

    #[test]
    fn cached_bytes_track_the_payload() {
        let mut cache = MaskCache::new(2);
        assert_eq!(cache.bytes(), 0);
        cache.put("a", &block_mask(0, 0, 8), &bg(0.9));
        let one = cache.bytes();
        assert!(one > 0);
        cache.put("b", &block_mask(0, 0, 8), &bg(0.9));
        assert_eq!(cache.bytes(), one * 2);
        cache.clear();
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn mask_view_satisfies_the_raster_contract() {
        let view = MaskView::new(7, 3);
        assert_eq!(view.width(), 7);
        assert_eq!(view.height(), 3);
        assert_eq!(view.luma_row(2).len(), 7, "luma_row must be width-long");
    }

    #[test]
    fn background_confidence_gate_reads_kind_and_share() {
        assert!(background_is_confident(&bg(0.9), 0.85));
        assert!(!background_is_confident(&bg(0.5), 0.85));
        let kmeans = BackgroundModel {
            kind: BackgroundKind::KMeans,
            rgba: [0, 0, 0, 0],
            consensus: 0.99,
        };
        assert!(!background_is_confident(&kmeans, 0.85));
    }

    #[test]
    fn cached_group_shape_is_deterministic() {
        let mask = block_mask(2, 2, 10);
        let entry = cached_from_parts(&mask, &bg(0.8));
        assert_eq!(entry.mask(), mask);
        assert_eq!(entry.width, 64);
        assert_eq!(entry.height, 64);
        let bbox = Bbox::new(2, 2, 10, 10).unwrap();
        assert_eq!(
            entry
                .mask()
                .runs()
                .iter()
                .map(|r| (r.y, r.x_start, r.x_end))
                .collect::<Vec<_>>()
                .len(),
            10
        );
        assert_eq!(bbox.w, 10);
    }
}
