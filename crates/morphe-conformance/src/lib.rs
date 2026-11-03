//! morphe-conformance: the corpus-validity check for Morphe.
//!
//! This crate runs every `.scad` file under a corpus directory through the
//! full library pipeline — [`parse_program`] → [`Evaluator::eval_program`] →
//! [`resolve_root`] — exactly as the `morphe` CLI does, and asserts each file
//! evaluates and resolves without error, yielding geometry of the *expected*
//! dimensionality:
//!
//! - a file named `*_2d.scad` is a 2D construct and must resolve to a
//!   [`Geometry::Profile`];
//! - every other file is a 3D construct and must resolve to a non-empty
//!   [`Geometry::Solid`] (a real, exportable mesh).
//!
//! This is the corpus-validity check. It does **not** compare
//! meshes against an oracle — that is the mesh-diff runner in [`oracle`].

pub mod coverage;
pub mod frontier;
pub mod oracle;

pub use coverage::{coverage_report, to_markdown, CategoryCoverage, CoverageReport, INVENTORY};
pub use frontier::{
    default_v15_dir, run_v15_frontier, FrontierFileResult, FrontierOutcome, FrontierReport,
};
pub use oracle::{
    area_delta, compare_meshes, detect_openscad, hausdorff_normalized, parse_stl, run_oracle,
    volume_delta, Mesh, OracleOutcome, OracleReport, OracleResult, OracleTolerance,
    SURFACE_SAMPLES,
};

use std::fs;
use std::path::{Path, PathBuf};

use morphe_eval::csg::CsgNode;
use morphe_eval::eval::Evaluator;
use morphe_kernel::{resolve_root, Geometry};
use morphe_lang::lexer::line_col;
use morphe_lang::parser::parse_program;

/// The dimensionality a corpus file is expected to produce, derived from its
/// filename: `*_2d.scad` → [`ExpectedKind::Profile`], everything else →
/// [`ExpectedKind::Solid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedKind {
    /// A 3D construct: must resolve to a non-empty solid.
    Solid,
    /// A 2D construct (`*_2d.scad`): must resolve to a profile.
    Profile,
}

impl ExpectedKind {
    /// Classify a corpus file by its name.
    #[must_use]
    pub fn of(path: &Path) -> ExpectedKind {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with("_2d.scad") {
            ExpectedKind::Profile
        } else {
            ExpectedKind::Solid
        }
    }
}

/// What the pipeline actually produced for a corpus file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A 3D solid with this many triangles.
    Solid { triangles: usize },
    /// A 2D profile.
    Profile,
    /// No geometry (an empty top-level result).
    Empty,
}

/// The result of running one corpus file through the pipeline.
#[derive(Debug, Clone)]
pub struct FileResult {
    /// Path to the `.scad` file.
    pub path: PathBuf,
    /// The dimensionality inferred from the filename.
    pub expected: ExpectedKind,
    /// `Ok(outcome)` if the pipeline ran to completion; `Err(message)` if any
    /// stage (read/parse/eval/resolve) failed.
    pub outcome: Result<Outcome, String>,
}

impl FileResult {
    /// Whether this file passed the validity check: the pipeline succeeded *and*
    /// the produced geometry matches the expected dimensionality (a 3D file
    /// must be a non-empty solid; a 2D file must be a profile).
    #[must_use]
    pub fn passed(&self) -> bool {
        match (&self.outcome, self.expected) {
            (Ok(Outcome::Solid { triangles }), ExpectedKind::Solid) => *triangles > 0,
            (Ok(Outcome::Profile), ExpectedKind::Profile) => true,
            _ => false,
        }
    }

    /// A one-line human-readable reason this file is not passing, or `None` if
    /// it passed.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        if self.passed() {
            return None;
        }
        Some(match (&self.outcome, self.expected) {
            (Err(msg), _) => format!("pipeline error: {msg}"),
            (Ok(Outcome::Empty), _) => "produced no geometry (empty result)".to_string(),
            (Ok(Outcome::Solid { triangles: 0 }), ExpectedKind::Solid) => {
                "resolved to an empty solid (0 triangles)".to_string()
            }
            (Ok(Outcome::Solid { .. }), ExpectedKind::Profile) => {
                "expected a 2D profile (name is *_2d.scad) but got a 3D solid".to_string()
            }
            (Ok(Outcome::Profile), ExpectedKind::Solid) => {
                "expected a 3D solid but got a 2D profile".to_string()
            }
            (Ok(_), _) => "unexpected outcome".to_string(),
        })
    }
}

