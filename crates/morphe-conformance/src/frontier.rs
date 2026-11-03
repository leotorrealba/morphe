//! The unsupported-construct gate. The **frontier** is the set of constructs
//! morphe *parses but does not yet evaluate*. That set is currently EMPTY,
//! so the gate's job today is to assert it STAYS EMPTY. New deferrals (e.g.
//! roof/fill/render) are added deliberately — an entry in
//! [`FRONTIER_SOURCES`] plus a `corpus/v15/<construct>.scad` file — and this
//! module checks them when they are.
//!
//! Where [`crate::run_corpus`] asserts every `corpus/core` file resolves to
//! real geometry, this runner asserts the opposite for every `corpus/v15`
//! file: each **must fail at eval** with an [`EvalError`] of kind
//! [`Unsupported`](morphe_eval::eval::EvalErrorKind::Unsupported) naming the
//! frontier construct the file exercises — not a parse error, not a crash,
//! not a silent pass, not some other error. The construct a file exercises
//! is its filename stem: a future `corpus/v15/render.scad` would pin
//! `render`.
//!
//! # The ratchet
//!
//! The gate works as a one-way ratchet: the frontier can only shrink
//! deliberately. A frontier file has exactly three fates, per [`FrontierOutcome`]:
//!
//! - [`CorrectlyRejected`](FrontierOutcome::CorrectlyRejected) — eval produced
//!   the expected `Unsupported` error. The construct is still deferred; all is
//!   well.
//! - [`UnexpectedlyPassed`](FrontierOutcome::UnexpectedlyPassed) — eval produced
//!   **no** error. The construct has been implemented. This is a deliberate,
//!   loud *signal*: the frontier gate FAILS with a message instructing the
//!   author to move the file to `corpus/core` and mark the construct
//!   `Supported` in the coverage inventory. Implementing a deferred construct
//!   is therefore forced to update the corpus in the same change.
//! - [`WrongError`](FrontierOutcome::WrongError) — eval failed, but with a parse
//!   error, a [`General`](morphe_eval::eval::EvalErrorKind::General) error, or
//!   an `Unsupported` error naming the *wrong* construct. That is a bug: the
//!   frontier contract (parse succeeds, eval rejects with the right construct)
//!   is broken.
//!
//! # The empty frontier is the success state
//!
//! With no deferrals, `corpus/v15` is empty (or absent entirely) and the
//! gate passes trivially: `total() == 0`, all-correctly-rejected vacuously
//! true. A missing directory is the same success state, never an error.

use std::fs;
use std::path::{Path, PathBuf};

use morphe_eval::eval::{EvalErrorKind, Evaluator};
use morphe_lang::lexer::line_col;
use morphe_lang::parser::parse_program;

use crate::collect_scad;

/// What running one `corpus/v15` file through parse → eval produced, measured
/// against the frontier contract. See the [module docs](self#the-ratchet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontierOutcome {
    /// Eval produced the expected [`Unsupported`] error naming this file's
    /// construct: the construct is still correctly deferred.
    ///
    /// [`Unsupported`]: morphe_eval::eval::EvalErrorKind::Unsupported
    CorrectlyRejected,
    /// Eval produced no error — the construct now evaluates. A ratchet signal
    /// (not a crash): the file must move to `corpus/core` and the construct be
    /// marked `Supported`. See [`FrontierFileResult::ratchet_message`].
    UnexpectedlyPassed,
    /// Eval failed, but not with an [`Unsupported`] error naming this file's
    /// construct (a parse error, a `General` error, or the wrong construct).
    /// The frontier contract is broken — a bug. `detail` explains what happened.
    ///
    /// [`Unsupported`]: morphe_eval::eval::EvalErrorKind::Unsupported
    WrongError {
        /// A human-readable account of the unexpected error(s).
        detail: String,
    },
}

