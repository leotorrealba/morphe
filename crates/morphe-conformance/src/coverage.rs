//! Language-surface coverage report.
//!
//! This is the "conformance dashboard" metric CI publishes on every PR:
//! **what fraction of the OpenSCAD language surface morphe supports today**.
//! It is emphatically *not* test coverage — no line or branch percentage of
//! the Rust code is involved. "Coverage" here means *language-surface
//! coverage against the goal of full OpenSCAD compatibility*: of the
//! enumerated OpenSCAD constructs morphe tracks, how many are already
//! implemented versus deferred (the set of not-yet-supported constructs).
//!
//! The metric has two halves:
//!
//! 1. **Static inventory** ([`INVENTORY`]) — the enumerated set of constructs,
//!    each tagged [`Supported`](Status::Supported) (implemented) or
//!    [`DeferredV15`](Status::DeferredV15) (a construct listed in
//!    [`UNSUPPORTED_MODULES`]; currently none). The headline coverage
//!    percentage is `supported / (supported + deferred) · 100`.
//! 2. **Dynamic cross-reference** — each supported construct should be
//!    *exercised* by a corpus file that actually passes the corpus-validity
//!    gate ([`crate::run_corpus`]). [`coverage_report`] cross-references the
//!    static inventory against a live [`Report`], so a construct we *claim* to
//!    support but never exercise end-to-end is flagged in
//!    [`CoverageReport::uncovered_supported`] as a gap rather than silently
//!    counted as covered.
//!
//! [`UNSUPPORTED_MODULES`]: morphe_eval::eval::UNSUPPORTED_MODULES

use serde::{Deserialize, Serialize};

use crate::Report;

/// The broad family a construct belongs to, used for the per-category
/// breakdown. Serializes snake_case for the JSON report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// The seven geometry primitives (cube/sphere/…/polygon) plus the deferred
    /// primitive-shaped loaders (surface/text/import).
    Primitive,
    /// The 2D↔3D family: linear_extrude/rotate_extrude, and the deferred
    /// inverse `projection`.
    Extrude,
    /// CSG boolean operators (union/difference/intersection/hull) and the
    /// deferred `minkowski`.
    Boolean,
    /// Affine and rendering transforms (translate/…/color) and the deferred
    /// 2D `offset`.
    Transform,
    /// Language-level constructs: variables, control flow, modules/functions,
    /// comprehensions, special variables, string/math builtins.
    Language,
}

impl Category {
    /// Every category, in display order.
    pub const ALL: [Category; 5] = [
        Category::Primitive,
        Category::Extrude,
        Category::Boolean,
        Category::Transform,
        Category::Language,
    ];

    /// A human-readable label for the Markdown table.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Category::Primitive => "Primitives",
            Category::Extrude => "Extrudes",
            Category::Boolean => "Booleans",
            Category::Transform => "Transforms",
            Category::Language => "Language",
        }
    }
}

/// Whether a construct is implemented or deferred (not yet supported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Implemented and resolvable in v1.
    Supported,
    /// Parsed but not yet supported (one of [`UNSUPPORTED_MODULES`]).
    ///
    /// [`UNSUPPORTED_MODULES`]: morphe_eval::eval::UNSUPPORTED_MODULES
    DeferredV15,
}

/// One entry in the static construct inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Construct {
    /// The construct's name (the OpenSCAD keyword, or a stable identifier for
    /// language features like `list_comprehension`).
    pub name: &'static str,
    /// The family it belongs to.
    pub category: Category,
    /// Whether it is supported or not yet supported.
    pub status: Status,
    /// The corpus file (basename) that exercises it end-to-end, if any. Every
    /// supported construct should name one; a `None` on a supported construct
    /// is a coverage gap.
    pub corpus_file: Option<&'static str>,
}

