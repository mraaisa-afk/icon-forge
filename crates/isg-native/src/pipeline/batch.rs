//! §3.3 batch orchestration — the T2 VectorizeJob engine (W4).
//!
//! One call takes a sheet's bytes through every stage: ①–③ segmentation,
//! grouping, then ④–⑧ per icon in parallel (rayon) through the
//! content-addressed cache. Design constraints honoured here:
//!
//! * the vtracer pipeline is **not `Send`** — it is built inside
//!   [`vectorize_icon`] per task, so no shared state crosses workers;
//! * the SQLite [`Library`] is **not `Sync`** — workers only ever touch it
//!   through two short critical sections per icon (cache get, cache put)
//!   around the shared slot mutex; the vectorize+score compute itself runs
//!   lock-free;
//! * per-icon failures (stage ⑤–⑧ errors) are **counted, not fatal** —
//!   "flag and fall back"; only cancellation and infrastructure errors
//!   abort the batch;
//! * progress is throttled (every 16 icons + one final report) and the
//!   peak RSS is sampled by a side thread ([`crate::rss::RssWatcher`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use isg_core::{GroupingStrategy, TracePreset};
use rayon::prelude::*;

use crate::cache::CacheStore;
use crate::cancel::CancellationToken;
use crate::db::{IconVectorRow, Library};
use crate::rss::{current_rss_bytes, RssWatcher};

use super::background::BackgroundModel;
use super::group::RleCclGrouper;
use super::merge::{refine_groups_with_context, RefineParams};
use super::raster::SheetRaster;
use super::score::{vectorize_cache_key, vectorize_scored, ScoredIcon, VectorizeError};
use super::{segment, SegParams};

/// The library handle shared by commands and jobs (the `Option` models
/// "no project open"). Lives in the Tauri state; the batch locks it only
/// for the short cache-I/O sections.
pub type SharedLibrary = Mutex<Option<Library>>;

/// Batch run configuration.
#[derive(Clone, Debug)]
pub struct BatchOptions {
    /// Trace preset for every icon.
    pub preset: TracePreset,
    /// Segmentation parameters (stages ①–③).
    pub seg: SegParams,
    /// Normalize cap (stage ①).
    pub max_dim: u32,
    /// Grouping speckle filter (minimum component area).
    pub min_area: u32,
    /// §3.4 F4 (noise) + F1 (merge) refine stage. Disabled by default until
    /// the corpus is calibrated (W12), so the Phase 0–2 gates keep measuring
    /// raw CCL output.
    pub refine: RefineParams,
}

impl Default for BatchOptions {
    fn default() -> Self {
        BatchOptions {
            preset: TracePreset::Balanced,
            seg: SegParams::default(),
            max_dim: 4096,
            min_area: 16,
            refine: RefineParams::default(),
        }
    }
}

/// Identifies the sheet for DB persistence; `None` skips persistence
/// (pure pipeline runs, gate benchmarks that track their own state).
#[derive(Clone, Debug)]
pub struct SheetRef {
    /// 16-byte sheet id (deterministic from content).
    pub id: [u8; 16],
    /// blake3 hex digest of the source bytes (icon ids derive from it).
    pub content_hash: String,
}

/// Per-icon batch outcome.
struct ItemResult {
    bbox: (u32, u32, u32, u32),
    icon: Option<ScoredIcon>,
    cache_key: String,
    hit: bool,
}

/// Fatal batch errors — anything else (stage ⑤–⑧ failures) is per-icon.
#[derive(Debug)]
pub enum BatchError {
    /// The cancellation token fired.
    Cancelled,
    /// Stage ①–③ failure.
    Segment(crate::IsgError),
    /// No project is open (the shared library slot is empty).
    NoProject,
    /// The shared library mutex is poisoned.
    Library(String),
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchError::Cancelled => f.write_str("cancelled"),
            BatchError::Segment(e) => write!(f, "segmentation failed: {e}"),
            BatchError::NoProject => f.write_str("no project is open"),
            BatchError::Library(m) => write!(f, "library unavailable: {m}"),
        }
    }
}

impl std::error::Error for BatchError {}

