//! Memory-panel IPC tests: retire emits an event, and memory_list returns
//! structured events from the sidecar.
//!
//! Exercises the real `memory_list_inner` / `retire_constraint_inner` seams with
//! a seeded temp project (an opened `.scad` file whose parent carries a `.noos`
//! sidecar). No `noos` subprocess — reads morphe's own sidecar directly.

use std::fs;

use morphe_desktop_lib::morphe_ai::{read_event_lines, write_event, DesignEvent};
use morphe_desktop_lib::{memory_list_inner, retire_constraint_inner, AppState};

/// Build a fresh temp PROJECT (parent of `.noos/`) for one test, with a `.scad`
/// file the AppState can "open". Idempotent: wipes any prior dir at the path.
fn temp_project(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "morphe-desktop-memory-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".noos")).expect("create temp .noos dir");
    dir
}

// ---------------------------------------------------------------------------
// memory_list returns structured events from the sidecar
// ---------------------------------------------------------------------------

#[test]
fn memory_list_returns_events_from_sidecar() {
    let project = temp_project("list");
    let scad = project.join("bracket.scad");
    fs::write(&scad, "cube(1);").expect("seed scad");
    let noos_dir = project.join(".noos");

    write_event(
        &DesignEvent::Constraint {
            name: "wall_thickness".to_string(),
            value: "2mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed constraint");
    write_event(
        &DesignEvent::Intent {
            statement: "a 4-hole bracket".to_string(),
        },
        &noos_dir,
    )
    .expect("seed intent");

    let state = AppState::new_in_memory();
    state.set_opened_path(Some(scad.to_string_lossy().into_owned()));

    let events = memory_list_inner(&state).expect("memory_list Ok");
    // The panel's data source returns typed events from the sidecar.
    let has_constraint = events.iter().any(|e| {
        matches!(
            e,
            DesignEvent::Constraint { name, value }
                if name == "wall_thickness" && value == "2mm"
        )
    });
    assert!(
        has_constraint,
        "constraint must be returned; got {events:?}"
    );
    let has_intent = events.iter().any(|e| {
        matches!(
            e,
            DesignEvent::Intent { statement } if statement == "a 4-hole bracket"
        )
    });
    assert!(has_intent, "intent must be returned; got {events:?}");
}

#[test]
fn memory_list_no_file_open_returns_empty() {
    let state = AppState::new_in_memory();
    // opened_path None by default → no noos_dir → graceful empty, not error.
    let events = memory_list_inner(&state).expect("Ok (empty)");
    assert!(
        events.is_empty(),
        "no file open → empty list, not error/panic"
    );
}

// ---------------------------------------------------------------------------
// retire_constraint emits a ConstraintRetired event into the sidecar
// ---------------------------------------------------------------------------

#[test]
fn retire_constraint_emits_event_into_sidecar() {
    let project = temp_project("retire");
    let scad = project.join("part.scad");
    fs::write(&scad, "cube(1);").expect("seed scad");
    let noos_dir = project.join(".noos");

    let state = AppState::new_in_memory();
    state.set_opened_path(Some(scad.to_string_lossy().into_owned()));

    retire_constraint_inner(&state, "wall_thickness").expect("retire Ok");

    // The ConstraintRetired event must land in the sidecar (via the shared
    // write path — flock, sidecar+graph).
    let lines = read_event_lines(&noos_dir).expect("read sidecar");
    let retired = lines.iter().any(|l| {
        l.kind == "constraint_retired"
            && l.payload.get("name").and_then(|v| v.as_str()) == Some("wall_thickness")
    });
    assert!(
        retired,
        "a constraint_retired event for wall_thickness must be in the sidecar; kinds = {:?}",
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>()
    );

    // And memory_list surfaces it as a typed DesignEvent::ConstraintRetired.
    let events = memory_list_inner(&state).expect("memory_list Ok");
    let has_retired = events.iter().any(|e| {
        matches!(
            e,
            DesignEvent::ConstraintRetired { name } if name == "wall_thickness"
        )
    });
    assert!(
        has_retired,
        "memory_list must return the ConstraintRetired event"
    );
}

#[test]
fn retire_constraint_no_file_open_errors() {
    // No file open → retire can't derive a noos_dir → graceful Err (surfaced
    // to the panel as an error cue), not a panic.
    let state = AppState::new_in_memory();
    let err = retire_constraint_inner(&state, "anything").unwrap_err();
    assert!(
        err.contains("no project file open"),
        "useful error; got: {err}"
    );
}
