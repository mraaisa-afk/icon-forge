//! Phase 2 exit gate (ARCHITECTURE.md §8): **C1 — 1000+ icons ≤ 90 s,
//! SSIM ≥ 0.97, 0 invalid SVGs, RSS ≤ 2 GB**, plus the two guarantees
//! that ride along per §10: byte-deterministic reruns and a warm run
//! served entirely from the stage ⑧ cache.
//!
//! The gate drives the *production* batch path
//! [`vectorize_sheet_batch`](isg_native::pipeline::vectorize_sheet_batch)
//! over the frozen corpus sheet `bench/corpus/11_c1_batch_grid.png`
//! (4096×4096, 1024 simple mono icons with a hand-verified ground-truth
//! count), with the `mono-fast` preset this corpus entry targets and a
//! real sheet row so every icon persists like it does in the app.
//!
//! "SSIM ≥ 0.97" is enforced as the batch MEAN over all persisted icons
//! (the standard batch-quality reading); the per-icon minimum is a
//! sanity floor and is printed per shape class for calibration.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use isg_core::TracePreset;
use isg_native::cache::CacheStore;
use isg_native::cancel::CancellationToken;
use isg_native::db::{Library, NewSheet};
use isg_native::pipeline::{
    vectorize_sheet_batch, BatchOptions, BatchSummary, ScoredIcon, SharedLibrary, SheetRef,
};
use serde::Deserialize;

/// Hand-verified ground-truth sidecar written by `isg-gen-corpus`.
#[derive(Deserialize)]
struct Truth {
    expected_groups: u32,
    width: u32,
    height: u32,
    icons: Vec<TruthIcon>,
}

#[derive(Deserialize)]
struct TruthIcon {
    bbox: (u32, u32, u32, u32),
    shape: String,
}

/// One persisted icon, enough to compare reruns byte-for-byte.
#[derive(Debug, PartialEq)]
struct Persisted {
    id: [u8; 16],
    bbox: (u32, u32, u32, u32),
    ssim: f32,
    svg: String,
}

const SHEET_ID: [u8; 16] = [0x11; 16];
const TIME_LIMIT: Duration = Duration::from_secs(90);
const RSS_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const MEAN_SSIM_GATE: f32 = 0.97;
const MIN_SSIM_SANITY: f32 = 0.60;

fn corpus(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/corpus");
    (dir.join(format!("{name}.png")), dir.join(format!("{name}.json")))
}

/// Per-test cache root (pid-scoped — parallel tests never share a dir).
fn cache_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("isg-c1-gate-{tag}-{}", std::process::id()))
}

fn open_project(dir: &Path, truth: &Truth, hash_hex: &str) -> (CacheStore, SharedLibrary) {
    let cache = CacheStore::new(dir);
    let slot: SharedLibrary = Mutex::new(Some(Library::open_in_memory().unwrap()));
    let mut guard = slot.lock().unwrap();
    guard
        .as_mut()
        .unwrap()
        .insert_sheet(&NewSheet {
            id: SHEET_ID,
            source_path: "bench/corpus/11_c1_batch_grid.png".to_string(),
            content_hash: hash_hex.to_string(),
            width: truth.width,
            height: truth.height,
        })
        .unwrap();
    drop(guard);
    (cache, slot)
}

fn run(cache: &CacheStore, slot: &SharedLibrary, bytes: &[u8], hash_hex: &str) -> BatchSummary {
    vectorize_sheet_batch(
        bytes,
        cache,
        slot,
        Some(SheetRef {
            id: SHEET_ID,
            content_hash: hash_hex.to_string(),
        }),
        &BatchOptions {
            preset: TracePreset::Draft,
            ..BatchOptions::default()
        },
        &CancellationToken::new(),
        &|_, _| {},
    )
    .unwrap()
}

/// Reads every persisted row, re-parses its cached `ScoredIcon` payload
/// and pushes the SVG through the independent usvg gate — a document
/// that fails here counts against the "0 invalid SVGs" criterion.
fn persisted(cache: &CacheStore, slot: &SharedLibrary) -> Vec<Persisted> {
    let guard = slot.lock().unwrap();
    let lib = guard.as_ref().unwrap();
    let mut rows = lib.icons_for_sheet(&SHEET_ID).unwrap();
    rows.sort_by_key(|r| r.bbox);
    rows.into_iter()
        .map(|r| {
            let payload = cache.get(lib, &r.svg_key).unwrap().expect("cache payload");
            let scored: ScoredIcon = serde_json::from_slice(&payload).unwrap();
            let valid =
                resvg::usvg::Tree::from_str(&scored.svg, &resvg::usvg::Options::default()).is_ok();
            assert!(valid, "SVG at {:?} failed the usvg gate", r.bbox);
            Persisted {
                id: r.id,
                bbox: r.bbox,
                ssim: r.ssim,
                svg: scored.svg,
            }
        })
        .collect()
}

