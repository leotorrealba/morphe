//! `include <file>` and `use <file>`.
//!
//! Semantics follow upstream OpenSCAD: `include` is textual insertion
//! (`src/core/lexer.l includefile`), `use` imports only module and function
//! definitions, evaluated in the used file's own scope (`SourceFile.cc`
//! `registerUse`/`usedlibs`, `FileContext.cc lookup_local_function` /
//! `lookup_local_module`), and paths resolve against the including file's
//! directory, then the library path (`parsersettings.cc find_valid_path`).
//! Containment to the source tree / OPENSCADPATH, the hard errors for
//! missing files and cycles, and the size/depth caps are morphe's own.

use std::path::{Path, PathBuf};

use morphe_eval::csg::{CsgNode, Primitive};
use morphe_eval::eval::{EvalError, Evaluator};
use morphe_lang::lexer::line_col;
use morphe_lang::parser::parse_program;

/// A fresh, empty temporary directory unique to `name`.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "morphe-include-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(dir: &Path, rel: &str, text: &str) -> PathBuf {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

/// Evaluate `src` as if saved at `main` (or unsaved when `None`), with the
/// given library paths (never the process environment).
fn run(src: &str, main: Option<&Path>, libs: Vec<PathBuf>) -> (CsgNode, Evaluator) {
    let mut ev = Evaluator::new();
    ev.set_library_paths(libs);
    ev.set_source(src);
    ev.set_source_path(main);
    let program = parse_program(src).expect("main program parses");
    let root = ev.eval_program(&program);
    (root, ev)
}

/// Evaluate the file at `main` (reading it from disk).
fn run_file(main: &Path) -> (CsgNode, Evaluator) {
    let src = std::fs::read_to_string(main).unwrap();
    run(&src, Some(main), Vec::new())
}

fn primitives(node: &CsgNode, out: &mut Vec<Primitive>) {
    match node {
        CsgNode::Primitive { primitive, .. } => out.push(primitive.clone()),
        CsgNode::Group { children, .. } => children.iter().for_each(|c| primitives(c, out)),
        CsgNode::Transform { child, .. } => primitives(child, out),
        _ => {}
    }
}

fn all_primitives(node: &CsgNode) -> Vec<Primitive> {
    let mut out = Vec::new();
    primitives(node, &mut out);
    out
}

fn error_messages(ev: &Evaluator) -> Vec<String> {
    ev.errors().iter().map(|e| e.message.clone()).collect()
}

fn single_error(ev: &Evaluator) -> &EvalError {
    assert_eq!(ev.errors().len(), 1, "errors: {:?}", error_messages(ev));
    &ev.errors()[0]
}

// ------------------------------------------------------------------ include

#[test]
fn include_brings_geometry_variables_and_definitions() {
    let dir = temp_dir("inc-basic");
    write(
        &dir,
        "lib.scad",
        "v = 5;\nfunction f(x) = x * 3;\nmodule m() sphere(v);\ncube(1);\n",
    );
    let main = write(
        &dir,
        "main.scad",
        "include <lib.scad>\necho(v, f(2));\nm();\n",
    );
    let (root, ev) = run_file(&main);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(ev.console(), ["ECHO: 5, 6"]);
    let prims = all_primitives(&root);
    assert_eq!(
        prims.len(),
        2,
        "the lib's cube plus m()'s sphere: {prims:?}"
    );
    assert!(matches!(prims[0], Primitive::Cube { .. }));
    assert!(matches!(prims[1], Primitive::Sphere { .. }));
}

#[test]
fn include_is_textual_so_assignments_merge_last_wins() {
    // Upstream inserts the file's text, so a later assignment in the main
    // file replaces the included one in place (parser.y handle_assignment).
    let dir = temp_dir("inc-merge");
    write(
        &dir,
        "lib.scad",
        "size = 1;\nfunction twice() = size * 2;\n",
    );
    let main = write(
        &dir,
        "main.scad",
        "include <lib.scad>\nsize = 4;\necho(twice());\n",
    );
    let (_, ev) = run_file(&main);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(ev.console(), ["ECHO: 8"]);
    assert!(ev
        .warnings()
        .iter()
        .any(|w| w.contains("size was assigned twice")));
}

