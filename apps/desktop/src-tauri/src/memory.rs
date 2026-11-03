//! Memory-panel IPC commands.
//!
//! The USER-FACING read/retire surface over the `.noos` substrate — complements
//! the AI's explicit `query_noos` tool and the automatic session digest
//! (prompt-injected). `memory_list` returns the project's typed design events to
//! the panel (read from the morphe-owned sidecar directly — no `noos`
//! subprocess); `retire_constraint` writes a `ConstraintRetired` tombstone via
//! the shared event write path. Both degrade gracefully when no file is open.

use morphe_ai::{event_from_line, read_event_lines, write_event, DesignEvent};
use tauri::Manager;

use crate::ai::{noos_dir_for_path, AppState};

/// Cap on events returned to the panel. Bounds the DOM
/// render cost so a pathological sidecar (hand-edited to 100k lines, or a very
/// long-running project) can't freeze the panel. Keeps the most recent
/// (file/append order = chronological).
pub const MEMORY_LIST_MAX: usize = 500;

/// Fetch the project's design events for the memory panel.
///
/// Reads the morphe-owned sidecar (`morphe-events.jsonl`) directly via
/// [`read_event_lines`], reconstructs typed [`DesignEvent`]s via
/// [`event_from_line`] (so the wire-shape snapshot test pins the exact shape that
/// crosses IPC), and returns them. Empty when no file is open or there is no
/// sidecar. Reads the SIDECAR, not the graph node (graph nodes carry identity
/// only, not the event payload).
// Inner (takes `&AppState`, no Tauri `State`) so tests call it directly without
// a Tauri runtime — the same pattern ai.rs uses for ai_accept_edit_inner etc.
pub fn memory_list_inner(state: &AppState) -> Result<Vec<DesignEvent>, String> {
    let Some(noos_dir) = state.opened_path().as_deref().and_then(noos_dir_for_path) else {
        return Ok(Vec::new());
    };
    let lines = read_event_lines(&noos_dir).map_err(|e| e.to_string())?;
    let mut events: Vec<DesignEvent> = lines.iter().filter_map(event_from_line).collect();
    // Bound the render cost: keep only the most recent
    // MEMORY_LIST_MAX. drain the oldest so the Vec owns the retained tail.
    if events.len() > MEMORY_LIST_MAX {
        let drop = events.len() - MEMORY_LIST_MAX;
        events.drain(..drop);
    }
    Ok(events)
}

/// Retire a constraint by name: emit a `ConstraintRetired` tombstone via
/// the shared event write path.
///
/// Reuses [`write_event`] (flock-protected, sidecar+graph durability) — no new
/// writer, so retirement inherits the same durability guarantees EXCEPT the
/// jsonl retry queue: a user-initiated write surfaces failure directly to the
/// panel (as an error cue) rather than enqueuing, since the user can retry. Errors when no file is open or the write fails. Retirement is a
/// tombstone, not a delete — the prior `Constraint` stays in history.
pub fn retire_constraint_inner(state: &AppState, name: &str) -> Result<(), String> {
    let Some(noos_dir) = state.opened_path().as_deref().and_then(noos_dir_for_path) else {
        return Err("no project file open".to_string());
    };
    write_event(
        &DesignEvent::ConstraintRetired {
            name: name.to_string(),
        },
        &noos_dir,
    )
    .map_err(|e| e.to_string())
}

/// IPC wrapper over [`memory_list_inner`].
/// `async` + `spawn_blocking` so the sidecar read stays off the main thread.
#[tauri::command]
pub async fn memory_list_cmd(app: tauri::AppHandle) -> Result<Vec<DesignEvent>, String> {
    tauri::async_runtime::spawn_blocking(move || memory_list_inner(&app.state::<AppState>()))
        .await
        .map_err(|e| format!("internal error: memory task failed: {e}"))?
}

/// IPC wrapper over [`retire_constraint_inner`]. `async` + `spawn_blocking`:
/// the write takes the project's exclusive `.noos/write.lock` `flock`, which
/// must not block the main thread.
#[tauri::command]
pub async fn retire_constraint_cmd(name: String, app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        retire_constraint_inner(&app.state::<AppState>(), &name)
    })
    .await
    .map_err(|e| format!("internal error: memory task failed: {e}"))?
}