/// The enumerated OpenSCAD language surface morphe tracks.
///
/// Tracked: the seven primitives, both extrudes, the four booleans, the
/// seven transforms, and thirteen language constructs. Added later (formerly
/// [`UNSUPPORTED_MODULES`](morphe_eval::eval::UNSUPPORTED_MODULES)):
/// minkowski/surface/text/offset/import/projection, each slotted into its
/// category.
pub const INVENTORY: &[Construct] = &[
    // ── Primitives ──────────────────────────────────────────────────────────
    sup("cube", Category::Primitive, "cube_scalar.scad"),
    sup("sphere", Category::Primitive, "sphere_fn.scad"),
    sup("cylinder", Category::Primitive, "cylinder_straight.scad"),
    sup("polyhedron", Category::Primitive, "polyhedron.scad"),
    sup("square", Category::Primitive, "square_2d.scad"),
    sup("circle", Category::Primitive, "circle_2d.scad"),
    sup("polygon", Category::Primitive, "polygon_2d.scad"),
    // ── Extrudes ────────────────────────────────────────────────────────────
    sup(
        "linear_extrude",
        Category::Extrude,
        "linear_extrude_basic.scad",
    ),
    sup(
        "rotate_extrude",
        Category::Extrude,
        "rotate_extrude_full.scad",
    ),
    // ── Booleans ────────────────────────────────────────────────────────────
    sup("union", Category::Boolean, "union.scad"),
    sup("difference", Category::Boolean, "difference.scad"),
    sup("intersection", Category::Boolean, "intersection.scad"),
    sup("hull", Category::Boolean, "hull_3d.scad"),
    // ── Transforms ──────────────────────────────────────────────────────────
    sup("translate", Category::Transform, "translate.scad"),
    sup("rotate", Category::Transform, "rotate_scalar.scad"),
    sup("scale", Category::Transform, "scale.scad"),
    sup("mirror", Category::Transform, "mirror.scad"),
    sup("multmatrix", Category::Transform, "multmatrix.scad"),
    sup("resize", Category::Transform, "resize.scad"),
    sup("color", Category::Transform, "color.scad"),
    // ── Language ────────────────────────────────────────────────────────────
    sup("variables", Category::Language, "variables.scad"),
    sup("for", Category::Language, "for_loop.scad"),
    sup("if", Category::Language, "if_else.scad"),
    sup("module", Category::Language, "module_children.scad"),
    sup("function", Category::Language, "function_call.scad"),
    sup("children", Category::Language, "module_children.scad"),
    sup(
        "list_comprehension",
        Category::Language,
        "list_comprehension.scad",
    ),
    sup("let", Category::Language, "let.scad"),
    sup("special_variables", Category::Language, "special_vars.scad"),
    sup("recursion", Category::Language, "recursion_function.scad"),
    sup("ternary", Category::Language, "ternary.scad"),
    sup("string_functions", Category::Language, "string_ops.scad"),
    sup("math_builtins", Category::Language, "arithmetic.scad"),
    // ── Formerly deferred (previously in UNSUPPORTED_MODULES), now supported.
    // The set of unsupported constructs is now EMPTY (pinned by
    // `coverage_reports_100_percent_39_of_39`).
    sup("minkowski", Category::Boolean, "minkowski.scad"),
    sup("surface", Category::Primitive, "surface.scad"),
    sup("text", Category::Primitive, "text.scad"),
    sup("offset", Category::Transform, "offset.scad"),
    sup("import", Category::Primitive, "import.scad"),
    sup("projection", Category::Extrude, "projection_2d.scad"),
];

/// A supported construct with the corpus file that exercises it.
const fn sup(name: &'static str, category: Category, corpus_file: &'static str) -> Construct {
    Construct {
        name,
        category,
        status: Status::Supported,
        corpus_file: Some(corpus_file),
    }
}

/// A deferred construct (no corpus file — it does not resolve yet).
/// Currently UNUSED (nothing is deferred) — kept for the next set of
/// unsupported constructs (roof/fill/render), which will need this
/// constructor again.
#[allow(dead_code)]
const fn deferred(name: &'static str, category: Category) -> Construct {
    Construct {
        name,
        category,
        status: Status::DeferredV15,
        corpus_file: None,
    }
}

/// Coverage figures for one construct category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryCoverage {
    /// The category.
    pub category: Category,
    /// Supported constructs in this category.
    pub supported: usize,
    /// Not-yet-supported constructs in this category.
    pub deferred: usize,
    /// `supported / (supported + deferred) · 100`.
    pub coverage_percent: f64,
}

/// The machine-readable coverage report, serialized to JSON on every PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Total tracked constructs (`supported_count + deferred_count`).
    pub total_constructs: usize,
    /// Constructs implemented in v1.
    pub supported_count: usize,
    /// Constructs not yet supported.
    pub deferred_count: usize,
    /// `supported_count / total_constructs · 100` — the headline metric.
    pub coverage_percent: f64,
    /// Per-category breakdown, in [`Category::ALL`] order.
    pub categories: Vec<CategoryCoverage>,
    /// Names of every supported construct.
    pub supported: Vec<String>,
    /// Names of every deferred construct.
    pub deferred: Vec<String>,
    /// Supported constructs whose corpus file passed the validity gate — the
    /// dynamically-confirmed subset.
    pub corpus_exercised: Vec<String>,
    /// Supported constructs with no passing corpus file (a coverage gap: either
    /// no corpus file is assigned, or its file did not pass this run).
    pub uncovered_supported: Vec<String>,
}

