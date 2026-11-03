//! morphe-conformance: the corpus-validity check runner.
//!
//! Enumerates every `.scad` file under a corpus directory (default
//! `corpus/core`, or a path given as the first argument), runs each through the
//! Morphe pipeline as a library, and prints a pass/fail summary. Exits nonzero
//! if any file fails to evaluate + resolve to the expected geometry.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use morphe_conformance::{
    coverage_report, default_corpus_dir, run_corpus, run_oracle, to_markdown, OracleOutcome,
    OracleTolerance, Outcome,
};

fn main() -> ExitCode {
    let dir: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_corpus_dir);

    // ── Corpus-validity check ─────────────────────────────────────────────────
    let report = match run_corpus(&dir) {
        Ok(report) => report,
        Err(err) => {
            eprintln!(
                "morphe-conformance: cannot read corpus {}: {err}",
                dir.display()
            );
            return ExitCode::FAILURE;
        }
    };

    for file in &report.files {
        let name = file.path.display();
        if file.passed() {
            match file.outcome {
                Ok(Outcome::Solid { triangles }) => {
                    println!("PASS  {name}  (solid, {triangles} triangles)");
                }
                Ok(Outcome::Profile) => println!("PASS  {name}  (profile)"),
                _ => println!("PASS  {name}"),
            }
        } else {
            let reason = file.failure_reason().unwrap_or_default();
            println!("FAIL  {name}  -- {reason}");
        }
    }

    println!(
        "\ncorpus: {} files, {} passed, {} failed",
        report.total(),
        report.passed(),
        report.total() - report.passed()
    );

    // ── Oracle mesh-diff check ────────────────────────────────────────────────
    let oracle = match run_oracle(&dir, &OracleTolerance::default()) {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!(
                "morphe-conformance: cannot read corpus {} for oracle: {err}",
                dir.display()
            );
            return ExitCode::FAILURE;
        }
    };

    println!();
    match &oracle.openscad {
        Some(bin) => println!("oracle: comparing against openscad at {}", bin.display()),
        None => println!("oracle: openscad not found — all comparisons skipped"),
    }
    for r in &oracle.results {
        let name = r.path.display();
        match &r.outcome {
            OracleOutcome::Pass {
                volume_delta,
                hausdorff,
                area_delta,
            } => println!(
                "  PASS  {name}  (Δvol {volume_delta:.4}, hausdorff {hausdorff:.4}, Δarea {area_delta:.4})"
            ),
            OracleOutcome::Fail { reason, .. } => println!("  FAIL  {name}  -- {reason}"),
            OracleOutcome::Error(why) => println!("  ERR   {name}  ({why})"),
            OracleOutcome::Skipped(why) => println!("  SKIP  {name}  ({why})"),
        }
    }
    println!(
        "\noracle: {} compared, {} passed, {} failed, {} errored, {} skipped",
        oracle.compared(),
        oracle.passed(),
        oracle.failed(),
        oracle.errored(),
        oracle.skipped()
    );

    // ── Language-coverage dashboard ─────────────────────────────────────────
    // The conformance dashboard published on every CI run: what fraction
    // of the OpenSCAD language surface morphe supports, cross-referenced against
    // the corpus run just completed. Always emitted (JSON + Markdown).
    let coverage = coverage_report(&report);
    let markdown = to_markdown(&coverage);
    println!();
    print!("{markdown}");
    emit_coverage_files(&coverage, &markdown);

    // Exit nonzero if the corpus check failed OR a *present*-oracle comparison
    // failed. A skipped oracle (no openscad) never fails the run.
    if report.all_passed() && oracle.exit_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Write the coverage report to `target/conformance/{coverage.json,coverage.md}`
/// under the workspace root. A write failure is a warning, not a hard error —
/// the summary already went to stdout for the CI job-summary.
fn emit_coverage_files(coverage: &morphe_conformance::CoverageReport, markdown: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("conformance");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("morphe-conformance: cannot create {}: {err}", dir.display());
        return;
    }
    let json = match serde_json::to_string_pretty(coverage) {
        Ok(json) => json,
        Err(err) => {
            eprintln!("morphe-conformance: cannot serialize coverage: {err}");
            return;
        }
    };
    let json_path = dir.join("coverage.json");
    let md_path = dir.join("coverage.md");
    if let Err(err) = std::fs::write(&json_path, json) {
        eprintln!(
            "morphe-conformance: cannot write {}: {err}",
            json_path.display()
        );
    }
    if let Err(err) = std::fs::write(&md_path, markdown) {
        eprintln!(
            "morphe-conformance: cannot write {}: {err}",
            md_path.display()
        );
    }
}
