//! Integration tests for the `morphe compile` subcommand.
//!
//! Each test drives the real `morphe` binary (via `CARGO_BIN_EXE_morphe`) over a
//! temp `.scad` file and asserts on the exit status, stderr, and the STL it
//! writes — exercising the whole pipeline end to end (parse → eval → resolve →
//! STL export).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A unique, freshly-created temp directory for one test.
fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("morphe-cli-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Write `source` to `<dir>/<name>` and return its path.
fn write_scad(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, source).expect("write scad");
    path
}

/// Run `morphe compile <input> -o <output> [--ascii]`.
fn run_compile(input: &Path, output: &Path, ascii: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_morphe"));
    cmd.arg("compile").arg(input).arg("-o").arg(output);
    if ascii {
        cmd.arg("--ascii");
    }
    cmd.output().expect("run morphe")
}

/// Triangle count from a binary STL's `u32` count field (offset 80).
fn stl_triangle_count(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[80..84].try_into().unwrap())
}

#[test]
fn compile_cube_produces_valid_binary_stl() {
    let dir = tmp_dir("cube-bin");
    let input = write_scad(&dir, "cube.scad", "cube([2,2,2]);");
    let output = dir.join("cube.stl");

    let out = run_compile(&input, &output, false);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = fs::read(&output).expect("output STL exists");
    let count = stl_triangle_count(&bytes);
    // A cube is 12 triangles; the file is exactly 84 + 50*n bytes.
    assert_eq!(count, 12, "a cube has 12 triangles");
    assert_eq!(bytes.len(), 84 + 50 * 12, "binary STL is 84 + 50*n bytes");
    // Header must not masquerade as ASCII STL.
    assert_ne!(&bytes[..5], b"solid");
}

#[test]
fn compile_ascii_produces_ascii_stl() {
    let dir = tmp_dir("cube-ascii");
    let input = write_scad(&dir, "cube.scad", "cube([1,1,1]);");
    let output = dir.join("cube.stl");

    let out = run_compile(&input, &output, true);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = fs::read_to_string(&output).expect("output STL exists");
    assert!(text.starts_with("solid morphe\n"), "ASCII STL header");
    assert!(
        text.trim_end().ends_with("endsolid morphe"),
        "ASCII STL footer"
    );
    assert_eq!(
        text.matches("facet normal").count(),
        12,
        "12 facets for a cube"
    );
}

#[test]
fn compile_difference_has_fewer_facets_but_valid_stl() {
    let dir = tmp_dir("difference");
    // A box with a smaller box carved out — a difference resolves to a valid,
    // non-empty solid with more triangles than a bare cube (the hole adds faces).
    let input = write_scad(
        &dir,
        "diff.scad",
        "difference() { cube([4,4,4], center=true); cube([2,2,2], center=true); }",
    );
    let output = dir.join("diff.stl");

    let out = run_compile(&input, &output, false);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = fs::read(&output).unwrap();
    let count = stl_triangle_count(&bytes);
    assert!(
        count > 12,
        "a hollowed box has more than a cube's 12 tris, got {count}"
    );
    assert_eq!(bytes.len(), 84 + 50 * count as usize);
}

#[test]
fn compile_unsupported_construct_errors_and_writes_no_file() {
    let dir = tmp_dir("minkowski");
    let input = write_scad(&dir, "mink.scad", "surface(file = \"h.dat\") { cube(1); }");
    let output = dir.join("mink.stl");

    let out = run_compile(&input, &output, false);
    assert!(
        !out.status.success(),
        "unsupported construct must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("surface"),
        "stderr names the construct: {stderr}"
    );
    assert!(!output.exists(), "no STL should be written on eval error");
}

#[test]
fn compile_syntax_error_exits_nonzero() {
    let dir = tmp_dir("syntax");
    let input = write_scad(&dir, "bad.scad", "cube([1,1,1)");
    let output = dir.join("bad.stl");

    let out = run_compile(&input, &output, false);
    assert!(!out.status.success(), "a syntax error must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error"),
        "stderr reports the error: {stderr}"
    );
    assert!(
        !output.exists(),
        "no STL should be written on a parse error"
    );
}

#[test]
fn compile_2d_only_file_errors() {
    let dir = tmp_dir("twod");
    let input = write_scad(&dir, "circ.scad", "circle(5);");
    let output = dir.join("circ.stl");

    let out = run_compile(&input, &output, false);
    assert!(
        !out.status.success(),
        "a 2D-only top level cannot export STL"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a 3D object"),
        "stderr explains the 2D top-level error: {stderr}"
    );
    assert!(!output.exists());
}

#[test]
fn compile_writes_to_the_specified_output_path() {
    let dir = tmp_dir("outpath");
    let input = write_scad(&dir, "cube.scad", "cube([1,1,1]);");
    let output = dir.join("nested").join("chosen-name.stl");
    fs::create_dir_all(output.parent().unwrap()).unwrap();

    let out = run_compile(&input, &output, false);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.exists(), "-o path must receive the STL");
    assert_eq!(stl_triangle_count(&fs::read(&output).unwrap()), 12);
}

