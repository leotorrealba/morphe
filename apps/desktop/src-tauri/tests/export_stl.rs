//! Integration tests for the `export_stl` IPC backing function.
//!
//! These drive the **pure** `export_stl` seam directly — no Tauri runtime —
//! reusing the same parse → eval → resolve front end as `compile_source`, then
//! writing STL bytes through the kernel exporter. Mirrors `morphe-cli`'s
//! `compile` semantics: 2D-only top level → error (STL is 3D); empty result →
//! a valid empty STL; a Solid → binary or ASCII STL on disk.

use std::fs;
use std::path::PathBuf;

use morphe_desktop_lib::{export_stl, ExportError, ExportStats};

/// A unique temp path for a given suffix. Uses `std::env::temp_dir()` (the
/// `tempfile` crate is not a workspace dep); callers clean up via `remove_file`.
fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    // Pin under a per-process dir to avoid collisions with parallel runs.
    p.push(format!("morphe-export-stl-{}-{name}", std::process::id()));
    fs::create_dir_all(&p).expect("create temp dir");
    p
}

/// A unit cube compiles to 12 triangles; the written binary STL must be exactly
/// `84 + 50 * 12` bytes with the correct triangle count in the header.
#[test]
fn export_stl_writes_valid_binary_stl() {
    let dir = temp_path("cube_binary");
    let out = dir.join("cube.stl");

    let stats: ExportStats = export_stl("cube([10, 10, 10]);", None, &out, false)
        .expect("a cube must export to STL; a real compile error would surface as Err(Compile)");

    assert_eq!(stats.format, "binary");
    assert_eq!(stats.triangle_count, 12, "a cube has 12 triangles");
    // 84-byte header (80 + u32 count) + one 50-byte record per triangle.
    let expected_len = 84 + 50 * 12;
    assert_eq!(stats.bytes_written, expected_len);

    let bytes = fs::read(&out).expect("STL file must be written to disk");
    assert_eq!(
        bytes.len(),
        expected_len,
        "file length must match STL layout"
    );

    // 80-byte header is free-form (kernel writes a fixed zero-padded tag that
    // deliberately does not start with `solid`). The triangle count is the
    // little-endian u32 at offset 80.
    let tri_count = u32::from_le_bytes(bytes[80..84].try_into().expect("count slot is 4 bytes"));
    assert_eq!(
        tri_count, 12,
        "STL header triangle count must match the cube"
    );

    fs::remove_file(&out).ok();
}

/// ASCII variant: the same cube, written as ASCII STL, round-trips as a
/// `solid morphe` document with one `facet` block per triangle.
#[test]
fn export_stl_writes_valid_ascii_stl() {
    let dir = temp_path("cube_ascii");
    let out = dir.join("cube_ascii.stl");

    let stats: ExportStats =
        export_stl("cube([2, 2, 2]);", None, &out, true).expect("ASCII cube export succeeds");

    assert_eq!(stats.format, "ascii");
    assert_eq!(stats.triangle_count, 12);

    let text = fs::read_to_string(&out).expect("ASCII STL file must be readable as text");
    assert!(
        text.starts_with("solid morphe\n"),
        "ASCII STL must start with the `solid` header, got: {text:?}"
    );
    assert!(
        text.ends_with("endsolid morphe\n"),
        "ASCII STL must end with `endsolid`, got tail: {text:?}"
    );
    let facet_count = text.matches("  facet normal").count();
    assert_eq!(facet_count, 12, "one facet block per triangle");

    fs::remove_file(&out).ok();
}

/// A 2D-only top level cannot be exported to STL (STL is 3D). Mirrors the CLI's
/// "Current top level object is not a 3D object" failure.
#[test]
fn export_stl_rejects_2d_top_level() {
    let dir = temp_path("square");
    let out = dir.join("square.stl");

    let err = export_stl("square([10, 10]);", None, &out, false)
        .expect_err("a 2D-only result must not export to STL");
    assert!(
        matches!(err, ExportError::Not3D),
        "2D top level must surface as Not3D, got {err:?}"
    );
    // No partial file should be written on the rejection path.
    assert!(!out.exists(), "no STL on the 2D-rejection path");

    fs::remove_file(&out).ok();
}

/// A compile error (parse/eval/resolver) surfaces as `ExportError::Compile`
/// carrying the structured diagnostics, matching `compile_source`'s errors.
#[test]
fn export_stl_surfaces_compile_errors() {
    let dir = temp_path("bad");
    let out = dir.join("bad.stl");

    let err = export_stl("cube(;", None, &out, false)
        .expect_err("a parse error must surface as a compile error");
    match err {
        ExportError::Compile { errors, .. } => {
            assert!(
                !errors.is_empty(),
                "compile errors must carry the diagnostics"
            );
            assert_eq!(errors[0].kind, morphe_desktop_lib::ErrorKind::Parse);
        }
        other => panic!("expected ExportError::Compile, got {other:?}"),
    }
    assert!(!out.exists(), "no STL on the compile-error path");

    fs::remove_file(&out).ok();
}

/// Export-side parity — an import()-bearing buffer
/// exports successfully when the open-file path carries the source dir, the
/// same context compile uses (preview == export).
#[test]
fn export_stl_with_open_file_path_resolves_import() {
    let dir = temp_path("export_import_parity");
    fs::create_dir_all(&dir).expect("mkdir");

    // A tiny closed binary STL (unit cube) as the import target.
    let verts: [[f64; 3]; 8] = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.0, 1.0, 1.0],
    ];
    let tris: [[u32; 3]; 12] = [
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [2, 3, 7],
        [2, 7, 6],
        [0, 4, 7],
        [0, 7, 3],
        [1, 2, 6],
        [1, 6, 5],
    ];
    let mut stl = b"export parity cube".to_vec();
    stl.resize(80, b' ');
    stl.extend_from_slice(&(12u32).to_le_bytes());
    for tri in tris {
        for _ in 0..3 {
            stl.extend_from_slice(&0f32.to_le_bytes());
        }
        for idx in tri {
            for c in verts[idx as usize] {
                stl.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        stl.extend_from_slice(&0u16.to_le_bytes());
    }
    fs::write(dir.join("model.stl"), &stl).expect("fixture write");
    let open_file = dir.join("part.scad");

    let out = dir.join("out.stl");
    let stats = export_stl("import(\"model.stl\");", Some(&open_file), &out, false)
        .expect("export with the open-file path succeeds");
    assert_eq!(stats.triangle_count, 12, "the imported cube's 12 triangles");

    // Without the path the same export fails loudly (no CWD magic).
    let out2 = dir.join("out2.stl");
    let err = export_stl("import(\"model.stl\");", None, &out2, false)
        .expect_err("string-only export cannot resolve a relative import");
    assert!(matches!(err, ExportError::Compile { .. }));

    let _ = fs::remove_dir_all(&dir);
}
