//! Hardened file access inside the project's `.noos/` design-memory directory.
//!
//! `.noos/` lives inside a project the user opened, and a project may come
//! from an untrusted source (a cloned repository, a downloaded archive). A
//! crafted project could ship `.noos` itself, or any file morphe writes inside
//! it (`morphe-events.jsonl`, `morphe-queue.jsonl`, `graph.json`, the
//! `*.tmp` files used for atomic rewrites, `write.lock`), as a symlink to some
//! other file the user can write — a shell rc file, an SSH config, another
//! project. Following such a link would let the project make morphe append to,
//! truncate, or overwrite that file.
//!
//! Every open in the design-memory directory therefore goes through this
//! module, which:
//!
//! - refuses a design-memory directory that is itself a symlink;
//! - refuses an existing target that is a symlink or not a regular file
//!   (FIFOs, devices, directories);
//! - on unix, opens with `O_NOFOLLOW` (so a symlink swapped in after the check
//!   still fails to open) and `O_NONBLOCK` (so a FIFO swapped in cannot block
//!   the open), then re-checks that the opened handle is a regular file;
//! - writes atomic-rewrite temp files with `create_new` (`O_EXCL`, which never
//!   follows a symlink) after removing any stale entry at that path.
//!
//! On non-unix targets the symlink/regular-file pre-check is the only guard
//! (best effort: it cannot close the check-then-open race).
//!
//! # Durability
//!
//! [`append_line`] fsyncs the file after appending, and [`write_atomic`]
//! fsyncs the temp file before the rename and (on unix) the directory after
//! it, so an acknowledged write survives a crash or power loss. A crash during
//! an append can still leave a torn final line with no trailing newline;
//! [`append_line`] starts the next record on a fresh line so the torn bytes
//! never swallow it, and readers skip the torn line as malformed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Build the error returned when a path is refused.
fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::other(format!("refusing to use {}: {why}", path.display()))
}

/// Refuse `dir` if it exists as a symlink. A missing `dir` is fine (the caller
/// may be about to create it, or is reading and will find nothing).
pub(crate) fn reject_symlinked_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(refused(dir, "it is a symlink")),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Create `dir` (and its parents) unless it exists, refusing a symlinked `dir`.
pub(crate) fn ensure_dir(dir: &Path) -> io::Result<()> {
    reject_symlinked_dir(dir)?;
    fs::create_dir_all(dir)?;
    // Re-check: `create_dir_all` succeeds on a symlink to a directory, so a
    // link planted between the first check and the create must still fail.
    reject_symlinked_dir(dir)
}

/// Refuse an existing `path` that is a symlink or not a regular file. Returns
/// `Ok(())` when `path` does not exist.
fn reject_non_regular(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(refused(path, "it is a symlink")),
        Ok(meta) if !meta.is_file() => Err(refused(path, "it is not a regular file")),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Open `path` with `opts`, refusing symlinks and non-regular files (see the
/// [module docs](self)). The parent directory is checked too, so readers that
/// never call [`ensure_dir`] still refuse a symlinked design-memory directory.
pub(crate) fn open(path: &Path, opts: &mut OpenOptions) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        reject_symlinked_dir(parent)?;
    }
    reject_non_regular(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(refused(path, "it is not a regular file"));
    }
    Ok(file)
}

/// Read the whole of `path`. Returns `Ok(None)` when it does not exist.
pub(crate) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut file = match open(path, OpenOptions::new().read(true)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(Some(buf))
}

/// A line that was just appended by [`append_line`], kept so the append can be
/// undone if a later step of the same logical write fails.
pub(crate) struct Appended {
    file: File,
    prior_len: u64,
}

impl Appended {
    /// Truncate the file back to its length before the append (best effort).
    pub(crate) fn rollback(self) {
        if self.file.set_len(self.prior_len).is_ok() {
            let _ = self.file.sync_data();
        }
    }
}

/// Append `line` plus a trailing newline to `path` (created if missing) and
/// fsync it.
///
/// If the file does not end with a newline (a torn final line left by a crash
/// mid-append), a newline is written first so the new record starts on its
/// own line instead of being glued onto the torn bytes. On error the file is
/// truncated back to its previous length, so a partial write cannot linger.
pub(crate) fn append_line(path: &Path, line: &[u8]) -> io::Result<Appended> {
    let mut file = open(
        path,
        OpenOptions::new().read(true).append(true).create(true),
    )?;
    let prior_len = file.metadata()?.len();
    let mut buf = Vec::with_capacity(line.len() + 2);
    if prior_len > 0 && !ends_with_newline(&mut file, prior_len)? {
        buf.push(b'\n');
    }
    buf.extend_from_slice(line);
    buf.push(b'\n');
    if let Err(e) = file.write_all(&buf).and_then(|()| file.sync_data()) {
        let _ = file.set_len(prior_len);
        return Err(e);
    }
    if prior_len == 0 {
        // The file may have just been created: persist its directory entry.
        sync_parent_dir(path);
    }
    Ok(Appended { file, prior_len })
}

/// Whether the last byte of `file` (of length `len > 0`) is a newline.
fn ends_with_newline(file: &mut File, len: u64) -> io::Result<bool> {
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

/// Replace `path` with `contents` atomically: write a fresh `tmp` file next to
/// it, fsync it, rename `tmp` over `path`, then fsync the directory.
///
/// Any stale entry at `tmp` is removed first (removing a symlink removes the
/// link, never its target), and `tmp` is created with `create_new` so the
/// write can never land in a file planted there. `rename` replaces a symlink
/// at `path` rather than writing through it.
pub(crate) fn write_atomic(path: &Path, tmp: &Path, contents: &[u8]) -> io::Result<()> {
    match fs::remove_file(tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = open(tmp, OpenOptions::new().write(true).create_new(true))?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)?;
    sync_parent_dir(path);
    Ok(())
}

/// Remove `path` if present (a missing file is not an error), then fsync the
/// directory so the removal is durable.
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            sync_parent_dir(path);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Fsync the directory containing `path` so a create/rename/remove in it is
/// durable (unix only; a no-op elsewhere).
///
/// Best effort by design: it runs after the operation itself has succeeded,
/// and reporting a failure then would make callers retry (and duplicate) a
/// write that did happen.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(dir) = File::open(dir) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}
