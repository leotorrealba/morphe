//! noos writer integration tests.
//!
//! Exercises the noos writer against a temp `.noos` dir + the real `noos`
//! binary (an optional external tool). The tests marked "Binary" below
//! invoke it via `std::process::Command`; when `noos` is not on `PATH` those
//! tests skip cleanly (early `return`, never panic) — mirroring the OpenSCAD
//! oracle's degrade-to-skip pattern (`morphe-conformance::oracle::detect_openscad`).
//!
//! # Coverage
//!
//! | What                                                                | Binary |
//! |---------------------------------------------------------------------|--------|
//! | morphe-events.jsonl line envelope (kind+subject+sequence+provenance+...)  | no     |
//! | graph.json event-node shape (id, node_type, file_path, tags)       | no     |
//! | subject derived per DesignEvent variant                             | no     |
//! | written graph passes the real `noos validate`                       | yes    |
//! | two writes same subject → sequences #1, #2 (unique ids)             | no     |
//! | `noos scan` drops event nodes; `rehydrate_events` restores them     | yes    |

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use morphe_ai::{rehydrate_events, write_event, DesignEvent, WriteError};
use serde_json::Value;

// ---------------------------------------------------------------------------
// temp-dir helpers (no `tempfile` dep — matches the workspace's
// `std::env::temp_dir() + pid` convention from morphe-cli).
// ---------------------------------------------------------------------------

/// Build a fresh temp `.noos` dir for one test, named by `tag`. Idempotent:
/// wipes any prior dir at the same path so re-runs are deterministic.
fn temp_noos_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "morphe-ai-noos-writer-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp .noos dir");
    dir
}

/// Build a fresh temp git repo (with one ingestible file) for tests that need
/// `noos scan` to actually run. Includes a `.noos/` subdir.
fn temp_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "morphe-ai-noos-writer-{tag}-repo-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp repo dir");

    // git init + identity (no global config assumed).
    Command::new("git")
        .args(["init", "-q"])
        .arg(&dir)
        .output()
        .expect("git init");
    for (k, v) in [("user.email", "t@t.t"), ("user.name", "t")] {
        Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["config", k, v])
            .output()
            .expect("git config");
    }

    // One ingestible .rs file so `noos scan` has something to chew on.
    fs::create_dir_all(dir.join("src")).expect("mkdir src");
    fs::write(
        dir.join("src/lib.rs"),
        "//! test\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .expect("seed src/lib.rs");
    Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["add", "-A"])
        .output()
        .expect("git add");
    Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["commit", "-qm", "init"])
        .output()
        .expect("git commit");

    fs::create_dir_all(dir.join(".noos")).expect("mkdir .noos");
    dir
}

/// Detect the `noos` binary on `PATH` (or via `NOOS_BIN` override). Mirrors
/// `detect_openscad` from the OpenSCAD oracle. `None` → tests degrade-to-skip.
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

// ---------------------------------------------------------------------------
// graph / sidecar inspection helpers
// ---------------------------------------------------------------------------

/// Read the graph.json under `noos_dir` as a JSON value.
fn read_graph(noos_dir: &Path) -> Value {
    let path = noos_dir.join("graph.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read graph.json {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse graph.json: {e}"))
}

