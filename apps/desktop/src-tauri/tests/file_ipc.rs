//! Integration tests for the `open_file` / `save_file` IPC pure logic.
//!
//! The native file dialog is behind an injectable `FileDialogPicker` seam; these
//! tests inject a **fake** picker returning canned paths, so no OS dialog is
//! shown. The real `tauri-plugin-dialog` wiring is correct-by-construction but cannot be driven headless.

use std::fs;
use std::path::PathBuf;

use morphe_desktop_lib::{
    open_file, open_file_with_state, save_file, save_file_with_state, save_or_save_as_with_state,
    AppState, FileDialogPicker, FileError,
};

/// A fake picker that returns canned open/save paths (or `None` to simulate the
/// user cancelling). Implements the real seam, so the pure logic can't tell it
/// apart from the Tauri-backed picker.
struct FakePicker {
    open_path: Option<PathBuf>,
    save_path: Option<PathBuf>,
}

impl FileDialogPicker for FakePicker {
    fn pick_open(&self) -> Option<PathBuf> {
        self.open_path.clone()
    }
    fn pick_save(&self) -> Option<PathBuf> {
        self.save_path.clone()
    }
    fn pick_save_stl(&self) -> Option<PathBuf> {
        None
    }
}

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("morphe-file-ipc-{}-{name}", std::process::id()));
    fs::create_dir_all(&p).expect("create temp dir");
    p
}

/// `open_file` picks a path, reads it, and returns `{ path, source }`.
#[test]
fn open_file_reads_picked_file() {
    let dir = temp_path("open_ok");
    let file = dir.join("part.scad");
    let source = "cube([1, 1, 1]);";
    fs::write(&file, source).expect("seed the picked file");

    let picker = FakePicker {
        open_path: Some(file.clone()),
        save_path: None,
    };
    let result = open_file(&picker).expect("open_file reads the picked file");

    assert_eq!(result.source, source, "source must be the file's contents");
    assert!(
        result.path.ends_with("part.scad"),
        "path must be the picked path, got {}",
        result.path
    );
}

/// `open_file` rejects a non-.scad pick at the source. The dialog's filter is
/// only a hint on macOS (the user can pick "All Files"); without this guard,
/// reading a binary / non-.scad file crashes (OOM on a large binary
/// read_to_string, or a downstream parse of non-scad content).
#[test]
fn open_file_rejects_non_scad() {
    let dir = temp_path("open_nonscad");
    let file = dir.join("not-a-scad.txt");
    fs::write(&file, "hello").expect("seed a non-.scad file");

    let picker = FakePicker {
        open_path: Some(file),
        save_path: None,
    };
    let err = open_file(&picker).expect_err("non-.scad must be rejected before read");
    match err {
        FileError::Io { message } => assert!(
            message.contains("not a .scad file"),
            "clear rejection message; got: {message}"
        ),
        other => panic!("expected FileError::Io for non-.scad, got {other:?}"),
    }
}

/// `check_size_cap` rejects an oversized file before `read_to_string`: a `.scad` symlink to a binary or a huge `.scad` would OOM
/// `read_to_string` (full read-then-validate). Inject a tiny cap so the
/// rejection is exercisable in a unit test (the production `MAX_SCAD_BYTES` is
/// too large to exercise here).
#[test]
fn check_size_cap_rejects_oversized_and_accepts_within() {
    let dir = temp_path("sizecap");
    let file = dir.join("big.scad");
    fs::write(&file, "12345").expect("seed a 5-byte .scad");

    let err = morphe_desktop_lib::check_size_cap(&file, 4).expect_err("over-cap must reject");
    match err {
        FileError::Io { message } => assert!(
            message.contains("too large"),
            "oversize rejection message; got: {message}"
        ),
        other => panic!("expected FileError::Io for over-cap, got {other:?}"),
    }
    morphe_desktop_lib::check_size_cap(&file, 5).expect("at-cap is accepted");
    morphe_desktop_lib::check_size_cap(&file, 100).expect("under-cap is accepted");
}

/// `save_file` picks a path and writes `source` to it.
#[test]
fn save_file_writes_to_picked_path() {
    let dir = temp_path("save_ok");
    let file = dir.join("out.scad");
    let source = "cube([2, 2, 2]);";

    let picker = FakePicker {
        open_path: None,
        save_path: Some(file.clone()),
    };
    let result = save_file(&picker, source).expect("save_file writes the picked path");

    assert!(
        result.path.ends_with("out.scad"),
        "path must be the picked path, got {}",
        result.path
    );
    let read_back = fs::read_to_string(&file).expect("file must be written");
    assert_eq!(
        read_back, source,
        "file contents must match the saved source"
    );
}

