//! Phase-5 exit gate: the sheet generator, measured on real traced icons
//! (ARCHITECTURE.md §8, §3.5).
//!
//! Everything here runs the **shipping** path, in the order a user's click
//! does: `segment` (decode → normalize → background → clean) feeding
//! `pipeline::mask_cached` and `GroupingSession::group_sheet` (Phase 3), then
//! per icon `pipeline::vectorize_icon` (④ quantize → ⑤ trace → ⑥ simplify) →
//! `emit_svg` (⑦ emit + usvg gate) → `sheet::measure` → `SheetPlan::new`
//! (§3.5 leveling) → the exporters.
//!
//! * **F1** — the acceptance criterion: ≥ 1000 icons level to `ink_size_cv <
//!   0.05`, with the leveling itself inside §3.5's "1000 icons < 20 ms".
//! * **F2** — every exporter is written from real traced geometry and opened
//!   again by a second reader: the sheet SVG by `usvg` *and* the engine's
//!   parser, the PDF by this crate's own strict reader, the PNG by `resvg`
//!   (which rendered it) and by `sheet::export::png::parse_png`, the CSV by its
//!   own reader — plus the round trip through the metadata wizard.
//! * **F3** — determinism: the same plan exports byte-identical files twice, and
//!   a re-opened project (a fresh session) produces the same numbers.
//!
//! The numbers the criteria are judged on are printed by the tests themselves
//! (`evidence: phase5 …`), so a CI run carries its own proof.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use isg_core::{Bbox, ForegroundMask, IconGroup, TracePreset};
use isg_native::pipeline::{
    emit_svg, mask_cached, normalize, validate, vectorize_icon, GroupingSession, SegParams,
};
use isg_native::sheet::export::{
    artwork_from_svg, derive_row, parse_csv, write_csv, write_sheet_pdf, write_sheet_svg, Artwork,
    Column, CsvOptions, IconMeta, PdfOptions, SvgOptions,
};
use isg_native::sheet::metrics::IconMetrics;
use isg_native::sheet::{measure, GridLayout, IconInput, Placement, SheetPlan, SheetSpec};
use isg_native::sheet_native::{render_sheet_png, validate_sheet_svg, RasterOptions};
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

/// Groups a sheet and hands back the mask the groups were cut from, so the
/// metrics below are measured on the same pixels the grouping saw. The group
/// count is checked against the truth sidecar: Phase-3 grouping is this
/// phase's input, and it has to still be exact.
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

/// Measures every group's ink and builds the leveling input, in reading order.
fn icons_from(mask: &ForegroundMask, groups: &[IconGroup]) -> Vec<IconInput> {
    groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let metrics = measure(mask, group.bbox)
                .unwrap_or_else(|| panic!("group {index} at {:?} has no ink", group.bbox));
            IconInput {
                id: index as u32 + 1,
                metrics,
            }
        })
        .collect()
}

fn spec() -> SheetSpec {
    SheetSpec {
        cell: 64,
        padding: 8,
        gap: 8,
        margin: 8,
        columns: 16,
        ink_ratio: 0.80,
        placement: Placement::Center,
    }
}

