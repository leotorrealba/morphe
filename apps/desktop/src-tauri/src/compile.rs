//! The `compile_source` IPC seam.
//!
//! Pure compile pipeline — parse → eval → resolve → mesh — with no Tauri types
//! in its signature, so integration tests drive it directly without a runtime.
//! Mirrors `morphe-cli`'s `evaluate_scad` for the first three stages, then
//! extracts a raw vertex/index buffer (`Solid::to_mesh`) instead of an STL.
//!
//! The `#[tauri::command]` in [`crate::lib`] (registered in `run`) is a thin
//! shell over [`compile_source`], packing the result into a single binary
//! [`tauri::ipc::Response`] so the mesh crosses the IPC boundary as raw bytes,
//! never JSON-encoded.

use std::path::Path;

use serde::{Deserialize, Serialize};

use morphe_eval::csg::CsgNode;
use morphe_eval::eval::{EvalErrorKind, Evaluator};
use morphe_kernel::{geometry_stats, resolve_root, Geometry, GeometryStats, Solid};
use morphe_lang::lexer::line_col;
use morphe_lang::parser::parse_program;

/// The stable category of a [`CompileError`]. Serialized as snake_case so the
/// wire value is `"parse"` / `"unsupported"` / `"general"` / `"resolver"` —
/// exactly what the TS union (`ipc.ts`) declares. Enum-bounded: Rust cannot
/// produce a kind outside this set, so the wire-shape snapshot test pins it
/// bidirectionally (was a free-form `String` — a 5th kind would have been
/// uncaught).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Parse,
    Unsupported,
    General,
    Resolver,
}

/// One diagnostic from the compile pipeline.
///
/// Mirrors `morphe-lang`'s parse errors and `morphe-eval`'s `EvalError`: the
/// 1-based source location, the human-readable message, and a stable `kind`
/// the frontend can route on. Serialized as snake_case JSON via
/// [`CompileResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileError {
    /// 1-based source line (from `line_col` on the error span's start).
    pub line: usize,
    /// 1-based source column.
    pub col: usize,
    /// Human-readable message (e.g. `"unsupported: minkowski (not yet implemented in morphe)"`).
    pub message: String,
    /// An [`ErrorKind`] (serializes as `"parse"` / `"unsupported"` / `"general"` / `"resolver"`).
    pub kind: ErrorKind,
}

/// The full result of compiling a source string.
///
/// `mesh` is a packed [`MeshBuffer`](crate::compile#layout) as raw bytes (no
/// JSON encoding) so the viewport can upload it straight to a GPU buffer.
/// `stats` is `None` when no 3D solid was produced (errors, a 2D-only result,
/// or an empty top level); `errors` is empty only on a fully successful compile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileResult {
    /// Packed binary mesh buffer (see the layout section in the module docs).
    /// Empty when compile failed or produced no 3D solid.
    pub mesh: Vec<u8>,
    /// Geometry stats for the resolved 3D solid, if any.
    pub stats: Option<GeometryStats>,
    /// Hard errors (parse, eval, resolver). Empty on success.
    pub errors: Vec<CompileError>,
    /// Non-fatal warnings from eval + the resolver. Eval warnings are
    /// prefixed with their location — `line:col: ` in the open buffer,
    /// `lib.scad:line:col: ` in an included or used file (see
    /// [`located_warnings`]); resolver warnings have none.
    pub warnings: Vec<String>,
}

// ── MeshBuffer layout ───────────────────────────────────────────────────────
//
// `Solid::to_mesh()` returns `(Vec<[f64; 3]>, Vec<[u32; 3]>)` — an indexed mesh
// — so the buffer is indexed, little-endian, laid out as:
//
//   [u32 vertex_count ]   V = positions.len() / 3
//   [u32 index_count  ]   I = indices.len()      (3 per triangle)
//   [f32 x V x 3       ]  positions, xyz interleaved
//   [u32 x I           ]  indices, flat (3 per triangle, CCW outward)
//
// Positions are narrowed f64→f32 (viewport precision is more than enough at
// f32, and it halves the upload size). The TS mirror lives in `src/ipc.ts`.

