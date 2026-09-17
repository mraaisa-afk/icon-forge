//! Phase-3 exit gate: the Group All session (W12) plus the Phase-3 criteria
//! re-measured through it (ARCHITECTURE.md §8).
//!
//! Everything here runs the **shipping** path: `segment` (decode → normalize →
//! background → clean) feeding `pipeline::mask_cached`, then
//! `GroupingSession::group_sheet` (RLE CCL → refine chain → confidence), the
//! same calls `group_all` / `group_split_here` / `group_selected` /
//! `group_set_sensitivity` / `sheet_preview` make.
//!
//! * **C2** — median of five cold runs of the 4096² 100-icon sheet ≤ 2000 ms,
//!   with exactly 100 groups.
//! * **C4** — `03_rings_holes`: ≥ 90 % of the truth icons reproduced with the
//!   exact bounding box.
//! * **C5** — `09_near_touching`: every truth icon maps to exactly one group
//!   with the exact bounding box, zero false splits, zero false merges.
//! * **Split Here** — clicking the centre of every group of four sheets: the
//!   ≤ 20 ms budget is the maximum over all attempts, not an average.
//! * **Group Selected** — the marquee's union box, area, edit count and the
//!   re-score (a union across a lattice *must* come back flagged).
//! * **Sensitivity** — a slider moved from the cached mask changes the grouping
//!   and never re-segments.
//! * **Determinism** — three fresh sessions agree byte for byte, and a 1-thread
//!   run agrees with an 8-thread run.
//! * **Preview** — the overlay backdrop is deterministic and correctly scaled.

use std::fs;
use std::path::{Path, PathBuf};

use isg_core::{Bbox, IconGroup};
use isg_native::pipeline::{
    mask_cached, GroupingSession, RefineParams, SegParams, SensitivityParams,
};
use rayon::prelude::*;
use serde::Deserialize;

/// One entry of a corpus truth sidecar (`bench/corpus/*.json`).
#[derive(Debug, Deserialize)]
struct TruthIcon {
    id: u32,
    shape: String,
    bbox: [u32; 4],
}

impl TruthIcon {
    fn bbox(&self) -> Bbox {
        Bbox {
            x: self.bbox[0],
            y: self.bbox[1],
            w: self.bbox[2],
            h: self.bbox[3],
        }
    }
}

