//! The unsupported-construct (frontier) gate: every `corpus/v15` file must
//! fail at eval with an `Unsupported` error naming its construct — the
//! ratchet that pins the parse-but-defer set — and the frontier corpus must
//! stay in lock-step with the coverage inventory's DeferredV15 list.

use std::collections::HashSet;

use morphe_conformance::{
    default_v15_dir, run_v15_frontier, FrontierOutcome, FrontierReport, INVENTORY,
};

/// The construct each frontier file exercises: its filename stem.
fn v15_corpus_constructs(report: &FrontierReport) -> HashSet<String> {
    report.files.iter().map(|f| f.construct.clone()).collect()
}

/// The DeferredV15 construct names from the coverage inventory.
fn deferred_inventory_constructs() -> HashSet<String> {
    use morphe_conformance::coverage::Status;
    INVENTORY
        .iter()
        .filter(|c| c.status == Status::DeferredV15)
        .map(|c| c.name.to_string())
        .collect()
}

/// THE frontier gate: every `corpus/v15` file must be correctly rejected with
/// the expected `Unsupported` error. If a file unexpectedly passes (its
/// construct got implemented) the panic names it and tells the author to move
/// it to `corpus/core` — the ratchet that forces implementing a deferred construct
/// to update the corpus deliberately. A wrong error (parse failure, generic
/// error, wrong construct) is flagged as a bug.
#[test]
fn v15_frontier_all_correctly_rejected() {
    let dir = default_v15_dir();
    let report = run_v15_frontier(&dir).expect("corpus/v15 directory is readable");

    // The frontier corpus is currently EMPTY — the success state.
    // (`frontier_empty_is_the_success_state` pins the missing-dir variant.)
    assert_eq!(
        report.total(),
        0,
        "the frontier is empty: corpus/v15 must be empty, found {}",
        report.total()
    );

    if !report.all_correctly_rejected() {
        let details: String = report
            .failures()
            .map(|f| {
                format!(
                    "\n  {}: {}",
                    f.path.display(),
                    f.failure_reason().unwrap_or_default()
                )
            })
            .collect();
        panic!(
            "{} of {} frontier files did not uphold the frontier contract:{details}",
            report.total()
                - report
                    .files
                    .iter()
                    .filter(|f| f.correctly_rejected())
                    .count(),
            report.total()
        );
    }
}

/// Every frontier file evaluates to the ratchet's `CorrectlyRejected` outcome —
/// never a silent pass or a wrong error — for the whole live corpus.
#[test]
fn v15_frontier_outcomes_are_all_correctly_rejected() {
    let report = run_v15_frontier(&default_v15_dir()).expect("corpus/v15 is readable");
    for f in &report.files {
        assert_eq!(
            f.outcome,
            FrontierOutcome::CorrectlyRejected,
            "{} produced {:?}",
            f.path.display(),
            f.outcome
        );
    }
}

/// The frontier corpus covers exactly the `UNSUPPORTED_MODULES`.
#[test]
fn v15_corpus_covers_exactly_the_unsupported_modules() {
    use morphe_eval::eval::UNSUPPORTED_MODULES;
    let report = run_v15_frontier(&default_v15_dir()).expect("corpus/v15 is readable");
    let corpus = v15_corpus_constructs(&report);
    let unsupported: HashSet<String> = UNSUPPORTED_MODULES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        corpus, unsupported,
        "the corpus/v15 files must correspond one-to-one with UNSUPPORTED_MODULES"
    );
}

/// The inventory↔corpus consistency check (ties the coverage report to the
/// frontier corpus): every DeferredV15 construct has a `corpus/v15/<name>.scad`
/// file, and every frontier file corresponds to a DeferredV15 construct.
#[test]
fn coverage_deferred_list_matches_v15_corpus() {
    let report = run_v15_frontier(&default_v15_dir()).expect("corpus/v15 is readable");
    let corpus = v15_corpus_constructs(&report);
    let deferred = deferred_inventory_constructs();
    assert_eq!(
        deferred, corpus,
        "the coverage inventory's DeferredV15 list and the corpus/v15 files must \
         stay in lock-step — implementing a construct must update both"
    );
}

/// Every frontier file names the exact greppable `unsupported:` message for its
/// construct — the same stable diagnostic the CLI and AI loop key off, asserted
/// end-to-end over the real corpus files (not just in-memory sources).
#[test]
fn v15_corpus_files_emit_the_exact_unsupported_message() {
    use morphe_eval::eval::{EvalErrorKind, Evaluator};
    use morphe_lang::parser::parse_program;

    let report = run_v15_frontier(&default_v15_dir()).expect("corpus/v15 is readable");
    for f in &report.files {
        let src = std::fs::read_to_string(&f.path).expect("frontier file readable");
        let stmts =
            parse_program(&src).unwrap_or_else(|_| panic!("{} must parse", f.path.display()));
        let mut ev = Evaluator::new();
        ev.set_source(&src);
        let _ = ev.eval_program(&stmts);
        let err = ev
            .errors()
            .iter()
            .find(|e| matches!(&e.kind, EvalErrorKind::Unsupported { construct, .. } if *construct == f.construct))
            .unwrap_or_else(|| panic!("{} must emit an Unsupported error for {}", f.path.display(), f.construct));
        assert_eq!(
            err.message,
            format!(
                "unsupported: {} (not yet implemented in morphe)",
                f.construct
            ),
            "{} must emit the exact frontier message",
            f.path.display()
        );
    }
}

// ── the empty-frontier success state ───────────────────────────────────────

/// A MISSING `corpus/v15` directory is the success state (with nothing
/// deferred it need not exist): Ok, zero files — never an error.
#[test]
fn frontier_empty_is_the_success_state() {
    let dir = std::env::temp_dir().join(format!("morphe-frontier-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // ensure absent
    let report = run_v15_frontier(&dir).expect("a missing frontier dir is the success state");
    assert_eq!(report.total(), 0, "missing dir → 0 frontier files");
    assert!(
        report.all_correctly_rejected(),
        "all-correctly-rejected is vacuously true over the empty set"
    );
}

/// The full cross-assertion for the current state: the frontier corpus is
/// empty, the inventory's DeferredV15 list is empty, and UNSUPPORTED_MODULES
/// is a typed empty slice — all three agree, empty == empty == empty.
#[test]
fn the_frontier_is_closed_everywhere() {
    let report = run_v15_frontier(&default_v15_dir()).expect("frontier dir readable or absent");
    assert_eq!(report.total(), 0, "no corpus/v15 files");
    assert!(
        deferred_inventory_constructs().is_empty(),
        "no DeferredV15 constructs"
    );
    assert!(
        morphe_eval::eval::UNSUPPORTED_MODULES.is_empty(),
        "UNSUPPORTED_MODULES is a typed empty slice"
    );
    let _ = morphe_conformance::FrontierOutcome::CorrectlyRejected; // machinery alive
}
