//! Phase 7 (perf/memory): a long session must stay inside its budget.
//!
//! Phase 2's gate bounds the memory of *one* batch. The app is not one batch —
//! it is a process a user leaves open while importing sheet after sheet, and the
//! failure that matters there is cumulative: a cache, a thread pool, or an arena
//! that keeps one sheet's working set per import eventually takes the process
//! down, hours in, with the user's session in it.
//!
//! This test runs a short session in a single process — six different sheets
//! through one open library, so every sheet is a cold trace — and reads the
//! process's own RSS (the same source the app's UI reports). Two things it
//! deliberately does *not* do:
//!
//! * It does not run beside other tests. RSS is per-process, so this suite lives
//!   in its own test binary and other suites cannot inflate it.
//! * It does not assert a tight growth number. Allocators do not return freed
//!   pages reliably, so a few tens of MB of growth is behaviour, not leakage;
//!   the assertion is the ceiling that a real leak crosses, and the measurements
//!   are printed every run so drift is visible long before it is fatal.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use isg_core::TracePreset;
use isg_native::cancel::CancellationToken;
use isg_native::db::{Library, NewSheet};
use isg_native::pipeline::{vectorize_sheet_batch, BatchOptions, SharedLibrary, SheetRef};
use isg_native::rss::{current_rss_bytes, RssWatcher};

/// Same ceiling as the Phase-2 batch gate: the session may not exceed what one
/// batch is allowed, which is the promise the app makes to a 4 GB machine.
const SESSION_RSS_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// Growth allowed between the first sheet and the last, beyond which the run is
/// holding on to something per-sheet that it should have released. Sized to
/// catch a leak worth a bug report (≥100 MB per import over five imports) while
/// leaving allocator fragmentation out of the verdict.
const GROWTH_LIMIT: u64 = 512 * 1024 * 1024;

/// A session's worth of sheets: small ones (9–36 icons) so the gate is fast,
/// different shapes and layouts so no single code path dominates.
const SESSION: [&str; 6] = [
    "01_basic_grid",
    "02_mixed_grid",
    "03_rings_holes",
    "04_scattered",
    "06_thick_strokes",
    "15_c9_duplicates",
];

fn corpus(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/corpus");
    (
        dir.join(format!("{name}.png")),
        dir.join(format!("{name}.json")),
    )
}

fn sheet_id(i: usize) -> [u8; 16] {
    let mut id = [0x71u8; 16];
    id[..8].copy_from_slice(&(i as u64).to_le_bytes());
    id
}

#[test]
fn a_session_of_imports_stays_inside_the_memory_budget() {
    let dir = std::env::temp_dir().join(format!("isg-long-session-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");

    let cache = isg_native::cache::CacheStore::new(dir.join("cache"));
    let slot: SharedLibrary = Mutex::new(Some(Library::open_in_memory().expect("library")));

    let watcher = RssWatcher::start(8);
    let mut readings: Vec<(String, u64)> = Vec::new();

    for (i, name) in SESSION.iter().enumerate() {
        let (png, json) = corpus(name);
        let bytes = fs::read(&png).unwrap_or_else(|e| panic!("corpus {png:?}: {e}"));
        let truth: serde_json::Value =
            serde_json::from_slice(&fs::read(&json).expect("corpus truth json"))
                .expect("truth json");
        let width = truth["width"].as_u64().unwrap_or(0) as u32;
        let height = truth["height"].as_u64().unwrap_or(0) as u32;
        let hash_hex = blake3::hash(&bytes).to_hex().to_string();
        let id = sheet_id(i);

        {
            let mut guard = slot.lock().expect("library slot");
            guard
                .as_mut()
                .expect("library")
                .insert_sheet(&NewSheet {
                    id,
                    source_path: name.to_string(),
                    content_hash: hash_hex.clone(),
                    width,
                    height,
                })
                .expect("insert sheet");
        }

        let summary = vectorize_sheet_batch(
            &bytes,
            &cache,
            &slot,
            Some(SheetRef {
                id,
                content_hash: hash_hex,
            }),
            &BatchOptions {
                preset: TracePreset::Pixel,
                ..BatchOptions::default()
            },
            &CancellationToken::new(),
            &|_, _| {},
        )
        .expect("vectorize");

        assert!(
            summary.icons > 0 && summary.failed == 0,
            "{name}: icons={} failed={}",
            summary.icons,
            summary.failed
        );
        // The expected count comes from the same sidecar the other gates use.
        let expected = truth["expected_groups"].as_u64().unwrap_or(0) as u32;
        assert_eq!(summary.icons, expected, "{name}: grouped count != truth");

        readings.push(((*name).to_string(), current_rss_bytes().unwrap_or(0)));
    }

    let peak = watcher.peak();
    let warm = readings.first().map(|r| r.1).unwrap_or(0);
    let end = readings.last().map(|r| r.1).unwrap_or(0);
    let growth = end.saturating_sub(warm);
    let mb = |bytes: u64| bytes / (1024 * 1024);

    let per_sheet: Vec<String> = readings
        .iter()
        .map(|(name, rss)| format!("{name}={}MB", mb(*rss)))
        .collect();
    eprintln!(
        "evidence: phase7 M1 session_sheets={} warm_mb={} end_mb={} growth_mb={} peak_mb={} \
         growth_budget_mb={} peak_budget_mb={} per_sheet=[{}]",
        SESSION.len(),
        mb(warm),
        mb(end),
        mb(growth),
        mb(peak),
        mb(GROWTH_LIMIT),
        mb(SESSION_RSS_LIMIT),
        per_sheet.join(" ")
    );

    // A measurement that never ran is not a pass: this platform reports RSS.
    assert!(warm > 0 && end > 0, "RSS was not readable on this platform");
    assert!(
        peak <= SESSION_RSS_LIMIT,
        "session peak RSS {} MiB exceeds the {} MiB budget",
        mb(peak),
        mb(SESSION_RSS_LIMIT)
    );
    assert!(
        growth <= GROWTH_LIMIT,
        "RSS grew {} MiB across {} imports (budget {} MiB) — something is held per sheet",
        mb(growth),
        SESSION.len(),
        mb(GROWTH_LIMIT)
    );

    drop(slot);
    let _ = fs::remove_dir_all(&dir);
}