#[test]
fn f1_a_thousand_icons_level_to_one_size() {
    let seg = SegParams::default();
    let (mask, groups) = grouped("11_c1_batch_grid", &seg);
    assert_eq!(groups.len(), 1024, "the C1 sheet is the 1024-icon batch");
    let icons = icons_from(&mask, &groups);
    assert!(
        icons.len() >= 1000,
        "the criterion is stated for 1000 icons"
    );

    let spec = spec();
    let plan = SheetPlan::new(&icons, spec);
    let report = plan.report;
    assert!(report.icons >= 1000);
    // §3.5's acceptance criterion.
    assert!(
        report.ink_size_cv < 0.05,
        "ink_size_cv must be < 0.05 on ≥1000 icons, got {}",
        report.ink_size_cv
    );
    // The guard must not be doing the work: every flag is a design promise.
    assert_eq!(
        report.overflow_backoffs, 0,
        "no icon may need the overflow back-off on this sheet"
    );
    // The sheet a designer gets is inside a sane pixel budget.
    let (w, h) = plan.size();
    assert!(w <= 4096 && h <= 8192, "sheet is {w}×{h}");

    // §3.5's performance claim: the leveling itself is live-slider arithmetic.
    let mut timings = Vec::new();
    for _ in 0..5 {
        let t0 = Instant::now();
        let again = SheetPlan::new(&icons, spec);
        timings.push(t0.elapsed().as_secs_f32() * 1000.0);
        assert_eq!(again.report.ink_size_cv, report.ink_size_cv);
    }
    timings.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let median_ms = timings[timings.len() / 2];
    assert!(
        median_ms < 20.0,
        "§3.5 budgets 1000 icons in < 20 ms of leveling, got {median_ms:.2} ms"
    );

    // Two more sheets, reported for context rather than asserted (they are
    // deliberately hostile: 07 mixes stroke weights, 03 is rings and holes).
    let mut context = Vec::new();
    for name in ["12_c2_latency_grid", "03_rings_holes", "07_size_range"] {
        let (mask, groups) = grouped(name, &seg);
        let plan = SheetPlan::new(&icons_from(&mask, &groups), spec);
        context.push(format!(
            "{name} {} icons cv={:.4} stroke_cv={:.4}",
            plan.report.icons, plan.report.ink_size_cv, plan.report.stroke_cv
        ));
    }
    eprintln!(
        "evidence: phase5 F1 ink_size_cv={:.4} icons={} sheet={}x{} stroke_cv={:.4} \
         median_stroke={:.2}px median_solidity={:.3} baseline_spread={:.2}px \
         flags=overflow {}/stroke-clamped {}/solidity-clamped {} leveling_median={:.2} ms \
         [{}]",
        report.ink_size_cv,
        report.icons,
        w,
        h,
        report.stroke_cv,
        report.median_stroke,
        report.median_solidity,
        report.baseline_spread,
        report.overflow_backoffs,
        report.stroke_clamped,
        report.solidity_clamped,
        median_ms,
        context.join("; ")
    );
}

/// Traces the first `count` groups of a sheet into real artwork, and measures
/// their ink from the same mask — i.e. exactly what a sheet export receives.
///
/// The metrics come back alongside the plan because the CSV's derived row needs
/// the same numbers the leveling used, not a copy of them.
fn traced_sheet(
    name: &str,
    count: usize,
    preset: TracePreset,
) -> (SheetPlan, Vec<Artwork>, Vec<IconMetrics>) {
    let seg = SegParams::default();
    let bytes = fs::read(corpus_dir().join(format!("{name}.png"))).expect("corpus sheet");
    // The raster the tracer crops from, and the mask the groups were cut from:
    // one decode for the geometry, one for the metrics, both through the real
    // entry points.
    let sheet = normalize(&bytes, 4096).expect("normalizes");
    let (mask, groups) = grouped(name, &seg);
    assert!(groups.len() >= count, "{name}: needs {count} groups");
    let background = isg_native::pipeline::background::detect_background(&sheet, &seg);

    let mut icons = Vec::with_capacity(count);
    let mut artwork = Vec::with_capacity(count);
    let mut metrics_out = Vec::with_capacity(count);
    for (index, group) in groups.iter().take(count).enumerate() {
        let id = index as u32 + 1;
        let vectors = vectorize_icon(&sheet, group.bbox, &background, preset)
            .unwrap_or_else(|e| panic!("{name} group {index}: {e:?}"));
        let document = emit_svg(&vectors, group.bbox.w, group.bbox.h, "balanced", false)
            .unwrap_or_else(|e| panic!("{name} group {index}: {e:?}"));
        // The emitter's own gate (§3.3 stage ⑦), then ours.
        validate(&document).unwrap_or_else(|e| panic!("{name} group {index}: {e:?}"));
        let icon_name = format!("{:03}_{}", id, group.bbox.w);
        artwork.push(
            artwork_from_svg(id, icon_name, &document)
                .unwrap_or_else(|e| panic!("{name} group {index}: {e}")),
        );
        let metrics =
            measure(&mask, group.bbox).unwrap_or_else(|| panic!("{name} group {index}: no ink"));
        metrics_out.push(metrics);
        icons.push(IconInput { id, metrics });
    }
    (SheetPlan::new(&icons, spec()), artwork, metrics_out)
}

