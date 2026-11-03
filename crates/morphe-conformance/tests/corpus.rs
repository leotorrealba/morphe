//! The corpus-validity gate: the entire `corpus/core` must
//! evaluate + resolve to the expected geometry, 100%, with zero unsupported
//! constructs or errors.

use morphe_conformance::{default_corpus_dir, evaluate_file, run_corpus, ExpectedKind, Outcome};

/// Every `.scad` file in `corpus/core` runs through the full pipeline and
/// resolves to the geometry its name implies — a hard gate. On any failure the
/// panic lists each offending file and why, so a break points straight at
/// either a bad corpus file or a real kernel/eval gap.
#[test]
fn corpus_core_all_compile() {
    let dir = default_corpus_dir();
    let report = run_corpus(&dir).expect("corpus/core directory is readable");

    // The corpus must hold 30+ construct files plus 5 real parts.
    assert!(
        report.total() >= 35,
        "expected at least 35 corpus files, found {}",
        report.total()
    );

    if !report.all_passed() {
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
            "{} of {} corpus files failed the validity gate:{details}",
            report.total() - report.passed(),
            report.total()
        );
    }
}

/// Files named `*_2d.scad` are the 2D-primitive coverage and must resolve to
/// profiles; every other file must resolve to a non-empty solid.
#[test]
fn dimensionality_matches_naming() {
    let dir = default_corpus_dir();
    let report = run_corpus(&dir).expect("corpus/core directory is readable");

    let mut profiles = 0;
    let mut solids = 0;
    for file in &report.files {
        match (file.expected, &file.outcome) {
            (ExpectedKind::Profile, Ok(Outcome::Profile)) => profiles += 1,
            (ExpectedKind::Solid, Ok(Outcome::Solid { triangles })) => {
                assert!(
                    *triangles > 0,
                    "{} is a 3D file but resolved to an empty solid",
                    file.path.display()
                );
                solids += 1;
            }
            (expected, outcome) => panic!(
                "{}: expected {expected:?} but got {outcome:?}",
                file.path.display()
            ),
        }
    }
    assert!(
        profiles >= 5,
        "expected the five *_2d profile files, got {profiles}"
    );
    assert!(solids >= 30, "expected 30+ solid files, got {solids}");
}

/// A known real part resolves to a non-empty mesh with a sane triangle count —
/// a concrete floor so a silent regression to empty geometry is caught.
#[test]
fn bracket_part_has_geometry() {
    let path = default_corpus_dir().join("parts").join("bracket.scad");
    match evaluate_file(&path).expect("bracket.scad compiles") {
        Outcome::Solid { triangles } => assert!(
            triangles > 12,
            "bracket.scad should produce a substantial mesh, got {triangles} triangles"
        ),
        other => panic!("bracket.scad should be a solid, got {other:?}"),
    }
}
