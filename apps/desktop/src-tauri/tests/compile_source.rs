//! Integration tests for the `compile_source` IPC backing function.
//!
//! These drive the **pure** `compile_source` seam directly — no Tauri runtime
//! — invoking the IPC backing function directly as the backend seam. The
//! `#[tauri::command]` wrapper in
//! `lib.rs` is a thin shell over this function.

use morphe_desktop_lib::{compile_source, CompileError, CompileResult, ErrorKind};

/// Write a tiny closed binary STL (a unit cube: 8 verts, 12 tris) to
/// `dir/model.stl` — the open-file import fixture. Built inline so the
/// test does not depend on the repo corpus layout.
fn write_binary_cube(dir: &std::path::Path) -> std::path::PathBuf {
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
    let mut out = b"gui-parity cube".to_vec();
    out.resize(80, b' ');
    out.extend_from_slice(&(12u32).to_le_bytes());
    for tri in tris {
        for _ in 0..3 {
            out.extend_from_slice(&0f32.to_le_bytes());
        }
        for idx in tri {
            let v = verts[idx as usize];
            for c in v {
                out.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    let path = dir.join("model.stl");
    std::fs::write(&path, &out).expect("fixture write");
    path
}

/// Decode a `MeshBuffer`'s 8-byte little-endian header into
/// `(vertex_count, index_count)`. The layout (see `ipc.ts`) is:
/// `[u32 vertex_count][u32 index_count][f32 x V x 3 positions][u32 x I indices]`.
fn mesh_header(mesh: &[u8]) -> (u32, u32) {
    assert!(
        mesh.len() >= 8,
        "mesh buffer must carry at least its 8-byte header, got {} bytes",
        mesh.len()
    );
    let vertex_count = u32::from_le_bytes(mesh[0..4].try_into().unwrap());
    let index_count = u32::from_le_bytes(mesh[4..8].try_into().unwrap());
    (vertex_count, index_count)
}

/// A cube compiles to a watertight mesh: the index buffer is nonempty and the
/// stats report `is_manifold: true`.
#[test]
fn ipc_compile_returns_mesh_and_stats() {
    let result: CompileResult = compile_source("cube([10,10,10]);", None);

    assert!(
        result.errors.is_empty(),
        "a cube should compile cleanly, got errors: {:#?}",
        result.errors
    );

    // Nonempty index buffer: a cube tessellates to 8 verts / 12 triangles
    // (36 indices). The header alone proves both buffers are populated.
    let (vertex_count, index_count) = mesh_header(&result.mesh);
    assert_eq!(vertex_count, 8, "a cube has 8 vertices");
    assert_eq!(index_count, 36, "a cube has 12 triangles = 36 indices");
    assert!(index_count > 0, "index buffer must be nonempty for a cube");

    let stats = result
        .stats
        .as_ref()
        .expect("a cube must produce geometry stats");
    assert!(
        stats.is_manifold,
        "a cube must be manifold, got stats: {stats:?}"
    );
    assert_eq!(stats.triangle_count, 12);
    assert_eq!(stats.vertex_count, 8);
}

/// A hard eval error surfaces as a structured error carrying a source line
/// number. (No supported source produces an `Unsupported` error anymore, so
/// the structured-error shape is pinned via a failed assert instead.)
#[test]
fn ipc_compile_surfaces_eval_errors() {
    let result: CompileResult = compile_source("assert(false);\ncube(1);", None);

    assert!(
        !result.errors.is_empty(),
        "a failed assert must surface at least one error"
    );

    let err: &CompileError = result
        .errors
        .iter()
        .find(|e| e.kind == ErrorKind::General)
        .expect("a General eval error");

    assert!(
        err.message.contains("Assertion"),
        "error message names the assertion, got: {}",
        err.message
    );
    assert!(
        err.line >= 1,
        "error must carry a 1-based line number, got line = {}",
        err.line
    );
}

/// `compile_source` with the open-file path
/// resolves a relative `import("model.stl")` against that path — the app now
/// compiles import()-bearing buffers the same way the CLI does.
#[test]
fn ipc_compile_with_open_file_path_resolves_import() {
    let dir = std::env::temp_dir().join(format!("morphe-open-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    write_binary_cube(&dir);
    let open_file = dir.join("part.scad");

    let result = compile_source("import(\"model.stl\");", Some(&open_file));
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        result.errors.is_empty(),
        "import via the open-file path must compile cleanly: {:?}",
        result.errors
    );
    let (vertex_count, index_count) = mesh_header(&result.mesh);
    assert_eq!(vertex_count, 8, "the imported cube has 8 vertices");
    assert_eq!(index_count, 36, "the imported cube has 36 indices");
    let stats = result.stats.expect("stats on a solid");
    assert!(stats.is_manifold, "imported mesh is manifold");
}

/// Without a path (untitled buffer), a relative import fails loudly
/// (the resolver names the unresolved path) — the same behavior as the CLI.
#[test]
fn ipc_compile_without_path_still_errors_on_relative_import() {
    let result = compile_source("import(\"definitely-missing.stl\");", None);
    assert!(
        !result.errors.is_empty(),
        "a missing import must error, got: {:?}",
        result.errors
    );
}

/// An error inside an included file is marked on the `include` line of the
/// editor buffer, and its message names the included file and its location
/// (file name only — no absolute path).
#[test]
fn error_in_included_file_marks_include_line_and_names_the_file() {
    let dir = std::env::temp_dir().join(format!("morphe-desktop-include-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("lib.scad"),
        "a = 1;\n\n  assert(false, \"boom\");\n",
    )
    .expect("lib");
    let main = dir.join("main.scad");
    let source = "cube(1);\ninclude <lib.scad>\n";
    std::fs::write(&main, source).expect("main");

    let result = compile_source(source, Some(&main));
    assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
    let err = &result.errors[0];
    assert_eq!((err.line, err.col), (2, 1), "marked on the include line");
    assert!(err.message.starts_with("lib.scad:3:3: "), "{}", err.message);
    assert!(err.message.contains("boom"), "{}", err.message);
    assert!(
        !err.message.contains(&dir.display().to_string()),
        "no absolute path"
    );
}