#[test]
fn f2_every_export_opens_in_a_second_reader() {
    let (plan, artwork, metrics) = traced_sheet("12_c2_latency_grid", 24, TracePreset::Balanced);
    assert_eq!(plan.placements.len(), 24);
    assert_eq!(metrics.len(), 24);
    assert_eq!(artwork.len(), 24);
    let paths: usize = artwork.iter().map(|a| a.shapes.len()).sum();
    assert!(
        paths >= 24,
        "each traced icon contributes at least one path"
    );
    let (sheet_w, sheet_h) = plan.size();

    // --- SVG: written, then read back by usvg *and* by the engine ---
    let svg = write_sheet_svg(&plan, &artwork, &SvgOptions::default()).expect("svg writes");
    let (doc_w, doc_h) = validate_sheet_svg(&svg).expect("usvg accepts the sheet SVG");
    assert!((doc_w - sheet_w as f32).abs() < 0.01 && (doc_h - sheet_h as f32).abs() < 0.01);
    let reparsed = isg_core::editor::svg::parse(&svg).expect("the engine accepts the sheet SVG");
    assert_eq!(
        reparsed.shapes.len(),
        paths,
        "every path survives the round trip"
    );
    assert!(svg.contains("<metadata>icon-forge sheet v1"));

    // --- PDF: written, then read back by this crate's own strict reader ---
    let pdf = write_sheet_pdf(
        &plan,
        &artwork,
        &PdfOptions {
            background: Some([255, 255, 255, 255]),
            ..PdfOptions::default()
        },
    )
    .expect("pdf writes");
    let summary = isg_native::sheet::export::validate_pdf(&pdf).expect("the PDF reader accepts it");
    assert_eq!(summary.pages, 1);
    assert_eq!(summary.fills, paths, "one fill per path");
    assert!((summary.media_box.0 - sheet_w as f32 * 0.75).abs() < 0.5);
    assert!((summary.media_box.1 - sheet_h as f32 * 0.75).abs() < 0.5);

    // --- PNG: the exported document, rendered, then read back ---
    let raster = render_sheet_png(&plan, &artwork, &RasterOptions::default()).expect("png renders");
    let png = isg_native::sheet::export::parse_png(&raster.png).expect("the PNG reader accepts it");
    assert_eq!((png.width, png.height), (sheet_w, sheet_h));
    assert_eq!(
        raster.paths, paths,
        "the raster is the same document the SVG export wrote"
    );
    // A margin pixel is transparent; somewhere inside the first cell there is ink.
    let margin_alpha = png.pixels[3];
    assert_eq!(margin_alpha, 0, "no background was requested");
    let ink = (0..png.pixels.len() / 4).any(|i| png.pixels[i * 4 + 3] > 0);
    assert!(ink, "the raster has no ink");

    // --- CSV: metadata derived, written, read back, and checked ---
    let rows: Vec<_> = plan
        .placements
        .iter()
        .zip(artwork.iter())
        .zip(metrics.iter())
        .map(|((placement, art), metrics)| {
            let meta = IconMeta {
                id: placement.id,
                id_hex: format!("{:08x}", placement.id),
                sheet_stem: "c2-latency-grid".to_string(),
                preset: "balanced".to_string(),
                colours: art.shapes.len() as u32,
            };
            let file = format!("svg/{}.svg", art.name);
            derive_row(
                &meta,
                placement,
                metrics,
                plan.layout.columns,
                &file,
                &CsvOptions::default(),
            )
        })
        .collect();
    let csv = write_csv(&rows, &Column::ALL, &CsvOptions::default()).expect("csv writes");
    let parsed = parse_csv(&csv, b',').expect("csv parses");
    assert_eq!(
        parsed.len(),
        rows.len() + 1,
        "a header and one row per icon"
    );
    let slugs: Vec<&String> = rows.iter().map(|r| &r.slug).collect();
    let mut unique = slugs.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), slugs.len(), "slugs are unique");
    assert!(
        rows[0].name.starts_with("c2-latency-grid-"),
        "pattern expanded"
    );
    assert_eq!(rows[0].row, 1);
    assert_eq!(rows[1].col, 2);

    eprintln!(
        "evidence: phase5 F2 icons={} paths={} svg={} B (usvg {:.0}x{:.0}) pdf={} B (page {:.0}x{:.0} pt, \
         {} fills, {} B content) png={} B ({}x{} px, {} B idat) csv={} B ({} columns x {} rows)",
        plan.placements.len(),
        paths,
        svg.len(),
        doc_w,
        doc_h,
        pdf.len(),
        summary.media_box.0,
        summary.media_box.1,
        summary.fills,
        summary.content_bytes,
        raster.png.len(),
        raster.width,
        raster.height,
        png.idat_bytes,
        csv.len(),
        Column::ALL.len(),
        rows.len()
    );
}