/// Aggregate run summary (counts and score aggregates only — timings are
/// deliberately excluded so summaries are byte-comparable across runs).
#[derive(Clone, Debug, PartialEq)]
pub struct BatchSummary {
    /// Icons discovered by grouping.
    pub icons: u32,
    /// Icons traced, emitted and scored successfully.
    pub ok: u32,
    /// Icons whose stage ⑤–⑧ failed (flagged, no SVG produced).
    pub failed: u32,
    /// Successful icons served from the stage ⑧ cache.
    pub cache_hits: u32,
    /// Lowest SSIM among successful icons (1.0 when none).
    pub min_ssim: f32,
    /// Lowest composite among successful icons (1.0 when none).
    pub min_composite: f32,
    /// Mean composite over successful icons.
    pub mean_composite: f32,
    /// Peak resident set observed during the batch.
    pub peak_rss_bytes: u64,
}

fn lock_lib(
    slot: &SharedLibrary,
) -> Result<std::sync::MutexGuard<'_, Option<Library>>, BatchError> {
    slot.lock()
        .map_err(|_| BatchError::Library("library mutex poisoned".to_string()))
}

fn lock_lib_any(
    slot: &SharedLibrary,
) -> Result<std::sync::MutexGuard<'_, Option<Library>>, VectorizeError> {
    slot.lock().map_err(|_| {
        VectorizeError::Cache(crate::IsgError::Corrupt(
            "library mutex poisoned".to_string(),
        ))
    })
}

/// Stage ⑤–⑧ for one icon, then the store-side of the cache (critical
/// section 2). Called only on a cache miss or corrupt payload.
fn compute_and_store(
    sheet: &SheetRaster,
    bbox: isg_core::Bbox,
    bg: &BackgroundModel,
    preset: TracePreset,
    cache: &CacheStore,
    lib_slot: &SharedLibrary,
    key: &str,
) -> Result<ScoredIcon, VectorizeError> {
    let icon = vectorize_scored(sheet, bbox, bg, preset)?;
    let payload = serde_json::to_vec(&icon).map_err(|e| VectorizeError::Json(e.to_string()))?;
    let guard = lock_lib_any(lib_slot)?;
    let lib = guard.as_ref().ok_or_else(|| {
        VectorizeError::Cache(crate::IsgError::Corrupt(
            "library vanished mid-batch".to_string(),
        ))
    })?;
    cache.put(lib, key, &payload)?;
    Ok(icon)
}

/// Deterministic 16-byte icon id: blake3(sheet content hash ‖ bbox LE).
fn icon_id(sheet_hash_hex: &str, bbox: (u32, u32, u32, u32)) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(sheet_hash_hex.as_bytes());
    for v in [bbox.0, bbox.1, bbox.2, bbox.3] {
        h.update(&v.to_le_bytes());
    }
    let d = h.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&d.as_bytes()[..16]);
    id
}

