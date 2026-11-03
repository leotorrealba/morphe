//! query_noos tool integration tests.
//!
//! Exercises the `query_noos` AI tool against a temp `.noos` dir + the real
//! `noos` binary (an optional external tool). Tests marked "Binary" below
//! invoke the real binary and skip cleanly when `noos` is not on `PATH` (early
//! `return`, never panic) — mirroring the OpenSCAD oracle's degrade-to-skip
//! pattern and the noos_writer tests. The others do NOT depend on the binary
//! (the missing-binary test injects a fake path via the
//! [`Dispatcher::with_noos_binary`] test seam); they must NOT skip.
//!
//! # Coverage
//!
//! | What                                                            | Binary |
//! |-----------------------------------------------------------------|--------|
//! | query_noos returns ranked hits from noos query over seeded graph | yes   |
//! | no noos_dir → graceful "open a project file" ToolResult (Ok)     | no    |
//! | noos binary absent → graceful "noos not found" ToolResult (Ok)   | no    |
//! | token-bounded: --budget const is reasonable + flag reaches noos  | no    |
//! | Dispatcher carries noos_dir; queries THAT graph, not cwd         | yes   |
//! | distinct query terms → distinct results (term is passed through) | yes   |

use std::fs;
use std::path::{Path, PathBuf};

use morphe_ai::{Dispatcher, InMemoryWorkspace, QUERY_BUDGET};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// temp-dir + noos-detect helpers (match the noos_writer test conventions)
// ---------------------------------------------------------------------------

/// Build a fresh temp PROJECT ROOT (the parent of `.noos/`) for one test. The
/// caller seeds `<root>/.noos/graph.json`; the dispatcher's `noos_dir` is
/// `<root>/.noos`. Idempotent: wipes any prior dir at the same path so re-runs
/// are deterministic.
fn temp_project_root(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("morphe-ai-query-noos-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".noos")).expect("create temp .noos dir");
    dir
}

/// Detect the `noos` binary on `PATH` (or via `NOOS_BIN` override). Mirrors
/// `detect_noos` from the noos_writer tests + `detect_openscad` from the
/// OpenSCAD oracle.
/// `None` → tests degrade-to-skip.
fn detect_noos() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NOOS_BIN") {
        let p = PathBuf::from(p);
        return if p.is_file() { Some(p) } else { None };
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("noos"))
            .find(|p| p.is_file())
    })
}

/// Seed `<root>/.noos/graph.json` with a minimal valid envelope containing one
/// symbol node named `symbol_name`. The graph passes `noos validate` and
/// `noos query <symbol_name>` returns the seed (verified against the real
/// binary).
fn seed_graph_with_symbol(root: &Path, symbol_name: &str) {
    seed_graph_with_nodes(root, &[symbol_name.to_string()]);
}

/// Seed `<root>/.noos/graph.json` with a minimal valid envelope containing a
/// symbol node per name in `symbol_names`. Used by the distinct-terms test (two distinct terms in
/// one graph).
fn seed_graph_with_nodes(root: &Path, symbol_names: &[String]) {
    let nodes: Vec<Value> = symbol_names
        .iter()
        .map(|name| {
            serde_json::json!({
                "id": format!("code:src/lib.rs#{name}"),
                "node_type": "symbol",
                "name": name,
                "file_path": "src/lib.rs",
                "summary": format!("function {name} in lib.rs"),
                "tags": ["function", "rust"],
                "complexity": 1
            })
        })
        .collect();
    let graph = serde_json::json!({
        "version": "0.2.0",
        "project": {
            "name": "query-noos-seed",
            "languages": ["rust"],
            "git_commit_hash": "abc",
            "ingested_at": "2026-07-19T00:00:00Z"
        },
        "nodes": nodes,
        "edges": [],
        "layers": [],
        "tour": [],
        "communities": [],
        "duplicates": []
    });
    fs::write(
        root.join(".noos").join("graph.json"),
        serde_json::to_string_pretty(&graph).expect("serialize seed graph"),
    )
    .expect("seed graph.json");
}

/// Build a Dispatcher wired to `noos_dir` with the real noos binary path
/// (when present). The binary-backed tests use this; they call `.with_noos_binary` after
/// to inject the detected binary path so the test honors `NOOS_BIN`.
fn dispatcher_with_noos_dir(noos_dir: PathBuf) -> Dispatcher {
    Dispatcher::new(Box::new(InMemoryWorkspace::new()), Some(noos_dir))
}

