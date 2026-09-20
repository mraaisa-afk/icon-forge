//! Phase-6 exit gate: the review system, measured on real traced icons
//! (ARCHITECTURE.md §8 weeks 22–24, §3.6).
//!
//! Everything runs the **shipping** path in the order a user's click does:
//! `mask_cached` and `GroupingSession::group_sheet` (Phase 3) → per icon
//! `vectorize_icon` → `emit_svg` → the review pass over the documents those
//! produce. No fixture documents, no synthetic planes: if the cascade finds a
//! duplicate, it is because two real traces of two real icons look alike.
//!
//! * **G1** — duplicate recall ≥ 0.95 and precision ≥ 0.90 against the corpus
//!   sidecar's own ground truth (the sheet generator recorded which icons are
//!   the same artwork), on `15_c9_duplicates`.
//! * **G1b** — the same cascade at 1000-icon scale, where the LSH's job is not
//!   accuracy but *cost*: the pair count is printed against the all-pairs count
//!   it replaces.
//! * **G2** — the quality flags mean what §3.6 says: every `LowQuality` in the
//!   report is below the composite threshold, every `OverComplex` is over its
//!   node budget, every `OpenContour` is open — and a document that is *wrong*
//!   is flagged on the same crop, so the detector is shown to fail when it
//!   should, not only to pass.
//! * **G3** — outliers: the roadmap's own example (a filled blob among 99
//!   outlines) is detected, a uniform sheet produces nothing, and every outlier
//!   the real sheet reports is justified by its z-score.
//! * **G4** — triage at scale: 1000 decisions, undoable to empty, exported to
//!   `review.csv` and read back byte-identically, with the mechanical cost per
//!   decision printed. The roadmap's *"1000 icons triaged ≤ 20 min by 3
//!   users"* has a human half that no test can measure; this is the half that
//!   can be, and the exit report says so in as many words.
//!
//! The numbers the criteria are judged on are printed by the tests themselves
//! (`evidence: phase6 …`), so a CI run carries its own proof.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use isg_core::{Bbox, ForegroundMask, IconGroup, TracePreset};
use isg_native::pipeline::{
    compare_planes, emit_svg, mask_cached, normalize, score_svg, validate, vectorize_icon,
    BackgroundKind, GroupingSession, SegParams, SheetRaster,
};
use isg_native::review::{
    median, node_budget, quality_flags, scan_outliers, HashItem, IconStat, OutlierKind,
    QualityFlag, QualityInput, StyleClass, TriageAction, TriageLog, LOW_QUALITY_COMPOSITE,
    LSH_BANDS, OUTLIER_Z,
};
use isg_native::review_native::{
    duplicate_cascade, normalized_plane, plane_digest, review_sheet, upscale_nearest, ReviewInput,
    ReviewOptions, CELL,
};
use serde::Deserialize;

/// One entry of a corpus truth sidecar (`bench/corpus/*.json`).
///
/// Only the two fields this gate reads: the sidecar's `id` is its own
/// numbering, and the mapping below is by geometry, so a gate that trusted the
/// numbering would be trusting the thing it is trying to verify.
#[derive(Debug, Deserialize)]
struct TruthIcon {
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

