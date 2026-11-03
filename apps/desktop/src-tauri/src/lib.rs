//! morphe-desktop: Tauri 2 shell for the morphe CAD application.
//!
//! The backend exposes IPC commands for compiling (`compile_source`, which runs
//! the parse → eval → resolve → mesh pipeline and returns the mesh as raw bytes
//! plus stats/errors), STL export, file open/save, the AI chat and the
//! per-project design memory.

use serde::Serialize;
use tauri::Manager;

mod ai;
mod compile;
mod export;
mod file;
mod memory;

pub use ai::{
    ai_accept_edit, ai_cancel, ai_get_provider_config, ai_has_api_key, ai_reject_edit,
    ai_remove_api_key, ai_set_provider_config, ai_turn, reconstruct_final_source,
    reject_edit_inner, revert_patch, run_turn_blocking, AiEvent, AiTurnRequest, AppState,
    ProviderConfigInfo, TurnOutcome, TurnSlotGuard,
};
// Re-export morphe-ai so integration tests can name Transport/RawResponse/etc.
// without adding a second dependency edge (mirrors the morphe_kernel re-export).
pub use compile::{compile_source, CompileError, CompileResult, ErrorKind};
pub use export::{export_stl, export_stl_via_dialog, ExportError, ExportStats};
pub use file::{
    check_size_cap, open_file, save_file, save_to_path, FileDialogPicker, FileError,
    OpenFileResult, SaveFileResult, MAX_SCAD_BYTES,
};
pub use memory::{
    memory_list_cmd, memory_list_inner, retire_constraint_cmd, retire_constraint_inner,
};
pub use morphe_ai;
// Re-export so the frontend types and integration tests can name the stats
// shape without a second window into morphe-kernel.
pub use morphe_kernel::GeometryStats;

/// Entry point shared by the desktop binary and (via `mobile_entry_point`)
/// a future mobile target.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Native open/save dialogs scoped to `.scad`. The
        // plugin backs the `TauriDialogPicker` impl in `file.rs`; JS never
        // calls it directly, so no capability entry is needed.
        .plugin(tauri_plugin_dialog::init())
        // The AI subsystem state: SecretStore (the OS credential store —
        // Keychain, Windows Credential Manager or Secret Service — falling
        // back to in-memory if unavailable), the current turn's cancel token, and the
        // authoritative .scad source buffer.
        .manage(AppState::new_default())
        .invoke_handler(tauri::generate_handler![
            compile_source_cmd,
            export_stl_cmd,
            open_file_cmd,
            save_file_cmd,
            ai_has_api_key,
            ai_get_provider_config,
            ai_set_provider_config,
            ai_remove_api_key,
            ai_turn,
            ai_cancel,
            ai_accept_edit,
            ai_reject_edit,
            memory_list_cmd,
            retire_constraint_cmd
        ])
        .run(tauri::generate_context!())
        .expect("error while running morphe desktop application");
}

/// IPC wrapper over the pure [`compile::compile_source`] seam. For parity
/// with the CLI it reads the ALREADY-OPEN file's path from [`AppState`]
/// (recorded by [`open_file_with_state`] — the Rust side that owns the
/// dialog), and attaches it as the compile's source-dir context so
/// `import()`/`surface()` resolve relative paths identically to the CLI. No
/// arbitrary frontend path crosses the IPC trust boundary: the frontend sends
/// only source text.
///
/// Returns a [`tauri::ipc::Response`] whose body is the full
/// [`CompileResult`] packed into one binary frame: a little-endian
/// `u32` metadata length, that many bytes of JSON (`{ stats, errors, warnings }`),
/// then the raw `MeshBuffer` bytes appended verbatim. The mesh therefore
/// crosses IPC as raw bytes (never a JSON array of numbers); only the small
/// metadata header is JSON, for easy decoding on the frontend.
///
/// **Threading:** an `async` command whose pipeline runs on a dedicated
/// large-stack worker thread ([`run_off_main`]). A sync `#[tauri::command]`
/// runs on the app's MAIN thread in Tauri 2, so a heavy model (minkowski, a
/// high-`$fn` sphere) used to freeze the whole window, and the 250 ms
/// debounce queued more compiles behind it. A panic in the pipeline becomes a
/// `general` compile error instead of taking the command down.
#[tauri::command]
async fn compile_source_cmd(app: tauri::AppHandle, source: String) -> tauri::ipc::Response {
    let opened = app.state::<AppState>().opened_path();
    let result = run_off_main(move || {
        compile_source(&source, opened.map(std::path::PathBuf::from).as_deref())
    })
    .await
    .unwrap_or_else(|message| CompileResult {
        mesh: Vec::new(),
        stats: None,
        errors: vec![CompileError {
            line: 1,
            col: 1,
            message,
            kind: ErrorKind::General,
        }],
        warnings: Vec::new(),
    });
    tauri::ipc::Response::new(to_wire(result))
}

