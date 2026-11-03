//! Integration tests for the `morphe stats` subcommand.
//!
//! Each test drives the real `morphe` binary (via `CARGO_BIN_EXE_morphe`) over a
//! temp `.scad` file and asserts on the exit status, stdout (the stats output),
//! and stderr — exercising the whole pipeline end to end (parse → eval → resolve
//! → stats). Unlike `compile`, `stats` reports a 2D top-level result rather than
//! erroring on it.
//!
//! # JSON shape
//!
//! `morphe stats <file> --json` prints a single pretty-JSON object tagged with a
//! `"kind"` field: `"solid"` for a 3D result (fields `bbox`, `bbox_size`,
//! `volume`, `surface_area`, `is_manifold`, `triangle_count`, `vertex_count`) or
//! `"profile"` for a 2D result (fields `bbox`, `bbox_size`, `area`,
//! `contour_count`, `vertex_count`). An empty result is reported as an empty
//! `"solid"` (volume `0`, `bbox` null, `0` triangles).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A unique, freshly-created temp directory for one test.
fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("morphe-stats-{}-{tag}", std::process::id()));
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

/// Run `morphe stats <input> [--json]`.
fn run_stats(input: &Path, json: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_morphe"));
    cmd.arg("stats").arg(input);
    if json {
        cmd.arg("--json");
    }
    cmd.output().expect("run morphe")
}

/// Parse the stats JSON printed to stdout into a `serde_json::Value`.
fn stdout_json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON ({e}): {stdout}"))
}

#[test]
fn stats_cube_json_reports_volume_tris_manifold_bbox() {
    let dir = tmp_dir("cube-json");
    let input = write_scad(&dir, "cube.scad", "cube([2,2,2]);");

    let out = run_stats(&input, true);
    let v = stdout_json(&out);

    assert_eq!(v["kind"], "solid", "cube is a 3D solid");
    assert!(
        (v["volume"].as_f64().unwrap() - 8.0).abs() < 1e-9,
        "cube(2) volume is 8, got {}",
        v["volume"]
    );
    assert_eq!(v["triangle_count"], 12, "a cube has 12 triangles");
    assert_eq!(v["is_manifold"], true, "a cube is a valid manifold");
    assert!(
        v["bbox"].is_array(),
        "cube has a bounding box: {}",
        v["bbox"]
    );
}

#[test]
fn stats_json_is_valid_parseable_json() {
    let dir = tmp_dir("valid-json");
    let input = write_scad(&dir, "cube.scad", "cube([1,2,3]);");

    let out = run_stats(&input, true);
    // The parse itself is the assertion (panics if stdout is not JSON).
    let v = stdout_json(&out);
    assert!(v.is_object(), "stats JSON is a single object");
}

#[test]
fn stats_human_readable_contains_volume_and_manifold() {
    let dir = tmp_dir("human");
    let input = write_scad(&dir, "cube.scad", "cube([2,2,2]);");

    let out = run_stats(&input, false);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        stdout.contains("volume"),
        "human output names volume: {stdout}"
    );
    assert!(
        stdout.contains("manifold"),
        "human output names manifold: {stdout}"
    );
}

#[test]
fn stats_2d_file_reports_profile_area_not_an_error() {
    let dir = tmp_dir("twod");
    // Unlike `compile`, `stats` reports a 2D top-level result rather than
    // erroring. circle(5) is a fragment-polygon inscribed in radius 5, so its
    // area approaches (but stays under) pi*25 = 78.54.
    let input = write_scad(&dir, "circ.scad", "circle(5);");

    let out = run_stats(&input, true);
    let v = stdout_json(&out);

    assert_eq!(v["kind"], "profile", "circle is a 2D profile");
    let area = v["area"].as_f64().unwrap();
    assert!(
        area > 70.0 && area < 78.55,
        "circle(5) area approaches pi*25=78.54, got {area}"
    );
    assert!(v["bbox"].is_array(), "profile has a 2D bounding box");
}

#[test]
fn stats_sphere_volume_within_tolerance() {
    let dir = tmp_dir("sphere");
    let input = write_scad(&dir, "sphere.scad", "sphere(10, $fn=128);");

    let out = run_stats(&input, true);
    let v = stdout_json(&out);

    assert_eq!(v["kind"], "solid");
    let vol = v["volume"].as_f64().unwrap();
    let analytic = 4.0 / 3.0 * std::f64::consts::PI * 1000.0;
    assert!(
        (vol - analytic).abs() / analytic < 0.02,
        "sphere(10) volume {vol} should be near {analytic}"
    );
}

#[test]
fn stats_unsupported_construct_errors_nonzero_no_stats() {
    let dir = tmp_dir("mink");
    let input = write_scad(&dir, "mink.scad", "surface(file = \"h.dat\") { cube(1); }");

    let out = run_stats(&input, true);
    assert!(
        !out.status.success(),
        "unsupported construct must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("surface"),
        "stderr names the construct: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "no stats should be printed on eval error"
    );
}

#[test]
fn stats_syntax_error_exits_nonzero() {
    let dir = tmp_dir("syntax");
    let input = write_scad(&dir, "bad.scad", "cube([1,1,1)");

    let out = run_stats(&input, true);
    assert!(!out.status.success(), "a syntax error must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error"),
        "stderr reports the error: {stderr}"
    );
    assert!(out.stdout.is_empty(), "no stats on a parse error");
}

#[test]
fn stats_empty_result_is_zero_volume_and_exits_zero() {
    let dir = tmp_dir("empty");
    // A difference that removes everything: a valid, volume-less solid.
    let input = write_scad(
        &dir,
        "empty.scad",
        "difference() { cube([2,2,2], center=true); cube([4,4,4], center=true); }",
    );

    let out = run_stats(&input, true);
    let v = stdout_json(&out);

    assert_eq!(
        v["kind"], "solid",
        "an empty result reports as an empty solid"
    );
    assert_eq!(v["volume"].as_f64().unwrap(), 0.0, "empty volume is 0");
    assert_eq!(v["triangle_count"], 0, "empty solid has no triangles");
    assert!(
        v["bbox"].is_null(),
        "empty solid has no bbox: {}",
        v["bbox"]
    );
    assert_eq!(v["is_manifold"], true, "an empty body is a valid manifold");
}

#[test]
fn stats_reports_is_manifold_true_on_a_well_formed_solid() {
    let dir = tmp_dir("manifold");
    // Acceptance: a combined, well-formed part — a box with a hole plus
    // a translated cylinder, unioned — reports is_manifold true.
    let source = "
        union() {
          difference() {
            cube([20, 20, 10], center=true);
            cube([10, 10, 20], center=true);
          }
          translate([15, 0, 0]) cylinder(h=10, r=3, center=true, $fn=32);
        }
    ";
    let input = write_scad(&dir, "part.scad", source);

    let out = run_stats(&input, true);
    let v = stdout_json(&out);

    assert_eq!(v["kind"], "solid");
    assert_eq!(
        v["is_manifold"], true,
        "a well-formed corpus-style solid is a manifold"
    );
    assert!(
        v["triangle_count"].as_u64().unwrap() > 0,
        "the combined model has triangles"
    );
    assert!(
        v["volume"].as_f64().unwrap() > 0.0,
        "the combined model has volume"
    );
}
