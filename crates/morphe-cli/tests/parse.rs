//! Integration tests for the `morphe parse` subcommand, driving the real
//! binary via `CARGO_BIN_EXE_morphe`.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// A temp `.scad` file that removes itself on drop.
struct TempScad {
    path: PathBuf,
}

impl TempScad {
    fn new(contents: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "morphe-cli-parse-test-{}-{n}.scad",
            std::process::id()
        ));
        fs::write(&path, contents).expect("write temp .scad file");
        TempScad { path }
    }

    fn path_str(&self) -> String {
        self.path.display().to_string()
    }
}

impl Drop for TempScad {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn run_morphe(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_morphe"))
        .args(args)
        .output()
        .expect("run morphe binary")
}

#[test]
fn bare_invocation_prints_version() {
    let output = run_morphe(&[]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.trim(), "morphe 0.1.0");
}

#[test]
fn parse_valid_file_reports_ok_and_exits_zero() {
    let file = TempScad::new("cube(1);\nsphere(2);\n");
    let output = run_morphe(&["parse", &file.path_str()]);
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("{}: ok (2 statements)", file.path_str())),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn parse_syntax_error_reports_line_col_and_exits_one() {
    let file = TempScad::new("cube(");
    let output = run_morphe(&["parse", &file.path_str()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Error is at end of input on line 1: `<file>:1:<col>: error: <message>`.
    assert!(
        stderr.contains(&format!("{}:1:", file.path_str())),
        "unexpected stderr: {stderr}"
    );
    assert!(stderr.contains(": error: "), "unexpected stderr: {stderr}");
}

#[test]
fn parse_multiple_files_mixed_reports_both_and_exits_one() {
    let good = TempScad::new("cube(1);\n");
    let bad = TempScad::new("cube(");
    let output = run_morphe(&["parse", &good.path_str(), &bad.path_str()]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stdout.contains(&format!("{}: ok (1 statements)", good.path_str())),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stderr.contains(&format!("{}:1:", bad.path_str())),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn parse_missing_file_reports_error_and_exits_one() {
    let path = std::env::temp_dir().join(format!(
        "morphe-cli-parse-test-missing-{}.scad",
        std::process::id()
    ));
    let path_str = path.display().to_string();
    let output = run_morphe(&["parse", &path_str]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(&format!("{path_str}: error: ")),
        "unexpected stderr: {stderr}"
    );
}