/// Stack size for the off-main worker threads that run the compile pipeline.
/// The evaluator and the CSG resolver recurse over the user's program; deep
/// (but legal) recursion overflowed the 2 MiB default of a blocking-pool
/// thread and aborted the whole app. This is a virtual reservation, not
/// committed memory.
pub const WORKER_STACK_BYTES: usize = 256 * 1024 * 1024;

/// Run blocking work `f` off the main thread: on a fresh thread with a
/// [`WORKER_STACK_BYTES`] stack, awaited from the async runtime without
/// blocking any of its workers. A panic inside `f` is returned as `Err` with a
/// short message instead of unwinding into the command.
pub async fn run_off_main<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || run_on_big_stack(f))
        .await
        .map_err(|e| format!("internal error: worker task failed: {e}"))?
}

/// Synchronous core of [`run_off_main`]: run `f` on a fresh
/// [`WORKER_STACK_BYTES`] thread and join it, mapping a panic to `Err`.
/// Exposed so tests can drive deep recursion without a Tauri runtime.
pub fn run_on_big_stack<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("morphe-worker".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(f)
        .map_err(|e| format!("internal error: cannot start worker thread: {e}"))?;
    handle.join().map_err(|panic| {
        let detail = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        format!("internal error while compiling: {detail}")
    })
}

/// IPC wrapper over the pure [`export::export_stl_via_dialog`] seam. Opens its
/// own `.stl` save dialog (the destination path originates inside this process
/// and never crosses the IPC trust boundary as a frontend-supplied value), then
/// writes the resolved solid's STL bytes (binary when `ascii` is false). Returns
/// [`ExportStats`] on success, or an [`ExportError`] (user cancel, compile
/// diagnostics, a 2D top level, or an I/O failure) as a serialized rejection.
///
/// `async` + [`run_off_main`]: the compile and the blocking save dialog run
/// off the main thread (`blocking_save_file` must not be called on it).
#[tauri::command]
async fn export_stl_cmd(
    app: tauri::AppHandle,
    source: String,
    ascii: bool,
) -> Result<ExportStats, ExportError> {
    // Export resolves imports against the SAME open
    // file's dir as compile — preview/export parity, still no frontend path.
    let opened = app.state::<AppState>().opened_path();
    let picker = file::TauriDialogPicker::new(app);
    run_off_main(move || {
        export_stl_via_dialog(
            &picker,
            &source,
            opened.map(std::path::PathBuf::from).as_deref(),
            ascii,
        )
    })
    .await
    .map_err(|message| ExportError::Io { message })?
}

/// IPC wrapper over [`file::open_file`] with the real `tauri-plugin-dialog`
/// picker. Picks a `.scad` file, reads it, AND records the opened disk path in
/// [`AppState::opened_path`] (via the pure [`open_file_with_state`] seam) so
/// [`ai::ai_accept_edit`] can derive the per-project `.noos/` dir for memory
/// writes. Returns [`FileError`] (`Cancelled` / `Io`) as a serialized
/// rejection.
///
/// `async` + [`run_off_main`]: the blocking open dialog, the read and the
/// noos rehydrate/flush run off the main thread.
#[tauri::command]
async fn open_file_cmd(app: tauri::AppHandle) -> Result<OpenFileResult, FileError> {
    let picker = file::TauriDialogPicker::new(app.clone());
    run_off_main(move || open_file_with_state(&picker, app.state::<AppState>().inner()))
        .await
        .map_err(|message| FileError::Io { message })?
}

