//! Phase-3 W11 gates: §3.4 confidence scoring + the cached-mask slider path.
//!
//! Both halves run through the **real** pipeline (`segment` → RLE CCL →
//! `refine_groups_with_context`), with the refine chain enabled, exactly as the
//! app calls it. What this file proves:
//!
//! * every PNG sheet scores in `[0, 1]`, deterministically, with the review list
//!   exactly as long as the count the §3.4 status line reports;
//! * the sheets that needed work say so — `09_near_touching` scores 0.91 because
//!   F5 had to undo three of F1's merges (six groups), `07_size_range` loses
//!   points to real size spread, `10_edge_corner` to icons on the border;
//! * the unaffected sheets score exactly 1.0 (no deduction is invented for a
//!   clean grouping);
//! * the sensitivity-slider path (`mask_cached`) answers the second call from
//!   the cached mask — no decode, no segmentation — and `regroup_cached`
//!   reproduces the cold grouping byte-for-byte.
//!
//! Every number asserted below was measured locally through the same real
//! sources before the assertion was written. `13_c7_jpeg_grid` stays out (no
//! run-produced number for it was ever observed here).

use std::fs;
use std::path::{Path, PathBuf};

use isg_core::{ForegroundMask, GroupingStrategy, IconGroup};
use isg_native::pipeline::{
    mask_cached, regroup_cached, segment, summary_line, MaskCache, RefineParams, RefineStats,
    RleCclGrouper, SegParams, WarningKind,
};
use serde::Deserialize;

/// The parts of a corpus truth sidecar this gate reads (serde ignores the rest).
#[derive(Debug, Deserialize)]
struct Truth {
    expected_groups: u32,
    width: u32,
    height: u32,
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/corpus")
}

fn load(name: &str) -> (Vec<u8>, Truth) {
    let dir = corpus_dir();
    let bytes = fs::read(dir.join(format!("{name}.png")))
        .unwrap_or_else(|e| panic!("corpus sheet {name}.png: {e}"));
    let truth: Truth = serde_json::from_slice(
        &fs::read(dir.join(format!("{name}.json"))).unwrap_or_else(|e| panic!("{name}.json: {e}")),
    )
    .expect("truth sidecar parses");
    (bytes, truth)
}

/// The fifteen PNG sheets (same set as the W10 count gate).
const PNG_SHEETS: [&str; 15] = [
    "01_basic_grid",
    "02_mixed_grid",
    "03_rings_holes",
    "04_scattered",
    "05_dense_grid",
    "06_thick_strokes",
    "07_size_range",
    "08_grid_drift",
    "09_near_touching",
    "10_edge_corner",
    "11_c1_batch_grid",
    "12_c2_latency_grid",
    "14_c8_colour_icons",
    "15_c9_duplicates",
    "16_c10_noisy_scan",
];

/// The sheets whose grouping needs no correction at all: measured score 1.000.
const CLEAN_SHEETS: [&str; 12] = [
    "01_basic_grid",
    "02_mixed_grid",
    "03_rings_holes",
    "04_scattered",
    "05_dense_grid",
    "06_thick_strokes",
    "08_grid_drift",
    "11_c1_batch_grid",
    "12_c2_latency_grid",
    "14_c8_colour_icons",
    "15_c9_duplicates",
    "16_c10_noisy_scan",
];

fn refine_params() -> RefineParams {
    RefineParams {
        enabled: true,
        ..RefineParams::default()
    }
}

/// The app's grouping call: stages ①–③ → RLE CCL → the refine chain (+ score).
fn group_sheet(
    name: &str,
    params: &RefineParams,
) -> (Vec<u8>, ForegroundMask, Vec<IconGroup>, RefineStats, Truth) {
    let (bytes, truth) = load(name);
    let out = segment(&bytes, 4096, &SegParams::default())
        .unwrap_or_else(|e| panic!("segment {name}: {e:?}"));
    assert_eq!(out.sheet.width(), truth.width, "{name}: width");
    assert_eq!(out.sheet.height(), truth.height, "{name}: height");
    let raw = RleCclGrouper::default().group_all(&out.sheet, &out.mask);
    let (groups, stats) = isg_native::pipeline::refine_groups_with_context(
        raw,
        &out.mask,
        Some(&out.background),
        params,
    );
    assert_eq!(
        groups.len(),
        truth.expected_groups as usize,
        "{name}: group count"
    );
    (bytes, out.mask, groups, stats, truth)
}

