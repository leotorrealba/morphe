//! noos writer for morphe design events.
//!
//! Translates a [`DesignEvent`] into the project's `.noos/` directory — the
//! store of noos, an optional external design-memory tool — by
//! appending **two** durable records together:
//!
//! 1. a JSON line to `<noos_dir>/morphe-events.jsonl` — the durable sidecar, the
//!    single source of truth for what design events morphe has recorded.
//! 2. an `event` node to `<noos_dir>/graph.json` — the queryable index that
//!    `noos validate` / `noos explain` / `noos query` consume.
//!
//! # Why both
//!
//! The sidecar is durable: it survives `noos scan` (which regenerates
//! `graph.json` from ingestible code/docs and **drops** hand-added event
//! nodes — see [`rehydrate_events`]). The graph node makes the event visible
//! to `noos query` / `noos explain` immediately. Writing both keeps the SSOT
//! (sidecar) and the index (graph) in lockstep on every emit.
//!
//! Never write one without the other: [`write_event`] does both in sequence
//! and is the only public entry point for emitting an event.
//!
//! # Sequencing
//!
//! Event ids are `event:<subject>#<sequence>` and must be unique per subject.
//! The next sequence is derived by scanning the sidecar for prior lines with
//! the same subject and taking `max + 1` (starting at 1). Sequence assignment
//! is therefore idempotent across graph rebuilds — the sidecar is the SSOT.
//!
//! # Subject derivation
//!
//! The subject is the stable identifier of "the thing this event is about":
//!
//! | Variant       | Subject              |
//! |---------------|----------------------|
//! | `Decision`    | the edited `path`    |
//! | `Constraint`  | the constraint `name`|
//! | `PrintSetting`| the slicer `key`     |
//! | `Intent`      | the statement, sanitized + truncated to 32 chars |
//!
//! Subjects are sanitized to be id-safe (`[a-zA-Z0-9_./-]`) because they
//! appear inside node ids (`event:<subject>#<seq>`) and inside tags
//! (`subject:<subject>`). `#` and `:` are forbidden because they delimit the
//! id.
//!
//! # Scan survival
//!
//! `noos scan` regenerates `graph.json` and drops hand-added event nodes
//! (verified against the real noos binary). The sidecar survives.
//! [`rehydrate_events`] rebuilds the graph's event nodes from the sidecar —
//! call it after a scan (idempotent: nodes already present are not duplicated).
//!
//! # Concurrency (.noos/write.lock)
//!
//! Both [`write_event`] and [`rehydrate_events`] acquire an advisory
//! `flock(LOCK_EX)` on `<noos_dir>/write.lock` around their full critical
//! section (the sidecar append + graph read-modify-write). This mirrors noos's
//! OWN single-writer protocol — the noos binary uses `flock(2)` on the same
//! 0-byte `.noos/write.lock` for its `scan` and `enrich` commands (verified
//! via binary strings). morphe therefore serializes its writes against each
//! other AND against a user-run `noos scan`/`enrich`: whoever holds
//! the lock owns the substrate for the duration.
//!
//! The lock is acquired INSIDE the writer (not by callers) so every emit site
//! is safe by construction. Blocking semantics: a noos scan holding the lock
//! blocks the next `write_event` until the scan releases. For user-paced
//! accepts this is fine (scans are rare + brief). No timeout yet —
//! one can be added if the agent loop ever blocks unacceptably.
//!
//! # Untrusted projects
//!
//! `.noos/` ships inside the project, so its entries may be crafted. Every open
//! goes through [`crate::noos_fs`], which refuses a symlinked `.noos`, refuses
//! symlinked or non-regular targets, and (on unix) opens with `O_NOFOLLOW`, so
//! a project cannot redirect morphe's writes to a file outside it.

use std::fs::OpenOptions;
use std::path::Path;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::noos_fs;
use crate::DesignEvent;