/// The result of running one `corpus/v15` file through the frontier gate.
#[derive(Debug, Clone)]
pub struct FrontierFileResult {
    /// Path to the `.scad` file.
    pub path: PathBuf,
    /// The frontier construct this file is expected to reject — its filename
    /// stem (`minkowski.scad` → `"minkowski"`).
    pub construct: String,
    /// What the pipeline actually did with it.
    pub outcome: FrontierOutcome,
}

impl FrontierFileResult {
    /// Whether this file upheld the frontier contract (was correctly rejected).
    #[must_use]
    pub fn correctly_rejected(&self) -> bool {
        self.outcome == FrontierOutcome::CorrectlyRejected
    }

    /// The loud ratchet message for a file whose construct now evaluates, or
    /// `None` if the construct is still deferred. Naming the construct in the
    /// message is what makes implementing a deferred feature force a deliberate
    /// corpus update.
    #[must_use]
    pub fn ratchet_message(&self) -> Option<String> {
        match self.outcome {
            FrontierOutcome::UnexpectedlyPassed => Some(format!(
                "frontier construct {c} now evaluates — move corpus/v15/{c}.scad to \
                 corpus/core and mark {c} Supported in the coverage inventory",
                c = self.construct
            )),
            _ => None,
        }
    }

    /// A one-line reason this file did not uphold the frontier contract, or
    /// `None` if it was correctly rejected. For an unexpected pass this is the
    /// [`ratchet_message`](Self::ratchet_message); for a wrong error it is the
    /// error detail.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        match &self.outcome {
            FrontierOutcome::CorrectlyRejected => None,
            FrontierOutcome::UnexpectedlyPassed => self.ratchet_message(),
            FrontierOutcome::WrongError { detail } => Some(format!(
                "expected an Unsupported error naming '{}', got: {detail}",
                self.construct
            )),
        }
    }
}

/// A summary over a whole `corpus/v15` run.
#[derive(Debug, Clone)]
pub struct FrontierReport {
    /// Per-file results, sorted by path.
    pub files: Vec<FrontierFileResult>,
}

impl FrontierReport {
    /// Total number of frontier files examined.
    #[must_use]
    pub fn total(&self) -> usize {
        self.files.len()
    }

    /// Whether every file was correctly rejected — the gate passes iff true.
    #[must_use]
    pub fn all_correctly_rejected(&self) -> bool {
        self.files
            .iter()
            .all(FrontierFileResult::correctly_rejected)
    }

    /// The files that broke the frontier contract, in path order.
    pub fn failures(&self) -> impl Iterator<Item = &FrontierFileResult> {
        self.files.iter().filter(|f| !f.correctly_rejected())
    }
}

/// The frontier construct a file exercises: its filename stem.
fn construct_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Run one `corpus/v15` file through the frontier gate.
#[must_use]
pub fn run_v15_file(path: &Path) -> FrontierFileResult {
    let construct = construct_of(path);
    let outcome = match fs::read_to_string(path) {
        Ok(src) => classify_source(&src, &construct),
        Err(e) => FrontierOutcome::WrongError {
            detail: format!("read: {e}"),
        },
    };
    FrontierFileResult {
        path: path.to_path_buf(),
        construct,
        outcome,
    }
}

/// Classify an in-memory frontier source against the construct it is expected
/// to reject (the testable core of [`run_v15_file`]).
///
/// The contract: parse must succeed (the failure is at *eval*, not parse), and
/// eval must record an [`Unsupported`](EvalErrorKind::Unsupported) error whose
/// `construct` equals `expected`.
#[must_use]
pub fn classify_source(src: &str, expected: &str) -> FrontierOutcome {
    // A frontier construct parses under the full grammar; it fails at EVAL.
    let stmts = match parse_program(src) {
        Ok(stmts) => stmts,
        Err(err) => {
            let (line, col) = line_col(src, err.span.start);
            return FrontierOutcome::WrongError {
                detail: format!(
                    "parse error at {line}:{col}: {} \
                     (a frontier file must parse; it must fail at eval, not parse)",
                    err.message
                ),
            };
        }
    };

    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    let _ = evaluator.eval_program(&stmts);
    let errors = evaluator.errors();

    // No error at all: the construct now evaluates — the ratchet signal.
    if errors.is_empty() {
        return FrontierOutcome::UnexpectedlyPassed;
    }

    // An Unsupported error naming exactly the expected construct is the pass.
    let matched = errors.iter().any(|e| {
        matches!(&e.kind, EvalErrorKind::Unsupported { construct, .. } if construct == expected)
    });
    if matched {
        FrontierOutcome::CorrectlyRejected
    } else {
        let detail = errors
            .iter()
            .map(|e| match &e.kind {
                EvalErrorKind::Unsupported { construct, .. } => {
                    format!("Unsupported('{construct}'): {}", e.message)
                }
                EvalErrorKind::General => format!("General: {}", e.message),
            })
            .collect::<Vec<_>>()
            .join("; ");
        FrontierOutcome::WrongError { detail }
    }
}