/// Cancelling the open dialog surfaces as a `cancelled` error — no file is read.
#[test]
fn open_file_cancelled() {
    let picker = FakePicker {
        open_path: None,
        save_path: None,
    };
    let err = open_file(&picker).expect_err("a cancelled open must error");
    assert!(
        matches!(err, FileError::Cancelled),
        "cancel must surface as Cancelled, got {err:?}"
    );
}

/// Cancelling the save dialog surfaces as a `cancelled` error — no file is
/// written.
#[test]
fn save_file_cancelled() {
    let picker = FakePicker {
        open_path: None,
        save_path: None,
    };
    let err = save_file(&picker, "x").expect_err("a cancelled save must error");
    assert!(
        matches!(err, FileError::Cancelled),
        "cancel must surface as Cancelled, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// open_file rehydrates dropped event nodes from the project's .noos sidecar
// (rehydrate-on-open, since morphe never scans in-app). Best-effort: rehydrate failure must NOT fail the open.
// ---------------------------------------------------------------------------

/// Count `node_type == "event"` nodes in `<project>/.noos/graph.json`.
fn count_event_nodes(project: &std::path::Path) -> usize {
    let graph_path = project.join(".noos").join("graph.json");
    let text = match fs::read_to_string(&graph_path) {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| n.get("node_type").and_then(serde_json::Value::as_str) == Some("event"))
                .count()
        })
        .unwrap_or(0)
}

/// `open_file` rehydrates event nodes an external `noos scan` dropped
/// since the last morphe run. Seeds a sidecar line + an emptied graph, opens the
/// project's `.scad`, and asserts the graph was rebuilt from the sidecar.
#[test]
fn open_file_rehydrates_dropped_event_nodes() {
    let dir = temp_path("open_rehydrate");
    let scad = dir.join("part.scad");
    fs::write(&scad, "cube([1,1,1]);").expect("seed scad");

    // Seed the project's .noos with one Decision event, then drop the event
    // node from the graph (simulating `noos scan`). The sidecar survives.
    let noos_dir = dir.join(".noos");
    fs::create_dir_all(&noos_dir).expect("mkdir .noos");
    morphe_desktop_lib::morphe_ai::write_event(
        &morphe_desktop_lib::morphe_ai::DesignEvent::Decision {
            path: "part.scad".to_string(),
            old: "cube([0]);".to_string(),
            new: "cube([1]);".to_string(),
        },
        &noos_dir,
    )
    .expect("seed event via write_event");
    assert_eq!(
        count_event_nodes(&dir),
        1,
        "pre: one event node present after write_event"
    );

    // Simulate `noos scan` dropping hand-added event nodes by emptying the
    // graph's nodes array (the sidecar survives — it's the SSOT).
    let graph_path = noos_dir.join("graph.json");
    let mut graph: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&graph_path).expect("read graph"))
            .expect("parse graph");
    graph["nodes"] = serde_json::Value::Array(vec![]);
    fs::write(&graph_path, serde_json::to_string_pretty(&graph).unwrap()).expect("empty the graph");
    assert_eq!(count_event_nodes(&dir), 0, "post-scan: event nodes dropped");

    // open_file reads the .scad AND rehydrates the graph from the sidecar.
    let picker = FakePicker {
        open_path: Some(scad.clone()),
        save_path: None,
    };
    let result = open_file(&picker).expect("open_file succeeds — source read is the primary");

    assert_eq!(result.source, "cube([1,1,1]);", "source read returned");
    assert_eq!(
        count_event_nodes(&dir),
        1,
        "post-open: event node rehydrated from the sidecar"
    );
}

