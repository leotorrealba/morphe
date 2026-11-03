//! jsonl retry queue integration tests.
//!
//! Exercises the design-event retry queue against a temp `.noos` dir.
//! The queue catches transient [`write_event`] failures (corrupt `graph.json`,
//! I/O) so an emit that fails does not silently lose the event; the next emit
//! or `open_file` drains the backlog when the substrate is healthy again.
//!
//! # Coverage
//!
//! | What                                                                | How |
//! |---------------------------------------------------------------------|-----|
//! | write_event failure → enqueue gains a jsonl line                    | real corrupt graph.json + real write_event + real enqueue_event |
//! | flush drains the queue into sidecar+graph when substrate is healthy | seed queue + healthy .noos → flush |
//! | sequence assigned at flush (queue lines have NO sequence)           | inspect queue lines + flushed sidecar lines |
//! | queue survives a "restart" (drop handle, reload from file)         | enqueue; drop; re-open via flush |
//! | flush is idempotent + removes only successful entries               | interleave healthy/corrupt; assert no double-write |
//! | queue depth is observable (enqueue returns depth, flush returns drained) | assert return values |
//!
//! # Real-wire discipline
//!
//! A test that bypasses the real wire with a synthesized seam can pass while
//! the production emit path is dead. These tests MUST seed a real
//! `write_event` failure (corrupt `graph.json` in a temp `.noos`) and drive the
//! real `enqueue_event` / `flush_queue` / `write_event` — never a synthesized
//! helper that pretends the failure.

use std::fs;
use std::path::{Path, PathBuf};

use morphe_ai::{enqueue_event, flush_queue, write_event, DesignEvent, WriteError};
use serde_json::Value;

// ---------------------------------------------------------------------------
// temp-dir + inspection helpers (match the workspace's `std::env::temp_dir() +
// pid` convention from noos_writer.rs / morphe-cli)
// ---------------------------------------------------------------------------

/// Build a fresh temp `.noos` dir for one test, named by `tag`. Idempotent:
/// wipes any prior dir at the same path so re-runs are deterministic.
fn temp_noos_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "morphe-ai-event-queue-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp .noos dir");
    dir
}

/// Read `<noos_dir>/morphe-queue.jsonl` as a list of trimmed, non-blank lines.
/// Returns an empty Vec when the file does not exist yet (queue is empty).
fn read_queue_lines(noos_dir: &Path) -> Vec<String> {
    let path = noos_dir.join("morphe-queue.jsonl");
    match fs::read_to_string(&path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Read `<noos_dir>/morphe-events.jsonl` (the sidecar) as JSON values.
fn read_sidecar(noos_dir: &Path) -> Vec<Value> {
    let path = noos_dir.join("morphe-events.jsonl");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("sidecar line parses"))
        .collect()
}

/// Count `node_type == "event"` nodes in `<noos_dir>/graph.json`.
fn count_event_nodes(noos_dir: &Path) -> usize {
    let path = noos_dir.join("graph.json");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| n.get("node_type").and_then(Value::as_str) == Some("event"))
                .count()
        })
        .unwrap_or(0)
}

/// Seed `<noos_dir>/graph.json` with bytes that fail to parse as JSON. The
/// real `write_event` reads graph.json, so a corrupt seed makes write_event
/// fail at `read_graph` — a REAL failure (no synthesized seam).
fn seed_corrupt_graph(noos_dir: &Path) {
    fs::write(noos_dir.join("graph.json"), "{ NOT JSON").expect("seed corrupt graph");
}

// ---------------------------------------------------------------------------
// write_event failure (real corrupt graph.json) → enqueue gains a line
// ---------------------------------------------------------------------------