/// Run every `.scad` file under `dir` (recursively) through the frontier gate.
///
/// Returns a [`FrontierReport`]; a missing/unreadable directory yields an `Err`.
pub fn run_v15_frontier(dir: &Path) -> std::io::Result<FrontierReport> {
    let mut paths = Vec::new();
    // A MISSING directory is the empty-frontier success state (with nothing
    // deferred, `corpus/v15` need not exist) — an empty report, never an
    // error.
    if dir.is_dir() {
        collect_scad(dir, &mut paths)?;
    }
    paths.sort();
    let files = paths.iter().map(|p| run_v15_file(p)).collect();
    Ok(FrontierReport { files })
}

/// The default frontier corpus directory: `corpus/v15`, resolved relative to the
/// workspace root (baked in at compile time, like [`crate::default_corpus_dir`]).
#[must_use]
pub fn default_v15_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("corpus")
        .join("v15")
}

#[cfg(test)]
mod tests {
    use super::*;
    use morphe_eval::eval::{EvalErrorKind, Evaluator};
    use morphe_lang::parser::parse_program;

    /// A representative source for each frontier construct — the same shape as
    /// the corresponding `corpus/v15/<construct>.scad` file.
    // The frontier is currently EMPTY: every tracked construct evaluates.
    // The machinery stays for future deferrals (roof/fill/render) — add one
    // deliberately with an entry here + a corpus/v15 file.
    const FRONTIER_SOURCES: &[(&str, &str)] = &[];

    // ── each frontier construct is correctly rejected ─────────────────────────

    #[test]
    fn every_frontier_construct_is_correctly_rejected() {
        for (construct, src) in FRONTIER_SOURCES {
            assert_eq!(
                classify_source(src, construct),
                FrontierOutcome::CorrectlyRejected,
                "{construct} should be correctly rejected"
            );
        }
    }

    /// The exact stable diagnostic each frontier construct produces: the
    /// greppable `"unsupported: <construct> (not yet implemented in morphe)"` message plus a
    /// structured `Unsupported { construct }` kind naming the construct.
    #[test]
    fn exact_unsupported_message_and_construct() {
        for (construct, src) in FRONTIER_SOURCES {
            let stmts = parse_program(src).expect("frontier source parses");
            let mut ev = Evaluator::new();
            ev.set_source(*src);
            let _ = ev.eval_program(&stmts);
            let err = ev
                .errors()
                .iter()
                .find(|e| matches!(&e.kind, EvalErrorKind::Unsupported { .. }))
                .unwrap_or_else(|| panic!("{construct} must record an Unsupported error"));
            assert_eq!(
                err.message,
                format!("unsupported: {construct} (not yet implemented in morphe)"),
                "exact frontier message for {construct}"
            );
            match &err.kind {
                EvalErrorKind::Unsupported { construct: c, hint } => {
                    assert_eq!(c, construct, "kind must name the construct");
                    assert!(
                        hint.contains("not yet implemented"),
                        "hint for {construct} should say it is not yet implemented: {hint}"
                    );
                }
                other => panic!("expected Unsupported for {construct}, got {other:?}"),
            }
        }
    }