// ---------------------------------------------------------------------------
// query_noos returns ranked hits from noos query over a seeded graph
// ---------------------------------------------------------------------------

#[test]
fn query_noos_returns_ranked_hits_from_real_noos() {
    let noos_bin = match detect_noos() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: `noos` binary not on PATH (degrade-to-skip)");
            return;
        }
    };

    let root = temp_project_root("case1");
    seed_graph_with_symbol(&root, "widget_mount");
    let noos_dir = root.join(".noos");

    let mut dispatcher = dispatcher_with_noos_dir(noos_dir).with_noos_binary(noos_bin);
    let result = dispatcher.dispatch("query_noos", &json!({"query": "widget_mount"}));

    assert!(
        !result.is_error,
        "query_noos must return Ok (graceful), not error/panic; got: {}",
        result.content
    );
    assert!(
        result.content.contains("widget_mount"),
        "result must mention the seed symbol 'widget_mount'; got:\n{}",
        result.content
    );
    // The noos injection-guard preamble is noos's own warning to console
    // consumers, not query content. morphe strips it before returning so the
    // AI receives only the ranked hits.
    assert!(
        !result.content.contains("_The content below is extracted"),
        "noos preamble must be stripped before returning to the AI; got preamble in:\n{}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// degrades gracefully when no noos_dir (no project file open)
// ---------------------------------------------------------------------------

#[test]
fn query_noos_no_noos_dir_returns_useful_message() {
    // Dispatcher with noos_dir=None mirrors "no file open" in the editor.
    // The tool must surface a useful Ok message (not Err, not panic).
    let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
    let result = dispatcher.dispatch("query_noos", &json!({"query": "anything"}));

    assert!(
        !result.is_error,
        "no noos_dir must surface as graceful Ok, not error/panic"
    );
    assert!(
        result.content.contains("no design memory"),
        "useful message about no design memory; got: {}",
        result.content
    );
    assert!(
        result.content.contains("open a project file"),
        "message should nudge the user to open a project file; got: {}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// degrades gracefully when the noos binary is absent
// ---------------------------------------------------------------------------

#[test]
fn query_noos_handles_missing_binary_gracefully() {
    // Inject a non-existent binary path via the test seam. This genuinely
    // simulates noos absence REGARDLESS of whether noos is actually on PATH
    // (the spawn fails with NotFound before noos could do anything). The test
    // therefore never skips — the production NotFound branch is exercised
    // deterministically on every machine, including dev (where noos exists).
    let root = temp_project_root("case3");
    let noos_dir = root.join(".noos");
    // An empty-but-valid graph is fine — the spawn fails before noos reads it.
    fs::write(
        noos_dir.join("graph.json"),
        "{\"version\":\"0.2.0\",\"project\":{\"name\":\"x\"},\"nodes\":[],\"edges\":[]}",
    )
    .expect("seed empty graph");

    let mut dispatcher = dispatcher_with_noos_dir(noos_dir)
        .with_noos_binary("/definitely/not/a/real/noos/binary/anywhere");
    let result = dispatcher.dispatch("query_noos", &json!({"query": "anything"}));

    assert!(
        !result.is_error,
        "missing noos binary must surface as graceful Ok, not error/panic"
    );
    assert!(
        result.content.contains("noos not found"),
        "useful 'noos not found' message; got: {}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// token-bounded (--budget in the invocation, value in reasonable range)
// ---------------------------------------------------------------------------

#[test]
fn query_noos_uses_reasonable_token_budget() {
    // The budget const must be in the reasonable range for an AI tool result
    // (2000–4000 tokens). A huge graph would otherwise blow
    // the AI's context window. Evaluated at compile time (const-eval) so a bad
    // value fails the build, not just at test time — stronger guarantee.
    const _: () = {
        assert!(QUERY_BUDGET >= 2000);
        assert!(QUERY_BUDGET <= 4000);
    };

    // The argv builder must include --budget (proves the flag reaches noos in
    // the production invocation — the only place the budget takes effect).
    let argv = morphe_ai::noos_query_argv("widget_mount");
    assert!(
        argv.iter().any(|s| s == "--budget"),
        "--budget must be in the noos query argv; got {argv:?}"
    );
    assert!(
        argv.iter().any(|s| s == &QUERY_BUDGET.to_string()),
        "QUERY_BUDGET value ({QUERY_BUDGET}) must be in the argv; got {argv:?}"
    );
    // The query term itself must reach noos as a single argv element (not
    // shell-formatted) — layer 1 of the load-bearing security property (no
    // command injection).
    assert!(
        argv.iter().any(|s| s == "widget_mount"),
        "the query term must be a single argv element; got {argv:?}"
    );
    // Layer 2 (no argument injection): a POSIX `--` terminator MUST follow all
    // real flags so an AI-controlled `--`-prefixed term (e.g. `--help`) is
    // parsed as a positional, never a noos flag. Without it, `query="--help"`
    // exits 0 printing CLI help → morphe returns help text as "design memory".
    // Pinned here because the argv shape IS the security boundary; verified
    // empirically against the real binary.
    assert!(
        argv.contains(&"--".to_string()),
        "the POSIX `--` terminator must be present so flag-shaped terms degrade to NoMatch, not CLI help; got {argv:?}"
    );
}

// ---------------------------------------------------------------------------
// Dispatcher carries noos_dir; queries THAT graph, not cwd
// ---------------------------------------------------------------------------

#[test]
fn dispatcher_uses_its_noos_dir_not_cwd() {
    let noos_bin = match detect_noos() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: `noos` binary not on PATH (degrade-to-skip)");
            return;
        }
    };

    // Seed graph A with `case5_alpha_only`, graph B with `case5_beta_only`. The
    // dispatcher with noos_dir=A must return only `case5_alpha_only` (NOT
    // `case5_beta_only`), proving the noos_dir field controls which graph is
    // queried — independent of the test process's cwd. The test must prove the wire is alive
    // end-to-end.
    let root_a = temp_project_root("case5_a");
    let root_b = temp_project_root("case5_b");
    seed_graph_with_symbol(&root_a, "case5_alpha_only");
    seed_graph_with_symbol(&root_b, "case5_beta_only");

    let mut dispatcher = dispatcher_with_noos_dir(root_a.join(".noos")).with_noos_binary(noos_bin);
    let result = dispatcher.dispatch("query_noos", &json!({"query": "case5_alpha_only"}));

    assert!(
        !result.is_error,
        "query against graph A should succeed; got: {}",
        result.content
    );
    assert!(
        result.content.contains("case5_alpha_only"),
        "querying noos_dir=A must mention case5_alpha_only (the seed in A); got:\n{}",
        result.content
    );
    assert!(
        !result.content.contains("case5_beta_only"),
        "querying noos_dir=A must NOT mention case5_beta_only (only in B); got:\n{}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// the AI's query term reaches noos query (distinct terms → distinct)
// ---------------------------------------------------------------------------

#[test]
fn distinct_query_terms_yield_distinct_results() {
    let noos_bin = match detect_noos() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: `noos` binary not on PATH (degrade-to-skip)");
            return;
        }
    };

    // One graph with two uniquely-named symbols. Querying term A must return
    // only A; querying term B must return only B. This proves the AI's term
    // reaches noos as the search term (not a hardcoded constant) — the term is
    // data flowing through the wire, not a placeholder.
    let root = temp_project_root("case6");
    seed_graph_with_nodes(
        &root,
        &[
            "case6_unique_alpha".to_string(),
            "case6_unique_beta".to_string(),
        ],
    );

    let mut dispatcher = dispatcher_with_noos_dir(root.join(".noos")).with_noos_binary(noos_bin);

    let result_alpha = dispatcher.dispatch("query_noos", &json!({"query": "case6_unique_alpha"}));
    assert!(
        !result_alpha.is_error,
        "alpha query should succeed; got: {}",
        result_alpha.content
    );
    assert!(
        result_alpha.content.contains("case6_unique_alpha"),
        "alpha query must return alpha; got:\n{}",
        result_alpha.content
    );
    assert!(
        !result_alpha.content.contains("case6_unique_beta"),
        "alpha query must NOT return beta (distinct term → distinct result); got:\n{}",
        result_alpha.content
    );

    let result_beta = dispatcher.dispatch("query_noos", &json!({"query": "case6_unique_beta"}));
    assert!(
        !result_beta.is_error,
        "beta query should succeed; got: {}",
        result_beta.content
    );
    assert!(
        result_beta.content.contains("case6_unique_beta"),
        "beta query must return beta; got:\n{}",
        result_beta.content
    );
    assert!(
        !result_beta.content.contains("case6_unique_alpha"),
        "beta query must NOT return alpha; got:\n{}",
        result_beta.content
    );
}
