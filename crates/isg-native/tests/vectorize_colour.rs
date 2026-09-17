//! Colour exit gate (ARCHITECTURE.md §11 C8): the `Balanced` preset
//! (flat-8 colour clustering) must keep colour — every icon traced from
//! the frozen `bench/corpus/14_c8_colour_icons.png` sheet carries at
//! least two distinct opaque palette colours, every SVG survives the
//! independent usvg re-parse, and quantization does not cost fidelity.
//!
//! The SSIM floor is set at 0.90: corpus shapes are hard-edged flat
//! fills (no anti-aliasing), so near-lossless traces are the honest
//! expectation — a mono collapse of a colour sheet scores far lower.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

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
}

/// One audited icon: bbox, distinct opaque palette colours, and score.
#[derive(Debug, PartialEq)]
struct Audited {
    bbox: (u32, u32, u32, u32),
    distinct: usize,
    ssim: f32,
}

const SHEET_ID: [u8; 16] = [0xc8; 16];
const SSIM_FLOOR: f32 = 0.90;

fn corpus(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/corpus");
    (dir.join(format!("{name}.png")), dir.join(format!("{name}.json")))
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
            source_path: "bench/corpus/14_c8_colour_icons.png".to_string(),
            content_hash: hash_hex.to_string(),
            width: truth.width,
            height: truth.height,
        })
        .unwrap();
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
            preset: TracePreset::Balanced,
            ..BatchOptions::default()
        },
        &CancellationToken::new(),
        &|_, _| {},
    )
    .unwrap()
}

/// Reads every persisted row, re-parses its cached `ScoredIcon` payload,
/// pushes the SVG through the independent usvg gate and counts distinct
/// opaque palette colours — a mono collapse shows up here immediately.
fn audited(cache: &CacheStore, slot: &SharedLibrary) -> Vec<Audited> {
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
            let mut fills = scored.palette.clone();
            fills.sort_unstable();
            fills.dedup();
            let distinct = fills.iter().filter(|c| c[3] != 0).count();
            Audited {
                bbox: r.bbox,
                distinct,
                ssim: r.ssim,
            }
        })
        .collect()
}

#[test]
fn c8_colour_survives_balanced() {
    let (png, json) = corpus("14_c8_colour_icons");
    let bytes = fs::read(png).expect("corpus sheet 14_c8_colour_icons.png exists");
    let truth: Truth =
        serde_json::from_slice(&fs::read(json).expect("corpus truth json exists")).unwrap();
    let hash_hex = blake3::hash(&bytes).to_hex().to_string();

    let dir = std::env::temp_dir().join(format!("isg-c8-gate-{}", std::process::id()));
    let t0 = Instant::now();
    let (cache, slot) = open_project(&dir, &truth, &hash_hex);
    let summary = run(&cache, &slot, &bytes, &hash_hex);
    eprintln!("C8 batch: {summary:?} in {:?}", t0.elapsed());
    assert_eq!(summary.icons, truth.expected_groups, "grouped count != truth");
    assert_eq!(summary.ok, truth.expected_groups);
    assert_eq!(summary.failed, 0);
    let m = summary.min_ssim;
    assert!(m >= SSIM_FLOOR, "min SSIM {m} below the colour floor: {summary:?}");

    // Colour must survive: a mono collapse would leave one fill colour.
    let icons = audited(&cache, &slot);
    assert_eq!(icons.len(), truth.expected_groups as usize);
    let mut min_distinct = usize::MAX;
    for icon in &icons {
        min_distinct = min_distinct.min(icon.distinct);
        assert!(icon.distinct >= 2, "icon {icon:?} collapsed to {} fill(s)", icon.distinct);
        assert!(icon.ssim >= SSIM_FLOOR, "icon {icon:?} SSIM {} below floor", icon.ssim);
    }
    let mean_ssim = icons.iter().map(|i| i.ssim).sum::<f32>() / icons.len() as f32;
    eprintln!(
        "C8 quality: min_ssim={:.4} mean_ssim={:.4} min_distinct_fills={min_distinct}",
        summary.min_ssim, mean_ssim
    );

    // Determinism rides along here too: same bytes, same SVGs.
    let again = audited(&cache, &slot);
    let warm = run(&cache, &slot, &bytes, &hash_hex);
    eprintln!("C8 warm run: {warm:?}");
    assert_eq!(warm.cache_hits, truth.expected_groups, "warm run must be all cache hits");
    assert_eq!(again, icons, "cache-served SVGs must be byte-identical");
}
