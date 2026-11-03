//! jsonl retry queue for transient `write_event` failures.
//!
//! When [`morphe_ai::write_event`] returns a [`morphe_ai::WriteError`] (a
//! transient substrate failure — corrupt `graph.json`, lock contention, I/O),
//! the event is appended to `<noos_dir>/morphe-queue.jsonl` instead of being
//! silently lost. The queue drains on the next emit (via [`flush_queue`]) and
//! on `open_file` (the flush-backlog path), so a transient failure never
//! becomes a permanent loss.
//!
//! # State model
//!
//! The jsonl file IS the state — there is no long-lived in-memory struct. Each
//! call reads (or appends) under the same flock(LOCK_EX) on `.noos/write.lock`
//! that [`write_event`] and [`rehydrate_events`] use, so queue ops serialize
//! against the rest of the substrate. No second lock.
//!
//! # Sequence at flush
//!
//! Queue lines store raw [`DesignEvent`]s with NO sequence/provenance wrapper.
//! At flush, [`flush_queue`] calls the body of `write_event` per entry (under
//! the lock already held — `write_event_locked` is the lock-free inner).
//! `write_event_locked` assigns the sequence from the sidecar at write time,
//! so queued events get sequences contiguously after any live writes — no
//! collision is possible because the flock serializes everything.
//!
//! # Idempotency
//!
//! [`flush_queue`] rewrites the queue file with ONLY the entries that failed to
//! flush (atomic via tmp+rename, matching [`write_graph`]'s pattern). A
//! successful flush therefore removes the entry; a subsequent flush does NOT
//! re-write it. Partial-flush semantics: if N of M entries succeed, only the
//! M−N failures remain in the queue.

use crate::noos_fs;
use crate::noos_writer::{ensure_noos_dir, with_write_lock, write_event_locked};

use std::path::Path;

use thiserror::Error;

use crate::DesignEvent;

/// Filename of the retry queue, relative to the noos dir.
const QUEUE_FILENAME: &str = "morphe-queue.jsonl";

/// Error from [`enqueue_event`] or [`flush_queue`].
#[derive(Debug, Error)]
pub enum QueueError {
    /// Could not read, write, or parse the queue file (`morphe-queue.jsonl`).
    #[error("morphe-queue.jsonl: {0}")]
    Io(String),
    /// A flush attempted [`write_event`] per queued entry and one or more
    /// entries failed. Successful entries were still removed from the queue;
    /// this variant carries the first failure so the caller can log it.
    /// (Failures stay in the queue for the next flush — see [module docs](self).)
    #[error("flush write_event failure: {0}")]
    Write(#[from] crate::WriteError),
}

/// Append `event` as one JSON line to `<noos_dir>/morphe-queue.jsonl`. Returns
/// the new queue depth (the line count of the file after the append).
///
/// Acquires the same flock(LOCK_EX) on `.noos/write.lock` that
/// [`write_event`] uses, so enqueues serialize against writes and flushes — no
/// second lock. Creates `noos_dir` (and the queue file) as needed.
pub fn enqueue_event(event: &DesignEvent, noos_dir: &Path) -> Result<usize, QueueError> {
    ensure_noos_dir(noos_dir).map_err(|e| QueueError::Io(e.to_string()))?;

    with_write_lock::<_, _, QueueError>(noos_dir, || {
        // Serialize the DesignEvent as one JSON line. Internally-tagged on
        // `kind` (DesignEvent's serde shape) — NO sequence/provenance wrapper.
        // write_event assigns those at flush from the sidecar under the lock.
        let serialized = serde_json::to_string(event)
            .map_err(|e| QueueError::Io(format!("serialize failed: {e}")))?;
        let path = noos_dir.join(QUEUE_FILENAME);
        // Count existing lines first (the pre-append depth). Lines are not
        // parsed here — parsing is the flush's job; enqueue only counts.
        let pre_depth = count_lines(&path)?;
        noos_fs::append_line(&path, serialized.as_bytes())
            .map_err(|e| QueueError::Io(format!("append {}: {e}", path.display())))?;
        Ok(pre_depth + 1)
    })
}

/// Drain `<noos_dir>/morphe-queue.jsonl` by calling the body of
/// [`morphe_ai::write_event`] (the lock-free inner [`write_event_locked`]) per
/// queued entry. Successful entries are removed from the queue (the file is
/// rewritten atomically with only the failures). Returns the count drained.
///
/// Acquires the same flock(LOCK_EX) on `.noos/write.lock` that `write_event`
/// uses; the per-entry writes call the lock-free inner directly so the lock
/// is held exactly once for the whole flush. Re-acquiring inside `write_event`
/// would deadlock (flock is per-open-file-description).
///
/// # Partial-failure semantics
///
/// If some entries write successfully and some fail (e.g. the substrate goes
/// from healthy to corrupt mid-flush), the successful entries are still
/// removed; the failures stay in the queue for the next flush. The function
/// returns the count successfully drained, even when there was a failure —
/// the caller logs the depth and moves on. A subsequent flush after the
/// substrate recovers drains the remainder.
///
/// # Idempotency
///
/// A successfully flushed entry is removed from the queue BEFORE the function
/// returns, so a subsequent flush cannot write it again.
pub fn flush_queue(noos_dir: &Path) -> Result<usize, QueueError> {
    ensure_noos_dir(noos_dir).map_err(|e| QueueError::Io(e.to_string()))?;

    with_write_lock::<_, _, QueueError>(noos_dir, || {
        let entries = read_queue_entries(noos_dir)?;
        if entries.is_empty() {
            return Ok(0);
        }
        let mut drained = 0usize;
        let mut failed: Vec<DesignEvent> = Vec::new();
        let mut first_err: Option<crate::WriteError> = None;
        for entry in entries {
            // The lock-free inner — write_event assigns sequence + provenance
            // from the sidecar under the lock we already hold. A failure here
            // means the substrate is still transiently broken; leave the entry
            // in the queue for the next flush.
            match write_event_locked(&entry, noos_dir) {
                Ok(()) => drained += 1,
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                    failed.push(entry);
                }
            }
        }
        // Rewrite the queue with ONLY the failures (atomic: tmp + rename,
        // mirroring write_graph's pattern). Successful entries are now
        // durably in the sidecar+graph and removed from the queue — a future
        // flush will not re-attempt them (idempotency).
        rewrite_queue(noos_dir, &failed)?;
        match first_err {
            Some(e) => Err(QueueError::Write(e)),
            None => Ok(drained),
        }
    })
}