    fn centre(&self) -> (i64, i64) {
        let rect = self.bbox();
        (
            i64::from(rect.x + rect.w / 2),
            i64::from(rect.y + rect.h / 2),
        )
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

/// Groups a sheet and hands back the mask the groups were cut from.
fn grouped(name: &str, seg: &SegParams) -> (ForegroundMask, Vec<IconGroup>) {
    let (bytes, truth) = load(name);
    let mut session = GroupingSession::new(2);
    let key = session.key_for(&bytes, seg);
    let (mask, _background, hit) = mask_cached(&bytes, 4096, seg, session.cache_mut())
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let report = session
        .group_sheet(&key, hit)
        .unwrap_or_else(|| panic!("{name}: no grouping report"));
    assert_eq!(
        report.groups.len(),
        truth.expected_groups as usize,
        "{name}: Phase-3 grouping is the input to this phase and must still be exact"
    );
    assert_eq!(report.width, truth.width);
    assert_eq!(report.height, truth.height);
    (mask, report.groups)
}

/// The colour the review composites over.
///
/// The sheet's own detected background when the detector found a colour for
/// it; opaque white when it did not (an alpha sheet has no colour to composite
/// and an Otsu split reports `[0; 4]`, which would read as a black sheet and
/// turn every background pixel into "ink" in the reference plane — the mistake
/// that cost run 35231648156 in stage ⑧). Both corpus families here are drawn
/// on white.
fn review_background(sheet: &SheetRaster, seg: &SegParams) -> [u8; 4] {
    let model = isg_native::pipeline::background::detect_background(sheet, seg);
    match model.kind {
        BackgroundKind::Otsu | BackgroundKind::Alpha => [255, 255, 255, 255],
        _ => model.rgba,
    }
}

/// Traces every group of a sheet through the shipping path: the review's input
/// is what the app would cache for those icons.
fn traced(
    name: &str,
    preset: TracePreset,
) -> (
    SheetRaster,
    ForegroundMask,
    Vec<IconGroup>,
    Vec<ReviewInput>,
    Truth,
) {
    let seg = SegParams::default();
    let (bytes, truth) = load(name);
    let sheet = normalize(&bytes, 4096).expect("normalizes");
    let (mask, groups) = grouped(name, &seg);
    let background = isg_native::pipeline::background::detect_background(&sheet, &seg);
    let mut inputs = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        let vectors = vectorize_icon(&sheet, group.bbox, &background, preset)
            .unwrap_or_else(|e| panic!("{name} group {index}: {e:?}"));
        let document = emit_svg(&vectors, group.bbox.w, group.bbox.h, "balanced", false)
            .unwrap_or_else(|e| panic!("{name} group {index}: {e:?}"));
        validate(&document).unwrap_or_else(|e| panic!("{name} group {index}: {e:?}"));
        inputs.push(ReviewInput {
            id: index as u32 + 1,
            document,
            bbox: group.bbox,
        });
    }
    (sheet, mask, groups, inputs, truth)
}

/// Maps a group onto the truth icon at its centre, so "these two icons are the
/// same artwork" comes from the corpus's own record rather than a guess from
/// the bytes. Returns `(truth icon index, shape)`.
fn truth_for(group: &IconGroup, truth: &Truth) -> (usize, String) {
    let centre = (
        i64::from(group.bbox.x + group.bbox.w / 2),
        i64::from(group.bbox.y + group.bbox.h / 2),
    );
    let mut best: Option<(usize, i64)> = None;
    for (index, icon) in truth.icons.iter().enumerate() {
        let (cx, cy) = icon.centre();
        let distance = (centre.0 - cx).abs() + (centre.1 - cy).abs();
        if best.is_none_or(|(_, d)| distance < d) {
            best = Some((index, distance));
        }
    }
    let (index, _) = best.expect("a truth sidecar with no icons cannot be mapped");
    (index, truth.icons[index].shape.clone())
}

/// Every pair inside a cluster, as an ordered pair of ids.
fn cluster_pairs(clusters: &[isg_native::review::DupCluster]) -> BTreeSet<(u32, u32)> {
    let mut pairs = BTreeSet::new();
    for cluster in clusters {
        for (i, a) in cluster.members.iter().enumerate() {
            for b in cluster.members.iter().skip(i + 1) {
                pairs.insert((*a, *b));
            }
        }
    }
    pairs
}

/// Recall and precision of a predicted pair set against the truth.
fn scores_of(predicted: &BTreeSet<(u32, u32)>, truth: &BTreeSet<(u32, u32)>) -> (f64, f64) {
    let hits = predicted.intersection(truth).count();
    let recall = if truth.is_empty() {
        1.0
    } else {
        hits as f64 / truth.len() as f64
    };
    let precision = if predicted.is_empty() {
        1.0
    } else {
        hits as f64 / predicted.len() as f64
    };
    (recall, precision)
}

#[test]
fn g1_duplicates_are_found_and_not_invented() {
    let preset = TracePreset::Balanced;
    let (sheet, mask, groups, inputs, truth) = traced("15_c9_duplicates", preset);
    let seg = SegParams::default();
    let options = ReviewOptions {
        background: review_background(&sheet, &seg),
        ..ReviewOptions::default()
    };
    let report = review_sheet(&sheet, &mask, &inputs, &options).expect("the review runs");

    // --- ground truth: the corpus says which icons share a shape ------------
    let mut mapped = BTreeSet::new();
    let mut shape_of: BTreeMap<u32, String> = BTreeMap::new();
    for (index, group) in groups.iter().enumerate() {
        let (truth_index, shape) = truth_for(group, &truth);
        assert!(
            mapped.insert(truth_index),
            "two groups claim the same truth icon — the mapping is not one-to-one"
        );
        shape_of.insert(inputs[index].id, shape);
    }
    assert_eq!(
        mapped.len(),
        truth.icons.len(),
        "every truth icon must map to exactly one group"
    );
    let truth_pairs: BTreeSet<(u32, u32)> = {
        let ids: Vec<u32> = shape_of.keys().copied().collect();
        let mut pairs = BTreeSet::new();
        for (i, a) in ids.iter().enumerate() {
            for b in ids.iter().skip(i + 1) {
                if shape_of[a] == shape_of[b] {
                    pairs.insert((*a, *b));
                }
            }
        }
        pairs
    };

    let predicted = cluster_pairs(&report.clusters);
    let (recall, precision) = scores_of(&predicted, &truth_pairs);
    assert!(
        recall >= 0.95,
        "duplicate recall {recall:.4} under the 0.95 the roadmap asks for \
         ({} of {} true pairs; clusters {:?})",
        predicted.intersection(&truth_pairs).count(),
        truth_pairs.len(),
        report.clusters
    );
    assert!(
        precision >= 0.90,
        "duplicate precision {precision:.4} under the 0.90 the roadmap asks for \
         ({} predicted pairs, {} true)",
        predicted.len(),
        truth_pairs.len()
    );
    // Every cluster's keeper must be one of its members, and clusters must not
    // be singletons: a "duplicate group" of one is not a decision.
    for cluster in &report.clusters {
        assert!(cluster.members.len() >= 2, "singleton cluster {cluster:?}");
        assert!(cluster.members.contains(&cluster.keeper));
    }

    let flagged = report.flags_of(0);
    assert!(flagged.is_empty(), "id 0 is not an icon of this sheet");
    eprintln!(
        "evidence: phase6 G1 icons={} clusters={} true_pairs={} predicted_pairs={} \
         recall={recall:.4} precision={precision:.4} cascade=propose {} / verify {} / confirm {} \
         reviewer_flags={} flags_of_id0={} is_duplicate(first)={} \
         (thresholds IoU {:.2} / Hausdorff {:.3} / SSIM {:.2})",
        report.icons.len(),
        report.clusters.len(),
        truth_pairs.len(),
        predicted.len(),
        report.cascade.candidates,
        report.cascade.verified,
        report.cascade.confirmed,
        report.flagged,
        flagged.len(),
        report.is_duplicate(report.icons[0].id),
        options.dupes.iou_min,
        options.dupes.hausdorff_max,
        options.dupes.ssim_min,
    );
}

#[test]
fn g1b_the_cascade_holds_at_a_thousand_icons() {
    // Sixty-three copies of the sixteen traced documents: 1008 icons of real
    // geometry, which is the shape of the job the LSH exists for.
    let (sheet, _mask, groups, inputs, truth) = traced("15_c9_duplicates", TracePreset::Balanced);
    let seg = SegParams::default();
    let background = review_background(&sheet, &seg);
    let copies = 63u32;
    let n = inputs.len() as u32 * copies;

    let started = Instant::now();
    let mut hash_items = Vec::with_capacity(n as usize);
    let mut planes = Vec::with_capacity(n as usize);
    let mut group_of = BTreeMap::new();
    for copy in 0..copies {
        for input in &inputs {
            let id = copy * inputs.len() as u32 + input.id;
            let plane = normalized_plane(&input.document, CELL, background, id)
                .unwrap_or_else(|e| panic!("icon {id}: {e}"));
            hash_items.push(HashItem {
                id,
                d: isg_native::review::d_hash(&plane, CELL, CELL).unwrap_or(0),
                a: isg_native::review::a_hash(&plane, CELL, CELL).unwrap_or(0),
                digest: plane_digest(&plane),
            });
            group_of.insert(id, input.id);
            planes.push(plane);
        }
    }
    let render_ms = started.elapsed().as_secs_f64() * 1000.0;

    let options = ReviewOptions {
        background,
        ..ReviewOptions::default()
    };
    let scores = vec![1.0f32; n as usize];
    let cascade_started = Instant::now();
    let (clusters, counts) = duplicate_cascade(&hash_items, &planes, &scores, &options);
    let cascade_ms = cascade_started.elapsed().as_secs_f64() * 1000.0;

    // Ground truth: two icons are duplicates exactly when they are the same
    // artwork, which is what the corpus's own sidecar records — the same
    // definition G1 uses, so the two tests cannot disagree about what a
    // duplicate is. (Counting *document* copies instead would call four
    // differently-sized copies of one shape "different", which is the opposite
    // of what "near-duplicate" means here — and with the cell fitted to the
    // ink, that is exactly the pair the cascade is expected to find.)
    let shape_of_doc: BTreeMap<u32, String> = groups
        .iter()
        .zip(&inputs)
        .map(|(group, input)| (input.id, truth_for(group, &truth).1))
        .collect();
    let ids: Vec<u32> = hash_items.iter().map(|h| h.id).collect();
    let mut truth_pairs = BTreeSet::new();
    for (i, a) in ids.iter().enumerate() {
        for b in ids.iter().skip(i + 1) {
            if shape_of_doc[&group_of[a]] == shape_of_doc[&group_of[b]] {
                truth_pairs.insert((*a, *b));
            }
        }
    }
    let predicted = cluster_pairs(&clusters);
    let (recall, precision) = scores_of(&predicted, &truth_pairs);
    assert!(
        recall >= 0.95,
        "1000-icon recall {recall:.4} ({} of {})",
        predicted.intersection(&truth_pairs).count(),
        truth_pairs.len()
    );
    assert!(
        precision >= 0.90,
        "1000-icon precision {precision:.4} ({} predicted)",
        predicted.len()
    );
    let all_pairs = u64::from(n) * u64::from(n - 1) / 2;
    assert!(
        (counts.candidates as u64) * 4 < all_pairs,
        "the LSH proposed {} pairs of a possible {all_pairs} — it must be a fraction of them",
        counts.candidates
    );
    // Structure: one cluster per shape family, each holding every copy of every
    // document of that shape — 4 families × 4 documents × 63 copies.
    let shapes: BTreeSet<&String> = shape_of_doc.values().collect();
    assert_eq!(
        clusters.len(),
        shapes.len(),
        "one cluster per shape family, got {:?}",
        clusters
            .iter()
            .map(|c| c.members.len())
            .collect::<Vec<usize>>()
    );
    for cluster in &clusters {
        let families: BTreeSet<&String> = cluster
            .members
            .iter()
            .map(|id| &shape_of_doc[&group_of[id]])
            .collect();
        assert_eq!(
            families.len(),
            1,
            "a cluster must not mix shapes: {families:?}"
        );
        assert_eq!(
            cluster.members.len(),
            (inputs.len() / shapes.len() as usize) * copies as usize,
            "a shape family's cluster holds every copy of its documents"
        );
    }
    eprintln!(
        "evidence: phase6 G1b icons={n} papers={} clusters={} candidates={} (all-pairs would be \
         {all_pairs}, {:.2}% proposed) verify={} confirm={} recall={recall:.4} \
         precision={precision:.4} render={render_ms:.0} ms cascade={cascade_ms:.0} ms \
         (per icon {:.3} ms)",
        hash_items.len(),
        clusters.len(),
        counts.candidates,
        100.0 * counts.candidates as f64 / all_pairs as f64,
        counts.verified,
        counts.confirmed,
        cascade_ms / f64::from(n),
    );
}

#[test]
fn g2_the_quality_flags_mean_what_section_36_says() {
    let (sheet, mask, _groups, inputs, _truth) =
        traced("12_c2_latency_grid", TracePreset::Balanced);
    let seg = SegParams::default();
    let background = review_background(&sheet, &seg);
    let options = ReviewOptions {
        background,
        ..ReviewOptions::default()
    };
    assert!(
        inputs.len() >= 64,
        "the quality gate wants a sheet with enough icons, got {}",
        inputs.len()
    );
    let report = review_sheet(&sheet, &mask, &inputs, &options).expect("the review runs");

    // --- 1. every flag in the report is justified by the numbers beside it --
    let mut counts: BTreeMap<QualityFlag, usize> = BTreeMap::new();
    for icon in &report.icons {
        for flag in &icon.flags {
            *counts.entry(*flag).or_default() += 1;
            match flag {
                QualityFlag::LowQuality => assert!(
                    icon.score.composite < LOW_QUALITY_COMPOSITE,
                    "icon {} is flagged low-quality at composite {}",
                    icon.id,
                    icon.score.composite
                ),
                QualityFlag::OverComplex => assert!(
                    f64::from(icon.node_count) > f64::from(node_budget(icon.ink_area)),
                    "icon {} is flagged over-complex at {} nodes",
                    icon.id,
                    icon.node_count
                ),
                QualityFlag::OpenContour => assert!(
                    !icon.closed,
                    "icon {} is flagged open-contour but every subpath is closed",
                    icon.id
                ),
            }
        }
        if icon.flags.is_empty() {
            assert!(
                icon.score.composite >= LOW_QUALITY_COMPOSITE,
                "icon {} carries no flag at composite {}",
                icon.id,
                icon.score.composite
            );
        }
    }

    // --- 2. a document that is wrong is flagged, on the same crop -----------
    let victim = &inputs[0];
    let rect = victim.bbox;
    let broken = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" \
         viewBox=\"0 0 {} {}\"><rect x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" fill=\"#101010\"/></svg>",
        rect.w, rect.h, rect.w, rect.h, rect.w, rect.h
    );
    let crop = sheet.crop_rgba(rect);
    let big = upscale_nearest(&crop, rect.w, rect.h, 2);
    let broken_score = score_svg(&broken, &big, background, rect.w * 2, rect.h * 2)
        .expect("the broken document still renders");
    let broken_flags = quality_flags(&QualityInput {
        id: victim.id,
        composite: broken_score.composite,
        node_count: 4,
        ink_area: 1024,
        closed: true,
    });
    assert!(
        broken_flags.contains(&QualityFlag::LowQuality),
        "a full-bleed rectangle scored {} against icon {} — that must read as low quality",
        broken_score.composite,
        victim.id
    );
    // ...and the same document is *not* what the icon actually traced to.
    let honest = &report.icons[0];
    assert!(
        honest.score.composite > broken_score.composite,
        "the real trace ({}) must beat a full-bleed rectangle ({})",
        honest.score.composite,
        broken_score.composite
    );