/// Pure inner of [`open_file_cmd`]: open via `picker`, read the source, AND
/// record the opened disk path in [`AppState::opened_path`] so
/// [`ai::ai_accept_edit`] can derive the per-project `.noos/` dir for memory
/// writes. Extracted as a pub seam so integration tests
/// drive it with a fake picker + an [`AppState`] (no Tauri runtime).
///
/// The OS file dialog always returns an absolute disk path, so the recorded
/// `opened_path` is absolute — that's the invariant
/// [`ai::ai_accept_edit`]'s noos-dir derivation relies on.
pub fn open_file_with_state<P: FileDialogPicker>(
    picker: &P,
    state: &AppState,
) -> Result<OpenFileResult, FileError> {
    let result = open_file(picker)?;
    state.set_opened_path(Some(result.path.clone()));
    Ok(result)
}

/// IPC wrapper over [`file::save_file`] with the real `tauri-plugin-dialog`
/// picker. Picks a destination, writes `source`, returns `{ path }`.
/// The saved path becomes the opened path, so a
/// Save-As retargets the compile source-dir context to the new location
/// (mirrors [`open_file_with_state`]'s bookkeeping).
///
/// `save_as == false` and a file is already open → write straight back to
/// the opened path (plain Save, no dialog). Otherwise (Save As, or an
/// in-memory buffer) the native save dialog picks the destination. Either way
/// the destination originates inside this process — no frontend-supplied path
/// crosses the IPC trust boundary. `async` + [`run_off_main`]: the blocking
/// dialog and the write run off the main thread.
#[tauri::command]
async fn save_file_cmd(
    app: tauri::AppHandle,
    source: String,
    save_as: Option<bool>,
) -> Result<SaveFileResult, FileError> {
    let picker = file::TauriDialogPicker::new(app.clone());
    let save_as = save_as.unwrap_or(true);
    run_off_main(move || {
        let state = app.state::<AppState>();
        if save_as {
            save_file_with_state(&picker, state.inner(), &source)
        } else {
            save_or_save_as_with_state(&picker, state.inner(), &source)
        }
    })
    .await
    .map_err(|message| FileError::Io { message })?
}

/// Pure inner of a plain Save: write back to [`AppState::opened_path`] when a
/// file is open; fall back to the Save-As dialog flow
/// ([`save_file_with_state`]) for an in-memory buffer.
pub fn save_or_save_as_with_state<P: FileDialogPicker>(
    picker: &P,
    state: &AppState,
    source: &str,
) -> Result<SaveFileResult, FileError> {
    match state.opened_path() {
        Some(path) => file::save_to_path(std::path::Path::new(&path), source),
        None => save_file_with_state(picker, state, source),
    }
}

/// Pure inner of [`save_file_cmd`]: save via `picker`, then record the saved
/// path as the opened path in [`AppState::opened_path`] — a Save-As is the
/// same "this buffer lives here now" statement as open, and the compile
/// source-dir context must follow it. Extracted as a pub seam so
/// integration tests drive it with a fake picker + an [`AppState`].
pub fn save_file_with_state<P: FileDialogPicker>(
    picker: &P,
    state: &AppState,
    source: &str,
) -> Result<SaveFileResult, FileError> {
    let result = save_file(picker, source)?;
    state.set_opened_path(Some(result.path.clone()));
    Ok(result)
}