// ---------------------------------------------------------------------------
// Queue file I/O (under the lock acquired by the public functions above)
// ---------------------------------------------------------------------------

/// Count non-blank trimmed lines in `path`. Returns 0 if the file does not
/// exist yet (queue is empty).
fn count_lines(path: &Path) -> Result<usize, QueueError> {
    let content = read_queue_text(path)?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .count())
}

/// Read the queue file as text (empty when absent), through the hardened
/// [`noos_fs::read`] so a symlinked or non-regular queue file is refused.
fn read_queue_text(path: &Path) -> Result<String, QueueError> {
    let bytes = noos_fs::read(path)
        .map_err(|e| QueueError::Io(format!("read {}: {e}", path.display())))?
        .unwrap_or_default();
    // Lossy: a line torn mid-character by a crash then fails to parse and is
    // skipped on its own, instead of failing the whole read.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read every non-blank line of the queue file and deserialize each as a
/// [`DesignEvent`]. Malformed lines are skipped (forward-compat with future
/// queue shapes or hand-edited content — same pattern as the sidecar's
/// `read_event_lines`). Returns an empty Vec when the file is absent.
fn read_queue_entries(noos_dir: &Path) -> Result<Vec<DesignEvent>, QueueError> {
    let path = noos_dir.join(QUEUE_FILENAME);
    let content = read_queue_text(&path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<DesignEvent>(trimmed) {
            Ok(ev) => out.push(ev),
            // Skip malformed lines — forward-compat. A corrupt queue line is
            // not a process-fatal event; the consumer's idempotency guarantee
            // (no silent loss of design history) is
            // honored by the well-formed lines that did parse.
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Atomically rewrite the queue file so it contains exactly `entries` (one
/// DesignEvent per line). If `entries` is empty, the queue file is removed
/// entirely so the substrate stays clean when there is no backlog. Atomic via
/// `<queue>.tmp` + rename — a crash mid-write never leaves a partial queue.
fn rewrite_queue(noos_dir: &Path, entries: &[DesignEvent]) -> Result<(), QueueError> {
    let path = noos_dir.join(QUEUE_FILENAME);
    if entries.is_empty() {
        // NotFound is not an error (queue already empty).
        noos_fs::remove(&path)
            .map_err(|e| QueueError::Io(format!("remove {}: {e}", path.display())))
    } else {
        let mut buf = String::new();
        for ev in entries {
            let serialized = serde_json::to_string(ev)
                .map_err(|e| QueueError::Io(format!("serialize failed: {e}")))?;
            buf.push_str(&serialized);
            buf.push('\n');
        }
        let tmp = path.with_extension("jsonl.tmp");
        noos_fs::write_atomic(&path, &tmp, buf.as_bytes()).map_err(|e| {
            QueueError::Io(format!(
                "write {} via {}: {e}",
                path.display(),
                tmp.display()
            ))
        })
    }
}