/// `supported / total · 100`, defined as `0.0` for an empty set. Yields exactly
/// `100.0` iff nothing is deferred (`supported == total`).
fn percent(supported: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        supported as f64 / total as f64 * 100.0
    }
}

/// Build the coverage report from the canonical [`INVENTORY`] and a live corpus
/// run, cross-referencing each supported construct against its corpus file.
#[must_use]
pub fn coverage_report(corpus_report: &Report) -> CoverageReport {
    coverage_from(INVENTORY, corpus_report)
}

/// The testable core of [`coverage_report`], parameterized over an inventory so
/// the corpus cross-reference can be exercised with a synthetic inventory.
fn coverage_from(inventory: &[Construct], corpus_report: &Report) -> CoverageReport {
    let supported: Vec<String> = inventory
        .iter()
        .filter(|c| c.status == Status::Supported)
        .map(|c| c.name.to_string())
        .collect();
    let deferred: Vec<String> = inventory
        .iter()
        .filter(|c| c.status == Status::DeferredV15)
        .map(|c| c.name.to_string())
        .collect();

    let mut corpus_exercised = Vec::new();
    let mut uncovered_supported = Vec::new();
    for c in inventory.iter().filter(|c| c.status == Status::Supported) {
        if c.corpus_file
            .is_some_and(|f| corpus_file_passed(corpus_report, f))
        {
            corpus_exercised.push(c.name.to_string());
        } else {
            uncovered_supported.push(c.name.to_string());
        }
    }

    let categories = Category::ALL
        .iter()
        .map(|&cat| {
            let sup = inventory
                .iter()
                .filter(|c| c.category == cat && c.status == Status::Supported)
                .count();
            let def = inventory
                .iter()
                .filter(|c| c.category == cat && c.status == Status::DeferredV15)
                .count();
            CategoryCoverage {
                category: cat,
                supported: sup,
                deferred: def,
                coverage_percent: percent(sup, sup + def),
            }
        })
        .collect();

    let supported_count = supported.len();
    let deferred_count = deferred.len();
    let total_constructs = supported_count + deferred_count;
    CoverageReport {
        total_constructs,
        supported_count,
        deferred_count,
        coverage_percent: percent(supported_count, total_constructs),
        categories,
        supported,
        deferred,
        corpus_exercised,
        uncovered_supported,
    }
}

/// Whether the corpus run has a file with basename `file` that passed the
/// validity gate.
fn corpus_file_passed(report: &Report, file: &str) -> bool {
    report
        .files
        .iter()
        .any(|f| f.path.file_name().and_then(|n| n.to_str()) == Some(file) && f.passed())
}

