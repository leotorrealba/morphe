//! morphe: command-line interface for Morphe.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use morphe_eval::csg::CsgNode;
use morphe_eval::eval::{EvalErrorKind, Evaluator};
use morphe_kernel::{
    geometry_stats, profile_stats, resolve_root, to_ascii_stl, to_binary_stl, Geometry,
    GeometryStats, ProfileStats, Solid,
};
use morphe_lang::lexer::line_col;
use morphe_lang::parser::parse_program;
use serde::Serialize;

#[derive(Parser)]
#[command(name = "morphe", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Parse `.scad` files and report syntax errors.
    Parse {
        /// Files to parse.
        #[arg(required = true, value_name = "FILES")]
        files: Vec<PathBuf>,
    },
    /// Compile a `.scad` file to an STL mesh.
    Compile {
        /// The `.scad` source file to compile.
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        /// Path to write the STL to.
        #[arg(short = 'o', long = "output", value_name = "OUTPUT")]
        output: PathBuf,
        /// Emit ASCII STL instead of the default binary STL.
        #[arg(long)]
        ascii: bool,
    },
    /// Report geometry statistics for a `.scad` file.
    ///
    /// Runs the same parse → eval → resolve pipeline as `compile`, then measures
    /// the result instead of exporting it. Unlike `compile` (STL is a 3D
    /// format), a 2D top-level result is not an error here: it reports 2D
    /// profile stats (area + 2D bounding box).
    Stats {
        /// The `.scad` source file to measure.
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        /// Emit statistics as pretty JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

/// A `stats` report for one `.scad` file, serialized as a single JSON object
/// tagged with a `"kind"` field so a consumer (the AI `get_geometry_stats` tool
/// and CLI users) knows which shape it got:
///
/// - `"solid"` — a 3D result: the [`GeometryStats`] fields `bbox`, `bbox_size`,
///   `volume`, `surface_area`, `is_manifold`, `triangle_count`, `vertex_count`.
/// - `"profile"` — a 2D result: the [`ProfileStats`] fields `bbox`, `bbox_size`,
///   `area`, `contour_count`, `vertex_count`.
///
/// An empty (volume-less) result is reported as an empty `"solid"`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum StatsReport {
    Solid(GeometryStats),
    Profile(ProfileStats),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => {
            println!("morphe {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(Command::Parse { files }) => parse_files(&files),
        Some(Command::Compile {
            input,
            output,
            ascii,
        }) => compile(&input, &output, ascii),
        Some(Command::Stats { input, json }) => stats(&input, json),
    }
}

/// Shared front end: read → [`parse_program`] → [`Evaluator::eval_program`] →
/// [`resolve_root`]. On success returns the resolved geometry (`None` = an empty
/// result). Parse errors, hard eval errors, and resolver errors are reported to
/// stderr with `file:line:col` (parse/eval) or `file: error:` (resolver) framing
/// and yield `Err(())`; eval warnings (`file:line:col: warning:`) and resolver
/// warnings (`file: warning:`) are printed but do not fail.
/// Both `compile` and `stats` drive this so the diagnostics stay identical.
///
/// The whole front end (including the AST/CSG-tree drops) runs on the
/// dedicated large-stack pipeline thread
/// ([`morphe_eval::stack::with_large_stack`]) so the depth guards, not the
/// main thread's native stack, bound pathological recursion.
fn evaluate_scad(input: &Path) -> Result<Option<Geometry>, ()> {
    morphe_eval::stack::with_large_stack(|| evaluate_scad_inner(input))
}

fn evaluate_scad_inner(input: &Path) -> Result<Option<Geometry>, ()> {
    let display = input.display();

    let src = fs::read_to_string(input).map_err(|err| {
        eprintln!("{display}: error: {err}");
    })?;

    // Parse.
    let stmts = match parse_program(&src) {
        Ok(stmts) => stmts,
        Err(err) => {
            let (line, col) = line_col(&src, err.span.start);
            eprintln!("{display}:{line}:{col}: error: {}", err.message);
            return Err(());
        }
    };

    // Evaluate to a CSG tree.
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src.as_str());
    // Record the importing file's dir — the base for
    // import()/surface() relative-path resolution.
    evaluator.set_source_path(Some(input));
    let root = evaluator.eval_program(&stmts);

    // Hard eval errors (unsupported constructs, failed asserts, …) fail; each is
    // reported with its source location, plus a hint for the not-yet-supported
    // constructs so the author can route around them.
    if !evaluator.errors().is_empty() {
        for err in evaluator.errors() {
            // An error inside an included/used file is reported at that
            // file's position (`err.origin`), the main file's otherwise.
            let (line, col) = err.line_col(&src);
            let file = err
                .origin
                .as_ref()
                .map_or_else(|| display.to_string(), |o| o.file.display().to_string());
            eprintln!("{file}:{line}:{col}: error: {}", err.message);
            if let EvalErrorKind::Unsupported { hint, .. } = &err.kind {
                eprintln!("{file}:{line}:{col}: hint: {hint}");
            }
        }
        return Err(());
    }
    // Warnings are framed like errors: `file:line:col: warning: …`, at the
    // included/used file's position when they come from one.
    for warning in evaluator.eval_warnings() {
        let file = warning
            .origin
            .as_ref()
            .map_or_else(|| display.to_string(), |o| o.file.display().to_string());
        match warning.line_col(&src) {
            Some((line, col)) => eprintln!("{file}:{line}:{col}: warning: {}", warning.message),
            None => eprintln!("{file}: warning: {}", warning.message),
        }
    }

    // The root is a group of the file's top-level statements; resolve them under
    // the implicit top-level union.
    let top_level = match root {
        CsgNode::Group { children, .. } => children,
        other => vec![other],
    };
    let mut resolver_warnings = Vec::new();
    let geometry = match resolve_root(&top_level, &mut resolver_warnings) {
        Ok(geometry) => geometry,
        Err(err) => {
            eprintln!("{display}: error: {err}");
            return Err(());
        }
    };
    for warning in &resolver_warnings {
        eprintln!("{display}: warning: {warning}");
    }

    Ok(geometry)
}