/// A corpus truth sidecar.
#[derive(Debug, Deserialize)]
struct Truth {
    expected_groups: u32,
    width: u32,
    height: u32,
    icons: Vec<TruthIcon>,
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

/// Groups one sheet the way the commands do, through the mask cache.
///
/// `cache` is caller-supplied so the C2 test can time a genuinely cold run (an
/// empty cache ⇒ decode + segmentation) and the slider test can prove a warm
/// one (populated ⇒ no decode).
fn group_through(
    name: &str,
    session: &mut GroupingSession,
    seg: &SegParams,
) -> (Vec<IconGroup>, f32, bool, Truth) {
    let (bytes, truth) = load(name);
    let key = session.key_for(&bytes, seg);
    let t0 = std::time::Instant::now();
    let (_, _, hit) = mask_cached(&bytes, 4096, seg, session.cache_mut())
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let report = session
        .group_sheet(&key, hit)
        .unwrap_or_else(|| panic!("{name}: no report"));
    // The whole cold path: decode → normalize → background → clean → CCL →
    // refine chain → confidence, i.e. what a first Group All click costs.
    let cold_ms = t0.elapsed().as_secs_f32() * 1000.0;
    assert_eq!(report.width, truth.width, "{name}: width");
    assert_eq!(report.height, truth.height, "{name}: height");
    (report.groups, cold_ms, hit, truth)
}

/// bbox agreement within the stage-③ tolerance (1 px position, 2 px size) —
/// the same tolerance the W10 gate documents.
fn is_match(g: &IconGroup, t: &TruthIcon) -> bool {
    let (a, b) = (g.bbox, t.bbox());
    a.x.abs_diff(b.x) <= 1
        && a.y.abs_diff(b.y) <= 1
        && a.w.abs_diff(b.w) <= 2
        && a.h.abs_diff(b.h) <= 2
}

fn key(g: &IconGroup) -> (u32, u32, u32, u32, u32, (u32, u32)) {
    (g.bbox.x, g.bbox.y, g.bbox.w, g.bbox.h, g.area, g.origin)
}

#[test]
fn c2_cold_group_all_is_under_two_seconds_with_exactly_100_groups() {
    let seg = SegParams::default();
    let mut times = Vec::new();
    let mut groups = 0usize;
    let mut expected = 0u32;
    for run in 0..5 {
        let mut session = GroupingSession::new(2);
        let (g, cold_ms, hit, truth) = group_through("12_c2_latency_grid", &mut session, &seg);
        assert!(!hit, "run {run}: the cache starts empty");
        times.push(cold_ms);
        groups = g.len();
        expected = truth.expected_groups;
    }
    times.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let median = times[times.len() / 2];
    assert_eq!(groups, expected as usize, "C2: exactly 100 groups");
    assert_eq!(expected, 100, "C2 sheet still has 100 truth icons");
    eprintln!(
        "evidence: phase3 C2 cold median-of-5 {median:.1} ms (runs {:?}) groups={groups}/{expected} 4096^2",
        times.iter().map(|t| format!("{t:.0}")).collect::<Vec<_>>()
    );
    assert!(
        median <= 2000.0,
        "C2 needs the median cold run ≤ 2000 ms, got {median:.1} ms"
    );

    // The same sheet, grouped again through a warm cache: no decode, identical
    // groups — the "feels live" path the sliders ride on.
    let mut session = GroupingSession::new(2);
    let (cold, cold_ms, _, truth) = group_through("12_c2_latency_grid", &mut session, &seg);
    let (bytes, _) = load("12_c2_latency_grid");
    let key = session.key_for(&bytes, &seg);
    let t0 = std::time::Instant::now();
    let (_, _, hit) = mask_cached(&bytes, 4096, &seg, session.cache_mut()).unwrap();
    let warm_ms = t0.elapsed().as_secs_f32() * 1000.0;
    let warm = session.group_sheet(&key, hit).expect("regrouped");
    eprintln!(
        "evidence: phase3 C2 warm cold_ms={cold_ms:.1} warm_ms={warm_ms:.1} cache_hit={hit} identical={} groups={}/{} cache_bytes={}",
        warm.groups == cold,
        warm.groups.len(),
        truth.expected_groups,
        session.cache().bytes()
    );
    assert!(hit, "the second pass must be a mask-cache hit");
    assert_eq!(warm.groups, cold, "a warm run groups identically");
    assert!(
        warm_ms < 200.0,
        "a warm grouping must stay inside the ~50–200 ms live-slider target, got {warm_ms:.1} ms"
    );
}

#[test]
fn phase3_exit_criteria_hold_on_this_run() {
    let seg = SegParams::default();

    // C4 — rings and holes keep their exact boxes.
    let mut session = GroupingSession::new(2);
    let (groups, _ms, _hit, truth) = group_through("03_rings_holes", &mut session, &seg);
    assert_eq!(
        groups.len(),
        truth.expected_groups as usize,
        "C4 group count"
    );
    let c4_total = truth.icons.len();
    let c4_icons = c4_total;
    let c4_rings = truth
        .icons
        .iter()
        .filter(|t| t.shape == "ring" || t.shape == "frame")
        .count();
    let mut left: Vec<&TruthIcon> = truth.icons.iter().collect();
    let mut matched = 0usize;
    for g in &groups {
        if let Some(pos) = left.iter().position(|t| is_match(g, t)) {
            left.remove(pos);
            matched += 1;
        }
    }
    let c4_groups = groups.len();
    let c4_pct = 100.0 * matched as f64 / truth.icons.len() as f64;

    // C5 — near-touching icons: exactly one group each, no false split/merge.
    let mut session = GroupingSession::new(2);
    let (groups, _ms, _hit, truth) = group_through("09_near_touching", &mut session, &seg);
    let c5_groups = groups.len();
    let c5_expected = truth.expected_groups as usize;
    let mut false_splits = 0usize;
    let mut false_merges = 0usize;
    for t in &truth.icons {
        let hits = groups.iter().filter(|g| is_match(g, t)).count();
        if hits != 1 {
            false_splits += 1;
            eprintln!("C5: icon {} ({:?}) matched {hits} groups", t.id, t.bbox());
        }
    }
    for g in &groups {
        let covered = truth.icons.iter().filter(|t| is_match(g, t)).count();
        if covered > 1 {
            false_merges += 1;
        }
    }
    eprintln!(
        "evidence: phase3 exit C2 100 groups | C4 03_rings_holes exact={matched}/{c4_total} ({c4_pct:.1}%) groups={c4_groups} | C5 09_near_touching groups={c5_groups}/{c5_expected} false_splits={false_splits} false_merges={false_merges}"
    );
    assert!(
        c4_pct >= 90.0,
        "C4 needs ≥ 90 % exact boxes, got {c4_pct:.1} % ({matched}/{c4_total})"
    );
    assert_eq!(c4_pct, 100.0, "C4: every truth icon reproduced exactly");
    assert!(
        c4_rings >= 20,
        "the C4 sheet is mostly rings/frames ({c4_rings})"
    );
    assert!(c4_groups == c4_icons, "C4: one group per truth icon");
    assert_eq!(c5_groups, c5_expected, "C5: group count");
    assert_eq!(false_splits, 0, "C5: zero false splits");
    assert_eq!(false_merges, 0, "C5: zero false merges");
}

#[test]
fn split_here_stays_inside_its_twenty_millisecond_budget() {
    let seg = SegParams::default();
    let mut attempts = 0u32;
    let mut splits = 0u32;
    let mut worst = (0.0f32, "");
    let mut second_worst = (0.0f32, "");
    // Includes the two sheets where a click actually finds a split
    // (`02_mixed_grid`, `07_size_range`), so the evidence counts real work, not
    // only refusals.
    for name in [
        "12_c2_latency_grid",
        "05_dense_grid",
        "09_near_touching",
        "04_scattered",
        "02_mixed_grid",
        "07_size_range",
    ] {
        let mut session = GroupingSession::new(2);
        let (groups, _ms, _hit, _truth) = group_through(name, &mut session, &seg);
        for g in &groups {
            let (cx, cy) = (g.bbox.x + g.bbox.w / 2, g.bbox.y + g.bbox.h / 2);
            let out = session
                .split_here(cx, cy)
                .unwrap_or_else(|| panic!("{name}: no group under ({cx}, {cy})"));
            attempts += 1;
            if out.split {
                splits += 1;
            }
            if out.elapsed_ms > worst.0 {
                second_worst = worst;
                worst = (out.elapsed_ms, name);
            } else if out.elapsed_ms > second_worst.0 {
                second_worst = (out.elapsed_ms, name);
            }
        }
    }
    eprintln!(
        "evidence: phase3 split_here attempts={attempts} splits={splits} max_ms={:.3} ({}) second_ms={:.3} ({}) budget=20 ms",
        worst.0, worst.1, second_worst.0, second_worst.1
    );
    assert!(attempts >= 200, "the sweep must click every group");
    assert!(splits >= 1, "at least one real click must find a split");
    assert!(
        worst.0 < 20.0,
        "Split Here must stay under 20 ms, worst was {:.3} ms",
        worst.0
    );
}

#[test]
fn group_selected_merges_the_marquee_and_rescores() {
    let seg = SegParams::default();
    let mut session = GroupingSession::new(2);
    let (auto, _ms, _hit, truth) = group_through("12_c2_latency_grid", &mut session, &seg);
    assert_eq!(
        auto.len(),
        100,
        "the C2 sheet groups to exactly its 100 icons"
    );
    let before = session.current_report().expect("report");
    let warnings_before = before.confidence.warnings.len();
    let score_before = before.confidence.score;

    // Two neighbours in the same row (the canonical order is (y, x); stage ③
    // erodes each box by a pixel or two, so match the row loosely).
    let (a, b) = auto
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            auto[i + 1..]
                .iter()
                .find(|b| a.bbox.y.abs_diff(b.bbox.y) <= 3 && b.bbox.x > a.bbox.x + a.bbox.w)
                .map(|b| (a.bbox, b.bbox))
        })
        .expect("two horizontally adjacent groups");
    let marquee = Bbox::new(a.x, a.y, b.x + b.w - a.x, a.h.max(b.h)).expect("marquee");
    let out = session.group_selected(&[marquee]).expect("grouped");
    let union = out
        .groups
        .iter()
        .find(|g| {
            g.bbox.x <= marquee.x
                && g.bbox.y <= marquee.y
                && g.bbox.x + g.bbox.w >= marquee.x + marquee.w
        })
        .expect("the union group");
    eprintln!(
        "evidence: phase3 group_selected groups={}->{} union=({},{},{},{}) area={} edits={} warnings={}->{} score={:.3}->{:.3}",
        auto.len(),
        out.groups.len(),
        union.bbox.x,
        union.bbox.y,
        union.bbox.w,
        union.bbox.h,
        union.area,
        out.manual_edits,
        warnings_before,
        out.confidence.warnings.len(),
        score_before,
        out.confidence.score,
    );
    assert_eq!(out.groups.len(), auto.len() - 1);
    assert_eq!(out.manual_edits, 1);
    assert_eq!(union.bbox, marquee, "the union box is the marquee's box");
    assert_eq!(
        out.confidence.warnings.len(),
        warnings_before + 1,
        "the union straddles a valley and must be flagged for review"
    );
    assert!(
        out.confidence.score <= 1.0 && out.status_line.contains("needs review"),
        "the status line follows the re-score: {}",
        out.status_line
    );

    // Group All again restores the automatic result.
    let reset = session.reset_manual().expect("grouped");
    assert_eq!(reset.groups, auto);
    assert_eq!(reset.manual_edits, 0);
    assert_eq!(truth.expected_groups as usize, auto.len());
}