/// `open_file` skips the rehydrate cleanly when the project has no `.noos/`
/// (no morphe events ever recorded there). The open must succeed — rehydrate
/// is best-effort.
#[test]
fn open_file_skips_rehydrate_when_no_noos_dir() {
    let dir = temp_path("open_no_noos");
    let scad = dir.join("part.scad");
    fs::write(&scad, "cube([2,2,2]);").expect("seed scad");
    // Intentionally NO .noos dir under `dir`.
    assert!(!dir.join(".noos").exists(), "precondition: no .noos");

    let picker = FakePicker {
        open_path: Some(scad.clone()),
        save_path: None,
    };
    let result = open_file(&picker).expect("open succeeds without a .noos");
    assert_eq!(result.source, "cube([2,2,2]);");
    // No .noos was created as a side effect (rehydrate is a no-op without one).
    assert!(
        !dir.join(".noos").exists(),
        "open_file must not create a .noos just by skipping rehydrate"
    );
}

/// `open_file` still succeeds when rehydrate itself fails (e.g. corrupt graph).
/// The source read is the primary result; rehydrate is best-effort.
#[test]
fn open_file_succeeds_when_rehydrate_errors() {
    let dir = temp_path("open_rehydrate_err");
    let scad = dir.join("part.scad");
    fs::write(&scad, "cube([3,3,3]);").expect("seed scad");

    let noos_dir = dir.join(".noos");
    fs::create_dir_all(&noos_dir).expect("mkdir .noos");
    // Corrupt graph.json + a sidecar line → rehydrate will error reading the
    // graph; open_file must still succeed.
    fs::write(
        noos_dir.join("morphe-events.jsonl"),
        "{\"kind\":\"decision\"}\n",
    )
    .expect("seed sidecar");
    fs::write(noos_dir.join("graph.json"), "{ NOT VALID JSON").expect("seed corrupt graph");

    let picker = FakePicker {
        open_path: Some(scad.clone()),
        save_path: None,
    };
    let result = open_file(&picker).expect("open succeeds despite rehydrate failure");
    assert_eq!(result.source, "cube([3,3,3]);");
}

// ---------------------------------------------------------------------------
// open_file_with_state records the opened disk path in AppState so
// ai_accept_edit can derive the per-project .noos/ dir for memory writes. Pure
// seam over open_file_cmd, driven by a fake picker + an AppState (no Tauri
// runtime). Proves open_file_cmd sets state.opened_path on a successful open —
// the agent loop always emits edit_code with path = WORKSPACE_PATH (relative),
// so deriving noos_dir from THAT path would always yield None and skip.
// ---------------------------------------------------------------------------

/// `open_file_with_state` records the opened disk path in AppState, so
/// `ai_accept_edit` can derive the per-project `.noos/` dir for memory writes.
/// Without this wiring the production emit path would never fire.
#[test]
fn open_file_with_state_records_opened_path_in_appstate() {
    let dir = temp_path("open_state");
    let file = dir.join("part.scad");
    fs::write(&file, "cube([1,1,1]);").expect("seed the picked file");

    let picker = FakePicker {
        open_path: Some(file.clone()),
        save_path: None,
    };
    let state = AppState::new_in_memory();
    assert_eq!(
        state.opened_path(),
        None,
        "pre: no opened path (in-memory editor)"
    );

    let result = open_file_with_state(&picker, &state).expect("open succeeds");
    assert!(
        result.path.ends_with("part.scad"),
        "returned path is the picked file"
    );
    assert_eq!(
        state.opened_path(),
        Some(result.path.clone()),
        "post: AppState.opened_path is set to the picked disk path (absolute)"
    );
    // The recorded path is absolute (the OS dialog invariant the noos-dir
    // derivation relies on). On unix/macos a temp_path is under /var or /tmp;
    // on windows under C:\...\Temp. Either way: is_absolute() must hold.
    assert!(
        PathBuf::from(state.opened_path().unwrap()).is_absolute(),
        "opened_path must be absolute (OS dialog invariant)"
    );
}

/// `open_file_with_state` does NOT mutate AppState when the user cancels —
/// `opened_path` stays whatever it was (no half-state). The `Cancelled` error
/// surfaces as-is; the open is the atomic unit of state change.
#[test]
fn open_file_with_state_leaves_state_untouched_on_cancel() {
    let state = AppState::new_in_memory();
    state.set_opened_path(Some("/prior/opened.scad".to_string()));
    let picker = FakePicker {
        open_path: None,
        save_path: None,
    };
    let err = open_file_with_state(&picker, &state).expect_err("cancel → error");
    assert!(
        matches!(err, FileError::Cancelled),
        "cancel surfaces as Cancelled, got {err:?}"
    );
    assert_eq!(
        state.opened_path(),
        Some("/prior/opened.scad".to_string()),
        "prior opened_path preserved — cancel does not half-clear state"
    );
}