/// Tag attached to every event node recording the morphe goal it serves.
const GOAL_TAG: &str = "goal:1.5";
/// Filename of the events sidecar, relative to the noos dir.
const EVENTS_FILENAME: &str = "morphe-events.jsonl";
/// Filename of the noos graph, relative to the noos dir.
const GRAPH_FILENAME: &str = "graph.json";
/// Filename of the single-writer lock, relative to the noos dir. Same path
/// noos itself flocks — morphe and noos share one advisory lock.
const WRITE_LOCK_FILENAME: &str = "write.lock";
/// `file_path` recorded on event nodes (the sidecar location, relative to the
/// repo root — matches how noos itself addresses files).
const NODE_FILE_PATH: &str = ".noos/morphe-events.jsonl";
/// Provenance for morphe-emitted events. The noos schema's `Provenance`
/// discriminant is EXTRACTED / AMBIGUOUS / ASSERTED / INFERRED; morphe asserts
/// design events it observed the agent apply.
const PROVENANCE: &str = "ASSERTED";
/// node_type for event nodes.
const NODE_TYPE: &str = "event";
/// Max chars of an Intent statement used to derive its subject.
const INTENT_SUBJECT_MAX_CHARS: usize = 32;

/// Error from [`write_event`] or [`rehydrate_events`].
#[derive(Debug, Error)]
pub enum WriteError {
    /// Could not read, write, or parse the events sidecar (`morphe-events.jsonl`).
    #[error("morphe-events.jsonl: {0}")]
    Events(String),
    /// Could not read, write, or parse the noos graph (`graph.json`).
    #[error("graph.json: {0}")]
    Graph(String),
    /// Could not open or `flock(LOCK_EX)` the `.noos/write.lock`. Surfaces (never
    /// panics) when the lockfile can't be created — e.g. read-only `.noos/`, or
    /// the lock path exists as a directory. Mutual exclusion itself is the OS's
    /// job; this variant is only for lock-acquisition failure, not for "another
    /// writer holds the lock" (that blocks until released, by design).
    #[error("write.lock: {0}")]
    Lock(String),
}

/// The line shape written to `morphe-events.jsonl`: a kind discriminant, identity
/// tier (subject + sequence), provenance, and the DesignEvent's variant
/// payload (the event's fields minus `kind`, which is hoisted to the top
/// level).
#[derive(Debug, Serialize, Deserialize)]
pub struct EventLine {
    pub kind: String,
    pub subject: String,
    pub sequence: u64,
    pub provenance: String,
    pub payload: Value,
}