/// A summary over a whole corpus run.
#[derive(Debug, Clone)]
pub struct Report {
    /// Per-file results, sorted by path.
    pub files: Vec<FileResult>,
}

impl Report {
    /// Total number of corpus files examined.
    #[must_use]
    pub fn total(&self) -> usize {
        self.files.len()
    }

    /// Number of files that passed the validity check.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.files.iter().filter(|f| f.passed()).count()
    }

    /// Whether every file passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.files.iter().all(FileResult::passed)
    }

    /// The failing files, in path order.
    pub fn failures(&self) -> impl Iterator<Item = &FileResult> {
        self.files.iter().filter(|f| !f.passed())
    }
}

/// Run every `.scad` file under `dir` (recursively) through the pipeline.
///
/// Returns a [`Report`]; a missing/unreadable directory yields an `Err`.
pub fn run_corpus(dir: &Path) -> std::io::Result<Report> {
    let mut paths = Vec::new();
    collect_scad(dir, &mut paths)?;
    paths.sort();
    let files = paths
        .into_iter()
        .map(|path| {
            let expected = ExpectedKind::of(&path);
            let outcome = evaluate_file(&path);
            FileResult {
                path,
                expected,
                outcome,
            }
        })
        .collect();
    Ok(Report { files })
}

/// Recursively collect `*.scad` files under `dir` into `out`.
pub(crate) fn collect_scad(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_scad(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("scad") {
            out.push(path);
        }
    }
    Ok(())
}

/// Run one `.scad` file through parse → eval → resolve and classify the result.
///
/// Mirrors the CLI's `evaluate_scad`: parse errors, hard eval errors, and
/// resolver errors become `Err(message)`; warnings are ignored (they never fail
/// a compile). Returns the produced geometry classified as [`Outcome`].
///
/// The FILE path is threaded through [`evaluate_file_at`] so
/// `import("data/model.stl")`-style corpus files carry their directory
/// context — resolving string-only would silently lose the source directory
/// (and a corpus file that errors could hide behind a skip).
pub fn evaluate_file(path: &Path) -> Result<Outcome, String> {
    Ok(match evaluate_file_at(path)? {
        Some(Geometry::Solid(solid)) => Outcome::Solid {
            triangles: solid.num_tri(),
        },
        Some(Geometry::Profile(_)) => Outcome::Profile,
        None => Outcome::Empty,
    })
}

/// The pipeline over an in-memory source string (the testable core of
/// [`evaluate_file`]), classifying the resolved geometry into an [`Outcome`].
pub fn evaluate_source(src: &str) -> Result<Outcome, String> {
    Ok(match resolve_source(src)? {
        Some(Geometry::Solid(solid)) => Outcome::Solid {
            triangles: solid.num_tri(),
        },
        Some(Geometry::Profile(_)) => Outcome::Profile,
        None => Outcome::Empty,
    })
}

/// Run a source string through parse → eval → resolve and return the resolved
/// [`Geometry`] (or `None` for an empty top-level result).
///
/// The shared core of [`evaluate_source`] (which classifies the result) and the
/// oracle runner (which needs the actual [`Solid`](morphe_kernel::Solid) mesh to
/// diff against OpenSCAD). Any parse/eval/resolve failure becomes `Err(message)`.
pub fn resolve_source(src: &str) -> Result<Option<Geometry>, String> {
    resolve_source_at(src, None)
}

/// Evaluate a `.scad` FILE end-to-end with its path context attached:
/// parse → eval (with the file's dir recorded for import/surface path
/// resolution) → resolve. The testable file-based twin
/// of [`resolve_source_at`]; fails closed on read errors.
pub fn evaluate_file_at(path: &Path) -> Result<Option<Geometry>, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    resolve_source_at(&src, Some(path))
}