#[test]
fn nested_include_resolves_against_the_including_file() {
    let dir = temp_dir("inc-nested");
    write(&dir, "sub/a.scad", "include <b.scad>\na = b + 1;\n");
    write(&dir, "sub/b.scad", "b = 10;\n");
    let main = write(&dir, "main.scad", "include <sub/a.scad>\necho(a);\n");
    let (_, ev) = run_file(&main);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(ev.console(), ["ECHO: 11"]);
}

#[test]
fn include_inside_a_module_body_splices_there() {
    let dir = temp_dir("inc-module");
    write(&dir, "body.scad", "cube(k);\n");
    let main = write(
        &dir,
        "main.scad",
        "module m(k) { include <body.scad> }\nm(2);\nm(3);\n",
    );
    let (root, ev) = run_file(&main);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(all_primitives(&root).len(), 2);
}

#[test]
fn include_cycle_is_an_error_not_a_stack_overflow() {
    let dir = temp_dir("inc-cycle");
    write(&dir, "a.scad", "include <b.scad>\n");
    write(&dir, "b.scad", "include <a.scad>\ncube(1);\n");
    let main = write(&dir, "main.scad", "include <a.scad>\n");
    let (root, ev) = run_file(&main);
    let err = single_error(&ev);
    assert!(err.message.contains("include cycle"), "{}", err.message);
    assert!(
        err.message.contains("a.scad -> b.scad -> a.scad"),
        "{}",
        err.message
    );
    // The non-cyclic part still evaluates.
    assert_eq!(all_primitives(&root).len(), 1);
}

#[test]
fn a_file_including_itself_is_a_cycle() {
    let dir = temp_dir("inc-self");
    let main = write(&dir, "main.scad", "include <main.scad>\ncube(1);\n");
    let (_, ev) = run_file(&main);
    assert!(single_error(&ev).message.contains("include cycle"));
}

#[test]
fn include_depth_is_capped() {
    let dir = temp_dir("inc-depth");
    for i in 0..80 {
        write(
            &dir,
            &format!("f{i}.scad"),
            &format!("include <f{}.scad>\n", i + 1),
        );
    }
    write(&dir, "f80.scad", "cube(1);\n");
    let main = write(&dir, "main.scad", "include <f0.scad>\n");
    let (_, ev) = run_file(&main);
    let err = single_error(&ev);
    assert!(
        err.message.contains("nested more than 64"),
        "{}",
        err.message
    );
}

#[test]
fn missing_include_is_an_error() {
    let dir = temp_dir("inc-missing");
    let main = write(&dir, "main.scad", "include <nope.scad>\ncube(1);\n");
    let (root, ev) = run_file(&main);
    let err = single_error(&ev);
    assert!(
        err.message.contains("Can't open include file 'nope.scad'"),
        "{}",
        err.message
    );
    assert!(err.origin.is_none());
    assert_eq!(all_primitives(&root).len(), 1);
}

#[test]
fn missing_use_is_an_error() {
    let dir = temp_dir("use-missing");
    let main = write(&dir, "main.scad", "use <nope.scad>\ncube(1);\n");
    let (_, ev) = run_file(&main);
    assert!(single_error(&ev)
        .message
        .contains("Can't open library 'nope.scad'"));
}

#[test]
fn include_outside_the_source_tree_is_refused() {
    let root = temp_dir("inc-outside");
    write(&root, "secret.scad", "cube(1);\n");
    let project = root.join("proj");
    let main = write(&project, "main.scad", "include <../secret.scad>\n");
    let (node, ev) = run_file(&main);
    let err = single_error(&ev);
    assert!(
        err.message.contains("outside the source file's directory"),
        "{}",
        err.message
    );
    assert!(all_primitives(&node).is_empty(), "nothing was read");

    // The same refusal for a path that does not exist: an escaping path
    // never reveals whether its target exists.
    let src = "include <../does-not-exist.scad>\n";
    let (_, ev) = run(src, Some(&main), Vec::new());
    assert!(single_error(&ev)
        .message
        .contains("outside the source file's directory"));

    // An absolute path outside the tree.
    let secret = root.join("secret.scad");
    let src = format!("use <{}>\n", secret.display());
    let (_, ev) = run(&src, Some(&main), Vec::new());
    assert!(single_error(&ev)
        .message
        .contains("outside the source file's directory"));
}