    let composites: Vec<f32> = report.icons.iter().map(|i| i.score.composite).collect();
    let minimum = composites.iter().copied().fold(f32::MAX, f32::min);
    eprintln!(
        "evidence: phase6 G2 icons={} flagged={} low_quality={} over_complex={} open_contour={} \
         composite min={minimum:.4} median={:.4} broken_document={:.4} \
         (threshold: composite < {LOW_QUALITY_COMPOSITE}, nodes > 4·√area)",
        report.icons.len(),
        report.flagged,
        counts.get(&QualityFlag::LowQuality).copied().unwrap_or(0),
        counts.get(&QualityFlag::OverComplex).copied().unwrap_or(0),
        counts.get(&QualityFlag::OpenContour).copied().unwrap_or(0),
        median(&composites),
        broken_score.composite,
    );
}

#[test]
fn g3_outliers_find_the_icon_that_is_not_like_the_others() {
    let (sheet, mask, _groups, inputs, _truth) =
        traced("12_c2_latency_grid", TracePreset::Balanced);
    let seg = SegParams::default();
    let options = ReviewOptions {
        background: review_background(&sheet, &seg),
        ..ReviewOptions::default()
    };
    let report = review_sheet(&sheet, &mask, &inputs, &options).expect("the review runs");
    let stats: Vec<IconStat> = report.icons.iter().map(|icon| icon.stat).collect();

    // --- 1. on the real sheet, every reported outlier is justified ----------
    for flag in &report.outliers {
        let values: BTreeMap<OutlierKind, Vec<f32>> = {
            let mut map = BTreeMap::new();
            for kind in [
                OutlierKind::InkSize,
                OutlierKind::Stroke,
                OutlierKind::NodeCount,
                OutlierKind::Colours,
                OutlierKind::Solidity,
            ] {
                let values = stats
                    .iter()
                    .map(|stat| match kind {
                        OutlierKind::InkSize => stat.ink_size,
                        OutlierKind::Stroke => stat.stroke,
                        OutlierKind::NodeCount => stat.node_count,
                        OutlierKind::Colours => stat.colours,
                        _ => stat.solidity,
                    })
                    .collect();
                map.insert(kind, values);
            }
            map
        };
        match flag.kind {
            OutlierKind::Palette | OutlierKind::Style => {}
            kind => {
                let values = &values[&kind];
                let sheet_median = median(values);
                assert!(
                    (flag.median - sheet_median).abs() < 1e-3,
                    "outlier {flag:?} reports median {} but the sheet's is {sheet_median}",
                    flag.median
                );
                assert!(
                    flag.z.abs() > OUTLIER_Z,
                    "outlier {flag:?} is below the {OUTLIER_Z} threshold"
                );
            }
        }
    }

    // --- 2. a uniform sheet has no outliers at all --------------------------
    let uniform: Vec<IconStat> = (0..100)
        .map(|index| IconStat {
            id: index,
            ..stats[0]
        })
        .collect();
    assert!(
        scan_outliers(&uniform, OUTLIER_Z).is_empty(),
        "identical icons are not outliers of each other"
    );

    // --- 3. the roadmap's own example: a filled blob among outlines ---------
    //
    // §3.6's defect is *"99 icons are 2px outline, one is a filled blob"*, and
    // the style flag is a **modal** test: a blob is only an outlier when the
    // sheet's own icons are outlines. Sheet 12's icons are solid glyphs, so the
    // same blob is not one there — which is a fact about the sheet, not a
    // failure of the detector, so the modal class is asserted before the
    // absence is.
    let sheet12_modal = isg_native::review::modal_style(&stats);
    assert_eq!(
        sheet12_modal,
        Some(StyleClass::Filled),
        "sheet 12's icons are solid, so the modal class must be filled — otherwise the \n\
         blob below would be judged against the wrong norm"
    );
    let blob = |id: u32, stroke: f32, fill_ratio: f32, palette: u64| IconStat {
        id,
        ink_size: median(&stats.iter().map(|s| s.ink_size).collect::<Vec<f32>>()),
        stroke,
        node_count: 4.0,
        colours: 1.0,
        solidity: 1.0,
        fill_ratio,
        palette,
    };
    let stroke_median = median(&stats.iter().map(|s| s.stroke).collect::<Vec<f32>>());
    let mut with_blob = stats.clone();
    let blob_id = 9999;
    with_blob.push(blob(blob_id, stroke_median * 3.0, 0.95, stats[0].palette));
    let with_blob_flags = scan_outliers(&with_blob, OUTLIER_Z);
    let blob_flags: Vec<OutlierKind> = with_blob_flags
        .iter()
        .filter(|flag| flag.id == blob_id)
        .map(|flag| flag.kind)
        .collect();
    assert!(
        blob_flags.contains(&OutlierKind::Stroke),
        "the blob's stroke is three times the sheet's median and must be flagged, got {blob_flags:?}"
    );
    assert_eq!(
        StyleClass::of(0.95),
        StyleClass::Filled,
        "the blob's fill ratio has to read as filled"
    );
    // A blob among *outlines* is where style and palette mismatch are defined,
    // so the sheet that produces them is the one with outlines: 03_rings_holes
    // is 25 rings, traced and measured like any other sheet.
    let (ring_sheet, ring_mask, _groups, ring_inputs, _truth) =
        traced("03_rings_holes", TracePreset::Balanced);
    let ring_options = ReviewOptions {
        background: review_background(&ring_sheet, &seg),
        ..ReviewOptions::default()
    };
    let ring_report = review_sheet(&ring_sheet, &ring_mask, &ring_inputs, &ring_options)
        .expect("the review runs");
    let mut outlines: Vec<IconStat> = ring_report.icons.iter().map(|icon| icon.stat).collect();
    assert!(
        outlines.iter().all(|stat| stat.fill_ratio < 0.5),
        "the rings must read as outlines or this case proves nothing: {:?}",
        outlines.iter().map(|s| s.fill_ratio).collect::<Vec<f32>>()
    );
    assert_eq!(
        isg_native::review::modal_style(&outlines),
        Some(StyleClass::Outline),
        "the rings are the norm"
    );
    let modal_palette =
        isg_native::review::modal_palette(&outlines).expect("one palette covers the rings");
    outlines.push(blob(
        blob_id,
        stroke_median * 3.0,
        0.95,
        modal_palette ^ 0xFFFF,
    ));
    let roadmap_flags = scan_outliers(&outlines, OUTLIER_Z);
    let roadmap_blob: Vec<OutlierKind> = roadmap_flags
        .iter()
        .filter(|flag| flag.id == blob_id)
        .map(|flag| flag.kind)
        .collect();
    for kind in [
        OutlierKind::Style,
        OutlierKind::Palette,
        OutlierKind::Stroke,
    ] {
        assert!(
            roadmap_blob.contains(&kind),
            "the roadmap's blob among {} outlines must be flagged {kind:?}, got {roadmap_blob:?}",
            outlines.len() - 1
        );
    }

    eprintln!(
        "evidence: phase6 G3 icons={} sheet_outliers={} ids={:?} injected_blob_on_solid_sheet={:?} \
         roadmap_blob_among_outlines={:?} uniform_sheet_outliers=0 median_stroke={stroke_median:.2} \
         (z threshold {OUTLIER_Z})",
        report.icons.len(),
        report.outliers.len(),
        report
            .outliers
            .iter()
            .map(|flag| flag.id)
            .collect::<BTreeSet<u32>>(),
        blob_flags,
        roadmap_blob,
    );
}