/// Runs the whole §3.3 pipeline over one sheet: segment → group →
/// vectorize + score every icon in parallel (through the cache) →
/// persist rows when `sheet` is given. Returns the run summary.
pub fn vectorize_sheet_batch(
    bytes: &[u8],
    cache: &CacheStore,
    lib_slot: &SharedLibrary,
    sheet: Option<SheetRef>,
    opts: &BatchOptions,
    token: &CancellationToken,
    progress: &(dyn Fn(u64, u64) + Sync),
) -> Result<BatchSummary, BatchError> {
    // A closed slot would fail every icon; reject up front.
    if lock_lib(lib_slot)?.as_ref().is_none() {
        return Err(BatchError::NoProject);
    }
    let out = segment(bytes, opts.max_dim, &opts.seg).map_err(BatchError::Segment)?;
    let raw_groups = RleCclGrouper {
        min_area: opts.min_area,
    }
    .group_all(&out.sheet, &out.mask);
    // §3.4 F4/F1/F2/F5/F3 + the confidence score. `RefineParams::default()` is
    // disabled, so this is a pass-through until W12 calibrates it against the
    // corpus; the returned stats then feed the UI and the audit trail. The
    // background model goes in so the score can see which detector won.
    let (groups, _refine) =
        refine_groups_with_context(raw_groups, &out.mask, Some(&out.background), &opts.refine);
    let total = groups.len() as u64;
    progress(0, total);

    let watcher = RssWatcher::start(8);
    let rss_floor = current_rss_bytes().unwrap_or(0);

    let seg_str = opts.seg.to_cache_string();
    let done = Arc::new(AtomicU64::new(0));
    let cancelled = || VectorizeError::Cache(crate::IsgError::Cancelled(crate::cancel::Cancelled));
    let items: Vec<Result<ItemResult, VectorizeError>> = groups
        .par_iter()
        .map(|g| -> Result<ItemResult, VectorizeError> {
            token.check().map_err(|_| cancelled())?;
            let key = vectorize_cache_key(&out.sheet, g.bbox, opts.preset, &seg_str);
            // Critical section 1: cache lookup.
            let cached = {
                let guard = lock_lib_any(lib_slot)?;
                let lib = guard.as_ref().ok_or_else(|| {
                    VectorizeError::Cache(crate::IsgError::Corrupt(
                        "library vanished mid-batch".to_string(),
                    ))
                })?;
                cache.get(lib, &key)?
            };
            let (icon, hit) = match cached {
                Some(bytes) => match serde_json::from_slice::<ScoredIcon>(&bytes) {
                    Ok(icon) => (icon, true),
                    // Corrupt payload: recompute and overwrite.
                    Err(_) => (
                        compute_and_store(
                            &out.sheet,
                            g.bbox,
                            &out.background,
                            opts.preset,
                            cache,
                            lib_slot,
                            &key,
                        )?,
                        false,
                    ),
                },
                None => (
                    compute_and_store(
                        &out.sheet,
                        g.bbox,
                        &out.background,
                        opts.preset,
                        cache,
                        lib_slot,
                        &key,
                    )?,
                    false,
                ),
            };
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if d.is_multiple_of(16) || d == total {
                progress(d, total);
            }
            Ok(ItemResult {
                bbox: (g.bbox.x, g.bbox.y, g.bbox.w, g.bbox.h),
                icon: Some(icon),
                cache_key: key,
                hit,
            })
        })
        .collect();

    // Fatal vs per-icon failure split. Cancellation wins.
    if token.is_cancelled() {
        return Err(BatchError::Cancelled);
    }
    let mut results = Vec::with_capacity(items.len());
    let mut failed = 0u32;
    for item in items {
        match item {
            Ok(r) => results.push(r),
            Err(VectorizeError::Cache(crate::IsgError::Cancelled(_))) => {
                return Err(BatchError::Cancelled);
            }
            Err(_) => failed += 1,
        }
    }

    let mut rows = Vec::with_capacity(results.len());
    let mut ok = 0u32;
    let mut cache_hits = 0u32;
    let mut min_ssim = 1.0f32;
    let mut min_composite = 1.0f32;
    let mut sum_composite = 0.0f64;
    for r in &results {
        let Some(icon) = r.icon.as_ref() else {
            continue;
        };
        ok += 1;
        if r.hit {
            cache_hits += 1;
        }
        min_ssim = min_ssim.min(icon.score.ssim);
        min_composite = min_composite.min(icon.score.composite);
        sum_composite += f64::from(icon.score.composite);
        if let Some(sheet_ref) = sheet.as_ref() {
            rows.push(IconVectorRow {
                id: icon_id(&sheet_ref.content_hash, r.bbox),
                bbox: r.bbox,
                svg_key: r.cache_key.clone(),
                preset: super::profiles::profile(opts.preset).doc_name.to_string(),
                mae: icon.score.mae,
                ssim: icon.score.ssim,
                iou: icon.score.iou,
            });
        }
    }
    if let Some(sheet_ref) = sheet.as_ref() {
        let mut guard = lock_lib(lib_slot)?;
        guard
            .as_mut()
            .ok_or(BatchError::NoProject)?
            .replace_sheet_icons(&sheet_ref.id, &rows)
            .map_err(BatchError::Segment)?;
    }

    progress(u64::from(ok), total);
    let peak = watcher.peak().max(rss_floor);
    drop(watcher);
    Ok(BatchSummary {
        icons: results.len() as u32 + failed,
        ok,
        failed,
        cache_hits,
        min_ssim,
        min_composite,
        mean_composite: if ok == 0 {
            1.0
        } else {
            (sum_composite / f64::from(ok)) as f32
        },
        peak_rss_bytes: peak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::NewSheet;
    use image::{ExtendedColorType, ImageEncoder};

    /// 96×48 white sheet with two near-black squares: 16×16 at (8,8) and
    /// 15×15 at (56,24). The crops MUST differ — the stage ⑧ cache is
    /// content-addressed over the crop pixels, so identical crops share one
    /// key and a cold run can legitimately serve the second twin from the
    /// first's put (race between the parallel items). Sizes differ instead
    /// of shades: mono presets render pure ink, so a mid-grey source would
    /// legitimately score ~0.87 SSIM (CI actual, run 35005378111).
    fn sheet_bytes() -> Vec<u8> {
        let mut rgba = vec![255u8; 96 * 48 * 4];
        for (x0, y0, size) in [(8usize, 8usize, 16usize), (56usize, 24usize, 15usize)] {
            for y in y0..y0 + size {
                for x in x0..x0 + size {
                    let i = (y * 96 + x) * 4;
                    rgba[i..i + 3].copy_from_slice(&[10, 10, 10]);
                }
            }
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, 96, 48, ExtendedColorType::Rgba8)
            .expect("png encode");
        png
    }

    fn fixture() -> (CacheStore, SharedLibrary, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "isg-batch-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        (
            CacheStore::new(&dir),
            Mutex::new(Some(Library::open_in_memory().unwrap())),
            dir,
        )
    }

    const HASH: &str = "abcd";

    #[test]
    fn batch_vectorizes_groups_scores_and_persists() {
        let (cache, slot, dir) = fixture();
        let bytes = sheet_bytes();
        {
            let mut guard = lock_lib(&slot).unwrap();
            guard
                .as_mut()
                .unwrap()
                .insert_sheet(&NewSheet {
                    id: [7; 16],
                    source_path: "sheet.png".into(),
                    content_hash: HASH.to_string(),
                    width: 96,
                    height: 48,
                })
                .unwrap();
        }
        let summary = vectorize_sheet_batch(
            &bytes,
            &cache,
            &slot,
            Some(SheetRef {
                id: [7; 16],
                content_hash: HASH.to_string(),
            }),
            &BatchOptions {
                preset: TracePreset::Draft,
                ..BatchOptions::default()
            },
            &CancellationToken::new(),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(summary.icons, 2);
        assert_eq!(summary.ok, 2);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.cache_hits, 0);
        assert!(summary.min_ssim > 0.9, "{summary:?}");
        assert!(summary.min_composite > 0.9, "{summary:?}");
        assert!(summary.peak_rss_bytes > 0);

        let lib = lock_lib(&slot).unwrap();
        let lib = lib.as_ref().unwrap();
        let rows = lib.icons_for_sheet(&[7; 16]).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.preset == "mono-fast"));
        assert!(rows.iter().all(|r| r.ssim > 0.9));
        let ids: Vec<[u8; 16]> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids[0], icon_id(HASH, rows[0].bbox));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn batch_rerun_serves_everything_from_cache() {
        let (cache, slot, dir) = fixture();
        let bytes = sheet_bytes();
        let opts = BatchOptions {
            preset: TracePreset::Draft,
            ..BatchOptions::default()
        };
        let first = vectorize_sheet_batch(
            &bytes,
            &cache,
            &slot,
            None,
            &opts,
            &CancellationToken::new(),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(first.cache_hits, 0);
        let second = vectorize_sheet_batch(
            &bytes,
            &cache,
            &slot,
            None,
            &opts,
            &CancellationToken::new(),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(second.icons, first.icons);
        assert_eq!(second.ok, first.ok);
        assert_eq!(second.cache_hits, second.ok, "everything is cached now");
        assert_eq!(second.min_ssim.to_bits(), first.min_ssim.to_bits());
        assert_eq!(
            second.mean_composite.to_bits(),
            first.mean_composite.to_bits()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_cancelled_token_aborts_the_batch() {
        let (cache, slot, dir) = fixture();
        let token = CancellationToken::new();
        token.cancel();
        let err = vectorize_sheet_batch(
            &sheet_bytes(),
            &cache,
            &slot,
            None,
            &BatchOptions::default(),
            &token,
            &|_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, BatchError::Cancelled), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blank_sheet_yields_zero_icons_and_reports_rss() {
        let (cache, slot, dir) = fixture();
        let mut blank = vec![255u8; 32 * 32 * 4];
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&blank, 32, 32, ExtendedColorType::Rgba8)
            .expect("png encode");
        blank = png;
        let summary = vectorize_sheet_batch(
            &blank,
            &cache,
            &slot,
            None,
            &BatchOptions::default(),
            &CancellationToken::new(),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(summary.icons, 0);
        assert_eq!(summary.ok, 0);
        assert_eq!(summary.min_ssim.to_bits(), 1.0f32.to_bits());
        assert!(summary.peak_rss_bytes > 0, "sync floor still samples");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