#[test]
fn scores_are_in_range_and_repeat_exactly() {
    let params = refine_params();
    let mut scores = String::new();
    for name in PNG_SHEETS {
        let (_bytes, _mask, groups, stats, _truth) = group_sheet(name, &params);
        let report = &stats.confidence;
        assert!(
            (0.0..=1.0).contains(&report.score),
            "{name}: score {} out of range",
            report.score
        );
        assert_eq!(
            report.review_groups as usize,
            report.warnings.len(),
            "{name}: the status-line count must equal the warning list"
        );
        for w in &report.warnings {
            assert!(
                (w.group as usize) < groups.len(),
                "{name}: warning index {} out of range",
                w.group
            );
        }
        scores.push_str(&format!("{name}={:.3} ", report.score));
    }
    eprintln!("evidence: W11 scores {scores}");

    // Repeating the whole chain reproduces the groups, the score, the signals
    // and the review list — the score reads only deterministic evidence.
    for name in ["09_near_touching", "07_size_range", "10_edge_corner"] {
        let (_, _, g1, s1, _) = group_sheet(name, &params);
        let (_, _, g2, s2, _) = group_sheet(name, &params);
        assert_eq!(g1, g2, "{name}: groups repeat");
        assert_eq!(s1.confidence.score, s2.confidence.score, "{name}: score");
        assert_eq!(s1.confidence.signals, s2.confidence.signals, "{name}");
        assert_eq!(s1.confidence.warnings, s2.confidence.warnings, "{name}");
    }
}

#[test]
fn the_sheets_that_needed_work_score_below_one() {
    let params = refine_params();

    // 09_near_touching: F1 glued three near-touching pairs, F5 proved each wrong
    // and handed six original groups back — six review items, 0.30 of spans.
    let (_b, _mask, _groups, stats, _t) = group_sheet("09_near_touching", &params);
    let near = stats.confidence.clone();
    assert_eq!(stats.grid.restored, 3, "three provenance restorations");
    // Captured before the later sheets shadow `stats` — the evidence line below
    // must report this sheet's own restoration count.
    let near_restored = stats.grid.restored;
    assert_eq!(near.review_groups, 6, "{near:?}");
    assert!(
        near.warnings
            .iter()
            .all(|w| w.kind == WarningKind::RestoredFromMerge),
        "{near:?}"
    );
    assert!(
        (near.signals.spans - 0.30).abs() < 1e-6,
        "6 of 20 groups are lattice flags: {near:?}"
    );
    assert!(
        (near.score - 0.91).abs() < 1e-3,
        "one 0.30-weight signal at 0.30 ⇒ 0.91: {near:?}"
    );
    assert_eq!(near.percent(), 91);

    // 07_size_range: real size spread (6 px … 60 px icons) moves the size MAD.
    let (_b, _mask, _groups, stats, _t) = group_sheet("07_size_range", &params);
    let sizes = stats.confidence.clone();
    assert!(
        sizes.signals.size_mad > 0.0,
        "a range of icon sizes must show up: {sizes:?}"
    );
    assert!(sizes.score < 1.0, "{sizes:?}");

    // 10_edge_corner: most groups sit on the sheet border by construction.
    let (_b, _mask, _groups, stats, _t) = group_sheet("10_edge_corner", &params);
    let border = stats.confidence.clone();
    assert!(border.signals.border_touch > 0.6, "{border:?}");
    assert!(border.score < 1.0, "{border:?}");

    eprintln!(
        "evidence: W11 deductions 09_near_touching score={:.3} review={} restored={} | 07_size_range score={:.3} mad={:.2} | 10_edge_corner score={:.3} border={:.2}",
        near.score, near.review_groups, near_restored,
        sizes.score, sizes.signals.size_mad,
        border.score, border.signals.border_touch
    );
}