/// Reconstruct a [`DesignEvent`] from a sidecar [`EventLine`].
///
/// [`EventLine`] stores the variant `payload` (the variant's fields MINUS
/// `kind`, which is hoisted to the envelope top level) — so this re-attaches
/// `kind` to the payload and deserializes. Returns `None` for an
/// unknown/malformed `kind` (forward-compat: a future variant the consumer
/// doesn't know yet, or a hand-edited line). The memory panel's `memory_list`
/// IPC uses this to return TYPED events to the frontend (so the wire-shape
/// tests pin the `DesignEvent` shape that actually crosses IPC).
pub fn event_from_line(line: &EventLine) -> Option<DesignEvent> {
    let mut merged = line.payload.clone();
    if let Some(obj) = merged.as_object_mut() {
        obj.insert("kind".to_string(), Value::String(line.kind.clone()));
    }
    serde_json::from_value(merged).ok()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write a single [`DesignEvent`] into the noos substrate under `noos_dir`:
/// appends one line to `<noos_dir>/morphe-events.jsonl` and one `event` node to
/// `<noos_dir>/graph.json`. Both writes happen together — never one without
/// the other.
///
/// `noos_dir` is a parameter (test seam): tests pass a temp dir; the
/// desktop emit site passes the project's `.noos` path.
///
/// Thin wrapper over [`write_event_locked`] (the lock-free inner): acquires the
/// single-writer flock, then runs the sidecar+graph writes. The inner exists
/// so the retry queue ([`crate::event_queue::flush_queue`]) can hold the
/// lock ONCE for the whole flush and call the inner per entry — re-acquiring
/// inside `write_event` would deadlock (flock is per-open-file-description).
///
/// # Errors
///
/// - [`WriteError::Events`] if the sidecar cannot be read/written/parsed.
/// - [`WriteError::Graph`] if the graph cannot be read/written/parsed.
///
/// On error, neither file has changed, so the event can be queued and retried
/// without producing a duplicate.
pub fn write_event(event: &DesignEvent, noos_dir: &Path) -> Result<(), WriteError> {
    ensure_noos_dir(noos_dir)?;

    with_write_lock(noos_dir, || write_event_locked(event, noos_dir))
}

/// Lock-free inner of [`write_event`]: assumes the caller already holds the
/// single-writer flock on `.noos/write.lock` (via [`with_write_lock`]) and
/// performs the sidecar append + graph node append under that lock. Public to
/// the crate so [`crate::event_queue::flush_queue`] can drain the retry queue
/// under one lock acquisition (one lock-hold for the whole flush, not per
/// entry).
///
/// Callers MUST hold the lock — this is the lock-critical-section body, not a
/// standalone public entry point. The only public entry for emitting an event
/// is [`write_event`].
///
/// # Failure leaves no trace
///
/// On `Err`, neither the sidecar nor the graph has changed. Callers re-queue a
/// failed event ([`crate::event_queue`]) and retry it on every later flush, so
/// a half-written event (a sidecar line without its graph node) would be
/// written again on each retry, accumulating duplicate sidecar lines. The
/// order is therefore:
///
/// 1. read and validate the graph (a corrupt graph fails here, before any
///    write);
/// 2. append the sidecar line (the SSOT);
/// 3. write the graph atomically; if that fails, truncate the sidecar back to
///    its previous length.
pub(crate) fn write_event_locked(event: &DesignEvent, noos_dir: &Path) -> Result<(), WriteError> {
    let mut graph = read_graph(noos_dir)?;
    let nodes = graph_nodes_mut(&mut graph)?;

    let subject = derive_subject(event);
    let sequence = next_sequence(&subject, noos_dir)?;
    let kind = kind_tag(event);
    let payload = payload_of(event);
    let line = EventLine {
        kind: kind.clone(),
        subject: subject.clone(),
        sequence,
        provenance: PROVENANCE.to_string(),
        payload,
    };
    // Built from the payload stored in the sidecar line so write_event and
    // rehydrate_events share one node-shape derivation path (see
    // build_event_node).
    nodes.push(build_event_node(&kind, &subject, sequence, &line.payload));

    let appended = append_event_line(noos_dir, &line)?;
    if let Err(e) = write_graph(noos_dir, &graph) {
        // Best effort: if the truncate itself fails, the sidecar keeps the
        // line and a later rehydrate still indexes it; the retry may then
        // record the event a second time.
        appended.rollback();
        return Err(e);
    }
    Ok(())
}

/// Rebuild every `event` node in `<noos_dir>/graph.json` from the sidecar at
/// `<noos_dir>/morphe-events.jsonl`. Idempotent: nodes already present (matched by
/// id) are not duplicated.
///
/// Use after a `noos scan` regenerated the graph and dropped hand-added event
/// nodes — the sidecar is the SSOT, the graph node is a rebuildable index.
/// Future wiring (e.g. a scan hook) should call this whenever the graph is
/// rescanned.
///
/// # Errors
///
/// - [`WriteError::Events`] if the sidecar cannot be read/parsed.
/// - [`WriteError::Graph`] if the graph cannot be read/written/parsed.
pub fn rehydrate_events(noos_dir: &Path) -> Result<(), WriteError> {
    ensure_noos_dir(noos_dir)?;

    with_write_lock(noos_dir, || {
        let lines = read_event_lines(noos_dir)?;
        let mut graph = read_graph(noos_dir)?;
        let nodes = graph_nodes_mut(&mut graph)?;

        let existing_ids: std::collections::HashSet<String> = nodes
            .iter()
            .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();

        let mut added = false;
        for line in lines {
            let id = format!("event:{}#{}", line.subject, line.sequence);
            if existing_ids.contains(&id) {
                continue;
            }
            // Same shape derivation as write_event — both call build_event_node so
            // the variant-tuned name/summary can never drift between the two paths.
            let node = build_event_node(&line.kind, &line.subject, line.sequence, &line.payload);
            nodes.push(node);
            added = true;
        }

        if added {
            write_graph(noos_dir, &graph)?;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Identity-tier derivation
// ---------------------------------------------------------------------------

/// The DesignEvent variant's snake_case discriminant (the `kind` tag).
fn kind_tag(event: &DesignEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// The event's payload: the DesignEvent serialized, with the `kind`
/// discriminant removed (it lives at the envelope's top level, not duplicated
/// inside the payload).
fn payload_of(event: &DesignEvent) -> Value {
    // DesignEvent is a simple enum of String fields, so serialization is
    // effectively infallible. `unwrap_or(Value::Null)` honors the
    // "no unwrap/expect in production paths" rule without changing
    // behavior on the reachable path.
    let mut v = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("kind");
    }
    v
}

/// The stable subject for an event. See the [module docs](self) for the
/// per-variant derivation table.
fn derive_subject(event: &DesignEvent) -> String {
    let raw: String = match event {
        DesignEvent::Decision { path, .. } => path.clone(),
        DesignEvent::Constraint { name, .. } => name.clone(),
        DesignEvent::PrintSetting { key, .. } => key.clone(),
        DesignEvent::ConstraintRetired { name } => name.clone(),
        DesignEvent::Intent { statement } => {
            // Truncate to a bounded length, then sanitize. Two distinct
            // statements that collide (rare) just bump the sequence — ids
            // stay unique. The kind is already in tags so the subject only
            // needs to disambiguate within Intents.
            let truncated: String = statement.chars().take(INTENT_SUBJECT_MAX_CHARS).collect();
            let sanitized = sanitize_subject(&truncated);
            if sanitized.is_empty() {
                "intent".to_string()
            } else {
                sanitized
            }
        }
    };
    sanitize_subject(&raw)
}

/// Map a string into the id-safe alphabet `[a-zA-Z0-9_./-]`. Any other char
/// (including the id delimiters `#` and `:`) becomes `_`. Whitespace,
/// punctuation, and non-ASCII all collapse to `_`.
fn sanitize_subject(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build the `event` graph node value for one event. The single node-shape
/// derivation used by BOTH `write_event` and `rehydrate_events` — feeding it
/// the payload (not the live DesignEvent) lets rehydrate reproduce the exact
/// variant-tuned name/summary from the sidecar alone.
fn build_event_node(kind: &str, subject: &str, sequence: u64, payload: &Value) -> Value {
    let id = format!("event:{subject}#{sequence}");
    let name = node_name(kind, subject, payload);
    let summary = node_summary(kind, subject);
    serde_json::json!({
        "id": id,
        "node_type": NODE_TYPE,
        "name": name,
        "file_path": NODE_FILE_PATH,
        "summary": summary,
        "tags": [
            format!("kind:{kind}"),
            format!("subject:{subject}"),
            format!("seq:{sequence}"),
            GOAL_TAG,
        ],
        "complexity": null,
    })
}

/// Human label for the node, variant-tuned where it adds value:
/// `decision: edit (path)`, `constraint: name = value`, etc. Matched on the
/// `kind` discriminant and reads `value` from the payload for the variants
/// that carry one, so the name derives from the same (kind, subject, payload)
/// triple both paths already have in hand.
fn node_name(kind: &str, subject: &str, payload: &Value) -> String {
    match kind {
        "decision" => format!("{kind}: edit ({subject})"),
        "constraint" | "print_setting" => {
            let value = payload.get("value").and_then(Value::as_str).unwrap_or("?");
            format!("{kind}: {subject} = {value}")
        }
        _ => format!("{kind}: {subject}"),
    }
}

/// Short human description for the node. Uniform across variants — the variant
/// detail lives in [`node_name`].
fn node_summary(kind: &str, subject: &str) -> String {
    format!("{kind} event for {subject}")
}

// ---------------------------------------------------------------------------
// Sidecar (morphe-events.jsonl) I/O
// ---------------------------------------------------------------------------

/// Compute the next sequence for `subject` by scanning the sidecar for prior
/// lines with the same subject. Starts at 1 for a new subject. Lines that
/// fail to parse as an [`EventLine`] are skipped (forward-compat with future
/// envelope fields).
fn next_sequence(subject: &str, noos_dir: &Path) -> Result<u64, WriteError> {
    let lines = read_event_lines(noos_dir)?;
    let max = lines
        .into_iter()
        .filter(|l| l.subject == *subject)
        .map(|l| l.sequence)
        .max()
        .unwrap_or(0);
    Ok(max + 1)
}

/// Append one durable (fsynced) line to the sidecar. The returned handle can
/// undo the append if the graph write that follows it fails.
fn append_event_line(noos_dir: &Path, line: &EventLine) -> Result<noos_fs::Appended, WriteError> {
    let serialized = serde_json::to_string(line)
        .map_err(|e| WriteError::Events(format!("serialize failed: {e}")))?;
    let path = noos_dir.join(EVENTS_FILENAME);
    noos_fs::append_line(&path, serialized.as_bytes())
        .map_err(|e| WriteError::Events(format!("append {}: {e}", path.display())))
}

pub fn read_event_lines(noos_dir: &Path) -> Result<Vec<EventLine>, WriteError> {
    let path = noos_dir.join(EVENTS_FILENAME);
    let Some(bytes) = noos_fs::read(&path)
        .map_err(|e| WriteError::Events(format!("read {}: {e}", path.display())))?
    else {
        return Ok(Vec::new());
    };
    // Lossy decoding: a line torn by a crash can end mid-character. Its
    // replacement chars make it fail to parse, so only that line is skipped.
    let content = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<EventLine>(trimmed) {
            Ok(parsed) => out.push(parsed),
            // Skip malformed lines — forward-compat with future envelope
            // fields or hand-edited content. The SSOT is best-effort.
            Err(_) => continue,
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Graph (graph.json) I/O
// ---------------------------------------------------------------------------

/// The graph's `nodes` array, or a [`WriteError::Graph`] if it has none.
fn graph_nodes_mut(graph: &mut Value) -> Result<&mut Vec<Value>, WriteError> {
    graph
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| WriteError::Graph("graph.json has no `nodes` array".into()))
}

/// Read `graph.json` as a generic JSON value. If the file is absent, returns
/// a minimal envelope matching the noos graph schema's required fields
/// (`version`, `project` with `name`/`languages`/`ingested_at`, `nodes`,
/// `edges`, `layers`, `tour`) so the writer can bootstrap a fresh graph that
/// passes `noos validate`. Extras (`communities`, `duplicates`) are included
/// to match noos's own exported shape.
fn read_graph(noos_dir: &Path) -> Result<Value, WriteError> {
    let path = noos_dir.join(GRAPH_FILENAME);
    let content = noos_fs::read(&path)
        .map_err(|e| WriteError::Graph(format!("read {}: {e}", path.display())))?;
    let Some(content) = content else {
        return Ok(serde_json::json!({
            "version": "0.2.0",
            "project": {
                "name": "morphe",
                "languages": [],
                // null = no commit recorded yet (the graph is bootstrapped
                // before any `noos scan` runs). Key is present for structural
                // parity with what `noos scan` emits, so the production
                // fallback and the test seed fail together if the schema ever
                // requires the key.
                "git_commit_hash": null,
                "ingested_at": "1970-01-01T00:00:00Z"
            },
            "nodes": [],
            "edges": [],
            "layers": [],
            "tour": [],
            "communities": [],
            "duplicates": []
        }));
    };
    serde_json::from_slice(&content)
        .map_err(|e| WriteError::Graph(format!("parse {}: {e}", path.display())))
}

/// Pretty-print the graph (2-space indent, matching noos's own output) and
/// write atomically via a same-dir tmp file + rename so a crash mid-write
/// never leaves a partial graph.
fn write_graph(noos_dir: &Path, graph: &Value) -> Result<(), WriteError> {
    let path = noos_dir.join(GRAPH_FILENAME);
    let pretty = serde_json::to_string_pretty(graph)
        .map_err(|e| WriteError::Graph(format!("serialize failed: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    noos_fs::write_atomic(&path, &tmp, format!("{pretty}\n").as_bytes()).map_err(|e| {
        WriteError::Graph(format!(
            "write {} via {}: {e}",
            path.display(),
            tmp.display()
        ))
    })
}

// ---------------------------------------------------------------------------
// Shared setup
// ---------------------------------------------------------------------------

pub(crate) fn ensure_noos_dir(noos_dir: &Path) -> Result<(), WriteError> {
    noos_fs::ensure_dir(noos_dir)
        .map_err(|e| WriteError::Events(format!("create_dir {}: {e}", noos_dir.display())))
}

/// Open `<noos_dir>/write.lock`, acquire an advisory `flock(LOCK_EX)`, run
/// `critical`, and release on return (RAII via the held `File`'s Drop). Mirrors
/// noos's own single-writer protocol on the same path, so morphe writes
/// serialize against each other AND against external `noos scan`/`enrich`.
///
/// Callers must guarantee `noos_dir` already exists (via [`ensure_noos_dir`]).
///
/// `pub(crate)` so the jsonl retry queue ([`crate::event_queue`]) can
/// acquire the SAME lock for enqueue/flush — no second lock is introduced. The
/// queue's flush holds this lock once and calls [`write_event_locked`] per
/// entry; re-acquiring inside `write_event` would deadlock (flock is per-open-
/// file-description, not per-process).
///
/// Generic over the error type so the closure may return its own error variant
/// (e.g. [`crate::event_queue::QueueError`]) without a double-conversion:
/// `E: From<WriteError>` lifts the lock-acquisition errors into the caller's
/// type, and `write_event` / `rehydrate_events` keep their `WriteError`
/// signature unchanged (identity `From`).
///
/// # Errors
///
/// `WriteError::Lock` (lifted into `E`) if the lockfile can't be created/opened
/// or the kernel `flock` call itself fails. "Another writer currently holds the
/// lock" is NOT an error here — `lock_exclusive` blocks until the lock is
/// acquired, which is the intended user-paced-accept behavior (no timeout).
pub(crate) fn with_write_lock<F, T, E>(noos_dir: &Path, critical: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<WriteError>,
{
    let lock_path = noos_dir.join(WRITE_LOCK_FILENAME);
    let lockfile = noos_fs::open(
        &lock_path,
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true),
    )
    .map_err(|e| WriteError::Lock(format!("open {}: {e}", lock_path.display())))?;
    lockfile
        .lock_exclusive()
        .map_err(|e| WriteError::Lock(format!("flock {}: {e}", lock_path.display())))?;
    // `lockfile` (holding LOCK_EX) is dropped at the end of this scope,
    // releasing the lock — including via `?` early-returns inside `critical`.
    critical()
}