#[test]
fn c1_batch_exit_gate_1024_icons() {
    let (png, json) = corpus("11_c1_batch_grid");
    let bytes = fs::read(png).expect("corpus sheet 11_c1_batch_grid.png exists");
    let truth: Truth =
        serde_json::from_slice(&fs::read(json).expect("corpus truth json exists")).unwrap();
    let hash_hex = blake3::hash(&bytes).to_hex().to_string();

    // --- Cold run 1: the timed exit-gate run. ---
    let dir_a = cache_dir("a");
    let t0 = Instant::now();
    let (cache_a, slot_a) = open_project(&dir_a, &truth, &hash_hex);
    let s1 = run(&cache_a, &slot_a, &bytes, &hash_hex);
    let cold1 = t0.elapsed();
    eprintln!("C1 cold run 1: {s1:?} in {cold1:?}");
    assert_eq!(s1.icons, truth.expected_groups, "grouped count != truth");
    assert_eq!(s1.ok, truth.expected_groups, "successful count != truth");
    assert_eq!(s1.failed, 0, "no icon may fail stages 5-8");
    assert!(cold1 <= TIME_LIMIT, "cold batch took {cold1:?}; limit 90 s");
    assert!(s1.peak_rss_bytes <= RSS_LIMIT, "peak RSS {} > 2 GiB budget", s1.peak_rss_bytes);

    // "0 invalid SVGs" over the persisted set, then full quality stats
    // (printed BEFORE the quality gates so CI logs always carry them).
    let first = persisted(&cache_a, &slot_a);
    assert_eq!(first.len(), truth.expected_groups as usize);
    let mean_ssim = first.iter().map(|p| p.ssim).sum::<f32>() / first.len() as f32;
    eprintln!(
        "C1 quality: mean_ssim={:.4} min_ssim={:.4} min_composite={:.4} mean_composite={:.4}",
        mean_ssim, s1.min_ssim, s1.min_composite, s1.mean_composite
    );
    let mut shape_of: HashMap<(u32, u32, u32, u32), String> = truth
        .icons
        .iter()
        .map(|i| (i.bbox, i.shape.clone()))
        .collect();
    let mut by_shape: BTreeMap<String, (f32, f32, u32)> = BTreeMap::new();
    for p in &first {
        if let Some(sh) = shape_of.remove(&p.bbox) {
            let e = by_shape.entry(sh).or_insert((0.0, f32::MAX, 0));
            e.0 += p.ssim;
            e.1 = e.1.min(p.ssim);
            e.2 += 1;
        }
    }
    for (sh, (sum, min, n)) in &by_shape {
        eprintln!("C1 {sh}: n={n} mean_ssim={:.4} min_ssim={:.4}", sum / *n as f32, min);
    }
    eprintln!("C1 peak RSS: {} MiB (budget 2048)", s1.peak_rss_bytes / (1024 * 1024));
    assert!(
        mean_ssim >= MEAN_SSIM_GATE,
        "mean SSIM {mean_ssim:.4} < the 0.97 exit gate (min {}): {s1:?}",
        s1.min_ssim
    );
    assert!(
        s1.min_ssim >= MIN_SSIM_SANITY,
        "min SSIM {} < the 0.60 sanity floor: {s1:?}",
        s1.min_ssim
    );

    // --- Cold run 2: independent cache — byte-deterministic rerun. ---
    let t1 = Instant::now();
    let (cache_b, slot_b) = open_project(&cache_dir("b"), &truth, &hash_hex);
    let s2 = run(&cache_b, &slot_b, &bytes, &hash_hex);
    let cold2 = t1.elapsed();
    eprintln!("C1 cold run 2: {s2:?} in {cold2:?}");
    assert_eq!(s2.icons, truth.expected_groups);
    assert_eq!(s2.ok, truth.expected_groups);
    assert_eq!(s2.failed, 0);
    assert!(cold2 <= TIME_LIMIT, "rerun took {cold2:?}; limit 90 s");
    let second = persisted(&cache_b, &slot_b);
    assert_eq!(second, first, "a cold rerun must be byte-identical");

    // --- Warm run 3: same cache — everything served from stage ⑧. ---
    let s3 = run(&cache_a, &slot_a, &bytes, &hash_hex);
    eprintln!("C1 warm run: {s3:?}");
    assert_eq!(s3.cache_hits, truth.expected_groups, "warm run must be all cache hits");
    assert_eq!(s3.ok, truth.expected_groups);
    assert_eq!(s3.failed, 0);
    assert_eq!(persisted(&cache_a, &slot_a), first, "cache-served SVGs byte-identical");
}
