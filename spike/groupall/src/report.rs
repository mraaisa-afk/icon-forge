//! Corpus runner + machine-readable report (the Phase 0 exit-criteria gate).
//!
//! Criteria (re-grounded on corpus v2; see README "Phase 0 exit criteria"):
//!
//! * **count** — ≥ 95 % of sheets have an exactly correct group count
//!   (with 16 sheets the ceiling makes that 16/16 — every sheet exact),
//! * **keystone time** — the C2 latency sheet (4096², 100 icons) completes
//!   Group All in ≤ 2000 ms. This *is* the roadmap's "2 s / 100 icons" claim.
//! * **corpus guard** — whole-corpus total stays under 5000 ms (CI-friendly
//!   regression bound; the 2 s phase criterion is the keystone sheet, not the
//!   sum over a corpus that now deliberately includes a 1024-icon batch).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use isg_core::{RasterView, SheetPipeline};

use serde::Serialize;

use crate::pipeline::GroupAllPipeline;
use crate::raster::PngRaster;

/// Wall-time limit for the C2 keystone latency sheet ("2 s / 100 icons").
pub const KEYSTONE_LIMIT_MS: u128 = 2000;
/// Whole-corpus regression guard (not the phase criterion; see module docs).
pub const CORPUS_GUARD_MS: u128 = 5000;

#[derive(Serialize)]
pub struct SheetReport {
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub expected_groups: usize,
    pub found_groups: usize,
    pub count_correct: bool,
    pub traced: usize,
    pub failed: usize,
    pub elapsed_ms: u128,
}

#[derive(Serialize)]
pub struct CorpusReport {
    pub corpus: String,
    pub sheets: Vec<SheetReport>,
    pub total_expected: usize,
    pub total_found: usize,
    pub total_ms: u128,
    pub corpus_guard_ms: u128,
    pub pass_total: bool,
    /// Slowest keystone-sheet (C2) elapsed time.
    pub keystone_ms: u128,
    pub keystone_limit_ms: u128,
    pub pass_time: bool,
    pub pass_count: bool,
    /// Sheets with an exactly correct count.
    pub correct_sheets: usize,
}

impl CorpusReport {
    pub fn passes(&self) -> bool {
        self.pass_total && self.pass_time && self.pass_count
    }

    /// ≥ 95 % of sheets exactly right, rounded up: with N sheets, at least
    /// ceil(0.95·N) must be exact.
    pub fn min_correct_sheets(n: usize) -> usize {
        (95usize.saturating_mul(n) + 99) / 100
    }
}

/// Runs the full spike over `corpus_dir` (manifest.json + sheets).
pub fn run_corpus(corpus_dir: &Path) -> CorpusReport {
    let manifest_raw = fs::read_to_string(corpus_dir.join("manifest.json"))
        .unwrap_or_else(|e| panic!("cannot read {}/manifest.json: {e}", corpus_dir.display()));
    #[derive(serde::Deserialize)]
    struct Manifest {
        sheets: Vec<SheetSpec>,
    }
    #[derive(serde::Deserialize)]
    struct SheetSpec {
        file: String,
        #[serde(default)]
        width: u32,
        #[serde(default)]
        height: u32,
        expected_groups: usize,
        /// C2-class latency sheet: its wall time is the keystone criterion.
        #[serde(default)]
        keystone: bool,
    }
    let manifest: Manifest = serde_json::from_str(&manifest_raw).expect("manifest.json is valid JSON");

    let pipeline = GroupAllPipeline::default();
    let mut sheets = Vec::new();
    let mut total_expected = 0usize;
    let mut total_found = 0usize;
    let mut total_ms: u128 = 0;
    let mut keystone_ms: u128 = 0;

    for spec in &manifest.sheets {
        let path = corpus_dir.join(&spec.file);
        let t0 = Instant::now();
        let raster = PngRaster::load(&path).unwrap_or_else(|e| panic!("decode {}: {e}", path.display()));
        let out = pipeline.group_all(&raster);
        let elapsed = t0.elapsed().as_millis();
        if spec.keystone {
            keystone_ms = keystone_ms.max(elapsed);
        }

        let failed = out.svgs.iter().filter(|s| s.is_err()).count();
        sheets.push(SheetReport {
            file: spec.file.clone(),
            width: raster.width(),
            height: raster.height(),
            expected_groups: spec.expected_groups,
            found_groups: out.groups.len(),
            count_correct: out.groups.len() == spec.expected_groups,
            traced: out.trace_successes(),
            failed,
            elapsed_ms: elapsed,
        });
        total_expected += spec.expected_groups;
        total_found += out.groups.len();
        total_ms += elapsed;
    }

    let correct = sheets.iter().filter(|s| s.count_correct).count();
    let pass_total = total_ms <= CORPUS_GUARD_MS;
    let pass_time = keystone_ms <= KEYSTONE_LIMIT_MS;
    let pass_count = correct >= CorpusReport::min_correct_sheets(sheets.len());
    CorpusReport {
        corpus: corpus_dir.display().to_string(),
        sheets,
        total_expected,
        total_found,
        total_ms,
        corpus_guard_ms: CORPUS_GUARD_MS,
        pass_total,
        keystone_ms,
        keystone_limit_ms: KEYSTONE_LIMIT_MS,
        pass_time,
        pass_count,
        correct_sheets: correct,
    }
}

/// Convenience for the CLI: resolve the default corpus dir from CWD or the
/// repo root (two levels up from spike/groupall).
#[must_use]
pub fn default_corpus_dir() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        for base in [
            cwd.clone(),
            cwd.parent().map(Path::to_path_buf).unwrap_or_default(),
            cwd.parent()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
                .unwrap_or_default(),
        ] {
            let c = base.join("bench/corpus");
            if c.join("manifest.json").exists() {
                return c;
            }
        }
    }
    PathBuf::from("bench/corpus")
}