#[test]
fn a_clean_grouping_is_never_penalised() {
    let params = refine_params();
    let mut lines = String::new();
    for name in CLEAN_SHEETS {
        let (_b, _mask, _g, stats, _t) = group_sheet(name, &params);
        assert_eq!(
            stats.confidence.score, 1.0,
            "{name} needs no correction and must score exactly 1.0: {:?}",
            stats.confidence.signals
        );
        assert_eq!(stats.confidence.review_groups, 0, "{name}");
        lines.push_str(&format!("{name}={:.3} ", stats.confidence.score));
    }
    eprintln!("evidence: W11 clean sheets at 1.000 — {lines}");
}

#[test]
fn slider_reruns_come_from_the_cached_mask() {
    let params = refine_params();
    let parsed = SegParams::default();
    let name = "12_c2_latency_grid";
    let (bytes, _truth) = load(name);

    // Cold: the full path the app runs when the sheet is opened.
    let t0 = std::time::Instant::now();
    let out = segment(&bytes, 4096, &parsed).expect("segment");
    let raw = RleCclGrouper::default().group_all(&out.sheet, &out.mask);
    let (cold_groups, _cold_stats) = isg_native::pipeline::refine_groups_with_context(
        raw,
        &out.mask,
        Some(&out.background),
        &params,
    );
    let cold_ms = t0.elapsed().as_secs_f32() * 1000.0;

    let mut cache = MaskCache::new(4);
    // First slider tick: miss ⇒ stage ①–③ run and the mask is stored.
    let (mask_a, _bg_a, hit_a) =
        mask_cached(&bytes, 4096, &parsed, &mut cache).expect("mask_cached cold");
    assert!(!hit_a, "first call must be a miss");
    // Second tick: hit ⇒ no decode at all.
    let t1 = std::time::Instant::now();
    let (mask_b, _bg_b, hit_b) =
        mask_cached(&bytes, 4096, &parsed, &mut cache).expect("mask_cached warm");
    let warm_seg_ms = t1.elapsed().as_secs_f32() * 1000.0;
    assert!(hit_b, "second call must hit the cache");
    assert_eq!(mask_a, mask_b, "the cached mask is the same mask");
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);

    // The slider's real work: regroup from the cached mask.
    let key = isg_native::pipeline::mask_key(&bytes, &parsed);
    let warm = regroup_cached(&key, &mut cache, &params).expect("cached regroup");
    assert_eq!(
        warm.groups, cold_groups,
        "the cached regrouping must equal the cold one"
    );

    eprintln!(
        "evidence: W11 slider {name} cold_ms={cold_ms:.1} warm_seg_ms={warm_seg_ms:.1} warm_regroup_ms={:.1} groups={} identical=true cache_masks={} cache_bytes={} (spec target 50–200 ms)",
        warm.elapsed_ms,
        warm.groups.len(),
        cache.len(),
        cache.bytes()
    );
    assert!(
        warm.elapsed_ms < 500.0,
        "a slider rerun must stay well inside the interactive budget: {:.1} ms",
        warm.elapsed_ms
    );
    assert!(
        warm.elapsed_ms * 4.0 < cold_ms,
        "cached regrouping must be far cheaper than the cold path: {:.1} ms vs {cold_ms:.1} ms",
        warm.elapsed_ms
    );
}

#[test]
fn status_lines_are_well_formed() {
    let params = refine_params();
    let mut lines = String::new();
    for name in ["09_near_touching", "07_size_range", "03_rings_holes"] {
        let (_b, _mask, groups, stats, _t) = group_sheet(name, &params);
        let report = &stats.confidence;
        let line = summary_line(groups.len() as u32, stats.elapsed_ms, report);
        assert!(
            line.starts_with(&format!("Grouped {} icons in ", groups.len())),
            "{line}"
        );
        assert!(
            line.contains(&format!("· confidence {}% ·", report.percent())),
            "{line}"
        );
        assert!(
            line.ends_with("groups need review") || line.ends_with("group needs review"),
            "{line}"
        );
        lines.push_str(&format!("[{line}] "));
    }
    eprintln!("evidence: W11 status lines {lines}");
}