// ---------------------------------------------------------------------------
// save_file_with_state records the saved path as the
// opened path, so a Save-As retargets the compile source-dir context (the same
// bookkeeping open_file_with_state does). Without it, saving to a new dir
// leaves import()/surface() resolution pointed at the OLD directory.
// ---------------------------------------------------------------------------

/// `save_file_with_state` records the saved path in AppState — a Save-As is
/// the same "this buffer lives here now" statement as open.
#[test]
fn save_file_with_state_records_saved_path_in_appstate() {
    let dir = temp_path("save_state");
    fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("saved.scad");

    let picker = FakePicker {
        open_path: None,
        save_path: Some(file.clone()),
    };
    let state = AppState::new_in_memory();
    assert_eq!(state.opened_path(), None);

    let result = save_file_with_state(&picker, &state, "cube(1);").expect("save succeeds");
    assert!(result.path.ends_with("saved.scad"));
    assert_eq!(
        state.opened_path(),
        Some(result.path.clone()),
        "post: AppState.opened_path follows the save target"
    );
    assert!(
        PathBuf::from(state.opened_path().unwrap()).is_absolute(),
        "opened_path must be absolute"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `save_file_with_state` does NOT mutate AppState when the user cancels.
#[test]
fn save_file_with_state_leaves_state_untouched_on_cancel() {
    let state = AppState::new_in_memory();
    state.set_opened_path(Some("/prior/opened.scad".to_string()));
    let picker = FakePicker {
        open_path: None,
        save_path: None,
    };
    let err = save_file_with_state(&picker, &state, "cube(1);").expect_err("cancel → error");
    assert!(matches!(err, FileError::Cancelled));
    assert_eq!(
        state.opened_path(),
        Some("/prior/opened.scad".to_string()),
        "prior opened_path preserved"
    );
}

// ---------------------------------------------------------------------------
// open_file flushes the retry-queue backlog from a prior
// session/crash. Best-effort: flush failure must NOT fail the open. This is
// the "queue survives a restart" recovery path: queued events from yesterday
// land in the substrate when the user reopens the project today.
// ---------------------------------------------------------------------------

/// Read `<project>/.noos/morphe-queue.jsonl` as a list of trimmed non-blank
/// lines. Returns an empty Vec when the file does not exist.
fn read_queue_lines(project: &std::path::Path) -> Vec<String> {
    let path = project.join(".noos").join("morphe-queue.jsonl");
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

/// `open_file` drains a backlog left by a prior session into the substrate.
/// The queue file IS the state — a process restart drops no in-memory struct
/// (there is none), so reopening the project discovers the queue file on disk
/// and flushes it via the emit path's `flush_queue` call. This is the
/// restart-recovery path at the integration boundary.
#[test]
fn open_file_drains_queue_backlog_from_prior_session() {
    let dir = temp_path("open_flush_backlog");
    let scad = dir.join("part.scad");
    fs::write(&scad, "cube([1,1,1]);").expect("seed scad");

    // Simulate a prior session's failed emit: enqueue two events directly into
    // the project's `.noos/morphe-queue.jsonl`. No in-memory state carries over
    // — the file IS the source of truth for the backlog.
    let noos_dir = dir.join(".noos");
    fs::create_dir_all(&noos_dir).expect("mkdir .noos");
    morphe_desktop_lib::morphe_ai::enqueue_event(
        &morphe_desktop_lib::morphe_ai::DesignEvent::Decision {
            path: "part.scad".to_string(),
            old: "cube([0]);".to_string(),
            new: "cube([1]);".to_string(),
        },
        &noos_dir,
    )
    .expect("enqueue 1");
    morphe_desktop_lib::morphe_ai::enqueue_event(
        &morphe_desktop_lib::morphe_ai::DesignEvent::Constraint {
            name: "wall".to_string(),
            value: "2mm".to_string(),
        },
        &noos_dir,
    )
    .expect("enqueue 2");
    assert_eq!(
        read_queue_lines(&dir).len(),
        2,
        "pre-open: backlog of 2 from prior session"
    );
    assert_eq!(
        count_event_nodes(&dir),
        0,
        "pre-open: no event nodes in graph"
    );

    // open_file reads the .scad AND drains the backlog (after rehydrate).
    let picker = FakePicker {
        open_path: Some(scad.clone()),
        save_path: None,
    };
    let result = open_file(&picker).expect("open succeeds — source read is the primary");

    assert_eq!(result.source, "cube([1,1,1]);", "source read returned");
    assert!(
        read_queue_lines(&dir).is_empty(),
        "post-open: backlog drained by flush_queue"
    );
    assert_eq!(
        count_event_nodes(&dir),
        2,
        "post-open: graph gained both drained events"
    );
}

/// `open_file` still succeeds when the flush itself fails (e.g. corrupt graph
/// that rehydrate also can't fix). The source read is the primary result;
/// flush is best-effort.
#[test]
fn open_file_succeeds_when_flush_errors() {
    let dir = temp_path("open_flush_err");
    let scad = dir.join("part.scad");
    fs::write(&scad, "cube([3,3,3]);").expect("seed scad");

    let noos_dir = dir.join(".noos");
    fs::create_dir_all(&noos_dir).expect("mkdir .noos");
    // Seed a backlog AND a corrupt graph. rehydrate fails on the corrupt
    // graph; flush_queue also fails (write_event_locked hits read_graph).
    // Both are best-effort — the open must still succeed.
    morphe_desktop_lib::morphe_ai::enqueue_event(
        &morphe_desktop_lib::morphe_ai::DesignEvent::Decision {
            path: "part.scad".to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &noos_dir,
    )
    .expect("enqueue");
    fs::write(noos_dir.join("graph.json"), "{ NOT VALID JSON").expect("seed corrupt graph");

    let picker = FakePicker {
        open_path: Some(scad.clone()),
        save_path: None,
    };
    let result = open_file(&picker).expect("open succeeds despite flush failure");
    assert_eq!(result.source, "cube([3,3,3]);");
    // The queue is intact (flush failed) — the backlog survives for the next
    // attempt once the substrate is repaired.
    assert_eq!(
        read_queue_lines(&dir).len(),
        1,
        "queue intact: flush failure leaves backlog for next attempt"
    );
}

/// Plain Save with a file open writes straight back to the opened path — no
/// dialog (the picker would cancel if asked).
#[test]
fn plain_save_writes_back_to_opened_path_without_dialog() {
    let dir = temp_path("plain_save");
    let file = dir.join("part.scad");
    fs::write(&file, "cube(1);").expect("seed");
    let state = AppState::new_in_memory();
    state.set_opened_path(Some(file.display().to_string()));
    let picker = FakePicker {
        open_path: None,
        save_path: None, // a dialog would surface as Cancelled
    };
    let result =
        save_or_save_as_with_state(&picker, &state, "cube(2);").expect("plain save succeeds");
    assert_eq!(result.path, file.display().to_string());
    assert_eq!(fs::read_to_string(&file).expect("read back"), "cube(2);");
}

/// Plain Save with no file open falls back to the Save-As dialog and the
/// saved path becomes the opened path.
#[test]
fn plain_save_without_open_file_falls_back_to_dialog() {
    let dir = temp_path("plain_save_fallback");
    let file = dir.join("new.scad");
    let state = AppState::new_in_memory();
    let picker = FakePicker {
        open_path: None,
        save_path: Some(file.clone()),
    };
    save_or_save_as_with_state(&picker, &state, "sphere(1);").expect("save-as fallback");
    assert_eq!(fs::read_to_string(&file).expect("read back"), "sphere(1);");
    assert_eq!(state.opened_path(), Some(file.display().to_string()));
}

/// A bare name typed into the save dialog gets the `.scad` extension; a
/// non-source extension is refused so a Save can never clobber another file.
#[test]
fn save_dialog_adds_scad_extension_and_refuses_other_extensions() {
    let dir = temp_path("save_ext");
    let bare = dir.join("bracket");
    let picker = FakePicker {
        open_path: None,
        save_path: Some(bare.clone()),
    };
    let result = save_file(&picker, "cube(1);").expect("bare name saves as .scad");
    assert!(result.path.ends_with("bracket.scad"), "got {}", result.path);
    assert!(dir.join("bracket.scad").exists());

    let other = dir.join("notes.txt");
    let picker = FakePicker {
        open_path: None,
        save_path: Some(other.clone()),
    };
    let err = save_file(&picker, "cube(1);").expect_err("non-.scad destination refused");
    assert!(matches!(err, FileError::Io { .. }));
    assert!(!other.exists(), "nothing written");
}
