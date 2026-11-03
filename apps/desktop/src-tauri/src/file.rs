//! The `open_file` / `save_file` IPC seam.
//!
//! Native file dialogs scoped to `.scad`, behind an injectable
//! [`FileDialogPicker`] trait so the pure read-after-pick / write-after-pick
//! logic is testable without an OS dialog. The Tauri command wrappers in
//! [`crate::lib`] hold the real `tauri-plugin-dialog`-backed picker and call
//! the same pure [`open_file`] / [`save_file`] functions tests drive.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Injectable file-dialog seam. `pick_open` / `pick_save` return the picked
/// path, or `None` when the user cancels.
///
/// The real impl ([`TauriDialogPicker`]) scopes both dialogs to `.scad` via
/// `tauri-plugin-dialog`; tests inject a fake returning canned paths. Keep the
/// trait synchronous (blocking): both call sites block on a single pick, and
/// the Tauri commands are dispatched on a thread where blocking is acceptable.
pub trait FileDialogPicker {
    /// Show an "open" dialog scoped to `.scad`; `None` = user cancelled.
    fn pick_open(&self) -> Option<PathBuf>;
    /// Show a "save" dialog scoped to `.scad`; `None` = user cancelled.
    fn pick_save(&self) -> Option<PathBuf>;
    /// Show a "save" dialog scoped to `.stl`; `None` = user cancelled.
    fn pick_save_stl(&self) -> Option<PathBuf>;
}

/// Result of `open_file` — the picked path and the file's `.scad` source.
/// Mirrors the Rust struct on the TS side as `OpenFileResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFileResult {
    /// The picked path, as a display string.
    pub path: String,
    /// The file's contents (UTF-8 `.scad` source).
    pub source: String,
}

/// Result of `save_file` — the path the source was written to. Mirrors the Rust
/// struct on the TS side as `SaveFileResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveFileResult {
    /// The picked path, as a display string.
    pub path: String,
}

/// One failure mode of [`open_file`] / [`save_file`]. Serialized as a tagged
/// union (`{ kind: "cancelled" | "io", ... }`) so the frontend can route on the
/// discriminant. Mirrors the Rust enum on the TS side as `FileError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileError {
    /// The user cancelled the dialog. No file is read or written.
    Cancelled,
    /// `std::fs::read` / `write` failed. `message` is the `io::Error` string.
    Io { message: String },
}

/// Max `.scad` file size `open_file` will read. `read_to_string`
/// does a full read-then-validate, so without this cap a multi-GB `.scad` (or a
/// `.scad` symlink to a binary) would OOM. 64 MB is far beyond any real source.
pub const MAX_SCAD_BYTES: u64 = 64 * 1024 * 1024;

/// Reject `path` if its on-disk size exceeds `max_bytes`.
/// `metadata().len()` follows symlinks, so a `.scad` symlink to a binary is
/// capped by the target's size. Extracted as a pub pure-ish seam so the
/// rejection is testable with a tiny injected cap (the production
/// [`MAX_SCAD_BYTES`] is too large to exercise in a unit test).
pub fn check_size_cap(path: &Path, max_bytes: u64) -> Result<(), FileError> {
    let len = std::fs::metadata(path)
        .map_err(|err| FileError::Io {
            message: err.to_string(),
        })?
        .len();
    if len > max_bytes {
        return Err(FileError::Io {
            message: format!(
                "{} is {} bytes — morphe caps .scad opens at {} bytes (too large to be source)",
                path.display(),
                len,
                max_bytes
            ),
        });
    }
    Ok(())
}