/// Pack a [`CompileResult`] into the binary wire frame described in
/// [`compile_source_cmd`]: `[u32 meta_len][meta_json bytes][mesh bytes]`.
fn to_wire(result: CompileResult) -> Vec<u8> {
    // Metadata = everything but the mesh, as JSON (small, structured, easy to
    // decode on the frontend). The mesh stays raw bytes.
    #[derive(Serialize)]
    struct Meta<'a> {
        stats: &'a Option<GeometryStats>,
        errors: &'a [CompileError],
        warnings: &'a [String],
        mesh_len: usize,
    }
    let mesh_len = result.mesh.len();
    let meta = Meta {
        stats: &result.stats,
        errors: &result.errors,
        warnings: &result.warnings,
        mesh_len,
    };
    let meta_json = serde_json::to_vec(&meta).expect("compile meta serializes to JSON");

    let mut out = Vec::with_capacity(4 + meta_json.len() + result.mesh.len());
    out.extend_from_slice(
        &u32::try_from(meta_json.len())
            .expect("meta JSON length fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(&meta_json);
    out.extend_from_slice(&result.mesh);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_deps_resolve() {
        // Proves src-tauri is wired to the morphe kernel crates (path
        // dependencies resolve and link).
        let cube = morphe_kernel::Solid::cube([1.0, 1.0, 1.0], false);
        assert!(cube.is_manifold());
        assert_eq!(morphe_eval::eval::MAX_CALL_DEPTH, 8_000);
    }

    /// Parse the `[u32 meta_len][meta_json]` prefix off `wire` and return
    /// `(meta_json_value, mesh_bytes)`. Asserts the frame is well-formed.
    fn parse_wire(wire: &[u8]) -> (serde_json::Value, &[u8]) {
        assert!(
            wire.len() >= 4,
            "wire frame shorter than 4-byte meta length prefix"
        );
        let meta_len = u32::from_le_bytes(wire[0..4].try_into().unwrap()) as usize;
        let meta_end = 4 + meta_len;
        assert!(
            wire.len() >= meta_end,
            "wire frame truncated: need {meta_end} bytes, have {}",
            wire.len()
        );
        let meta: serde_json::Value =
            serde_json::from_slice(&wire[4..meta_end]).expect("meta JSON decodes");
        (meta, &wire[meta_end..])
    }

    /// The errors / 2D-only / empty-top-level path emits an empty mesh. The TS
    /// decoder must see `mesh_len == 0` with zero trailing bytes or it crashes.
    /// This test pins the Rust side so the TS/Rust wire
    /// asymmetry can't recur silently.
    #[test]
    fn to_wire_empty_mesh_locks_error_path_contract() {
        let result = CompileResult {
            mesh: Vec::new(),
            stats: None,
            errors: vec![CompileError {
                line: 1,
                col: 1,
                message: "bad".to_string(),
                kind: ErrorKind::Parse,
            }],
            warnings: Vec::new(),
        };
        let wire = to_wire(result);
        let (meta, mesh_bytes) = parse_wire(&wire);
        assert_eq!(meta["mesh_len"].as_u64(), Some(0));
        assert!(
            mesh_bytes.is_empty(),
            "empty-mesh path must emit zero trailing bytes"
        );
        assert_eq!(meta["errors"][0]["kind"], "parse");
    }

    /// A non-empty mesh must round-trip through `to_wire`: `meta.mesh_len`
    /// equals the appended byte length, and the trailing bytes reconstruct the
    /// mesh header verbatim.
    #[test]
    fn to_wire_nonempty_mesh_reconstructs_frame() {
        // Tiny packed mesh: 1 vertex, 0 indices, positions = [0.0, 0.0, 0.0].
        let mut mesh = Vec::new();
        mesh.extend_from_slice(&1u32.to_le_bytes()); // vertex_count
        mesh.extend_from_slice(&0u32.to_le_bytes()); // index_count
        for f in [0.0_f32, 0.0, 0.0] {
            mesh.extend_from_slice(&f.to_le_bytes());
        }
        let mesh_len = mesh.len();

        let result = CompileResult {
            mesh,
            stats: None,
            errors: Vec::new(),
            warnings: Vec::new(),
        };
        let wire = to_wire(result);
        let (meta, mesh_bytes) = parse_wire(&wire);
        assert_eq!(meta["mesh_len"].as_u64(), Some(mesh_len as u64));
        assert_eq!(mesh_bytes.len(), mesh_len);
        // The mesh header survives the append verbatim.
        let vcount = u32::from_le_bytes(mesh_bytes[0..4].try_into().unwrap());
        assert_eq!(vcount, 1);
    }
}