#[cfg(unix)]
#[test]
fn symlink_escaping_the_source_tree_is_refused() {
    let root = temp_dir("inc-symlink");
    let secret = write(&root, "secret.scad", "cube(1);\n");
    let project = root.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::os::unix::fs::symlink(&secret, project.join("link.scad")).unwrap();
    let main = write(&project, "main.scad", "include <link.scad>\n");
    let (node, ev) = run_file(&main);
    assert!(single_error(&ev)
        .message
        .contains("outside the source file's directory"));
    assert!(all_primitives(&node).is_empty());
}

#[test]
fn include_of_a_directory_is_refused() {
    let dir = temp_dir("inc-dir");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let main = write(&dir, "main.scad", "include <sub>\n");
    let (_, ev) = run_file(&main);
    assert!(single_error(&ev).message.contains("not a regular file"));
}

#[test]
fn unsaved_buffer_cannot_include_relative_paths() {
    let (_, ev) = run("include <lib.scad>\ncube(1);\n", None, Vec::new());
    let err = single_error(&ev);
    assert!(
        err.message
            .contains("include requires the file to be saved"),
        "{}",
        err.message
    );
    let (_, ev) = run("use <lib.scad>\n", None, Vec::new());
    assert!(single_error(&ev)
        .message
        .contains("use requires the file to be saved"));
}

#[test]
fn library_paths_are_searched_and_allowed() {
    let libs = temp_dir("lib-path");
    write(&libs, "shapes/peg.scad", "function peg_r() = 3;\n");
    let project = temp_dir("lib-path-proj");
    let main = project.join("main.scad");
    let src = "use <shapes/peg.scad>\necho(peg_r());\n";
    let (_, ev) = run(src, Some(&main), vec![libs.clone()]);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(ev.console(), ["ECHO: 3"]);

    // The library path also serves an unsaved buffer.
    let (_, ev) = run(src, None, vec![libs]);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(ev.console(), ["ECHO: 3"]);
}

#[test]
fn the_including_directory_wins_over_library_paths() {
    let libs = temp_dir("lib-order");
    write(&libs, "v.scad", "v = \"library\";\n");
    let project = temp_dir("lib-order-proj");
    write(&project, "v.scad", "v = \"local\";\n");
    let main = project.join("main.scad");
    let (_, ev) = run("include <v.scad>\necho(v);\n", Some(&main), vec![libs]);
    assert_eq!(ev.console(), ["ECHO: \"local\""]);
}

// ---------------------------------------------------------------------- use

#[test]
fn use_imports_only_modules_and_functions() {
    let dir = temp_dir("use-basic");
    write(
        &dir,
        "lib.scad",
        "v = 5;\nfunction f(x) = x + v;\nmodule m() cube(v);\nsphere(1);\necho(\"lib top level\");\n",
    );
    let main = write(
        &dir,
        "main.scad",
        "use <lib.scad>\necho(f(1));\necho(is_undef(v));\nm();\n",
    );
    let (root, ev) = run_file(&main);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    // The library's own variable is visible inside its function and
    // module; to the importer it does not exist; its top-level echo and
    // sphere never run.
    assert_eq!(ev.console(), ["ECHO: 6", "ECHO: true"]);
    let prims = all_primitives(&root);
    assert_eq!(prims.len(), 1, "only m()'s cube: {prims:?}");
    match &prims[0] {
        Primitive::Cube { size, .. } => assert_eq!(*size, [5.0, 5.0, 5.0]),
        other => panic!("expected a cube, got {other:?}"),
    }
}

#[test]
fn used_file_does_not_see_the_importers_scope() {
    let dir = temp_dir("use-isolated");
    write(
        &dir,
        "lib.scad",
        "function sees_x() = is_undef(x);\nfunction calls_helper() = helper();\nfunction pi() = PI;\n",
    );
    let main = write(
        &dir,
        "main.scad",
        "x = 7;\nfunction helper() = 1;\nuse <lib.scad>\necho(sees_x(), calls_helper(), pi() == PI);\n",
    );
    let (_, ev) = run_file(&main);
    assert_eq!(ev.console(), ["ECHO: true, undef, true"]);
    assert!(ev
        .warnings()
        .iter()
        .any(|w| w.contains("Ignoring unknown function 'helper'")));
}

#[test]
fn own_definitions_shadow_used_ones() {
    let dir = temp_dir("use-shadow");
    write(
        &dir,
        "lib.scad",
        "function f() = \"lib\";\nmodule m() cube(1);\n",
    );
    let main = write(
        &dir,
        "main.scad",
        "use <lib.scad>\nfunction f() = \"main\";\necho(f());\n",
    );
    let (_, ev) = run_file(&main);
    assert_eq!(ev.console(), ["ECHO: \"main\""]);
}