    // ── the frontier set is exactly UNSUPPORTED_MODULES ───────────────────────

    #[test]
    fn frontier_sources_cover_exactly_unsupported_modules() {
        use morphe_eval::eval::UNSUPPORTED_MODULES;
        use std::collections::HashSet;
        let sources: HashSet<&str> = FRONTIER_SOURCES.iter().map(|(c, _)| *c).collect();
        let unsupported: HashSet<&str> = UNSUPPORTED_MODULES.iter().copied().collect();
        assert_eq!(
            sources, unsupported,
            "the frontier source set must equal UNSUPPORTED_MODULES"
        );
    }

    // ── sanity: a supported construct is not a frontier construct ─────────────

    #[test]
    fn supported_construct_cube_is_not_a_frontier_construct() {
        // A plain cube evaluates fine, so measured as a frontier file it does
        // NOT get flagged as "correctly rejected" — it reads as an unexpected
        // pass (the ratchet signal), proving the gate distinguishes a real
        // frontier construct from an already-supported one.
        let outcome = classify_source("cube(10);", "cube");
        assert_eq!(
            outcome,
            FrontierOutcome::UnexpectedlyPassed,
            "a supported construct must never look correctly-rejected"
        );
        assert_ne!(outcome, FrontierOutcome::CorrectlyRejected);
    }

    // ── the failure is at eval, not parse ─────────────────────────────────────

    #[test]
    fn frontier_files_parse_but_fail_at_eval() {
        for (construct, src) in FRONTIER_SOURCES {
            // Parse succeeds under the full grammar…
            assert!(
                parse_program(src).is_ok(),
                "{construct} frontier source must parse"
            );
            // …and the rejection happens at eval, as an Unsupported error.
            let stmts = parse_program(src).unwrap();
            let mut ev = Evaluator::new();
            ev.set_source(*src);
            let _ = ev.eval_program(&stmts);
            assert!(
                ev.errors()
                    .iter()
                    .any(|e| matches!(&e.kind, EvalErrorKind::Unsupported { .. })),
                "{construct} must be rejected at eval, not parse"
            );
        }
    }

    // ── wrong-construct detection ─────────────────────────────────────────────

    // NOTE: there is no wrong-construct-detection test at present — it
    // requires a live frontier construct to produce a wrong-named
    // Unsupported error, and the frontier is EMPTY. The classification
    // machinery (FrontierOutcome::WrongError) stays for future deferrals
    // (roof/fill/render); add the test back when one exists.

    // ── the ratchet message names the construct (acceptance) ──────────────────

    #[test]
    fn divergence_fails_named() {
        // When a frontier construct diverges from expectation by evaluating, the
        // gate must fail with a message that NAMES the construct so the author
        // knows exactly what to move to core and mark Supported.
        let result = FrontierFileResult {
            path: PathBuf::from("corpus/v15/minkowski.scad"),
            construct: "minkowski".to_string(),
            outcome: FrontierOutcome::UnexpectedlyPassed,
        };
        assert!(!result.correctly_rejected());
        let msg = result
            .ratchet_message()
            .expect("an unexpected pass ratchets");
        assert_eq!(
            msg,
            "frontier construct minkowski now evaluates — move corpus/v15/minkowski.scad \
             to corpus/core and mark minkowski Supported in the coverage inventory"
        );
        // The same message backs failure_reason so the gate panic names it too.
        assert_eq!(result.failure_reason(), Some(msg));
    }

    // ── a correctly-rejected file has no failure reason ───────────────────────

    #[test]
    fn correctly_rejected_has_no_failure_reason() {
        let result = FrontierFileResult {
            path: PathBuf::from("corpus/v15/text.scad"),
            construct: "text".to_string(),
            outcome: FrontierOutcome::CorrectlyRejected,
        };
        assert!(result.correctly_rejected());
        assert_eq!(result.ratchet_message(), None);
        assert_eq!(result.failure_reason(), None);
    }
}
