//! Phase 0 spike CLI: run the Group All pipeline over the benchmark corpus
//! and report the exit criteria (16/16 exact counts, C2 keystone ≤ 2000 ms,
//! corpus guard ≤ 5000 ms).
//!
//! ```text
//! cargo run -p isg-spike-groupall -- --corpus bench/corpus [--json]
//! ```
//!
//! Exit code 0 = all criteria met, 1 = at least one failed.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut corpus: Option<PathBuf> = None;
    let mut json = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                i += 1;
                corpus = args.get(i).map(PathBuf::from);
            }
            "--json" => json = true,
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let dir = corpus.unwrap_or_else(isg_spike_groupall::report::default_corpus_dir);

    let report = isg_spike_groupall::report::run_corpus(&dir);

    if json {
        println!("{}", serde_json::to_string(&report).unwrap());
    } else {
        println!("Group All — Phase 0 spike report");
        println!("corpus: {}", report.corpus);
        println!(
            "{:<24} {:>5} {:>7} {:>7} {:>6} {:>6} {:>6}",
            "sheet", "found", "expect", "ok?", "traced", "fail", "ms"
        );
        for s in &report.sheets {
            println!(
                "{:<24} {:>5} {:>7} {:>7} {:>6} {:>6} {:>6}",
                s.file,
                s.found_groups,
                s.expected_groups,
                if s.count_correct { "yes" } else { "NO" },
                s.traced,
                s.failed,
                s.elapsed_ms
            );
        }
        println!(
            "count: {}/{} sheets exact (min {}) — {}",
            report.correct_sheets,
            report.sheets.len(),
            isg_spike_groupall::report::CorpusReport::min_correct_sheets(report.sheets.len()),
            if report.pass_count { "PASS" } else { "FAIL" }
        );
        println!(
            "keystone (C2, 100 icons): {} ms (limit {}) — {}",
            report.keystone_ms,
            report.keystone_limit_ms,
            if report.pass_time { "PASS" } else { "FAIL" }
        );
        println!(
            "corpus total: {} ms (guard {}) — {}",
            report.total_ms,
            report.corpus_guard_ms,
            if report.pass_total { "PASS" } else { "FAIL" }
        );
        println!(
            "icons: {}/{} matched",
            report.total_found, report.total_expected
        );
    }
    if report.passes() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
