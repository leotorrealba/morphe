//! The `export_stl` IPC seam.
//!
//! Pure STL export — reuse the same parse → eval → resolve front end as
//! [`compile_source`](crate::compile_source), then write STL bytes through the
//! kernel exporter (`morphe_kernel::to_binary_stl` / `to_ascii_stl`). No Tauri
//! types in the signature, so integration tests drive it directly. Mirrors
//! `morphe-cli`'s `compile` semantics:
//!
//! - a 2D-only top level is an error (STL is a 3D format);
//! - an empty result writes a valid empty STL and succeeds;
//! - a `Solid` writes a binary (`ascii = false`) or ASCII (`ascii = true`) file.
//!
//! The `#[tauri::command]` in [`crate::lib`] is a thin shell over
//! [`export_stl`], registered alongside `compile_source_cmd`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use morphe_kernel::{geometry_stats, to_ascii_stl, to_binary_stl, Geometry, Solid};

use crate::compile::{resolve_source, CompileError};

/// Stats for a successful STL export. Mirrors the Rust struct on the TS side as
/// `ExportStats`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportStats {
    /// Triangle count of the exported solid (0 for an empty result).
    pub triangle_count: usize,
    /// Vertex count of the exported solid.
    pub vertex_count: usize,
    /// `"binary"` or `"ascii"` — which STL variant was written.
    pub format: String,
    /// Total bytes written to disk (`84 + 50 * triangle_count` for binary;
    /// header + facet blocks for ASCII).
    pub bytes_written: usize,
}

/// One failure mode of [`export_stl`]. Serialized as a tagged union
/// (`{ kind: "compile" | "not_3d" | "io", ... }`) so the frontend can route on
/// the discriminant. Mirrors the Rust enum on the TS side as `ExportError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportError {
    /// Parse / eval / resolver diagnostics — the front end failed. Carries the
    /// same structured [`CompileError`]s `compile_source` would have returned.
    Compile {
        errors: Vec<CompileError>,
        warnings: Vec<String>,
    },
    /// The top level resolved to a 2D `Profile`; STL is a 3D format (upstream
    /// "Current top level object is not a 3D object"). No file is written.
    #[serde(rename = "not_3d")]
    Not3D,
    /// `std::fs::write` failed (permissions, disk full, …). `message` is the
    /// underlying `io::Error` string. No file is written.
    Io { message: String },
    /// The user cancelled the save dialog. No file is written.
    Cancelled,
}

/// Run the parse → eval → resolve front end on `source`, then write the
/// resolved solid to `path` as binary (`ascii = false`) or ASCII (`ascii = true`)
/// STL. Pure apart from the single `std::fs::write` of the serialized bytes.
///
/// Returns [`ExportStats`] on success, or an [`ExportError`] (compile
/// diagnostics, a 2D top level, or an I/O failure). The 2D-rejection and
/// compile-error paths write no file.
pub fn export_stl(
    source: &str,
    source_path: Option<&Path>,
    path: &Path,
    ascii: bool,
) -> Result<ExportStats, ExportError> {
    // Export carries the SAME source-dir context as
    // compile — preview and export must resolve `import()` identically, or
    // an import-bearing buffer previews fine and then fails to export.
    let resolved = resolve_source(source, source_path);
    if !resolved.errors.is_empty() {
        return Err(ExportError::Compile {
            errors: resolved.errors,
            warnings: resolved.warnings,
        });
    }

    // STL is a 3D format: a 2D-only result cannot be exported. An empty result
    // writes a valid (zero-triangle) STL and succeeds — same as the CLI.
    let solid = match resolved.geometry {
        Some(Geometry::Solid(solid)) => solid,
        Some(Geometry::Profile(_)) => return Err(ExportError::Not3D),
        None => Solid::empty(),
    };

    let bytes = if ascii {
        to_ascii_stl(&solid).into_bytes()
    } else {
        to_binary_stl(&solid)
    };
    std::fs::write(path, &bytes).map_err(|err| ExportError::Io {
        message: err.to_string(),
    })?;

    let stats = geometry_stats(&solid);
    Ok(ExportStats {
        triangle_count: stats.triangle_count,
        vertex_count: stats.vertex_count,
        format: if ascii { "ascii" } else { "binary" }.to_string(),
        bytes_written: bytes.len(),
    })
}