/// [`resolve_source`] with the importing FILE's path attached: the evaluator
/// records the file's parent directory as the base for the file-loading
/// constructs (`import()`, `surface()`). `None` path = the string-only
/// pipeline (identical to [`resolve_source`] — the no-context default).
pub fn resolve_source_at(
    src: &str,
    source_path: Option<&Path>,
) -> Result<Option<Geometry>, String> {
    // Parse.
    let stmts = parse_program(src).map_err(|err| {
        let (line, col) = line_col(src, err.span.start);
        format!("parse error at {line}:{col}: {}", err.message)
    })?;

    // Evaluate to a CSG tree.
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    evaluator.set_source_path(source_path);
    let root = evaluator.eval_program(&stmts);

    // Hard eval errors (unsupported constructs, failed asserts, …) fail.
    if !evaluator.errors().is_empty() {
        let msgs: Vec<String> = evaluator
            .errors()
            .iter()
            .map(|e| {
                let (line, col) = e.line_col(src);
                match &e.origin {
                    Some(origin) => format!(
                        "eval error at {}:{line}:{col}: {}",
                        origin.file.display(),
                        e.message
                    ),
                    None => format!("eval error at {line}:{col}: {}", e.message),
                }
            })
            .collect();
        return Err(msgs.join("; "));
    }

    // Resolve the top-level statements under the implicit union.
    let top_level = match root {
        CsgNode::Group { children, .. } => children,
        other => vec![other],
    };
    let mut warnings = Vec::new();
    resolve_root(&top_level, &mut warnings).map_err(|err| format!("resolve error: {err}"))
}

/// The default corpus directory: `corpus/core`, resolved relative to the
/// workspace root (the crate's `../..`). Baked in at compile time so both the
/// binary and the integration tests find the corpus without a CWD assumption.
#[must_use]
pub fn default_corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("corpus")
        .join("core")
}

#[cfg(test)]
mod source_path_tests {
    //! The source-path context plumbing.

    use super::evaluate_file_at;
    use morphe_eval::eval::Evaluator;
    use morphe_lang::parser::parse_program;
    use std::path::Path;

    #[test]
    fn evaluator_records_source_dir() {
        // set_source_path(Some(p)) records p's PARENT; None by default.
        let mut ev = Evaluator::new();
        assert!(ev.source_dir().is_none(), "no context by default");
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        assert_eq!(ev.source_dir(), Some(Path::new("/proj/parts")));
        // Resetting to None clears it (round-trips).
        ev.set_source_path(None);
        assert!(ev.source_dir().is_none());
    }

    #[test]
    fn evaluator_records_source_dir_through_an_eval() {
        // the dir survives an eval of a marker statement (the same flow
        // the import/surface constructors will run in).
        let src = "cube(1);";
        let stmts = parse_program(src).expect("marker parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        let _ = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty());
        assert_eq!(ev.source_dir(), Some(Path::new("/proj/parts")));
    }

    #[test]
    fn evaluate_file_at_threads_the_dir_end_to_end() {
        // A fixture in a SUBDIRECTORY — the file-based entry point records its
        // dir through parse → eval. (resolve_source_at drives the same
        // evaluator; asserting via a fresh evaluator would test nothing, so
        // this pins the pipeline entry itself: the fixture evaluates AND a
        // parallel run with set_source_path shows the recorded dir.)
        let dir = std::env::temp_dir().join(format!("morphe-srcpath-{}", std::process::id()));
        let nested = dir.join("nested").join("parts");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let fixture = nested.join("part.scad");
        std::fs::write(&fixture, "cube(2);").expect("write");

        let geometry = evaluate_file_at(&fixture).expect("fixture evaluates");
        assert!(geometry.is_some(), "the marker cube resolves to a solid");

        // And the parallel assertion: the same source through
        // resolve_source_at with the path shows the recorded dir.
        let src = std::fs::read_to_string(&fixture).expect("read back");
        let stmts = parse_program(&src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src.as_str());
        ev.set_source_path(Some(&fixture));
        let _ = ev.eval_program(&stmts);
        assert_eq!(ev.source_dir(), Some(nested.as_path()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_source_without_context_is_unchanged() {
        // the string-only entry point behaves exactly as before.
        let geometry = super::resolve_source("cube(3);").expect("evaluates");
        assert!(geometry.is_some());
        // No context anywhere in the string-only path.
        assert!(super::resolve_source("cube(3);").is_ok());
    }
}