#[test]
fn write_event_failure_enqueues_event_to_jsonl() {
    let dir = temp_noos_dir("case1");
    // REAL failure seed: a graph.json that fails to parse. write_event reads
    // graph.json under the lock at `read_graph`; this corruption makes it
    // return WriteError::Graph before any mutation. This is the real wire —
    // not a synthesized failure injected via a test seam.
    seed_corrupt_graph(&dir);
    let event = DesignEvent::Decision {
        path: "bracket.scad".to_string(),
        old: "wall_thickness = 1.5;".to_string(),
        new: "wall_thickness = 2.0;".to_string(),
    };

    // Drive the REAL write_event — it must fail because the graph is corrupt.
    let err = write_event(&event, &dir).expect_err("real write_event must fail on corrupt graph");
    assert!(
        matches!(err, WriteError::Graph(_)),
        "expected WriteError::Graph, got {err:?}"
    );

    // The emit path catches the failure and enqueues for retry (real call).
    let depth = enqueue_event(&event, &dir).expect("enqueue after write_event failure");

    // Queue gained a line; depth observable.
    assert_eq!(depth, 1, "enqueue returned the new depth (1 line)");
    let lines = read_queue_lines(&dir);
    assert_eq!(lines.len(), 1, "queue holds exactly one line");

    // The line deserializes back to the DesignEvent (no sequence/provenance
    // wrapper — those are assigned at flush by write_event).
    let parsed: DesignEvent =
        serde_json::from_str(&lines[0]).expect("queue line deserializes as a DesignEvent");
    assert_eq!(parsed, event, "round-trips to the enqueued event");
    let as_value: Value = serde_json::from_str(&lines[0]).expect("parses as JSON");
    assert_eq!(
        as_value["kind"], "decision",
        "internally-tagged kind discriminant"
    );
    assert!(
        as_value.get("sequence").is_none(),
        "queue line carries NO sequence (assigned at flush)"
    );
    assert!(
        as_value.get("provenance").is_none(),
        "queue line carries NO provenance wrapper"
    );
}

// ---------------------------------------------------------------------------
// flush drains the queue into sidecar+graph when the substrate is healthy
// ---------------------------------------------------------------------------

#[test]
fn flush_drains_queue_into_sidecar_and_graph_when_healthy() {
    let dir = temp_noos_dir("case2");

    // Seed the queue with three events (real enqueue_event, real writes).
    // The substrate is healthy: no graph.json, no sidecar — write_event will
    // bootstrap both. (Pre-condition: nothing in the sidecar or graph yet.)
    let events = vec![
        DesignEvent::Decision {
            path: "a.scad".to_string(),
            old: "old_a".to_string(),
            new: "new_a".to_string(),
        },
        DesignEvent::Constraint {
            name: "wall_thickness".to_string(),
            value: "2mm".to_string(),
        },
        DesignEvent::PrintSetting {
            key: "layer_height".to_string(),
            value: "0.2mm".to_string(),
        },
    ];
    for ev in &events {
        enqueue_event(ev, &dir).expect("enqueue");
    }
    assert_eq!(read_queue_lines(&dir).len(), 3, "pre: queue holds 3 events");
    assert!(read_sidecar(&dir).is_empty(), "pre: sidecar empty");
    assert_eq!(count_event_nodes(&dir), 0, "pre: no event nodes");

    // Flush — drains via the REAL write_event body (write_event_locked).
    let drained = flush_queue(&dir).expect("flush");

    // All three drained into the sidecar+graph.
    assert_eq!(drained, 3, "flush returns the count drained");
    assert!(
        read_queue_lines(&dir).is_empty(),
        "post-flush: queue empty (all entries removed)"
    );
    assert_eq!(
        read_sidecar(&dir).len(),
        3,
        "post-flush: sidecar gained all 3 events"
    );
    assert_eq!(
        count_event_nodes(&dir),
        3,
        "post-flush: graph gained all 3 event nodes"
    );

    // The drained events went through write_event, so they carry the full
    // envelope (kind + subject + sequence + provenance + payload). Spot-check
    // one (the Decision): sequence 1, kind decision, provenance ASSERTED.
    let side = read_sidecar(&dir);
    let dec = side
        .iter()
        .find(|v| v["kind"] == "decision")
        .expect("decision drained into sidecar");
    assert_eq!(
        dec["sequence"], 1,
        "flush assigned sequence via write_event"
    );
    assert_eq!(dec["provenance"], "ASSERTED");
    assert_eq!(dec["payload"]["new"], "new_a");
}

// ---------------------------------------------------------------------------
// sequence assigned at flush, not enqueue
// ---------------------------------------------------------------------------