/// Compile one `.scad` file to an STL at `output`.
///
/// Pipeline: read → [`parse_program`] → [`Evaluator::eval_program`] →
/// [`resolve_root`] → STL export. Parse errors, eval errors, and resolver
/// errors are reported to stderr with `file:line:col` (parse/eval) or
/// `file: error:` (resolver) and exit nonzero; warnings are reported but do not
/// fail the compile. A 2D-only top-level result is an error (STL is a 3D
/// format, matching upstream "Current top level object is not a 3D object"); an
/// empty result writes a valid empty STL and succeeds.
fn compile(input: &Path, output: &Path, ascii: bool) -> ExitCode {
    let display = input.display();

    let geometry = match evaluate_scad(input) {
        Ok(geometry) => geometry,
        Err(()) => return ExitCode::FAILURE,
    };

    // STL is a 3D format: a 2D-only result cannot be exported (upstream errors
    // "Current top level object is not a 3D object"); an empty result writes an
    // empty STL and succeeds.
    let solid = match geometry {
        Some(Geometry::Solid(solid)) => solid,
        Some(Geometry::Profile(_)) => {
            eprintln!("{display}: error: Current top level object is not a 3D object");
            return ExitCode::FAILURE;
        }
        None => {
            eprintln!("{display}: warning: no geometry produced; writing an empty STL");
            Solid::empty()
        }
    };

    // Serialize and write.
    let write_result = if ascii {
        fs::write(output, to_ascii_stl(&solid))
    } else {
        fs::write(output, to_binary_stl(&solid))
    };
    if let Err(err) = write_result {
        eprintln!("{}: error: {err}", output.display());
        return ExitCode::FAILURE;
    }

    eprintln!(
        "{display}: wrote {} triangles to {}",
        solid.num_tri(),
        output.display()
    );
    ExitCode::SUCCESS
}

/// Report geometry statistics for one `.scad` file.
///
/// Runs the shared [`evaluate_scad`] pipeline, then measures the result: a 3D
/// [`Geometry::Solid`] via [`geometry_stats`], a 2D [`Geometry::Profile`] via
/// [`profile_stats`] (reported, not an error — the key difference from
/// `compile`), and an empty result as the stats of an empty [`Solid`]. With
/// `--json` the report is printed to stdout as pretty JSON tagged by `"kind"`;
/// otherwise a short human-readable summary is printed. Eval/resolve errors are
/// reported by [`evaluate_scad`] and exit nonzero with no stats output.
fn stats(input: &Path, json: bool) -> ExitCode {
    let geometry = match evaluate_scad(input) {
        Ok(geometry) => geometry,
        Err(()) => return ExitCode::FAILURE,
    };

    let report = match geometry {
        Some(Geometry::Solid(solid)) => StatsReport::Solid(geometry_stats(&solid)),
        Some(Geometry::Profile(profile)) => StatsReport::Profile(profile_stats(&profile)),
        // An empty result is a valid, volume-less solid.
        None => StatsReport::Solid(geometry_stats(&Solid::empty())),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("stats serialize to JSON")
        );
    } else {
        print_human(&report);
    }
    ExitCode::SUCCESS
}

/// Print a short, aligned human-readable summary of a [`StatsReport`].
fn print_human(report: &StatsReport) {
    match report {
        StatsReport::Solid(s) => {
            println!("kind:          solid (3D)");
            println!("volume:        {:.6}", s.volume);
            println!("surface area:  {:.6}", s.surface_area);
            match s.bbox {
                Some([min, max]) => {
                    println!(
                        "bbox min:      [{:.6}, {:.6}, {:.6}]",
                        min[0], min[1], min[2]
                    );
                    println!(
                        "bbox max:      [{:.6}, {:.6}, {:.6}]",
                        max[0], max[1], max[2]
                    );
                }
                None => println!("bbox:          (empty)"),
            }
            println!("triangles:     {}", s.triangle_count);
            println!("vertices:      {}", s.vertex_count);
            println!("manifold:      {}", s.is_manifold);
        }
        StatsReport::Profile(p) => {
            println!("kind:          profile (2D)");
            println!("area:          {:.6}", p.area);
            match p.bbox {
                Some([min, max]) => {
                    println!("bbox min:      [{:.6}, {:.6}]", min[0], min[1]);
                    println!("bbox max:      [{:.6}, {:.6}]", max[0], max[1]);
                }
                None => println!("bbox:          (empty)"),
            }
            println!("contours:      {}", p.contour_count);
            println!("vertices:      {}", p.vertex_count);
        }
    }
}

/// Parse each file, reporting per-file results. Returns failure if any
/// file was unreadable or failed to parse.
fn parse_files(files: &[PathBuf]) -> ExitCode {
    let mut failed = false;
    for file in files {
        let display = file.display();
        let src = match fs::read_to_string(file) {
            Ok(src) => src,
            Err(err) => {
                eprintln!("{display}: error: {err}");
                failed = true;
                continue;
            }
        };
        match parse_program(&src) {
            Ok(stmts) => println!("{display}: ok ({} statements)", stmts.len()),
            Err(err) => {
                let (line, col) = line_col(&src, err.span.start);
                eprintln!("{display}:{line}:{col}: error: {}", err.message);
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