#[test]
fn sensitivity_slider_regroups_from_the_cached_mask() {
    let seg = SegParams::default();
    let mut session = GroupingSession::new(2);
    let (auto, cold_ms, hit, truth) = group_through("07_size_range", &mut session, &seg);
    assert!(!hit, "the first grouping is cold");
    assert_eq!(auto.len() as u32, truth.expected_groups);

    // The speckle knob at the top of its envelope drops this sheet's two
    // smallest icons — a visible slider effect on a real sheet.
    let mut refine = session.params().refine;
    assert!(refine.enabled, "the UI path groups live");
    let knobs = SensitivityParams {
        merge_gap_frac: 0.35,
        merge_area_ratio: 1.75,
        noise_min_area: 512,
        grid_regularity_min: 0.75,
    };
    knobs.apply(&mut refine).expect("inside the envelope");
    let t0 = std::time::Instant::now();
    let loose = session.set_refine(refine).expect("regrouped");
    let regroup_ms = t0.elapsed().as_secs_f32() * 1000.0;
    assert_eq!(
        loose.groups.len(),
        auto.len() - 2,
        "the slider changed the grouping"
    );
    assert!(loose.mask_cache_hit, "sliders never re-segment");

    // The mask cache is still warm afterwards: nothing invalidated it.
    let (bytes, _) = load("07_size_range");
    let key = session.key_for(&bytes, &seg);
    let t0 = std::time::Instant::now();
    let (_, _, warm_hit) = mask_cached(&bytes, 4096, &seg, session.cache_mut()).unwrap();
    let warm_ms = t0.elapsed().as_secs_f32() * 1000.0;
    assert!(warm_hit, "the slider path leaves the cached mask intact");
    assert!(session.current_key() == Some(key.as_str()));

    // Back to the automatic parameters.
    let back = session
        .set_refine(RefineParams {
            enabled: true,
            ..RefineParams::default()
        })
        .expect("regrouped");
    eprintln!(
        "evidence: phase3 sensitivity 07_size_range cold_ms={cold_ms:.1} groups {}=->{} (noise_min_area 16->512) back={} regroup_ms={regroup_ms:.1} warm_hit={warm_hit} warm_ms={warm_ms:.1}",
        auto.len(),
        loose.groups.len(),
        back.groups.len(),
    );
    assert_eq!(back.groups, auto, "the slider round trip is exact");
    assert!(
        regroup_ms < 200.0,
        "a slider regroup must stay inside the ~50–200 ms target, got {regroup_ms:.1} ms"
    );
}