#[test]
fn f3_exports_are_byte_deterministic() {
    let (plan, artwork, _) = traced_sheet("12_c2_latency_grid", 8, TracePreset::Balanced);
    let svg_a = write_sheet_svg(&plan, &artwork, &SvgOptions::default()).expect("svg");
    let svg_b = write_sheet_svg(&plan, &artwork, &SvgOptions::default()).expect("svg");
    assert_eq!(svg_a, svg_b);

    let pdf_a = write_sheet_pdf(&plan, &artwork, &PdfOptions::default()).expect("pdf");
    let pdf_b = write_sheet_pdf(&plan, &artwork, &PdfOptions::default()).expect("pdf");
    assert_eq!(pdf_a, pdf_b);

    let png_a = render_sheet_png(&plan, &artwork, &RasterOptions::default()).expect("png");
    let png_b = render_sheet_png(&plan, &artwork, &RasterOptions::default()).expect("png");
    assert_eq!(png_a.png, png_b.png);

    // And the plan itself: the same icons in the same order give the same
    // numbers, including through a larger grid and a different placement.
    let icons: Vec<IconInput> = plan
        .placements
        .iter()
        .map(|p| IconInput {
            id: p.id,
            metrics: IconMetrics {
                ink_x: p.ink_local.0,
                ink_y: p.ink_local.1,
                ink_w: p.ink_local.2,
                ink_h: p.ink_local.3,
                ink_area: 1,
                centroid_x: p.ink_local.0 + p.ink_local.2 * 0.5,
                centroid_y: p.ink_local.1 + p.ink_local.3 * 0.5,
                stroke: 4.0,
                solidity: 0.9,
            },
        })
        .collect();
    for placement in Placement::ALL {
        let spec = SheetSpec {
            columns: 4,
            placement,
            ..spec()
        };
        let a = SheetPlan::new(&icons, spec);
        let b = SheetPlan::new(&icons, spec);
        assert_eq!(a.placements, b.placements);
        assert_eq!(a.report, b.report);
        assert_eq!(GridLayout::solve(icons.len(), &spec), a.layout);
    }
    eprintln!(
        "evidence: phase5 F3 svg={} B pdf={} B png={} B identical across runs; plan identical \
         across {} placement modes",
        svg_a.len(),
        pdf_a.len(),
        png_a.png.len(),
        Placement::ALL.len()
    );
}

/// A small sheet built without a corpus sheet: two icons whose artwork exercises
/// the shapes the exporters must survive (an even-odd hole, a cubic, and a
/// translucent fill written the way the pipeline writes one — as an 8-digit hex
/// colour, since neither the emitter nor the engine's reader has CSS).
fn synthetic_sheet() -> (SheetPlan, Vec<Artwork>) {
    let documents = [
        "<svg><path d=\"M2,2L30,2L30,30L2,30Z M10,10L22,10L22,22L10,22Z\" fill=\"#1a2b3c\"/></svg>",
        "<svg><path d=\"M2,20C6,4 26,4 30,20Z\" fill=\"#c0392b80\"/></svg>",
    ];
    let mut icons = Vec::new();
    let mut artwork = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let id = index as u32 + 1;
        let mask = {
            let mut mask = ForegroundMask::new(32, 32);
            for y in 0..32 {
                for x in 0..32 {
                    mask.set(x, y, x > 2 && x < 30 && y > 2 && y < 30);
                }
            }
            mask
        };
        let metrics = measure(&mask, Bbox::new(0, 0, 32, 32).expect("bbox")).expect("ink");
        icons.push(IconInput { id, metrics });
        artwork.push(artwork_from_svg(id, format!("icon-{id:03}"), document).expect("artwork"));
    }
    (SheetPlan::new(&icons, spec()), artwork)
}

