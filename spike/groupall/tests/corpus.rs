//! Phase 0 EXIT GATE (integration test): the Group All spike over the
//! committed benchmark corpus must satisfy ALL criteria:
//!
//! 1. ≥ 95 % of sheets with an exactly correct group count (16 sheets → 16/16),
//! 2. C2 keystone latency sheet (4096², 100 icons) ≤ 2000 ms,
//! 3. whole-corpus total ≤ 5000 ms (regression guard).
//!
//! This test is what makes `cargo test` enforce the phase gate locally and in
//! CI (windows-latest leg).

use isg_core::SheetPipeline;
use std::path::Path;

fn corpus_dir() -> std::path::PathBuf {
    // CWD is the crate root when cargo runs tests.
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = p.join("bench/corpus");
    assert!(
        dir.join("manifest.json").exists(),
        "corpus missing — run `cargo run -p isg-gen-corpus -- --out bench/corpus` first"
    );
    dir
}

#[test]
fn exit_criteria_group_all() {
    let report = isg_spike_groupall::report::run_corpus(&corpus_dir());

    for s in &report.sheets {
        println!(
            "  {:<24} found={:>4} expect={:>4} {} {} ms",
            s.file,
            s.found_groups,
            s.expected_groups,
            if s.count_correct { "ok" } else { "MISMATCH" },
            s.elapsed_ms
        );
    }
    println!(
        "  count: {}/{} sheets exact (min {})",
        report.correct_sheets,
        report.sheets.len(),
        isg_spike_groupall::report::CorpusReport::min_correct_sheets(report.sheets.len())
    );
    println!(
        "  keystone (C2): {} ms (limit {})",
        report.keystone_ms, report.keystone_limit_ms
    );
    println!(
        "  corpus total: {} ms (guard {})",
        report.total_ms, report.corpus_guard_ms
    );

    assert!(
        report.pass_count,
        "count criterion failed: only {}/{} sheets exact (need >= {})",
        report.correct_sheets,
        report.sheets.len(),
        isg_spike_groupall::report::CorpusReport::min_correct_sheets(report.sheets.len())
    );
    assert!(
        report.pass_time,
        "keystone time criterion failed: {} ms > {} ms limit",
        report.keystone_ms, report.keystone_limit_ms
    );
    assert!(
        report.pass_total,
        "corpus guard failed: {} ms > {} ms",
        report.total_ms, report.corpus_guard_ms
    );
    // Hard invariant: every group must trace successfully.
    for s in &report.sheets {
        assert_eq!(s.failed, 0, "sheet {} had failed traces", s.file);
    }
}

#[test]
fn svg_output_is_well_formed() {
    // Trace one real sheet and check the SVG strings are well-formed.
    let dir = corpus_dir();
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let first = value["sheets"][0]["file"].as_str().unwrap().to_string();

    let raster = isg_spike_groupall::raster::PngRaster::load(&dir.join(&first)).unwrap();
    let pipeline = isg_spike_groupall::pipeline::GroupAllPipeline::default();
    let out = pipeline.group_all(&raster);
    assert!(!out.groups.is_empty(), "no groups found on {first}");
    for (g, svg) in out.groups.iter().zip(out.svgs.iter()) {
        let svg = svg.unwrap_or_else(|e| panic!("trace failed for group {:?}: {e}", g.bbox));
        assert!(svg.starts_with("<?xml"), "missing xml decl");
        assert!(svg.contains("<svg"), "missing <svg root");
        assert!(svg.trim_end().ends_with("</svg>"), "missing </svg> close");
        assert!(svg.contains("<path") || svg.contains("<g"), "no geometry in output");
    }
}