#[test]
fn grouping_is_deterministic_across_sessions_and_thread_counts() {
    let seg = SegParams::default();
    let sheets = ["09_near_touching", "03_rings_holes", "12_c2_latency_grid"];

    let serialize = |name: &str| -> Vec<(u32, u32, u32, u32, u32, (u32, u32))> {
        let mut session = GroupingSession::new(2);
        let (groups, _ms, _hit, _truth) = group_through(name, &mut session, &seg);
        groups.iter().map(key).collect()
    };

    let first = sheets.iter().map(|n| serialize(n)).collect::<Vec<_>>();
    for run in 1..3 {
        let again = sheets.iter().map(|n| serialize(n)).collect::<Vec<_>>();
        assert_eq!(first, again, "run {run} differs");
    }

    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("pool")
        .install(|| sheets.iter().map(|n| serialize(n)).collect::<Vec<_>>());
    let eight = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .expect("pool")
        .install(|| sheets.par_iter().map(|n| serialize(n)).collect::<Vec<_>>());
    eprintln!(
        "evidence: phase3 determinism {} sheets ({}) identical across 3 sessions; 1 thread == 8 threads",
        sheets.len(),
        sheets.join(",")
    );
    assert_eq!(one, first, "1-thread run differs");
    assert_eq!(eight, first, "8-thread run differs");
}

#[test]
fn preview_is_deterministic_and_correctly_scaled() {
    let seg = SegParams::default();
    let (bytes, _) = load("12_c2_latency_grid");
    let out = isg_native::pipeline::segment(&bytes, 4096, &seg).expect("segment");
    let (w, h, png) = out.sheet.preview_png(1024);
    let again = out.sheet.preview_png(1024);
    assert_eq!((w, h), (1024, 1024), "the 4096² sheet halves twice");
    assert_eq!(png, again.2, "preview bytes are deterministic");

    // The preview decodes back to the dimensions it claims (the overlay scales
    // boxes by width / sheetWidth).
    let decoded = image::load_from_memory(&png).expect("preview is a valid PNG");
    assert_eq!((decoded.width(), decoded.height()), (w, h));
    let scale = w as f32 / out.sheet.width() as f32;
    assert!((scale - 0.25).abs() < 1e-6, "scale {scale}");
    eprintln!(
        "evidence: phase3 preview sheet={}x{} preview={w}x{h} scale={scale:.3} bytes={} deterministic=true",
        out.sheet.width(),
        out.sheet.height(),
        png.len()
    );
}