#[test]
fn used_files_use_their_own_libraries_without_reexporting_them() {
    let dir = temp_dir("use-chain");
    write(
        &dir,
        "lib/a.scad",
        "use <b.scad>\nfunction a() = b() + 1;\n",
    );
    write(&dir, "lib/b.scad", "function b() = 41;\n");
    let main = write(
        &dir,
        "main.scad",
        "use <lib/a.scad>\necho(a());\necho(b());\n",
    );
    let (_, ev) = run_file(&main);
    assert_eq!(ev.console(), ["ECHO: 42", "ECHO: undef"]);
}

#[test]
fn mutually_using_files_terminate() {
    let dir = temp_dir("use-mutual");
    write(
        &dir,
        "a.scad",
        "use <b.scad>\nfunction a(n) = n <= 0 ? 0 : 1 + b(n - 1);\n",
    );
    write(
        &dir,
        "b.scad",
        "use <a.scad>\nfunction b(n) = n <= 0 ? 0 : 1 + a(n - 1);\n",
    );
    let main = write(&dir, "main.scad", "use <a.scad>\necho(a(5));\n");
    let (_, ev) = run_file(&main);
    assert!(ev.errors().is_empty(), "{:?}", error_messages(&ev));
    assert_eq!(ev.console(), ["ECHO: 5"]);
}

// ------------------------------------------------------- error locations

#[test]
fn errors_in_included_files_report_that_file_and_line() {
    let dir = temp_dir("inc-location");
    let lib = write(&dir, "lib.scad", "a = 1;\n\n  assert(false, \"boom\");\n");
    let src = "cube(1);\ninclude <lib.scad>\n";
    let main = write(&dir, "main.scad", src);
    let (_, ev) = run_file(&main);
    let err = single_error(&ev);
    assert!(err.message.contains("boom"), "{}", err.message);
    let origin = err.origin.as_ref().expect("located in the included file");
    assert_eq!(origin.file, lib.canonicalize().unwrap());
    assert_eq!((origin.line, origin.col), (3, 3));
    assert_eq!(err.line_col(src), (3, 3));
    // The main-source span is the include statement.
    assert_eq!(line_col(src, err.span.start), (2, 1));
    assert_eq!(&src[err.span.start..err.span.end], "include <lib.scad>");
}

#[test]
fn assert_condition_text_is_quoted_from_the_included_file() {
    let dir = temp_dir("inc-assert-text");
    write(&dir, "lib.scad", "assert(1 > 2);\n");
    let main = write(&dir, "main.scad", "include <lib.scad>\n");
    let (_, ev) = run_file(&main);
    assert_eq!(single_error(&ev).message, "Assertion '1 > 2' failed");
}

#[test]
fn parse_errors_in_used_files_report_that_file_and_line() {
    let dir = temp_dir("use-parse-error");
    let lib = write(&dir, "lib.scad", "function f() = 1;\nmodule ( {\n");
    let main = write(&dir, "main.scad", "\n\nuse <lib.scad>\necho(f());\n");
    let (_, ev) = run_file(&main);
    let err = single_error(&ev);
    let origin = err.origin.as_ref().expect("located in the used file");
    assert_eq!(origin.file, lib.canonicalize().unwrap());
    assert_eq!(origin.line, 2);
    let src = std::fs::read_to_string(&main).unwrap();
    assert_eq!(line_col(&src, err.span.start), (3, 1));
    // The failed library contributes nothing.
    assert_eq!(ev.console(), ["ECHO: undef"]);
}

#[test]
fn errors_in_used_functions_report_the_library_location() {
    let dir = temp_dir("use-runtime-error");
    let lib = write(&dir, "lib.scad", "function deep(n) = deep(n + 1) + 1;\n");
    let main = write(&dir, "main.scad", "use <lib.scad>\nx = deep(0);\n");
    let (_, ev) = run_file(&main);
    let err = single_error(&ev);
    assert!(err.message.contains("recursion"), "{}", err.message);
    let origin = err.origin.as_ref().expect("located in the library");
    assert_eq!(origin.file, lib.canonicalize().unwrap());
    assert_eq!(origin.line, 1);
}