/// Pick a `.stl` destination via `picker`, then run [`export_stl`]. Returns
/// [`ExportError::Cancelled`] if the user cancels the dialog. The destination
/// path originates from the OS dialog inside this process — it never crosses the
/// IPC trust boundary as a frontend-supplied value.
pub fn export_stl_via_dialog<P: crate::file::FileDialogPicker>(
    picker: &P,
    source: &str,
    source_path: Option<&Path>,
    ascii: bool,
) -> Result<ExportStats, ExportError> {
    let path: PathBuf = picker.pick_save_stl().ok_or(ExportError::Cancelled)?;
    export_stl(source, source_path, &path, ascii)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::FileDialogPicker;
    use std::path::Path;

    /// Fake picker for the `export_stl_via_dialog` seam: returns a canned `.stl`
    /// path (or `None` to simulate cancel) and stubs the `.scad` dialogs the
    /// trait also requires. No OS dialog is shown.
    struct FakePicker {
        picked: Option<std::path::PathBuf>,
    }

    impl FileDialogPicker for FakePicker {
        fn pick_open(&self) -> Option<std::path::PathBuf> {
            None
        }
        fn pick_save(&self) -> Option<std::path::PathBuf> {
            None
        }
        fn pick_save_stl(&self) -> Option<std::path::PathBuf> {
            self.picked.clone()
        }
    }

    /// A real compile + binary STL write through `export_stl_via_dialog`: the
    /// fake picker returns a temp path, the cube compiles, and the file is
    /// written with `format == "binary"` and a non-zero triangle count.
    #[test]
    fn export_stl_via_dialog_writes_file_when_picked() {
        let path = std::env::temp_dir().join(format!(
            "morphe_export_stl_via_dialog_{}.stl",
            std::process::id()
        ));
        // Clean slate (a previous run may have left the file).
        let _ = std::fs::remove_file(&path);

        let picker = FakePicker {
            picked: Some(path.clone()),
        };
        let stats = export_stl_via_dialog(&picker, "cube([10,10,10]);", None, false)
            .expect("cube exports cleanly");

        assert_eq!(stats.format, "binary");
        assert!(stats.triangle_count > 0);
        let written = std::fs::read(&path).expect("STL file was written");
        assert!(!written.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    /// When the picker returns `None` (user cancelled), the seam returns
    /// [`ExportError::Cancelled`] and writes no file.
    #[test]
    fn export_stl_via_dialog_returns_cancelled_when_none() {
        let path = std::env::temp_dir().join(format!(
            "morphe_export_stl_via_dialog_cancel_{}.stl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let picker = FakePicker { picked: None };
        let err = export_stl_via_dialog(&picker, "cube([10,10,10]);", None, false)
            .expect_err("cancel must error");
        assert_eq!(err, ExportError::Cancelled);
        assert!(
            !Path::new(&path).exists(),
            "no file should be written on cancel"
        );
    }

    /// Pin the `Cancelled` wire shape at the IPC boundary. The TS union matches
    /// `{ kind: "cancelled" }` (ipc.ts); this asserts the Rust enum serializes to
    /// exactly that JSON, so the cancel shape can't silently regress if the
    /// `#[serde(tag = "kind", rename_all = "snake_case")]` attribute drifts.
    /// Tests on either side alone would only check an *assumed* shape; this
    /// locks the real boundary.
    #[test]
    fn export_error_cancelled_serializes_to_kind_cancelled() {
        assert_eq!(
            serde_json::to_string(&ExportError::Cancelled).unwrap(),
            r#"{"kind":"cancelled"}"#
        );
    }
}