/// Render the coverage report as a Markdown summary suitable for a CI
/// job-summary: a headline percentage, a per-category table, and the list of
/// not-yet-supported constructs.
#[must_use]
pub fn to_markdown(report: &CoverageReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Language coverage: {:.1}% ({}/{} constructs; {} not yet supported)\n\n",
        report.coverage_percent,
        report.supported_count,
        report.total_constructs,
        report.deferred_count,
    ));
    out.push_str(
        "_Language-surface coverage against the goal of full OpenSCAD \
         compatibility — not test coverage._\n\n",
    );

    out.push_str("| Category | Supported | Deferred | Coverage |\n");
    out.push_str("| --- | ---: | ---: | ---: |\n");
    for cat in &report.categories {
        out.push_str(&format!(
            "| {} | {} | {} | {:.1}% |\n",
            cat.category.label(),
            cat.supported,
            cat.deferred,
            cat.coverage_percent,
        ));
    }

    out.push_str("\n**Not yet supported:** ");
    if report.deferred.is_empty() {
        out.push_str("none\n");
    } else {
        out.push_str(&report.deferred.join(", "));
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use morphe_eval::eval::UNSUPPORTED_MODULES;
    use std::collections::HashSet;

    // Names of constructs that map to real handled leaves/nodes in the
    // evaluator (morphe-eval eval.rs), for the "maps to a real construct"
    // consistency check.
    const HANDLED_PRIMITIVES: &[&str] = &[
        "cube",
        "sphere",
        "cylinder",
        "square",
        "circle",
        "polygon",
        "polyhedron",
        // import() is in the Primitive category — a geometry leaf, like the
        // other primitives.
        "import",
        // surface() likewise (a .dat-driven solid leaf).
        "surface",
        // text() likewise (a glyph-outline Profile leaf).
        "text",
    ];
    const HANDLED_TRANSFORMS: &[&str] = &[
        "translate",
        "rotate",
        "scale",
        "mirror",
        "multmatrix",
        "resize",
        "color",
        // offset is in the Transform category.
        "offset",
    ];

    /// A report with a single named file that passed (a solid).
    fn passing_report(file: &str) -> Report {
        use crate::{ExpectedKind, FileResult, Outcome};
        use std::path::PathBuf;
        Report {
            files: vec![FileResult {
                path: PathBuf::from(format!("corpus/core/{file}")),
                expected: ExpectedKind::Solid,
                outcome: Ok(Outcome::Solid { triangles: 12 }),
            }],
        }
    }

    fn empty_report() -> Report {
        Report { files: vec![] }
    }

    // ── percentage math ───────────────────────────────────────────────────────

    #[test]
    fn percent_matches_worked_example() {
        // 40 supported + 6 deferred = 46 total → 40/46 = 86.9565…%.
        let p = percent(40, 46);
        assert!((p - 86.9565).abs() < 1e-3, "got {p}");
    }

    #[test]
    fn percent_is_100_only_when_nothing_deferred() {
        assert_eq!(percent(33, 33), 100.0);
        assert!(percent(33, 39) < 100.0);
    }

    #[test]
    fn empty_set_is_zero_not_nan() {
        assert_eq!(percent(0, 0), 0.0);
    }

    // ── inventory internal consistency ────────────────────────────────────────

    #[test]
    fn inventory_has_no_duplicate_names() {
        let mut seen = HashSet::new();
        for c in INVENTORY {
            assert!(seen.insert(c.name), "duplicate construct name: {}", c.name);
        }
    }

    #[test]
    fn deferred_set_equals_unsupported_modules() {
        let deferred: HashSet<&str> = INVENTORY
            .iter()
            .filter(|c| c.status == Status::DeferredV15)
            .map(|c| c.name)
            .collect();
        let unsupported: HashSet<&str> = UNSUPPORTED_MODULES.iter().copied().collect();
        assert_eq!(
            deferred, unsupported,
            "the DeferredV15 set must exactly equal UNSUPPORTED_MODULES"
        );
    }

    #[test]
    fn supported_primitives_and_transforms_map_to_handled_constructs() {
        for c in INVENTORY.iter().filter(|c| c.status == Status::Supported) {
            match c.category {
                Category::Primitive => assert!(
                    HANDLED_PRIMITIVES.contains(&c.name),
                    "supported primitive {} is not a handled primitive",
                    c.name
                ),
                Category::Transform => assert!(
                    HANDLED_TRANSFORMS.contains(&c.name),
                    "supported transform {} is not a handled transform",
                    c.name
                ),
                _ => {}
            }
        }
    }

    #[test]
    fn every_supported_construct_names_a_corpus_file() {
        for c in INVENTORY.iter().filter(|c| c.status == Status::Supported) {
            assert!(
                c.corpus_file.is_some(),
                "supported construct {} has no corpus file",
                c.name
            );
        }
    }

    /// Corpus completeness is DISK-checked —
    /// every supported construct's named corpus file must exist under
    /// corpus/core. (The validity gate checks the files that exist; THIS
    /// check catches a file silently deleted or renamed, which previously
    /// kept the inventory at 39/39 while oracle coverage of the construct
    /// vanished.)
    #[test]
    fn every_supported_constructs_corpus_file_exists_on_disk() {
        use std::path::Path;
        let corpus_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("corpus")
            .join("core");
        for c in INVENTORY.iter().filter(|c| c.status == Status::Supported) {
            let file = c.corpus_file.expect("supported constructs name a file");
            assert!(
                corpus_root.join(file).is_file(),
                "corpus file `{file}` for construct `{}` is missing from disk",
                c.name
            );
        }
    }

    #[test]
    fn deferred_constructs_have_no_corpus_file() {
        for c in INVENTORY.iter().filter(|c| c.status == Status::DeferredV15) {
            assert!(
                c.corpus_file.is_none(),
                "{} should have no corpus file",
                c.name
            );
        }
    }

    // ── the real report's numbers ─────────────────────────────────────────────

    /// The count test — the current state: 39/39, 100.0%, the deferred list
    /// empty.
    #[test]
    fn coverage_reports_100_percent_39_of_39() {
        let report = coverage_report(&empty_report());
        assert_eq!(report.supported_count, 39);
        assert_eq!(report.deferred_count, 0);
        assert_eq!(report.total_constructs, 39);
        assert!(
            (report.coverage_percent - 100.0).abs() < 1e-9,
            "coverage_percent was {}",
            report.coverage_percent
        );
        assert!(report.deferred.is_empty(), "nothing is not yet supported");
    }

    #[test]
    fn category_breakdown_sums_to_totals() {
        let report = coverage_report(&empty_report());
        let sup: usize = report.categories.iter().map(|c| c.supported).sum();
        let def: usize = report.categories.iter().map(|c| c.deferred).sum();
        assert_eq!(sup, report.supported_count);
        assert_eq!(def, report.deferred_count);
        // Language is fully covered; every other category has a gap.
        let lang = report
            .categories
            .iter()
            .find(|c| c.category == Category::Language)
            .unwrap();
        assert_eq!(lang.deferred, 0);
        assert_eq!(lang.coverage_percent, 100.0);
    }

    // ── corpus cross-reference ────────────────────────────────────────────────

    #[test]
    fn passing_corpus_file_marks_construct_exercised() {
        // cube maps to cube_scalar.scad; if that file passed, cube is exercised.
        let report = coverage_report(&passing_report("cube_scalar.scad"));
        assert!(
            report.corpus_exercised.contains(&"cube".to_string()),
            "cube should be exercised when cube_scalar.scad passes"
        );
        // sphere's file did not pass in this report → not exercised.
        assert!(!report.corpus_exercised.contains(&"sphere".to_string()));
        assert!(report.uncovered_supported.contains(&"sphere".to_string()));
    }

    #[test]
    fn supported_construct_without_corpus_file_is_uncovered() {
        // A synthetic inventory: one supported construct with no corpus file.
        const SYNTH: &[Construct] = &[
            sup("cube", Category::Primitive, "cube_scalar.scad"),
            Construct {
                name: "phantom",
                category: Category::Language,
                status: Status::Supported,
                corpus_file: None,
            },
        ];
        let report = coverage_from(SYNTH, &passing_report("cube_scalar.scad"));
        assert!(report.corpus_exercised.contains(&"cube".to_string()));
        assert!(
            report.uncovered_supported.contains(&"phantom".to_string()),
            "a supported construct with no corpus file must be uncovered"
        );
    }

    // ── markdown ──────────────────────────────────────────────────────────────

    #[test]
    fn markdown_has_headline_table_and_deferred_list() {
        let report = coverage_report(&empty_report());
        let md = to_markdown(&report);
        // Headline percentage.
        assert!(
            md.contains("Language coverage: 100.0%"),
            "headline missing:\n{md}"
        );
        assert!(
            md.contains("39/39 constructs"),
            "headline counts missing:\n{md}"
        );
        // Category table.
        assert!(md.contains("| Category | Supported | Deferred | Coverage |"));
        assert!(md.contains("| Primitives |"));
        assert!(md.contains("| Language |"));
        // Every not-yet-supported construct is listed.
        // The deferred list is currently EMPTY — and
        // the markdown says so explicitly.
        assert!(
            md.contains("Not yet supported:** none") || md.contains("Not yet supported:** none."),
            "the terminal markdown must say the deferred list is empty:\n{md}"
        );
        // States that "coverage" is language-surface, not test coverage.
        assert!(md.to_lowercase().contains("not test coverage"));
    }

    #[test]
    fn markdown_deferred_list_empty_when_none_deferred() {
        const SYNTH: &[Construct] = &[sup("cube", Category::Primitive, "cube_scalar.scad")];
        let report = coverage_from(SYNTH, &empty_report());
        assert_eq!(report.coverage_percent, 100.0);
        let md = to_markdown(&report);
        assert!(md.contains("**Not yet supported:** none"), "{md}");
    }

    // ── JSON round-trip ───────────────────────────────────────────────────────

    #[test]
    fn json_round_trips_with_snake_case_fields() {
        let report = coverage_report(&passing_report("cube_scalar.scad"));
        let json = serde_json::to_string_pretty(&report).unwrap();
        // snake_case field names.
        assert!(json.contains("\"total_constructs\""));
        assert!(json.contains("\"coverage_percent\""));
        assert!(json.contains("\"corpus_exercised\""));
        assert!(json.contains("\"uncovered_supported\""));
        // snake_case enum values.
        assert!(json.contains("\"deferred_v15\"") || json.contains("\"supported\""));
        let back: CoverageReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }
}