/// All nodes in the graph with `node_type == "event"`.
fn event_nodes(noos_dir: &Path) -> Vec<Value> {
    read_graph(noos_dir)["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.get("node_type").and_then(Value::as_str) == Some("event"))
        .collect()
}

/// All event-node ids in the graph, as owned strings.
fn event_node_ids(noos_dir: &Path) -> Vec<String> {
    event_nodes(noos_dir)
        .into_iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// The last non-blank line of a file (the newest morphe-events.jsonl entry).
fn last_line(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no lines in {}", path.display()))
}

// ---------------------------------------------------------------------------
// morphe-events.jsonl line envelope (kind+subject+sequence+provenance+payload)
// ---------------------------------------------------------------------------

#[test]
fn appends_events_jsonl_line_with_envelope() {
    let dir = temp_noos_dir("case1");
    let event = DesignEvent::Decision {
        path: "bracket.scad".to_string(),
        old: "wall_thickness = 1.5;".to_string(),
        new: "wall_thickness = 2.0;".to_string(),
    };
    write_event(&event, &dir).expect("write_event");

    let line = last_line(&dir.join("morphe-events.jsonl"));
    let v: Value = serde_json::from_str(&line).expect("line parses as JSON");

    assert_eq!(v["kind"], "decision", "kind discriminant");
    assert_eq!(v["subject"], "bracket.scad", "subject = decision path");
    assert_eq!(v["sequence"], 1, "first event for this subject → seq 1");
    assert_eq!(v["provenance"], "ASSERTED", "morphe-emitted → ASSERTED");

    // Payload carries the DesignEvent's variant fields (path/old/new). The
    // `kind` discriminant is hoisted to the top level — not duplicated in the
    // payload.
    assert_eq!(v["payload"]["path"], "bracket.scad");
    assert_eq!(v["payload"]["old"], "wall_thickness = 1.5;");
    assert_eq!(v["payload"]["new"], "wall_thickness = 2.0;");
    assert!(
        v["payload"].as_object().unwrap().get("kind").is_none(),
        "payload must NOT carry `kind` (it's top-level)"
    );
}

// ---------------------------------------------------------------------------
// graph.json event-node shape (id, node_type, file_path, tags)
// ---------------------------------------------------------------------------

#[test]
fn appends_event_node_with_correct_shape() {
    let dir = temp_noos_dir("case2");
    let event = DesignEvent::Decision {
        path: "bracket.scad".to_string(),
        old: "wall_thickness = 1.5;".to_string(),
        new: "wall_thickness = 2.0;".to_string(),
    };
    write_event(&event, &dir).expect("write_event");

    let events = event_nodes(&dir);
    assert_eq!(events.len(), 1, "exactly one event node after one write");
    let node = &events[0];

    assert_eq!(node["node_type"], "event", "node_type");
    assert_eq!(
        node["id"], "event:bracket.scad#1",
        "id = event:<subject>#<seq>"
    );
    assert_eq!(
        node["file_path"], ".noos/morphe-events.jsonl",
        "file_path points at the sidecar (where the payload lives)"
    );
    assert!(
        node["complexity"].is_null(),
        "event nodes have null complexity"
    );

    let tag_strs: Vec<&str> = node["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for required in ["kind:decision", "subject:bracket.scad", "seq:1", "goal:1.5"] {
        assert!(
            tag_strs.contains(&required),
            "tags missing {required:?}; got {tag_strs:?}"
        );
    }

    assert!(
        node.get("name")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "name is a non-empty human label"
    );
    assert!(
        node.get("summary")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "summary is a non-empty human description"
    );
}

// ---------------------------------------------------------------------------
// subject derived per DesignEvent variant
// ---------------------------------------------------------------------------

#[test]
fn subject_is_path_for_decision() {
    let dir = temp_noos_dir("case3_decision");
    write_event(
        &DesignEvent::Decision {
            path: "bracket.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .expect("write");
    let ids = event_node_ids(&dir);
    assert!(
        ids.contains(&"event:bracket.scad#1".to_string()),
        "Decision subject = path; got {ids:?}"
    );
}

#[test]
fn subject_is_name_for_constraint() {
    let dir = temp_noos_dir("case3_constraint");
    write_event(
        &DesignEvent::Constraint {
            name: "wall_thickness".to_string(),
            value: "2mm".to_string(),
        },
        &dir,
    )
    .expect("write");
    let ids = event_node_ids(&dir);
    assert!(
        ids.contains(&"event:wall_thickness#1".to_string()),
        "Constraint subject = name; got {ids:?}"
    );
}

#[test]
fn subject_is_key_for_print_setting() {
    let dir = temp_noos_dir("case3_print_setting");
    write_event(
        &DesignEvent::PrintSetting {
            key: "layer_height".to_string(),
            value: "0.2mm".to_string(),
        },
        &dir,
    )
    .expect("write");
    let ids = event_node_ids(&dir);
    assert!(
        ids.contains(&"event:layer_height#1".to_string()),
        "PrintSetting subject = key; got {ids:?}"
    );
}

#[test]
fn subject_is_derived_and_deterministic_for_intent() {
    // Same statement → same subject (determinism). Two runs in two dirs must
    // produce the same id.
    let stmt = "The bracket must mount to a 2020 extrusion.";
    let dir_a = temp_noos_dir("case3_intent_a");
    let dir_b = temp_noos_dir("case3_intent_b");
    write_event(
        &DesignEvent::Intent {
            statement: stmt.to_string(),
        },
        &dir_a,
    )
    .expect("write a");
    write_event(
        &DesignEvent::Intent {
            statement: stmt.to_string(),
        },
        &dir_b,
    )
    .expect("write b");

    let id_a = event_node_ids(&dir_a)[0].clone();
    let id_b = event_node_ids(&dir_b)[0].clone();
    assert_eq!(id_a, id_b, "same statement → same subject → same id");
    assert!(
        id_a.starts_with("event:") && id_a.ends_with("#1"),
        "id has the event:<subject>#1 shape; got {id_a}"
    );

    // Id-safety: the derived subject must be filesystem/id-safe.
    let subject = &id_a["event:".len()..id_a.len() - 2];
    assert!(!subject.is_empty(), "derived subject is non-empty");
    for c in subject.chars() {
        assert!(
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'),
            "non-id-safe char {c:?} in derived subject {subject:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// written graph passes the REAL `noos validate` binary
// ---------------------------------------------------------------------------

#[test]
fn written_graph_passes_real_noos_validate() {
    let noos_bin = match detect_noos() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: `noos` binary not on PATH (degrade-to-skip)");
            return;
        }
    };

    let dir = temp_noos_dir("case4");
    write_event(
        &DesignEvent::Decision {
            path: "bracket.scad".to_string(),
            old: "wall_thickness = 1.5;".to_string(),
            new: "wall_thickness = 2.0;".to_string(),
        },
        &dir,
    )
    .expect("write_event");

    let graph_path = dir.join("graph.json");
    let output = Command::new(&noos_bin)
        .arg("validate")
        .arg(&graph_path)
        .output()
        .expect("spawn noos validate");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "noos validate failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// sequence uniqueness per subject
// ---------------------------------------------------------------------------

#[test]
fn two_writes_same_subject_get_unique_sequences() {
    let dir = temp_noos_dir("case5");
    let e1 = DesignEvent::Decision {
        path: "x.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    };
    let e2 = DesignEvent::Decision {
        path: "x.scad".to_string(),
        old: "b".to_string(),
        new: "c".to_string(),
    };
    write_event(&e1, &dir).expect("write 1");
    write_event(&e2, &dir).expect("write 2");

    let ids = event_node_ids(&dir);
    assert!(
        ids.contains(&"event:x.scad#1".to_string()),
        "first write → #1; got {ids:?}"
    );
    assert!(
        ids.contains(&"event:x.scad#2".to_string()),
        "second write same subject → #2; got {ids:?}"
    );

    // The sidecar carries sequences 1 and 2 too.
    let text = fs::read_to_string(dir.join("morphe-events.jsonl")).expect("read sidecar");
    let seqs: Vec<u64> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("line parses"))
        .map(|v| v["sequence"].as_u64().expect("sequence is u64"))
        .collect();
    assert_eq!(seqs, vec![1, 2], "sidecar sequences");
}

// ---------------------------------------------------------------------------
// `noos scan` drops event nodes; rehydrate_events restores them
// ---------------------------------------------------------------------------

#[test]
fn noos_scan_drops_event_node_rehydrate_restores_it() {
    let noos_bin = match detect_noos() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: `noos` binary not on PATH (degrade-to-skip)");
            return;
        }
    };

    let repo = temp_repo("case6");
    let noos_dir = repo.join(".noos");

    // Seed a baseline graph + write one event into it.
    seed_minimal_graph(&noos_dir);
    write_event(
        &DesignEvent::Decision {
            path: "src/lib.rs".to_string(),
            old: "old".to_string(),
            new: "new".to_string(),
        },
        &noos_dir,
    )
    .expect("write_event");

    // Pre-scan: exactly one event node, graph validates.
    assert_eq!(
        event_nodes(&noos_dir).len(),
        1,
        "pre-scan: one event node present"
    );
    let pre = Command::new(&noos_bin)
        .arg("validate")
        .arg(noos_dir.join("graph.json"))
        .output()
        .expect("spawn noos validate (pre-scan)");
    assert!(
        pre.status.success(),
        "pre-scan graph must validate\n{}",
        String::from_utf8_lossy(&pre.stderr)
    );

    // Run `noos scan` against the repo — this regenerates graph.json from
    // ingestible files only, DROPPING hand-added event nodes.
    let scan = Command::new(&noos_bin)
        .arg("scan")
        .arg(&repo)
        .output()
        .expect("spawn noos scan");
    assert!(
        scan.status.success(),
        "noos scan failed\n{}",
        String::from_utf8_lossy(&scan.stderr)
    );

    assert_eq!(
        event_nodes(&noos_dir).len(),
        0,
        "post-scan: noos scan drops hand-added event nodes"
    );
    assert!(
        noos_dir.join("morphe-events.jsonl").exists(),
        "post-scan: sidecar must survive (it's the SSOT)"
    );

    // Rehydrate: rebuild event nodes from the sidecar.
    rehydrate_events(&noos_dir).expect("rehydrate_events");

    assert_eq!(
        event_nodes(&noos_dir).len(),
        1,
        "post-rehydrate: event node restored from sidecar"
    );
    let node = &event_nodes(&noos_dir)[0];
    assert_eq!(
        node["id"], "event:src/lib.rs#1",
        "rehydrated node has correct id"
    );

    // Rehydrated graph must still validate.
    let post = Command::new(&noos_bin)
        .arg("validate")
        .arg(noos_dir.join("graph.json"))
        .output()
        .expect("spawn noos validate (post-rehydrate)");
    assert!(
        post.status.success(),
        "post-rehydrate graph must validate\n{}",
        String::from_utf8_lossy(&post.stderr)
    );

    // Idempotency: a second rehydrate must NOT duplicate the node AND must not
    // mutate the existing node's id/tags (proves rehydrate is a true no-op when
    // all nodes are already present — a future bug overwriting tags would fail
    // here).
    let before_second = event_nodes(&noos_dir)
        .into_iter()
        .next()
        .expect("node before 2nd");
    let id_before = before_second["id"].clone();
    let tags_before = before_second["tags"].clone();
    rehydrate_events(&noos_dir).expect("rehydrate_events (idempotent re-run)");
    assert_eq!(
        event_nodes(&noos_dir).len(),
        1,
        "rehydrate is idempotent — no duplicate nodes"
    );
    let after_second = &event_nodes(&noos_dir)[0];
    assert_eq!(
        after_second["id"], id_before,
        "rehydrate must not mutate the existing node's id"
    );
    assert_eq!(
        after_second["tags"], tags_before,
        "rehydrate must leave existing node's tags byte-identical"
    );
}

// ---------------------------------------------------------------------------
// Regression — graph key order preserved through write_event's read→write
// round trip (requires serde_json `preserve_order`; under the default BTreeMap
// backing, a round trip rewrites every key alphabetically → noisy git diff on
// the real git-tracked .noos/graph.json).
// ---------------------------------------------------------------------------

#[test]
fn graph_key_order_preserved_through_round_trip() {
    let dir = temp_noos_dir("key_order");
    // Seed a graph whose top-level keys are deliberately NON-alphabetical.
    // Under BTreeMap, a deserialize → serialize round trip reorders them
    // alphabetically; under `preserve_order` (IndexMap), insertion order
    // survives.
    let seed = "{\n  \"nodes\": [],\n  \"version\": \"0.2.0\",\n  \"project\": { \"name\": \"test\" },\n  \"edges\": [],\n  \"layers\": [],\n  \"tour\": [],\n  \"communities\": [],\n  \"duplicates\": []\n}\n";
    fs::write(dir.join("graph.json"), seed).expect("seed graph");

    // write_event performs a full read → mutate → write round trip on the
    // graph (read_graph → append_event_node → write_graph).
    write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .expect("write_event");

    // Recover the top-level key order from the rewritten file. The parse
    // itself respects preserve_order, so this reflects the file's actual order.
    let raw = fs::read_to_string(dir.join("graph.json")).expect("read rewritten graph");
    let parsed: Value = serde_json::from_str(&raw).expect("rewritten graph parses");
    let top_keys: Vec<&str> = parsed
        .as_object()
        .expect("top-level object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        top_keys,
        [
            "nodes",
            "version",
            "project",
            "edges",
            "layers",
            "tour",
            "communities",
            "duplicates"
        ],
        "graph top-level key order must survive the write_event round trip \
         (preserve_order); got alphabetical reorder instead"
    );
}

// ---------------------------------------------------------------------------
// Regression — rehydrate rebuilds the SAME variant-tuned node name/summary that
// write_event produced (not a generic placeholder). Guards the shared name
// derivation helper so write and rehydrate can never diverge.
// ---------------------------------------------------------------------------

#[test]
fn rehydrate_preserves_variant_tuned_node_name() {
    let dir = temp_noos_dir("rehydrate_name");
    // One event of each variant.
    let events = vec![
        DesignEvent::Decision {
            path: "bracket.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        DesignEvent::Constraint {
            name: "wall_thickness".to_string(),
            value: "2mm".to_string(),
        },
        DesignEvent::PrintSetting {
            key: "layer_height".to_string(),
            value: "0.2mm".to_string(),
        },
        DesignEvent::Intent {
            statement: "mount to 2020 extrusion".to_string(),
        },
    ];
    for e in &events {
        write_event(e, &dir).expect("write");
    }

    // Capture write-time name + summary keyed by node id.
    let write_time: std::collections::HashMap<String, Value> = event_nodes(&dir)
        .into_iter()
        .map(|n| (n["id"].as_str().expect("id").to_string(), n))
        .collect();

    // Simulate `noos scan` dropping hand-added event nodes by clearing the
    // nodes array in place — the sidecar survives (it is the SSOT).
    let graph_path = dir.join("graph.json");
    let mut graph: Value =
        serde_json::from_str(&fs::read_to_string(&graph_path).expect("read graph"))
            .expect("parse graph");
    graph["nodes"] = Value::Array(vec![]);
    fs::write(
        &graph_path,
        serde_json::to_string_pretty(&graph).expect("rewrite"),
    )
    .expect("write emptied graph");
    assert!(
        event_nodes(&dir).is_empty(),
        "simulated scan dropped all event nodes"
    );

    rehydrate_events(&dir).expect("rehydrate");

    let rehydrated = event_nodes(&dir);
    assert_eq!(
        rehydrated.len(),
        events.len(),
        "rehydrate rebuilt one node per sidecar line"
    );
    for node in &rehydrated {
        let id = node["id"].as_str().expect("id");
        let original = write_time
            .get(id)
            .unwrap_or_else(|| panic!("rehydrated unknown id {id}"));
        assert_eq!(
            node["name"], original["name"],
            "rehydrated name drifts from write-time name for {id}"
        );
        assert_eq!(
            node["summary"], original["summary"],
            "rehydrated summary drifts from write-time summary for {id}"
        );
    }
}

// ---------------------------------------------------------------------------
// Regression — fallback graph envelope carries `project.git_commit_hash` for
// structural parity with what `noos scan` emits (so the production fallback
// used before any scan and the test seed fail together if the schema tightens).
// ---------------------------------------------------------------------------

#[test]
fn fallback_graph_envelope_includes_git_commit_hash() {
    let dir = temp_noos_dir("fallback_parity");
    // No pre-existing graph.json → write_event bootstraps the fallback envelope.
    write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .expect("write bootstraps fallback graph");

    let graph = read_graph(&dir);
    assert!(
        graph["project"].get("git_commit_hash").is_some(),
        "fallback project envelope must carry git_commit_hash for parity with \
         `noos scan` output; got project: {}",
        graph["project"]
    );
}

// ---------------------------------------------------------------------------
// Coverage — error paths return WriteError variants, never panic.
// ---------------------------------------------------------------------------

#[test]
fn malformed_graph_json_returns_graph_error_not_panic() {
    let dir = temp_noos_dir("malformed_graph");
    fs::write(dir.join("graph.json"), "{ this is not valid json").expect("seed corrupt graph");
    let err = write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .unwrap_err();
    assert!(
        matches!(err, WriteError::Graph(_)),
        "corrupt graph.json must surface as WriteError::Graph, not panic; got {err:?}"
    );
}

#[test]
fn write_error_surfaces_events_and_graph_variants() {
    let event = DesignEvent::Decision {
        path: "x.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    };

    // Events variant: point noos_dir at a file (not a dir) so create_dir_all
    // fails inside ensure_noos_dir.
    let file_path = std::env::temp_dir().join(format!(
        "morphe-ai-noos-writer-errfile-{}",
        std::process::id()
    ));
    fs::write(&file_path, "I am a file, not a dir").expect("seed blocking file");
    match write_event(&event, &file_path) {
        Err(WriteError::Events(_)) => {}
        other => panic!("expected WriteError::Events, got {other:?}"),
    }
    fs::remove_file(&file_path).ok();

    // Graph variant: corrupt graph.json → read_graph parse failure.
    let dir = temp_noos_dir("write_error_variants");
    fs::write(dir.join("graph.json"), "{{bad").expect("seed corrupt graph");
    match write_event(&event, &dir) {
        Err(WriteError::Graph(_)) => {}
        other => panic!("expected WriteError::Graph, got {other:?}"),
    }
}

#[test]
fn malformed_events_jsonl_line_skipped_good_line_ingested() {
    let dir = temp_noos_dir("malformed_sidecar");
    // Seed: malformed line + well-formed line (subject=x.scad, seq=5) + malformed.
    let good = r#"{"kind":"decision","subject":"x.scad","sequence":5,"provenance":"ASSERTED","payload":{"path":"x.scad","old":"a","new":"b"}}"#;
    let content = format!("this is not json\n{good}\n{{bad json\n");
    fs::write(dir.join("morphe-events.jsonl"), content).expect("seed mixed sidecar");

    // write_event for the same subject → next_sequence skips malformed lines,
    // reads the good line's seq=5, assigns seq=6.
    write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "b".to_string(),
            new: "c".to_string(),
        },
        &dir,
    )
    .expect("write_event skips malformed sidecar lines");

    let ids = event_node_ids(&dir);
    assert!(
        ids.contains(&"event:x.scad#6".to_string()),
        "next_sequence should skip malformed lines and continue from the good \
         line (seq=5 → 6); got {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// write_event / rehydrate_events acquire flock(LOCK_EX) on
// .noos/write.lock around the critical section. Deterministic mutual-exclusion
// probe (replaces the 250ms timing-based test): hold the lock from the test
// thread, try_lock_exclusive on a second handle returns Ok(false) immediately
// while held — no threads, no timing. Graceful WriteError::Lock (not panic)
// when the lockfile can't be opened.
// ---------------------------------------------------------------------------

#[test]
fn write_lock_mutual_exclusion_is_deterministic() {
    // A deterministic probe (no timing-based wait). fs2's try_lock_exclusive
    // uses LOCK_EX | LOCK_NB, which surfaces EWOULDBLOCK as Err(WouldBlock)
    // immediately when the lock is held by another fd — proving mutual
    // exclusion without any timing assumption or background thread.
    use fs2::FileExt;
    use std::io::ErrorKind;

    let dir = temp_noos_dir("case3_mutex_det");
    let lock_path = dir.join("write.lock");

    // Hold the lock from the test thread (the producer side of mutual
    // exclusion).
    let holder = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open holder lockfile");
    holder.lock_exclusive().expect("test thread holds LOCK_EX");

    // Open a SECOND handle to the same lockfile and probe with
    // try_lock_exclusive. While the holder has the lock, the probe must return
    // Err(WouldBlock) — flock with LOCK_NB resolves EWOULDBLOCK into an
    // io::Error rather than blocking.
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open probe handle");
    match probe.try_lock_exclusive() {
        Ok(()) => panic!(
            "while the holder has LOCK_EX, try_lock_exclusive on a second handle must return \
             Err(WouldBlock) (mutual exclusion); got Ok(()) — the lock is not exclusive"
        ),
        Err(e) => assert_eq!(
            e.kind(),
            ErrorKind::WouldBlock,
            "contention must surface as WouldBlock; got {e}"
        ),
    }

    // Release the lock; the probe can now acquire it. This proves the
    // lockfile + flock provide release-on-drop semantics (RAII) — the property
    // write_event's `with_write_lock` relies on for non-deadlock.
    drop(holder);
    probe
        .try_lock_exclusive()
        .expect("after the holder drops, try_lock_exclusive must succeed (lock released)");
}

#[test]
fn write_event_returns_lock_error_when_lockfile_unopenable() {
    let dir = temp_noos_dir("case3_lock_unopenable");
    // Pre-create write.lock as a DIRECTORY → OpenOptions::open fails because
    // a directory cannot be opened for writing. Proves the error surfaces as a
    // WriteError::Lock rather than a panic.
    fs::create_dir(dir.join("write.lock")).expect("seed write.lock as dir");

    let err = write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .unwrap_err();
    assert!(
        matches!(err, WriteError::Lock(_)),
        "lockfile open failure must surface as WriteError::Lock, not panic; got {err:?}"
    );
}

#[test]
fn rehydrate_events_also_takes_the_lock() {
    // Rehydrate modifies graph.json — it must share write_event's lock so the
    // two never race. Verified the same way: hold the lock, rehydrate blocks;
    // release, rehydrate completes.
    use fs2::FileExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let dir = temp_noos_dir("case3_rehydrate_lock");
    // Seed a sidecar line so rehydrate has work to do.
    write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .expect("seed via write_event");

    // Drop all event nodes from the graph to give rehydrate real work.
    let graph_path = dir.join("graph.json");
    let mut graph: Value =
        serde_json::from_str(&fs::read_to_string(&graph_path).expect("read graph"))
            .expect("parse graph");
    graph["nodes"] = Value::Array(vec![]);
    fs::write(&graph_path, serde_json::to_string_pretty(&graph).unwrap()).unwrap();
    assert!(event_nodes(&dir).is_empty());

    let holder = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("write.lock"))
        .expect("open lockfile");
    holder.lock_exclusive().expect("hold lock");

    let done = Arc::new(AtomicBool::new(false));
    let done_clone = Arc::clone(&done);
    let dir_clone = dir.clone();
    let handle = std::thread::spawn(move || {
        rehydrate_events(&dir_clone).expect("rehydrate after release");
        done_clone.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(250) {
        assert!(
            !done.load(Ordering::SeqCst),
            "rehydrate completed while lock held — mutual exclusion broken"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(holder);
    let released = Instant::now();
    while released.elapsed() < Duration::from_secs(2) {
        if done.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(done.load(Ordering::SeqCst), "rehydrate did not complete");
    handle.join().expect("clean");
    assert_eq!(event_nodes(&dir).len(), 1, "rehydrate restored the node");
}

// ---------------------------------------------------------------------------
// A failed write leaves no trace (so a queued retry cannot duplicate it).
// ---------------------------------------------------------------------------

#[test]
fn corrupt_graph_failure_leaves_sidecar_untouched() {
    let dir = temp_noos_dir("fail_no_sidecar");
    fs::write(dir.join("graph.json"), "{ NOT JSON").expect("seed corrupt graph");
    let err = write_event(
        &DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .unwrap_err();
    assert!(matches!(err, WriteError::Graph(_)), "got {err:?}");
    assert!(
        !dir.join("morphe-events.jsonl").exists(),
        "no sidecar line for a failed write"
    );
}

#[test]
fn graph_write_failure_rolls_back_sidecar_append() {
    let dir = temp_noos_dir("fail_rollback");
    let event = DesignEvent::Constraint {
        name: "wall".to_string(),
        value: "2mm".to_string(),
    };
    write_event(&event, &dir).expect("first write");
    let sidecar = dir.join("morphe-events.jsonl");
    let before = fs::read(&sidecar).expect("read sidecar");

    // The graph parses fine, but its atomic rewrite fails: the temp path is a
    // directory, which the writer refuses to replace.
    fs::create_dir(dir.join("graph.json.tmp")).expect("block temp path");
    let err = write_event(&event, &dir).unwrap_err();
    assert!(matches!(err, WriteError::Graph(_)), "got {err:?}");
    assert_eq!(
        fs::read(&sidecar).expect("read sidecar"),
        before,
        "the sidecar append is rolled back when the graph write fails"
    );

    // Once unblocked, the retry lands as sequence 2 (not 3).
    fs::remove_dir(dir.join("graph.json.tmp")).expect("unblock");
    write_event(&event, &dir).expect("retry");
    assert_eq!(event_node_ids(&dir), ["event:wall#1", "event:wall#2"]);
}

/// Seed a minimal valid noos graph envelope into `<noos_dir>/graph.json` for
/// tests that need a pre-existing graph (mirrors the empty-baseline shape
/// `noos scan` would produce against an empty repo).
fn seed_minimal_graph(noos_dir: &Path) {
    let graph = serde_json::json!({
        "version": "0.2.0",
        "project": {
            "name": "test",
            "languages": ["rust"],
            "git_commit_hash": "abc",
            "ingested_at": "2026-07-19T00:00:00Z"
        },
        "nodes": [],
        "edges": [],
        "layers": [],
        "tour": [],
        "communities": [],
        "duplicates": []
    });
    fs::write(
        noos_dir.join("graph.json"),
        serde_json::to_string_pretty(&graph).unwrap(),
    )
    .expect("seed minimal graph");
}

// ---------------------------------------------------------------------------
// Torn final line: a crash mid-append must not swallow the next record.
// ---------------------------------------------------------------------------

const GOOD_LINE: &str = r#"{"kind":"decision","subject":"x.scad","sequence":1,"provenance":"ASSERTED","payload":{"path":"x.scad","old":"a","new":"b"}}"#;

fn decision_b_to_c() -> DesignEvent {
    DesignEvent::Decision {
        path: "x.scad".to_string(),
        old: "b".to_string(),
        new: "c".to_string(),
    }
}

#[test]
fn append_after_torn_line_starts_on_a_new_line() {
    let dir = temp_noos_dir("torn_line");
    let torn = r#"{"kind":"decision","subject":"x.scad","seq"#;
    fs::write(
        dir.join("morphe-events.jsonl"),
        format!("{GOOD_LINE}\n{torn}"),
    )
    .expect("seed torn sidecar");

    write_event(&decision_b_to_c(), &dir).expect("write after torn line");

    let raw = fs::read_to_string(dir.join("morphe-events.jsonl")).expect("read sidecar");
    let raw_lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        raw_lines.len(),
        3,
        "torn line kept on its own line: {raw:?}"
    );
    assert_eq!(raw_lines[1], torn, "torn bytes untouched");
    assert!(raw.ends_with('\n'));

    // Only the torn line is skipped; the new record parses.
    let lines = morphe_ai::read_event_lines(&dir).expect("read lines");
    let seqs: Vec<u64> = lines.iter().map(|l| l.sequence).collect();
    assert_eq!(seqs, [1, 2], "good line + new line survive");
    assert_eq!(lines[1].payload["new"], "c");
}

#[test]
fn torn_line_ending_mid_character_skips_only_that_line() {
    let dir = temp_noos_dir("torn_utf8");
    let mut bytes = format!("{GOOD_LINE}\n").into_bytes();
    // A line cut inside the 3-byte encoding of U+2713.
    bytes.extend_from_slice(b"{\"kind\":\"intent\",\"payload\":{\"statement\":\"ok \xE2\x9C");
    fs::write(dir.join("morphe-events.jsonl"), bytes).expect("seed torn sidecar");

    let lines =
        morphe_ai::read_event_lines(&dir).expect("invalid UTF-8 in a torn line is not fatal");
    assert_eq!(lines.len(), 1);

    write_event(&decision_b_to_c(), &dir).expect("write after torn line");
    let lines = morphe_ai::read_event_lines(&dir).expect("read lines");
    let seqs: Vec<u64> = lines.iter().map(|l| l.sequence).collect();
    assert_eq!(seqs, [1, 2]);
}

#[test]
fn complete_line_missing_newline_is_preserved() {
    let dir = temp_noos_dir("no_trailing_newline");
    fs::write(dir.join("morphe-events.jsonl"), GOOD_LINE).expect("seed sidecar");

    write_event(&decision_b_to_c(), &dir).expect("write");
    let lines = morphe_ai::read_event_lines(&dir).expect("read lines");
    let seqs: Vec<u64> = lines.iter().map(|l| l.sequence).collect();
    assert_eq!(
        seqs,
        [1, 2],
        "the earlier record is not merged into the new one"
    );
}