#[test]
fn compile_multi_construct_end_to_end_is_a_valid_manifold() {
    let dir = tmp_dir("e2e");
    // End-to-end seed: several constructs combined — a base box with a
    // hole, plus a translated cylinder built via a primitive, unioned together.
    let source = "
        union() {
          difference() {
            cube([20, 20, 10], center=true);
            translate([0, 0, 0]) cube([10, 10, 20], center=true);
          }
          translate([15, 0, 0]) cylinder(h=10, r=3, center=true, $fn=32);
        }
    ";
    let input = write_scad(&dir, "part.scad", source);
    let output = dir.join("part.stl");

    let out = run_compile(&input, &output, false);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = fs::read(&output).unwrap();
    let count = stl_triangle_count(&bytes);
    assert!(count > 0, "the combined model has triangles");
    assert_eq!(
        bytes.len(),
        84 + 50 * count as usize,
        "well-formed binary STL"
    );

    // Read the mesh back and confirm it is watertight: in a closed manifold
    // every edge is shared by exactly two triangles. This is the honest,
    // topology-agnostic watertightness proof — it holds for any closed surface
    // regardless of genus or component count. (This fixture is a genus-1 tube —
    // the box has a through-hole — plus a disjoint cylinder, so its Euler
    // characteristic is NOT the genus-0 value 2; we deliberately do not assert
    // V - E + F here.)
    let mut verts: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for i in 0..count as usize {
        let base = 84 + 50 * i;
        let mut tri = [0u32; 3];
        for (c, corner) in tri.iter_mut().enumerate() {
            let off = base + 12 + c * 12;
            let key = [
                u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()),
                u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap()),
                u32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap()),
            ];
            let next = verts.len() as u32;
            *corner = *verts.entry(key).or_insert(next);
        }
        tris.push(tri);
    }
    let mut edges: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    for t in &tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = if a < b { (a, b) } else { (b, a) };
            *edges.entry(key).or_insert(0) += 1;
        }
    }
    assert!(
        edges.values().all(|&n| n == 2),
        "every edge must be shared by exactly two triangles (watertight manifold)"
    );
}

// ── robustness against untrusted input ────────────────────────────────────

#[test]
fn compile_zero_fa_fs_sphere_succeeds_not_panics() {
    // `$fa=0; $fs=0;` divided by zero into u32::MAX fragments and panicked
    // with `capacity overflow`; upstream floors both at 0.01.
    let dir = tmp_dir("zero-fa-fs");
    let input = write_scad(&dir, "s.scad", "$fa=0; $fs=0; sphere(1);");
    let output = dir.join("s.stl");
    let out = run_compile(&input, &output, false);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn compile_huge_fn_errors_cleanly() {
    let dir = tmp_dir("huge-fn");
    let input = write_scad(&dir, "s.scad", "$fn=1e7; sphere(1);");
    let output = dir.join("s.stl");
    let out = run_compile(&input, &output, false);
    assert!(!out.status.success(), "a capped fragment count must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sphere") && stderr.contains("cap"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn compile_deep_module_recursion_does_not_overflow() {
    let dir = tmp_dir("deep-rec");
    let input = write_scad(
        &dir,
        "r.scad",
        "module m(n) if (n > 0) translate([1,0,0]) m(n-1); m(510);",
    );
    let output = dir.join("r.stl");
    let out = run_compile(&input, &output, false);
    // m(0) produces no geometry: an empty STL, but a clean exit.
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn compile_long_binary_chain_is_a_parse_error() {
    let dir = tmp_dir("long-chain");
    let chain = vec!["1"; 100_000].join("+");
    let input = write_scad(&dir, "c.scad", &format!("x = {chain}; cube(x);"));
    let output = dir.join("c.stl");
    let out = run_compile(&input, &output, false);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nesting too deep"), "{stderr}");
}

#[test]
fn compile_warnings_carry_file_line_col() {
    let dir = tmp_dir("warn-loc");
    fs::create_dir_all(dir.join("lib")).unwrap();
    let lib = write_scad(&dir, "lib/part.scad", "cube(1);\n  sphere(r = rr);\n");
    let input = write_scad(
        &dir,
        "main.scad",
        "include <lib/part.scad>\ncube(  size = s);\n",
    );
    let output = dir.join("out.stl");

    let out = run_compile(&input, &output, false);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    // A warning in the main file names it with its line and column.
    let main_warning = format!(
        "{}:2:15: warning: Ignoring unknown variable 's'",
        input.display()
    );
    assert!(stderr.contains(&main_warning), "stderr: {stderr}");
    // One in an included file names that file instead.
    let lib_warning = format!(
        "{}:2:14: warning: Ignoring unknown variable 'rr'",
        lib.canonicalize().unwrap().display()
    );
    assert!(stderr.contains(&lib_warning), "stderr: {stderr}");
    let _ = fs::remove_dir_all(&dir);
}