#[test]
fn queued_lines_have_no_sequence_flushed_events_get_unique_sequences() {
    let dir = temp_noos_dir("case3");

    // Two Decision events with the SAME subject (same path) → would collide
    // if the queue assigned sequences. The queue assigns NONE; write_event
    // assigns 1, 2 at flush under the lock.
    let e1 = DesignEvent::Decision {
        path: "same.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    };
    let e2 = DesignEvent::Decision {
        path: "same.scad".to_string(),
        old: "b".to_string(),
        new: "c".to_string(),
    };
    enqueue_event(&e1, &dir).expect("enqueue 1");
    enqueue_event(&e2, &dir).expect("enqueue 2");

    // (a) queue lines have NO `sequence` field. The queue line is just
    // the DesignEvent's serde-tagged shape — kind + variant fields.
    for line in read_queue_lines(&dir) {
        let v: Value = serde_json::from_str(&line).expect("queue line parses");
        assert!(
            v.get("sequence").is_none(),
            "queue line must NOT carry a sequence (got {v})"
        );
        assert!(
            v.get("provenance").is_none(),
            "queue line must NOT carry provenance (got {v})"
        );
        assert!(
            v.get("subject").is_none(),
            "queue line must NOT carry subject (got {v})"
        );
    }

    // Flush under the lock — write_event assigns sequences from the sidecar.
    flush_queue(&dir).expect("flush");

    // (b) the two same-subject events now have UNIQUE sequences 1 and 2
    // (no collision — would have collided if the queue pre-assigned).
    let side = read_sidecar(&dir);
    let mut seqs: Vec<u64> = side
        .iter()
        .filter(|v| v["subject"] == "same.scad")
        .map(|v| v["sequence"].as_u64().expect("u64"))
        .collect();
    seqs.sort();
    assert_eq!(
        seqs,
        vec![1, 2],
        "flush assigned contiguous unique sequences"
    );
}

// ---------------------------------------------------------------------------
// queue survives a close/reopen (the jsonl file IS the state)
// ---------------------------------------------------------------------------

#[test]
fn queue_survives_restart_and_flushes_after_reload() {
    let dir = temp_noos_dir("case4");

    // Enqueue three events in "session 1." The queue module keeps NO in-memory
    // state — the file IS the state — so "dropping the handle" is a no-op for
    // us: there is no struct to drop. The test models restart by enqueueing
    // in one call and flushing in another, with no shared in-process state.
    let events = vec![
        DesignEvent::Decision {
            path: "x.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        DesignEvent::Constraint {
            name: "wall".to_string(),
            value: "2mm".to_string(),
        },
        DesignEvent::Intent {
            statement: "the bracket must be rigid".to_string(),
        },
    ];
    for ev in &events {
        enqueue_event(ev, &dir).expect("enqueue");
    }

    // "Restart": prove the queue file alone carries the backlog by reading it
    // directly (no in-memory struct involved). Three lines survive on disk.
    assert_eq!(
        read_queue_lines(&dir).len(),
        3,
        "pre-flush: queue file holds 3 lines across the simulated restart"
    );

    // "Session 2" flush — reads the queue fresh from disk and drains it via
    // write_event_locked. All three recovered + written.
    let drained = flush_queue(&dir).expect("flush after restart");
    assert_eq!(drained, 3, "all pre-restart events recovered + flushed");
    assert!(read_queue_lines(&dir).is_empty(), "queue empty after flush");
    assert_eq!(read_sidecar(&dir).len(), 3, "sidecar has all 3");
    assert_eq!(count_event_nodes(&dir), 3, "graph has all 3 event nodes");
}

// ---------------------------------------------------------------------------
// flush is idempotent + removes only successful entries; no double-write
// ---------------------------------------------------------------------------
//
// Two invariants:
//   (a) Successful entries are REMOVED from the queue → a subsequent flush
//       does NOT re-write them ("no event written twice" — the sidecar count
//       does not grow on a no-op re-flush).
//   (b) Failed entries STAY in the queue → a subsequent flush after the
//       substrate recovers drains the remainder.
//
// A failed write_event_locked leaves neither a sidecar line nor a graph node
// (the graph is validated before the sidecar append, and the append is rolled
// back if the graph write fails), so retrying a queued entry never duplicates
// it. See `repeated_failed_flushes_leave_no_duplicate_sidecar_lines`.

#[test]
fn flush_removes_successful_entries_no_rewrite_on_reflush() {
    let dir = temp_noos_dir("case5");

    // Two events, healthy substrate → flush succeeds on both.
    let e1 = DesignEvent::Decision {
        path: "p.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    };
    let e2 = DesignEvent::Constraint {
        name: "wall".to_string(),
        value: "2mm".to_string(),
    };
    enqueue_event(&e1, &dir).expect("enqueue 1");
    enqueue_event(&e2, &dir).expect("enqueue 2");

    let drained = flush_queue(&dir).expect("flush");
    assert_eq!(drained, 2, "both drained");
    assert!(
        read_queue_lines(&dir).is_empty(),
        "queue empty (entries removed)"
    );
    let sidecar_count_after_flush = read_sidecar(&dir).len();
    assert_eq!(sidecar_count_after_flush, 2);
    assert_eq!(count_event_nodes(&dir), 2);

    // RE-FLUSH: queue is empty → drained == 0, and the sidecar does NOT gain
    // any new lines. This is the "no event written twice" idempotency the
    // queue owns — successful entries were removed, so a re-flush can't
    // re-write them.
    let redrain = flush_queue(&dir).expect("re-flush");
    assert_eq!(redrain, 0, "re-flush drained nothing");
    assert_eq!(
        read_sidecar(&dir).len(),
        sidecar_count_after_flush,
        "sidecar count unchanged on re-flush (no duplicates from idempotent drain)"
    );
    assert_eq!(
        count_event_nodes(&dir),
        2,
        "graph node count unchanged on re-flush"
    );
}

/// Idempotency (b): a flush against a broken substrate leaves the queue intact (failed
/// entries stay for the next flush). The substrate failure here is graph.json
/// corruption — under it, every entry in the flush fails together (the only
/// natural failure mode of write_event_locked is uniform across entries).
#[test]
fn flush_against_broken_substrate_leaves_queue_intact() {
    let dir = temp_noos_dir("case5_fail");

    let e1 = DesignEvent::Decision {
        path: "p.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    };
    let e2 = DesignEvent::Decision {
        path: "p.scad".to_string(),
        old: "b".to_string(),
        new: "c".to_string(),
    };
    enqueue_event(&e1, &dir).expect("enqueue 1");
    enqueue_event(&e2, &dir).expect("enqueue 2");
    let pre_queue = read_queue_lines(&dir);
    assert_eq!(pre_queue.len(), 2);

    // Break the substrate, then flush. Every write_event_locked call fails at
    // read_graph; the loop's per-entry try/collect records both as failed.
    seed_corrupt_graph(&dir);
    let res = flush_queue(&dir);
    assert!(
        res.is_err(),
        "flush against corrupt substrate must error (got {res:?})"
    );

    // FAILED entries STAY in the queue — byte-for-byte the same lines.
    let post_queue = read_queue_lines(&dir);
    assert_eq!(
        post_queue, pre_queue,
        "queue unchanged after full-flush failure (failed entries stay)"
    );

    // Substrate RECOVERED → next flush drains the remainder.
    std::fs::remove_file(dir.join("graph.json")).expect("remove corrupt graph");
    let drained = flush_queue(&dir).expect("flush after recovery");
    assert_eq!(drained, 2, "remainder drained after recovery");
    assert!(
        read_queue_lines(&dir).is_empty(),
        "queue empty after recovery flush"
    );
    // Graph gained exactly 2 event nodes (one per drained entry), and the
    // failed flush left no orphan sidecar lines behind.
    assert_eq!(
        count_event_nodes(&dir),
        2,
        "graph gained the 2 drained events"
    );
    assert_eq!(
        read_sidecar(&dir).len(),
        2,
        "sidecar has exactly the 2 events"
    );
}

/// Regression: with a corrupt graph, every accept fails and re-queues, and
/// every later flush retries the whole backlog. A write that fails must leave
/// no sidecar line, or the backlog would grow ~k^2/2 duplicate lines (and the
/// session digest would repeat decisions). Mirrors the accept path: flush the
/// backlog, then write the new event, enqueueing it on failure.
#[test]
fn repeated_failed_flushes_leave_no_duplicate_sidecar_lines() {
    let dir = temp_noos_dir("no_dup_on_retry");
    seed_corrupt_graph(&dir);

    let events: Vec<DesignEvent> = (0..5)
        .map(|i| DesignEvent::Decision {
            path: "main.scad".to_string(),
            old: format!("v{i}"),
            new: format!("v{}", i + 1),
        })
        .collect();
    for event in &events {
        let _ = flush_queue(&dir);
        if write_event(event, &dir).is_err() {
            enqueue_event(event, &dir).expect("enqueue");
        }
    }
    let _ = flush_queue(&dir);

    assert!(
        read_sidecar(&dir).is_empty(),
        "failed writes must leave no sidecar lines; got {:?}",
        read_sidecar(&dir)
    );
    assert_eq!(read_queue_lines(&dir).len(), 5, "all 5 events queued once");

    // Repair the graph: one flush lands each event exactly once.
    fs::remove_file(dir.join("graph.json")).expect("remove corrupt graph");
    assert_eq!(flush_queue(&dir).expect("flush after repair"), 5);
    let side = read_sidecar(&dir);
    assert_eq!(side.len(), 5, "each event recorded exactly once");
    let news: Vec<&str> = side
        .iter()
        .map(|l| l["payload"]["new"].as_str().expect("new"))
        .collect();
    assert_eq!(
        news,
        ["v1", "v2", "v3", "v4", "v5"],
        "queue order preserved"
    );
    let seqs: Vec<u64> = side
        .iter()
        .map(|l| l["sequence"].as_u64().expect("seq"))
        .collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    assert_eq!(count_event_nodes(&dir), 5);
    assert!(read_queue_lines(&dir).is_empty());
}

/// Within-flush partial-success: a queue line that fails to parse as a
/// DesignEvent is skipped (forward-compat) — it does NOT abort the flush, and
/// well-formed peers drain normally. This exercises the per-entry skip path in
/// `read_queue_entries` (the "leave the entry for the next flush" semantics
/// for unparseable lines, distinct from write_event failures).
#[test]
fn malformed_queue_line_skipped_well_formed_lines_drain() {
    let dir = temp_noos_dir("case5_malformed");

    // Hand-craft a queue file with: a malformed line, a well-formed Decision,
    // another malformed line, a well-formed Constraint. (Real enqueue_event
    // only ever writes well-formed lines, but external tampering or a partial
    // write could leave malformed lines behind — the consumer must not crash.)
    let queue_path = dir.join("morphe-queue.jsonl");
    let good_decision = serde_json::to_string(&DesignEvent::Decision {
        path: "ok.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    })
    .unwrap();
    let good_constraint = serde_json::to_string(&DesignEvent::Constraint {
        name: "wall".to_string(),
        value: "1mm".to_string(),
    })
    .unwrap();
    std::fs::write(
        &queue_path,
        format!("{{ NOT JSON\n{good_decision}\n{{ also broken\n{good_constraint}\n",),
    )
    .expect("seed mixed queue");

    // Flush — does not panic; the two well-formed lines drain, the two
    // malformed lines are dropped (skipped, not enqueued for retry — there is
    // no recoverable event to keep).
    let drained = flush_queue(&dir).expect("flush mixed queue");
    assert_eq!(drained, 2, "only the two well-formed lines drain");
    assert!(
        read_queue_lines(&dir).is_empty(),
        "queue empty post-flush (malformed lines dropped, well-formed drained)"
    );
    assert_eq!(
        read_sidecar(&dir).len(),
        2,
        "sidecar gained the 2 well-formed"
    );
}

// ---------------------------------------------------------------------------
// queue depth is observable
// ---------------------------------------------------------------------------

#[test]
fn enqueue_returns_monotonic_depth_and_flush_returns_drained_count() {
    let dir = temp_noos_dir("case6");

    // Enqueue returns the new depth — monotonically increasing as the queue
    // grows. This is the observable count the emit path logs when depth > 0
    // (sustained failure must not be fully silent).
    let d1 = enqueue_event(
        &DesignEvent::Decision {
            path: "p.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &dir,
    )
    .expect("enqueue 1");
    assert_eq!(d1, 1, "first enqueue → depth 1");
    let d2 = enqueue_event(
        &DesignEvent::Decision {
            path: "p.scad".to_string(),
            old: "b".to_string(),
            new: "c".to_string(),
        },
        &dir,
    )
    .expect("enqueue 2");
    assert_eq!(d2, 2, "second enqueue → depth 2");
    let d3 = enqueue_event(
        &DesignEvent::Constraint {
            name: "wall".to_string(),
            value: "2mm".to_string(),
        },
        &dir,
    )
    .expect("enqueue 3");
    assert_eq!(d3, 3, "third enqueue → depth 3");

    // Flush returns the count drained — the observable count of how many
    // entries left the queue this call (someone watching logs sees
    // whether the backlog shrank).
    let drained = flush_queue(&dir).expect("flush");
    assert_eq!(
        drained, 3,
        "flush drained count == queue depth before flush"
    );

    // A second flush on an empty queue returns 0 (depth-observable property
    // holds on both the empty and non-empty sides).
    let redrain = flush_queue(&dir).expect("re-flush");
    assert_eq!(redrain, 0, "empty-queue flush returns 0");
}

/// A queue whose final line was torn by a crash mid-append: the next enqueue
/// starts on a fresh line, so only the torn entry is lost.
#[test]
fn enqueue_after_torn_queue_line_is_not_swallowed() {
    let dir = temp_noos_dir("torn_queue");
    fs::write(dir.join("morphe-queue.jsonl"), r#"{"kind":"decision","pa"#)
        .expect("seed torn queue");

    let event = DesignEvent::Constraint {
        name: "wall".to_string(),
        value: "2mm".to_string(),
    };
    enqueue_event(&event, &dir).expect("enqueue");
    assert_eq!(flush_queue(&dir).expect("flush"), 1, "the new entry drains");
    let side = read_sidecar(&dir);
    assert_eq!(side.len(), 1);
    assert_eq!(side[0]["payload"]["name"], "wall");
}