/// Pack an indexed `(vertices, triangles)` mesh into the `MeshBuffer` binary
/// layout documented above. `verts` are f64 `[x, y, z]`; `tris` are `u32` tri
/// index triples. Returns the raw bytes.
fn pack_mesh(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> Vec<u8> {
    let vertex_count = u32::try_from(verts.len()).expect("vertex count fits u32");
    let index_count = u32::try_from(tris.len() * 3).expect("index count fits u32");

    let mut out = Vec::with_capacity(8 + verts.len() * 12 + tris.len() * 12);
    out.extend_from_slice(&vertex_count.to_le_bytes());
    out.extend_from_slice(&index_count.to_le_bytes());
    for v in verts {
        out.extend_from_slice(&(v[0] as f32).to_le_bytes());
        out.extend_from_slice(&(v[1] as f32).to_le_bytes());
        out.extend_from_slice(&(v[2] as f32).to_le_bytes());
    }
    for t in tris {
        out.extend_from_slice(&t[0].to_le_bytes());
        out.extend_from_slice(&t[1].to_le_bytes());
        out.extend_from_slice(&t[2].to_le_bytes());
    }
    out
}

/// The shared front end's output: parse → eval → resolve, with every failure
/// mapped to [`CompileError`]s. Always carries `warnings` (eval + resolver);
/// `errors` is empty only on a clean resolve, and `geometry` is `None` for an
/// empty top-level result (no statements, or they union to nothing).
///
/// Both [`compile_source`] (mesh + stats) and `export_stl` (STL bytes) consume
/// this so the two IPC commands never diverge on diagnostics.
pub(crate) struct Resolved {
    pub(crate) geometry: Option<Geometry>,
    pub(crate) errors: Vec<CompileError>,
    pub(crate) warnings: Vec<String>,
}

/// Run the shared parse → eval → resolve front end on `source`. Pure: no Tauri
/// types, no I/O, infallible (every failure lands in `errors`).
///
/// Stages mirror `morphe-cli`'s `evaluate_scad`:
/// 1. `parse_program` — a parse error becomes a `"parse"` [`CompileError`] and
///    short-circuits (there is no AST to evaluate).
/// 2. `Evaluator::eval_program` — hard eval errors (unsupported constructs,
///    failed asserts) become `"unsupported"`/`"general"` errors and
///    short-circuit; `warnings` are collected but do not fail.
/// 3. `resolve_root` — the top-level CSG tree resolves into a `Geometry`; a
///    resolver error becomes a `"resolver"` error.
pub(crate) fn resolve_source(source: &str, source_path: Option<&Path>) -> Resolved {
    // 1. Parse.
    let stmts = match parse_program(source) {
        Ok(stmts) => stmts,
        Err(err) => {
            let (line, col) = line_col(source, err.span.start);
            return Resolved {
                geometry: None,
                errors: vec![CompileError {
                    line,
                    col,
                    message: err.message,
                    kind: ErrorKind::Parse,
                }],
                warnings: Vec::new(),
            };
        }
    };

    // 2. Evaluate to a CSG tree.
    let mut evaluator = Evaluator::new();
    evaluator.set_source(source);
    // Attach the open file's path when the Rust side
    // knows it, so import()/surface() resolve relative paths in the app the
    // same way the CLI does. None = the string-only pipeline (untitled
    // buffer, tests).
    evaluator.set_source_path(source_path);
    let root = evaluator.eval_program(&stmts);

    if !evaluator.errors().is_empty() {
        let errors = evaluator
            .errors()
            .iter()
            .map(|err| {
                let (line, col) = line_col(source, err.span.start);
                let kind = match &err.kind {
                    EvalErrorKind::Unsupported { .. } => ErrorKind::Unsupported,
                    EvalErrorKind::General => ErrorKind::General,
                };
                CompileError {
                    line,
                    col,
                    // Errors inside an included/used file are marked on the
                    // include line, with the real location in the message.
                    message: err.located_message(),
                    kind,
                }
            })
            .collect();
        return Resolved {
            geometry: None,
            errors,
            warnings: located_warnings(&evaluator, source),
        };
    }
    let mut warnings = located_warnings(&evaluator, source);

    // 3. The root is a group of the file's top-level statements; resolve them
    //    under the implicit top-level union.
    let top_level = match root {
        CsgNode::Group { children, .. } => children,
        other => vec![other],
    };
    let geometry = match resolve_root(&top_level, &mut warnings) {
        Ok(geometry) => geometry,
        Err(err) => {
            return Resolved {
                geometry: None,
                errors: vec![CompileError {
                    line: 1,
                    col: 1,
                    message: err.to_string(),
                    kind: ErrorKind::Resolver,
                }],
                warnings,
            };
        }
    };

    Resolved {
        geometry,
        errors: Vec::new(),
        warnings,
    }
}

/// The evaluator's warnings as wire strings, each prefixed with where it was
/// raised: `line:col: msg` for the open buffer (the editor knows which file
/// that is) and `lib.scad:line:col: msg` for an included or used file — the
/// file NAME only, never its path. The wire type stays a plain string array.
fn located_warnings(evaluator: &Evaluator, source: &str) -> Vec<String> {
    evaluator
        .eval_warnings()
        .iter()
        .map(|warning| warning.located_message(source, None))
        .collect()
}

/// Run the full compile pipeline on `source` and return its result. Pure: no
/// Tauri types, no I/O, infallible (every failure lands in `errors`).
///
/// Delegates the front end to [`resolve_source`], then — for a 3D [`Solid`] —
/// reads back the mesh via `Solid::to_mesh()` and packs it into the binary
/// `MeshBuffer`; `geometry_stats` reports the stats. A 2D `Profile` or empty
/// result yields an empty mesh and no stats (the viewport is 3D-only).
#[must_use]
pub fn compile_source(source: &str, source_path: Option<&Path>) -> CompileResult {
    let resolved = resolve_source(source, source_path);
    if !resolved.errors.is_empty() {
        return CompileResult {
            mesh: Vec::new(),
            stats: None,
            errors: resolved.errors,
            warnings: resolved.warnings,
        };
    }

    match resolved.geometry {
        Some(Geometry::Solid(solid)) => solid_to_result(solid, resolved.warnings),
        // A 2D-only or empty result has no 3D mesh.
        Some(Geometry::Profile(_)) | None => CompileResult {
            mesh: Vec::new(),
            stats: None,
            errors: Vec::new(),
            warnings: resolved.warnings,
        },
    }
}

/// Pack a resolved [`Solid`] into a [`CompileResult`] (mesh + stats + warnings).
fn solid_to_result(solid: Solid, warnings: Vec<String>) -> CompileResult {
    let stats = geometry_stats(&solid);
    let (verts, tris) = solid.to_mesh();
    CompileResult {
        mesh: pack_mesh(&verts, &tris),
        stats: Some(stats),
        errors: Vec::new(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_mesh_round_trips_header() {
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0, 2, 1]];
        let bytes = pack_mesh(&verts, &tris);
        assert_eq!(bytes.len(), 8 + 3 * 12 + 3 * 4);
        let v = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let i = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(v, 3);
        assert_eq!(i, 3);
    }

    #[test]
    fn warnings_are_prefixed_with_their_location() {
        let result = compile_source("cube(1);\n  sphere(r = rr);\n", None);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.warnings,
            ["2:14: Ignoring unknown variable 'rr'".to_string()]
        );
    }

    #[test]
    fn warnings_from_included_files_name_the_file_not_its_path() {
        let dir = std::env::temp_dir().join(format!(
            "morphe-desktop-warn-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/part.scad"), "\ncube(w);\n").unwrap();
        let main = dir.join("main.scad");
        let source = "include <lib/part.scad>\ncube(v);\n";
        let result = compile_source(source, Some(&main));
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.warnings,
            [
                "part.scad:2:6: Ignoring unknown variable 'w'".to_string(),
                "2:6: Ignoring unknown variable 'v'".to_string(),
            ]
        );
        assert!(result
            .warnings
            .iter()
            .all(|w| !w.contains(&*dir.to_string_lossy())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_error_reports_line_and_kind() {
        let result = compile_source("cube(;", None);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].kind, ErrorKind::Parse);
        assert!(result.errors[0].line >= 1);
        assert!(result.mesh.is_empty());
        assert!(result.stats.is_none());
    }
}