/// Pick a `.scad` file via `picker`, read it, and return its source. Returns
/// [`FileError::Cancelled`] if the user cancels, or [`FileError::Io`] if the
/// picked path can't be read as UTF-8.
///
/// After a successful read, if the opened file's project has a
/// `.noos/` dir, [`morphe_ai::rehydrate_events`] runs to rebuild any event
/// graph nodes an external `noos scan` dropped since the last morphe
/// run (morphe never scans in-app). Rehydrate is **best-effort**: errors are
/// logged and swallowed — the source read is the primary result and the open
/// must not fail because of a memory-substrate issue.
///
/// Rehydrate is followed by [`morphe_ai::flush_queue`], which
/// drains any backlog a prior emit (or a prior session/crash) left in
/// `<noos_dir>/morphe-queue.jsonl`. Also best-effort — a flush failure is
/// logged and the open proceeds. This is the "queue survives a restart"
/// recovery path: a queued event from yesterday's session lands in the
/// substrate when the user reopens the project today.
pub fn open_file<P: FileDialogPicker>(picker: &P) -> Result<OpenFileResult, FileError> {
    let path: PathBuf = picker.pick_open().ok_or(FileError::Cancelled)?;
    // Validate .scad BEFORE reading: the dialog's add_filter is only a hint on
    // macOS (the user can still pick "All Files"), and reading a binary /
    // non-.scad file crashes (OOM reading a large binary into a String, or a
    // downstream parse of non-scad content). Reject at the source with a clear
    // message the frontend toasts.
    let is_scad = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("scad"))
        .unwrap_or(false);
    if !is_scad {
        return Err(FileError::Io {
            message: format!(
                "{} is not a .scad file — morphe opens OpenSCAD source only",
                path.display()
            ),
        });
    }
    // Size cap before read_to_string: read_to_string does a
    // full read-then-validate, so a multi-GB .scad (or a .scad SYMLINK to a
    // binary) would OOM. metadata().len() follows the symlink, so a linked
    // binary is capped by its size. Extracted as [`check_size_cap`] so the
    // rejection is testable with a tiny injected cap (the production cap is too
    // large to exercise in a unit test).
    check_size_cap(&path, MAX_SCAD_BYTES)?;
    let source = std::fs::read_to_string(&path).map_err(|err| FileError::Io {
        message: err.to_string(),
    })?;

    // Rehydrate-on-open: morphe never scans in-app, so this is the per-project
    // moment to recover any event nodes an external scan dropped. Only runs
    // when the opened file's parent actually has a `.noos/`; skipped silently
    // otherwise (a project morphe has never written events into).
    if let Some(parent) = path.parent() {
        let noos_dir = parent.join(".noos");
        if noos_dir.is_dir() {
            if let Err(e) = morphe_ai::rehydrate_events(&noos_dir) {
                eprintln!(
                    "[morphe] open_file: noos rehydrate_events failed for {}: {e} \
                     (the open itself is unaffected)",
                    noos_dir.display()
                );
            }
            // Drain the retry queue — recovers any backlog from a prior
            // session/crash. Best-effort: failures are logged, the open
            // proceeds.
            if let Err(e) = morphe_ai::flush_queue(&noos_dir) {
                eprintln!(
                    "[morphe] open_file: noos flush_queue failed for {}: {e} \
                     (will retry on next emit/open; the open itself is unaffected)",
                    noos_dir.display()
                );
            }
        }
    }

    Ok(OpenFileResult {
        path: display_path(&path),
        source,
    })
}

/// Pick a destination via `picker`, then write `source` to it. Returns
/// [`FileError::Cancelled`] if the user cancels, or [`FileError::Io`] if the
/// write fails.
pub fn save_file<P: FileDialogPicker>(
    picker: &P,
    source: &str,
) -> Result<SaveFileResult, FileError> {
    let mut path: PathBuf = picker.pick_save().ok_or(FileError::Cancelled)?;
    // A bare name typed into the dialog ("bracket") gets the .scad extension.
    if path.extension().is_none() {
        path.set_extension("scad");
    }
    save_to_path(&path, source)
}

/// Write `source` to an already-known `path` (plain Save of the opened file —
/// the path came from an earlier dialog inside this process, never from the
/// frontend). Same `.scad`-only rule as [`open_file`], so a Save can never
/// overwrite a non-source file.
pub fn save_to_path(path: &Path, source: &str) -> Result<SaveFileResult, FileError> {
    let is_scad = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("scad"));
    if !is_scad {
        return Err(FileError::Io {
            message: format!(
                "{} is not a .scad file — morphe saves OpenSCAD source only",
                path.display()
            ),
        });
    }
    std::fs::write(path, source).map_err(|err| FileError::Io {
        message: err.to_string(),
    })?;
    Ok(SaveFileResult {
        path: display_path(path),
    })
}

/// Render a path as a display string (lossy, like `Path::display`).
fn display_path(path: &Path) -> String {
    path.display().to_string()
}

// ── Real picker (tauri-plugin-dialog) ───────────────────────────────────────
//
// Only the Tauri commands construct this; tests use their own fake. It cannot
// be driven headless (it shows a real OS dialog), so it is correct-by-
// construction and exercised only by the pure-logic tests above via the trait.

use tauri_plugin_dialog::DialogExt;

/// `tauri-plugin-dialog`-backed [`FileDialogPicker`]. Both dialogs are scoped
/// to `.scad`. Holds an [`tauri::AppHandle`] injected into the Tauri command.
pub(crate) struct TauriDialogPicker {
    app: tauri::AppHandle,
}

impl TauriDialogPicker {
    /// Wrap an [`tauri::AppHandle`] so the IPC commands can call the pure
    /// [`open_file`] / [`save_file`] logic with a real picker.
    pub(crate) fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl FileDialogPicker for TauriDialogPicker {
    fn pick_open(&self) -> Option<PathBuf> {
        self.app
            .dialog()
            .file()
            .add_filter("OpenSCAD source", &["scad"])
            .blocking_pick_file()
            .and_then(|picked| picked.into_path().ok())
    }

    fn pick_save(&self) -> Option<PathBuf> {
        self.app
            .dialog()
            .file()
            .add_filter("OpenSCAD source", &["scad"])
            .blocking_save_file()
            .and_then(|picked| picked.into_path().ok())
    }

    fn pick_save_stl(&self) -> Option<PathBuf> {
        self.app
            .dialog()
            .file()
            .add_filter("STL mesh", &["stl"])
            .blocking_save_file()
            .and_then(|picked| picked.into_path().ok())
    }
}