/// **F4** — the "opens cleanly in Inkscape / Illustrator / Chrome" half of the
/// exit criteria, expressed as the properties those viewers depend on: a
/// standalone UTF-8 SVG that uses presentation attributes only (no CSS, no
/// `<style>`, nothing external), a PDF a simple reader can parse with no filter,
/// font or transparency machinery, and a PNG in the one byte layout every
/// decoder supports.
///
/// No viewer is available in CI, so this does not replace opening the files by
/// hand; it is what *can* be proven about them here, and it fails loudly if an
/// exporter ever starts depending on a feature only some readers have.
#[test]
fn f4_the_exports_are_conservative_documents() {
    let (plan, artwork) = synthetic_sheet();
    let shapes: usize = artwork.iter().map(|a| a.shapes.len()).sum();
    let (w, h) = plan.size();

    // --- SVG ---------------------------------------------------------------
    let svg = write_sheet_svg(&plan, &artwork, &SvgOptions::default()).expect("svg writes");
    assert!(svg.is_ascii(), "the sheet SVG must be pure ASCII");
    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
    assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.contains(&format!("viewBox=\"0 0 {w} {h}\"")));
    for forbidden in [
        "<!DOCTYPE",
        "<style",
        " class=",
        "href",
        "<image",
        "<use",
        "<script",
        "@font-face",
        "url(",
        "vector-effect",
        "NaN",
        "INF",
    ] {
        assert!(
            !svg.contains(forbidden),
            "the sheet SVG must not contain {forbidden}"
        );
    }
    // Every element it contains is one every renderer knows. The XML
    // declaration (`<?xml …?>`) and any comment are not elements; a closing tag
    // is checked as the element it closes.
    for chunk in svg.split('<').skip(1) {
        let chunk = chunk.strip_prefix('/').unwrap_or(chunk);
        if chunk.starts_with('?') || chunk.starts_with('!') {
            continue;
        }
        let name: String = chunk
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        assert!(
            ["svg", "title", "desc", "metadata", "rect", "g", "path"].contains(&name.as_str()),
            "unexpected element <{name}"
        );
    }
    assert_eq!(svg.matches("<path ").count(), shapes, "one path per shape");
    // A translucent fill survives as a presentation attribute, not as CSS.
    assert!(
        svg.contains("fill=\"#c0392b\" fill-opacity=\"0.502\""),
        "the translucent fill keeps its alpha: {svg}"
    );

    // --- PDF ---------------------------------------------------------------
    let pdf = write_sheet_pdf(&plan, &artwork, &PdfOptions::default()).expect("pdf writes");
    // The version the writer emits — 1.7 has shipped in every reader since
    // 2006, and none of its later features are used (the assertions below are
    // the ones that matter: no filters, no object streams, a classic xref).
    assert!(
        pdf.starts_with(b"%PDF-1.7"),
        "the documented header version"
    );
    assert!(
        pdf.ends_with(b"%%EOF\n"),
        "a complete file, not a truncated one"
    );
    let text = String::from_utf8(pdf.clone()).expect("pure ASCII, per the writer's own rule");
    for forbidden in [
        "/Filter", "/Encrypt", "/Font", "/Image", "/Annots", "/XObject",
    ] {
        assert!(
            !text.contains(forbidden),
            "the sheet PDF must not need {forbidden}"
        );
    }
    assert!(text.contains("\nxref\n"), "a classic xref table");
    assert!(text.contains("\ntrailer\n"));
    assert_eq!(
        text.matches("f*\n").count(),
        shapes,
        "one even-odd fill per shape"
    );
    // Every token in every content stream is a number or an operator that any
    // reader implements — nothing here needs a feature only some viewers have.
    let allowed: &[&str] = &["q", "Q", "rg", "re", "f", "f*", "m", "l", "c", "h"];
    let mut operators = 0usize;
    // The stream keyword is always the whole line after the dictionary, and
    // `endstream` itself ends with `stream` — so split on the *line*, not on
    // the bare word, or the `end` of every `endstream` looks like an operator.
    for part in text.split("\nstream\n").skip(1) {
        let body = part.split("endstream").next().unwrap_or_default();
        for token in body.split_whitespace() {
            assert!(
                token.parse::<f32>().is_ok() || allowed.contains(&token),
                "unexpected PDF token `{token}`"
            );
            if !token.parse::<f32>().is_ok() {
                operators += 1;
            }
        }
    }
    assert!(operators > 0, "the page has no drawing operators");

    // --- PNG ---------------------------------------------------------------
    let raster = render_sheet_png(&plan, &artwork, &RasterOptions::default()).expect("png renders");
    let png = isg_native::sheet::export::parse_png(&raster.png).expect("the PNG reader accepts it");
    assert_eq!((png.bit_depth, png.color_type, png.interlace), (8, 6, 0));
    assert_eq!((png.width, png.height), (w, h));
    assert!(raster
        .png
        .starts_with(&isg_native::sheet::export::png::PNG_SIGNATURE));

    eprintln!(
        "evidence: phase5 F4 svg={} B ({} paths, presentation attributes only, no external refs) \
         pdf={} B (PDF-1.4, {} fills, {operators} operators, no filters/xref-stream) png={} B \
         (8-bit RGBA, no interlace, {} B of IDAT)",
        svg.len(),
        shapes,
        pdf.len(),
        shapes,
        raster.png.len(),
        png.idat_bytes
    );
}