#[test]
fn g4_a_thousand_decisions_are_undoable_timestamped_and_exported() {
    // The triage half of §3.6, at the scale the roadmap's target is written in:
    // 1008 icons, one decision each, every decision timestamped, the whole
    // session undoable to empty, and `review.csv` byte-stable through a read.
    let n = 1008u32;
    let mut log = TriageLog::new();
    let started = Instant::now();
    for id in 1..=n {
        let action = match id % 16 {
            0 => TriageAction::Duplicate,
            1 => TriageAction::Flag,
            _ => TriageAction::Approve,
        };
        log.apply(id, action, 1_700_000_000_000 + u64::from(id));
    }
    let triage_ms = started.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(log.len(), n as usize, "one decision per icon");
    let counts = log.counts();
    assert_eq!(
        counts.iter().sum::<usize>(),
        n as usize,
        "every decision is counted once: {counts:?}"
    );
    assert_eq!(counts[0], (n as usize) - 2 * (n as usize / 16), "approvals");
    assert_eq!(counts[2], (n as usize) / 16, "flags");
    assert_eq!(counts[3], (n as usize) / 16, "duplicates");

    let csv = log.to_csv();
    let lines = csv.lines().count();
    assert_eq!(lines, n as usize + 1, "header plus one row per icon");
    let back = TriageLog::from_csv(&csv).expect("review.csv parses");
    assert_eq!(back.len(), n as usize);
    assert_eq!(
        back.to_csv(),
        csv,
        "the export round-trips byte-identically"
    );
    let timestamps: Vec<u64> = back.decisions().map(|(_, d)| d.at_ms).collect();
    assert!(
        timestamps.windows(2).all(|w| w[1] > w[0]),
        "the log is chronological"
    );

    // Undo everything: the session must come back to empty, not to a partial
    // state with holes in it.
    let mut undone = 0usize;
    while log.undo().is_some() {
        undone += 1;
    }
    assert_eq!(undone, n as usize);
    assert!(log.is_empty());
    assert_eq!(
        log.to_csv().lines().count(),
        1,
        "header only after undo-all"
    );

    let per_icon_ms = triage_ms / f64::from(n);
    assert!(
        per_icon_ms < 1.0,
        "the mechanical cost is {per_icon_ms:.4} ms per icon — bookkeeping must not be what \
         a 1000-icon session spends its time on"
    );
    eprintln!(
        "evidence: phase6 G4 decisions={n} triage={triage_ms:.2} ms ({:.4} ms/icon) csv={} B \
         lines={lines} round_trip=identical undo_all=empty counts(approve/flag/duplicate)={}/{}/{} \
         (the 20-minute criterion's human half is not measurable in CI; this is the mechanical half)",
        per_icon_ms,
        csv.len(),
        counts[0], counts[2], counts[3],
    );
}

