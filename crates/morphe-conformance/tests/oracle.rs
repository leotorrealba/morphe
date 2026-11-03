//! The oracle mesh-diff gate: compare morphe's mesh against
//! OpenSCAD's for every 3D corpus file, degrading to *skipped* (never *failed*)
//! whenever the `openscad` binary is absent.

use std::sync::Mutex;

use morphe_conformance::{default_corpus_dir, detect_openscad, run_oracle, OracleTolerance};

/// Both tests below mutate/read `OPENSCAD_BIN` via `detect_openscad`; cargo
/// runs them in parallel THREADS of one process, so the env var is shared
/// mutable state. Serialize the detect/env sections (the flake was observed:
/// the present-test saw the absent-test's bogus path and compared 0 files).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// THE key acceptance test: with `OPENSCAD_BIN` pointing at a path that does
/// not exist, the oracle must run over the whole corpus and record **every**
/// file as skipped — no comparisons, no failures, exit OK. This is the
/// graceful-degradation contract that lets CI (which has no OpenSCAD) stay green.
#[test]
fn oracle_absent_degrades_to_skip() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let key = "OPENSCAD_BIN";
    let prev = std::env::var_os(key);
    std::env::set_var(key, "/definitely/not/a/real/openscad/binary");

    let report = run_oracle(&default_corpus_dir(), &OracleTolerance::default())
        .expect("corpus/core is readable");

    // Restore the env before asserting so a failure cannot leak the override.
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }

    assert!(
        report.openscad.is_none(),
        "a non-existent OPENSCAD_BIN must resolve to no binary"
    );
    assert_eq!(
        report.compared(),
        0,
        "nothing may be compared without openscad"
    );
    assert_eq!(
        report.failed(),
        0,
        "an absent oracle must never fail a file"
    );
    assert_eq!(
        report.skipped(),
        report.results.len(),
        "every corpus file must be skipped"
    );
    assert!(
        report.exit_ok(),
        "an all-skipped run must exit OK (this is the whole point)"
    );
    assert!(
        !report.results.is_empty(),
        "the corpus should not be empty — skip means we still walked it"
    );
}

/// When `openscad` *is* installed this exercises the real render-and-diff path:
/// every 3D corpus file must compare within tolerance, so the run passes with a
/// nonzero compared count. The test **self-skips** (returns early) when no
/// `openscad` is found, so CI without the binary still passes.
#[test]
fn oracle_present_compares_corpus() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let Some(bin) = detect_openscad() else {
        eprintln!("openscad not installed; skipping the real-comparison oracle test");
        return;
    };
    eprintln!("running oracle against openscad at {}", bin.display());

    let report = run_oracle(&default_corpus_dir(), &OracleTolerance::default())
        .expect("corpus/core is readable");

    assert!(
        report.compared() > 0,
        "with openscad present, some 3D files must actually be compared"
    );
    if report.failed() > 0 {
        let details: String = report
            .failures()
            .map(|r| format!("\n  {}: {:?}", r.path.display(), r.outcome))
            .collect();
        panic!(
            "{} corpus files diverged from openscad past tolerance:{details}",
            report.failed()
        );
    }
    assert!(report.exit_ok());
}

/// The skip-masks-error trap: in `oracle_one`, a
/// morphe-side resolve failure collapses to `Skipped("morphe error")` — a pass.
/// That's a legitimate degradation for genuinely-oracle-incomparable geometry,
/// but it must never mask a broken corpus file. This assertion pins the
/// complement: every corpus file whose source uses a file-loading construct
/// (`import`/`surface`) must resolve to a non-empty solid on the morphe side —
/// if it landed in `Skipped("morphe error")` the corpus gate (and this test)
/// would catch it.
#[test]
fn import_corpus_files_resolve_and_never_hide_behind_morphe_error_skips() {
    let dir = default_corpus_dir();
    let mut checked = 0;
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("corpus dir readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("scad") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("corpus file readable");
            let loads_file = src.contains("import(") || src.contains("import (");
            if !loads_file {
                continue;
            }
            checked += 1;
            match morphe_conformance::evaluate_file(&path) {
                Ok(morphe_conformance::Outcome::Solid { triangles }) => {
                    assert!(
                        triangles > 0,
                        "{} resolved to an empty solid — that would mask as a skip in the oracle",
                        path.display()
                    );
                }
                other => panic!(
                    "{} must resolve to a non-empty solid (never `Skipped(morphe error)`), got {other:?}",
                    path.display()
                ),
            }
        }
    }
    assert!(
        checked >= 2,
        "the import corpus files (import.scad + import_ascii.scad) must be present, checked {checked}"
    );
}