#[test]
fn g5_the_review_refuses_inputs_it_cannot_review() {
    // A gate that only ever sees good input proves half of what it claims.
    let sheet = SheetRaster::from_rgba(8, 8, vec![255; 8 * 8 * 4]);
    let mask = ForegroundMask::new(8, 8);
    let options = ReviewOptions::default();
    let item = |document: &str, bbox: Bbox| ReviewInput {
        id: 1,
        document: document.to_string(),
        bbox,
    };
    let good = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\"><rect width=\"4\" height=\"4\" fill=\"#000\"/></svg>";
    let boxed = Bbox {
        x: 2,
        y: 2,
        w: 4,
        h: 4,
    };

    // A document the engine cannot parse. Note the parser is lenient about
    // absent geometry — `not svg` parses to *no shapes*, which §3.9 calls a
    // `NoOp` import — so the malformed case has to be malformed path data.
    let error = review_sheet(
        &sheet,
        &mask,
        &[item("<svg><path d=\"M 0 0 L\"/></svg>", boxed)],
        &options,
    )
    .expect_err("a document with malformed path data must be refused");
    assert!(
        matches!(
            error,
            isg_native::review_native::ReviewError::BadArtwork { id: 1, .. }
        ),
        "expected a bad-artwork refusal, got {error:?}"
    );

    // A document that parses but draws nothing, on a crop with no ink: the
    // parser is happy, the mask is not — that is `NoInk`, not `BadArtwork`.
    let error = review_sheet(&sheet, &mask, &[item("not svg", boxed)], &options)
        .expect_err("a document with no ink must be refused");
    assert!(
        matches!(
            error,
            isg_native::review_native::ReviewError::NoInk { id: 1 }
        ),
        "expected a no-ink refusal, got {error:?}"
    );

    // A box that runs off the sheet.
    let off = Bbox {
        x: 6,
        y: 6,
        w: 4,
        h: 4,
    };
    let error = review_sheet(&sheet, &mask, &[item(good, off)], &options)
        .expect_err("a box off the sheet must be refused");
    assert!(matches!(
        error,
        isg_native::review_native::ReviewError::BadBbox { id: 1, .. }
    ));

    // A mask that is not the raster's size.
    let error = review_sheet(
        &sheet,
        &ForegroundMask::new(4, 4),
        &[item(good, boxed)],
        &options,
    )
    .expect_err("a raster and mask of different sizes must be refused");
    assert!(matches!(
        error,
        isg_native::review_native::ReviewError::SizeMismatch { .. }
    ));

    // A box with no ink in a matching mask.
    let error = review_sheet(&sheet, &mask, &[item(good, boxed)], &options)
        .expect_err("a group with no ink must be refused");
    assert!(matches!(
        error,
        isg_native::review_native::ReviewError::NoInk { id: 1 }
    ));

    // And the cascade's own compare step refuses a blank plane rather than
    // calling two blanks duplicates of each other.
    let blank = vec![0u8; (CELL * CELL) as usize];
    assert!(compare_planes(&blank, &blank, CELL, CELL).ssim > 0.9);
    assert_eq!(
        isg_native::review::hausdorff_normalised(&blank, &blank, CELL, CELL),
        None,
        "a blank plane has no shape to measure"
    );
    assert_eq!(
        isg_native::review::candidate_pairs(&[], LSH_BANDS).len(),
        0,
        "no icons, no candidates"
    );
}
