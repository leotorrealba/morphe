//! The `ai_*` IPC commands — wiring morphe-ai into the desktop backend.
//!
//! morphe-ai's agent loop is synchronous: its [`Transport`] trait is sync and
//! `ReqwestTransport` drives an internal tokio runtime via `block_on`. Calling
//! the loop from inside the Tauri async runtime would block a worker. This
//! module bridges that gap:
//!
//! - [`ai_turn`] (an `async` Tauri command) calls [`morphe_ai::run_on_thread`],
//!   which spawns the loop on a **dedicated OS thread** and returns immediately
//!   with a sync `mpsc::Receiver<LoopEvent>` + a `JoinHandle<LoopResult>`. The
//!   command never calls the sync transport itself.
//! - A [`tauri::async_runtime::spawn_blocking`] task drains the receiver (sync,
//!   on a blocking-pool thread) and forwards each [`LoopEvent`] as an [`AiEvent`]
//!   over the Tauri [`Channel`]. When the join handle resolves, it reconstructs
//!   the final workspace source from the streamed `edit_code` events and sends
//!   [`AiEvent::Done`].
//!
//! # Async-safety reasoning
//!
//! `run_on_thread`'s only blocking work (the `Transport::send` → `block_on`)
//! runs on its spawned OS thread, not on the Tauri runtime. The bridge task
//! performs only `rx.recv()` and `join_handle.join()` — both sync, both on a
//! `spawn_blocking` thread. No async-runtime worker ever blocks on the sync
//! transport, and the command itself never calls `Transport::send` at all. This
//! is the key async-safety invariant of this module.
//!
//! # Final source reconstruction
//!
//! `run_on_thread` consumes the [`Dispatcher`] (it owns the workspace) and
//! [`LoopResult`] carries only events + final text — not the workspace's final
//! source. Rather than modify morphe-ai's API, [`reconstruct_final_source`]
//! replays the successful `edit_code` tool calls (deterministic str_replace) in
//! dispatch order against the seeded source. This mirrors exactly what the
//! dispatcher applied, so the reconstructed source equals the workspace's final
//! source.
//!
//! # Edit accept/reject
//!
//! Full human-in-the-loop pausing (loop waits for accept before continuing) is
//! not implemented — it requires restructuring `AgentLoop::run`. Instead the loop applies
//! `edit_code` patches to the workspace as they arrive; the frontend renders a
//! diff from the streamed [`AiEvent::ToolCall`] (which carries `{path, old,
//! new}`). [`ai_accept_edit`] keeps the applied source (no-op on the buffer);
//! [`ai_reject_edit`] reverts a single patch (re-applies `new`→`old`) in
//! [`AppState`]'s source buffer so the next turn/compile reflects the revert.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use morphe_ai::{
    assemble_system_prompt, client_from_secret, tool_definitions, CancellationToken, Client,
    ClientConfig, Dispatcher, InMemorySecretStore, InputContentBlock, InputMessage, LoopEvent,
    LoopStopReason, MessageContent, PromptContext, Provider, ProviderConfig, Request,
    SanitizedText, SecretStore, ThinkingConfig, ToolChoice,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{Manager, State};

/// The fixed workspace path the agent edits. The desktop app has a single editor
/// buffer; the agent's `InMemoryWorkspace` seeds it under this name.
pub(crate) const WORKSPACE_PATH: &str = "main.scad";

// ---------------------------------------------------------------------------
// AiEvent — streamed to the frontend over the Channel
// ---------------------------------------------------------------------------

/// One event in an AI turn, streamed to the frontend. The frontend renders text
/// inline, shows tool calls as collapsibles (with a diff preview for
/// `edit_code`), and stops on [`AiEvent::Done`].
///
/// Mirrors [`LoopEvent`] with an added terminal [`AiEvent::Done`] carrying the
/// final workspace source + the loop's stop reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiEvent {
    /// Assistant text content (one per text block).
    Text { text: String },
    /// The model requested a tool call. `input` for `edit_code` is
    /// `{path, old, new}` — the frontend renders a diff preview + accept/reject.
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// The dispatcher produced a tool result for the given tool-use id.
    /// `content` is JSON for structured tools (compile/get_geometry_stats),
    /// plain text for stubs; `is_error` flags tool-call failures.
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
    /// The loop finished. `stop_reason` is the serialized [`LoopStopReason`];
    /// `final_source` is the workspace's `.scad` after all edits applied (the
    /// frontend syncs its editor to this).
    Done {
        stop_reason: String,
        final_source: String,
    },
}

impl AiEvent {
    /// Map a [`LoopEvent`] from the agent loop to an [`AiEvent`] for the channel.
    pub fn from_loop_event(event: &LoopEvent) -> Self {
        match event {
            LoopEvent::AssistantText(text) => AiEvent::Text { text: text.clone() },
            LoopEvent::ToolCall { id, name, input } => AiEvent::ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            LoopEvent::ToolResult {
                id,
                content,
                is_error,
            } => AiEvent::ToolResult {
                id: id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// AiTurnRequest + serialized stop reason
// ---------------------------------------------------------------------------

/// The request payload for [`ai_turn`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTurnRequest {
    /// The current `.scad` source from the editor (seeds the agent workspace).
    pub source: String,
    /// Optional design-intent context (rendered into the volatile system block).
    pub design_intent: Option<String>,
}

/// Render a [`LoopStopReason`] as a stable string for the frontend to route on
/// (`"end_turn"`, `"cancelled"`, `"round_cap"`, `"max_tokens"`, `"refusal"`,
/// `"error"`).
fn stop_reason_str(reason: &LoopStopReason) -> String {
    match reason {
        LoopStopReason::EndTurn => "end_turn",
        LoopStopReason::Cancelled => "cancelled",
        LoopStopReason::RoundCap => "round_cap",
        LoopStopReason::MaxTokens => "max_tokens",
        LoopStopReason::Refusal => "refusal",
        LoopStopReason::Error(_) => "error",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Pure seam: reconstruct the final source from streamed edit_code events
// ---------------------------------------------------------------------------

/// Reconstruct the workspace's final source by replaying the successful
/// `edit_code` tool calls in dispatch order against `initial_source`, skipping
/// any patch whose tool-use id is in `rejected`.
///
/// The dispatcher's `edit_code` is a deterministic str_replace (`old`→`new`)
/// that succeeds only when `old` matches exactly once. The loop emits a
/// `ToolCall{edit_code}` followed by its `ToolResult`; a non-error result means
/// the patch applied. Replaying those patches in order reproduces the
/// workspace's final source exactly — the same sequence of str_replace
/// operations the dispatcher performed.
///
/// `rejected` carries the tool-use ids the user rejected mid-turn (via
/// [`ai_reject_edit`]); those patches are skipped so the reconstructed source —
/// and thus the next turn's seed (`AppState.source`) — reflects the reject.
/// Without this the bridge would overwrite the user's reject with the full
/// (edit-applied) source.
///
/// Pure (no I/O, no Tauri types) so tests assert it directly.
pub fn reconstruct_final_source(
    events: &[LoopEvent],
    initial_source: &str,
    rejected: &HashSet<String>,
) -> String {
    // Collect edit_code patches by tool-use id (the id ties a ToolCall to its
    // ToolResult). Only edit_code carries a patch; other tools are no-ops here.
    let mut patches: HashMap<String, (String, String)> = HashMap::new();
    for event in events {
        if let LoopEvent::ToolCall { id, name, input } = event {
            if name == "edit_code" {
                let old = input.get("old").and_then(Value::as_str).unwrap_or("");
                let new = input.get("new").and_then(Value::as_str).unwrap_or("");
                patches.insert(id.clone(), (old.to_string(), new.to_string()));
            }
        }
    }

    // Walk events in order; on each successful edit_code ToolResult, apply the
    // patch — unless the user rejected it. Preserves dispatch order (the order
    // the dispatcher applied them).
    //
    // `replay_diverged` tracks whether the replay's source state has diverged
    // from what the dispatcher saw, which happens once any successful patch is
    // skipped (user-rejected, or its own `old` wasn't found after an earlier
    // skip). Before any divergence the source mirrors the dispatcher's state
    // exactly, so a successful edit's `old` is guaranteed present — a None there
    // is a reconstruct/dispatcher divergence bug, asserted in tests. After a
    // divergence `old` may legitimately be absent (the source no longer matches
    // the dispatcher's), so the no-op is the correct floor there and in prod.
    let mut source = initial_source.to_string();
    let mut replay_diverged = false;
    for event in events {
        if let LoopEvent::ToolResult {
            id,
            is_error: false,
            ..
        } = event
        {
            if rejected.contains(id) {
                replay_diverged = true;
                continue;
            }
            if let Some((old, new)) = patches.get(id) {
                let idx = first_match(&source, old);
                if !replay_diverged {
                    debug_assert!(
                        idx.is_some(),
                        "reconstruct diverged from dispatcher: successful edit's `old` not found"
                    );
                }
                if let Some(idx) = idx {
                    let mut out = String::with_capacity(source.len() - old.len() + new.len());
                    out.push_str(&source[..idx]);
                    out.push_str(new);
                    out.push_str(&source[idx + old.len()..]);
                    source = out;
                } else {
                    replay_diverged = true;
                }
            }
        }
    }
    source
}

/// The byte index of the first occurrence of `needle` in `hay`, or `None`.
fn first_match(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.match_indices(needle).next().map(|(i, _)| i)
}

// ---------------------------------------------------------------------------
// AppState — the managed Tauri state
// ---------------------------------------------------------------------------

/// Tauri-managed state for the AI subsystem.
///
/// Holds the [`SecretStore`] (the OS credential store in production, falling
/// back to in-memory when it is unavailable), the current session's cancellation token (for [`ai_cancel`]),
/// the authoritative `.scad` source buffer (the single source of truth the
/// next turn seeds from and accept/reject operate on), the set of tool-use
/// ids rejected this turn (so [`reconstruct_final_source`] can skip them),
/// and the disk path of the file the user opened (so [`ai_accept_edit`]
/// can derive the per-project `.noos/` dir for memory writes).
///
/// `Send + Sync` (all fields are behind `Mutex`/`Arc`) so Tauri can share it
/// across command invocations. The source is `Arc<Mutex<String>>` so the
/// [`ai_turn`] bridge task (on a `spawn_blocking` thread) can write the final
/// source back after the loop completes.
pub struct AppState {
    /// The API-key store. `Arc<dyn SecretStore>` so commands share one backend.
    secret: Arc<dyn SecretStore>,
    /// The current turn's cancel token; `None` when no turn is running. Set by
    /// [`ai_turn`] (atomically with the in-flight check) and cleared by the
    /// turn's bridge task when the turn finishes — including after a cancel,
    /// so a cancelled-but-still-draining turn keeps blocking a new one until
    /// it has actually stopped. [`ai_cancel`] sets it.
    cancel: Arc<Mutex<Option<CancellationToken>>>,
    /// The authoritative `.scad` source. [`ai_turn`] seeds the workspace from
    /// the request's `source` and, on completion, writes the reconstructed final
    /// source here. Accept/reject operate on this buffer.
    source: Arc<Mutex<String>>,
    /// Tool-use ids the user rejected this turn (via [`ai_reject_edit`]).
    /// Cleared at the start of each [`ai_turn`]; read by the bridge after the
    /// loop to exclude rejected edits from `final_source` reconstruction.
    rejected: Arc<Mutex<HashSet<String>>>,
    /// The disk path of the `.scad` file the user opened via [`open_file_cmd`],
    /// or `None` when the editor holds an in-memory buffer (no file open). The
    /// agent loop always emits `edit_code` patches against the relative
    /// [`WORKSPACE_PATH`] ("main.scad"); the per-project `.noos/` dir for memory
    /// writes must therefore come from THIS field (the real disk path), not the
    /// patch path. Set by [`open_file_cmd`] on a successful open.
    opened_path: Arc<Mutex<Option<String>>>,
}

impl AppState {
    /// Construct with a specific [`SecretStore`] backend. Production
    /// ([`new_default`]) uses the OS credential store; tests inject
    /// [`InMemorySecretStore`].
    #[must_use]
    pub(crate) fn with_secret(secret: Arc<dyn SecretStore>) -> Self {
        Self {
            secret,
            cancel: Arc::new(Mutex::new(None)),
            source: Arc::new(Mutex::new(String::new())),
            rejected: Arc::new(Mutex::new(HashSet::new())),
            opened_path: Arc::new(Mutex::new(None)),
        }
    }

    /// Construct the production default backend: the OS credential store
    /// (macOS Keychain, Windows Credential Manager, Linux Secret Service), so
    /// the API key persists across launches.
    ///
    /// Falls back to the in-memory store, logging once, when the platform
    /// store is unreachable at startup (e.g. a Linux session without a Secret
    /// Service daemon); the key then lasts only for this run.
    #[must_use]
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    pub(crate) fn new_default() -> Self {
        use morphe_ai::KeychainSecretStore;
        let store = KeychainSecretStore::new();
        match store.probe() {
            Ok(()) => Self::with_secret(Arc::new(store)),
            Err(e) => {
                eprintln!(
                    "[morphe] OS credential store unavailable ({e}); the API key \
                     will be kept in memory for this session only"
                );
                Self::new_in_memory()
            }
        }
    }

    /// Default on targets without a credential-store backend: in-memory.
    #[must_use]
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub(crate) fn new_default() -> Self {
        Self::new_in_memory()
    }

    /// The in-memory store default, usable from tests on any target.
    #[must_use]
    pub fn new_in_memory() -> Self {
        Self::with_secret(Arc::new(InMemorySecretStore::new()))
    }

    /// Whether an API key is set in the secret store. This is what
    /// [`ai_has_api_key`] returns; exposed `pub` so integration tests drive the
    /// SecretStore seam without a Tauri runtime.
    pub fn has_api_key(&self) -> bool {
        self.secret.get_key().map(|k| k.is_some()).unwrap_or(false)
    }

    /// Store the full provider config. This is what
    /// [`ai_set_provider_config`] calls; exposed `pub` for tests. The
    /// ProviderConfig is serialized to JSON so one store entry holds
    /// provider + key + optional base_url/model overrides.
    pub fn set_provider_config(
        &self,
        provider: Provider,
        api_key: &str,
        base_url: Option<String>,
        model: Option<String>,
    ) -> Result<(), String> {
        // Trim key + optional fields (a padded key would keep its whitespace
        // and 401 at send time; the UI trims too, but the IPC boundary is the
        // enforcement point — one writer, one rule).
        let key = api_key.trim();
        let config = ProviderConfig {
            provider,
            api_key: key.to_string(),
            base_url: base_url
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            model: model
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        };
        let stored = config.to_stored_string().map_err(|e| e.to_string())?;
        self.secret.set_key(&stored).map_err(|e| e.to_string())
    }

    /// Read the stored provider config WITHOUT the api key. This is
    /// what [`ai_get_provider_config`] returns; exposed `pub` for tests. The
    /// key itself NEVER crosses this seam toward the frontend — only the
    /// `has_key` boolean. A legacy bare key reads as Anthropic + defaults
    /// (`ProviderConfig::from_stored_string` backward compat); an empty store
    /// reads as the Anthropic-default info with `has_key: false` (the
    /// onboarding form's initial state), never an error.
    pub fn get_provider_config(&self) -> Result<ProviderConfigInfo, String> {
        match self.secret.get_key().map_err(|e| e.to_string())? {
            None => Ok(ProviderConfigInfo {
                provider: Provider::Anthropic,
                base_url: None,
                model: None,
                has_key: false,
            }),
            Some(stored) => {
                let config =
                    ProviderConfig::from_stored_string(&stored).map_err(|e| e.to_string())?;
                Ok(ProviderConfigInfo {
                    provider: config.provider,
                    base_url: config.base_url,
                    model: config.model,
                    has_key: true,
                })
            }
        }
    }

    /// Remove the API key (idempotent). This is what [`ai_remove_api_key`]
    /// calls; exposed `pub` for tests.
    pub fn remove_api_key(&self) -> Result<(), String> {
        self.secret.remove_key().map_err(|e| e.to_string())
    }

    /// Seed the RAW stored secret string (test support): lets
    /// integration tests simulate a legacy install (a bare Anthropic key
    /// stored by the old `ai_set_api_key` command) without reaching into the
    /// private store. Rust-API test surface only — never a Tauri command.
    pub fn set_stored_secret_for_tests(&self, stored: &str) -> Result<(), String> {
        self.secret.set_key(stored).map_err(|e| e.to_string())
    }

    /// Read the RAW stored secret string (test support): pins what
    /// `set_provider_config` actually wrote (the ProviderConfig JSON
    /// vs a bare key). Rust-API test surface only — never a Tauri command.
    pub fn stored_secret_for_tests(&self) -> Result<Option<String>, String> {
        self.secret.get_key().map_err(|e| e.to_string())
    }

    /// Read the current source buffer (for tests + accept/reject).
    #[must_use]
    pub fn source(&self) -> String {
        self.source.lock().expect("source lock").clone()
    }

    /// Clone the `Arc<Mutex<String>>` backing the source buffer, so the
    /// [`ai_turn`] bridge task (on a `spawn_blocking` thread) can write the
    /// final source back with `'static` lifetime.
    #[must_use]
    fn source_arc(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.source)
    }

    /// Clone the `Arc<Mutex<HashSet<String>>>` backing the rejected-id set, so
    /// the [`ai_turn`] bridge task can read it (after the loop) with a `'static`
    /// lifetime when reconstructing the final source. See [`ai_reject_edit`].
    #[must_use]
    fn rejected_arc(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.rejected)
    }

    /// Clone the `Arc` backing the in-flight turn slot, so the [`ai_turn`]
    /// bridge task can clear it (with a `'static` lifetime) when the turn ends.
    #[must_use]
    fn cancel_arc(&self) -> Arc<Mutex<Option<CancellationToken>>> {
        Arc::clone(&self.cancel)
    }

    /// Whether an AI turn is currently registered (running or still stopping).
    #[must_use]
    pub fn turn_in_flight(&self) -> bool {
        self.cancel.lock().expect("cancel lock").is_some()
    }

    /// Claim the in-flight slot for a new turn: an atomic check-and-store
    /// under ONE lock. Errs if any turn is still registered — running, or
    /// cancelled but not yet finished (a cancel cannot interrupt an in-flight
    /// HTTP send, so the old turn may still be draining). On success returns
    /// the turn's [`CancellationToken`] plus a [`TurnSlotGuard`] that clears
    /// the slot when dropped, i.e. when the turn's bridge task ends.
    ///
    /// Previously [`ai_turn`] checked `!is_cancelled()` and never cleared the
    /// slot, so every turn after the first failed with "already in flight";
    /// the check and the store also took the lock separately (a race between
    /// two concurrent calls).
    pub fn try_begin_turn(&self) -> Result<(CancellationToken, TurnSlotGuard), String> {
        let mut slot = self.cancel.lock().expect("cancel lock");
        if let Some(existing) = slot.as_ref() {
            return Err(if existing.is_cancelled() {
                "the previous AI turn is still stopping; try again in a moment".to_string()
            } else {
                "an AI turn is already in flight".to_string()
            });
        }
        let cancel = CancellationToken::new();
        *slot = Some(cancel.clone());
        drop(slot);
        Ok((cancel, TurnSlotGuard(self.cancel_arc())))
    }

    /// Set the source buffer (for tests).
    pub fn set_source(&self, src: String) {
        *self.source.lock().expect("source lock") = src;
    }

    /// The disk path of the file the user opened via [`open_file_cmd`], or
    /// `None` when the editor holds an in-memory buffer. Read by
    /// [`ai_accept_edit`] (via [`accept_edit_inner`]) to derive the per-project
    /// `.noos/` dir for memory writes — the agent loop's `edit_code` patch path
    /// is always the relative [`WORKSPACE_PATH`], so the disk path must come
    /// from here. Exposed `pub` so integration tests drive the open flow
    /// without a Tauri runtime.
    #[must_use]
    pub fn opened_path(&self) -> Option<String> {
        self.opened_path.lock().expect("opened_path lock").clone()
    }

    /// Record the disk path of the file the user opened. Called by
    /// [`open_file_cmd`] (via the pure [`open_file_with_state`] seam) after a
    /// successful open. Passing `Some(abs_path)` replaces any prior opened
    /// path; passing `None` clears it. Exposed `pub` so integration tests
    /// drive the open flow.
    pub fn set_opened_path(&self, path: Option<String>) {
        *self.opened_path.lock().expect("opened_path lock") = path;
    }
}

/// Releases the in-flight turn slot when dropped. Held by the [`ai_turn`]
/// bridge task for the lifetime of the turn; dropping it (at the end of the
/// task, or while unwinding from a panic) clears the slot so the next
/// [`ai_turn`] is accepted.
pub struct TurnSlotGuard(Arc<Mutex<Option<CancellationToken>>>);

impl Drop for TurnSlotGuard {
    fn drop(&mut self) {
        // Never panic in drop: recover the slot even from a poisoned lock.
        let mut slot = match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = None;
    }
}

// ---------------------------------------------------------------------------
// Pure seam: build the initial agent Request
// ---------------------------------------------------------------------------

/// Build the initial Messages-API [`Request`] for a turn: the assembled system
/// prompt (stable + volatile blocks) plus one user message carrying the design
/// intent, with the six morphe tools attached.
///
/// Pure (no I/O, no Tauri types) so tests assert the request shape directly.
#[must_use]
pub fn build_turn_request(
    source: &str,
    design_intent: Option<&str>,
    prior_memory: Option<&str>,
    config: &ClientConfig,
) -> Request {
    let context = PromptContext {
        // External-sourced fields go through the NAMED constructors — the
        // length caps are bound inside the type, so call sites cannot pass a
        // cap. The verbatim user message travels in
        // Request.messages; these slots are the sanitized system-prompt
        // echoes.
        current_code: Some(SanitizedText::code(source)),
        design_intent: design_intent.map(SanitizedText::intent),
        prior_memory: prior_memory.map(SanitizedText::memory),
        ..PromptContext::default()
    };
    Request {
        // model + max_tokens come from the CLIENT's config — the stored ProviderConfig's provider/model/base_url reach the
        // wire end-to-end. Hardcoding ClientConfig::default().model here sent
        // "claude-sonnet-5" to OpenAI-compatible endpoints (provider 400 on
        // every turn with a stored model override).
        model: config.model.clone(),
        max_tokens: config.max_tokens,
        system: assemble_system_prompt(&context),
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![InputContentBlock::Text {
                text: design_intent
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "Iterate on the current source.".to_string()),
            }]),
        }],
        tools: tool_definitions(),
        tool_choice: Some(ToolChoice::Auto),
        thinking: ThinkingConfig::adaptive(),
        stream: true,
    }
}

// ---------------------------------------------------------------------------
// Pure seam: revert an edit_code patch in a source string
// ---------------------------------------------------------------------------

/// Revert a single `edit_code` patch (`old`→`new`) in `source` by re-applying
/// `new`→`old`. Returns the reverted source, or `Err` if `new` matches 0 or >1
/// times (the patch can't be uniquely inverted — the source diverged).
///
/// Pure so tests drive it directly; [`ai_reject_edit`] shells over it.
pub fn revert_patch(source: &str, old: &str, new: &str) -> Result<String, String> {
    let matches: Vec<usize> = source.match_indices(new).map(|(i, _)| i).collect();
    match matches.len() {
        0 => Err(format!(
            "revert failed: `new` text not found (the source may have moved on): {new:?}"
        )),
        1 => {
            let idx = matches[0];
            let mut out = String::with_capacity(source.len() - new.len() + old.len());
            out.push_str(&source[..idx]);
            out.push_str(old);
            out.push_str(&source[idx + new.len()..]);
            Ok(out)
        }
        n => Err(format!(
            "revert failed: `new` text matches {n} times (cannot uniquely invert); widen the patch to disambiguate"
        )),
    }
}

// ---------------------------------------------------------------------------
// run_turn_blocking — the testable core of ai_turn (pure seam)
// ---------------------------------------------------------------------------

/// The outcome of a completed turn: the serialized stop reason and the final
/// workspace source (after all successful `edit_code` patches applied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    /// `"end_turn"`, `"cancelled"`, `"round_cap"`, `"max_tokens"`, `"refusal"`,
    /// or `"error"` — see [`stop_reason_str`].
    pub stop_reason: String,
    /// The workspace's `.scad` after all successful `edit_code` patches
    /// (reconstructed via [`reconstruct_final_source`]).
    pub final_source: String,
}

/// Run one agent turn synchronously, calling `on_event` for each [`AiEvent`]
/// (including the terminal [`AiEvent::Done`]). This is the testable core of
/// [`ai_turn`]: it takes an injected [`Client`] (so tests use a scripted
/// transport — no real API) and a caller-supplied callback (so the command
/// forwards over a [`Channel`] while a test collects into a `Vec`).
///
/// `rejected` carries the tool-use ids the user rejected mid-turn; the final
/// source is reconstructed with those edits skipped.
///
/// `noos_dir` threads the project's `.noos` dir (derived from
/// [`AppState::opened_path`] in [`ai_turn`]) into the [`Dispatcher`] so
/// `query_noos` can shell to `noos query` against the opened project's graph.
/// `None` (no file open) → `query_noos` degrades to a useful `Ok` message.
///
/// # Async safety
///
/// This blocks on `run_on_thread`'s OS thread (draining the sync `mpsc::Receiver`
/// and joining the handle). It never calls the sync transport itself. In the
/// Tauri command it runs inside [`tauri::async_runtime::spawn_blocking`], so no
/// async-runtime worker is blocked. In tests it runs on the test thread.
///
/// # Panics
///
/// A panic in the loop thread (`join()` → `Err`) OR anywhere in this bridge is
/// caught with `catch_unwind` and surfaced as an error turn: `on_event` receives
/// a terminal [`AiEvent::Done`] with `stop_reason: "error"` and the initial
/// source, and this function returns a [`TurnOutcome`] with `stop_reason:
/// "error"`. It never panics up to the caller — the frontend always gets a
/// `Done` (otherwise a panic would be silently swallowed by Tauri's
/// dropped `spawn_blocking` JoinHandle and ChatPane would hang `streaming=true`).
// Tauri→OS-thread bridge: the params are independent cross-cutting concerns
// (transport, two prompt slots, cancellation, reject set, memory dir, event
// sink) that a wrapper struct would merely re-bundle — clippy's 7-param budget
// is a heuristic, not an invariant worth a refactor at this seam.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_blocking<F: FnMut(AiEvent)>(
    client: Client,
    source: &str,
    design_intent: Option<&str>,
    prior_memory: Option<&str>,
    cancel: CancellationToken,
    rejected: Arc<Mutex<HashSet<String>>>,
    noos_dir: Option<PathBuf>,
    opened_path: Option<PathBuf>,
    mut on_event: F,
) -> TurnOutcome {
    let workspace =
        morphe_ai::InMemoryWorkspace::with_files([(WORKSPACE_PATH, source.to_string())]);
    // The compile/get_geometry_stats tools get
    // the same open-file source-dir context as the desktop compile IPC and
    // the CLI — AI-turn compiles of import()-bearing sources resolve against
    // the project dir, not the process CWD.
    let dispatcher = Dispatcher::new(Box::new(workspace), noos_dir).with_source_path(opened_path);
    let agent_request = build_turn_request(source, design_intent, prior_memory, client.config());

    // Launch the loop on its own OS thread (returns instantly).
    let (rx, join_handle) = morphe_ai::run_on_thread(agent_request, client, dispatcher, cancel);

    // Bridge body: drain streamed events, join the handle, reconstruct the
    // final source (skipping user-rejected edits). Wrapped in `catch_unwind`
    // so a panic in the loop thread (join → Err) OR anywhere in this
    // bridge surfaces as an error turn, never a silent hang. Without this the
    // panic re-panics on the `spawn_blocking` thread and Tauri drops that
    // thread's JoinHandle (the command never awaits it), so no `AiEvent::Done`
    // would ever reach the frontend.
    let bridge = std::panic::AssertUnwindSafe(|| {
        while let Ok(event) = rx.recv() {
            on_event(AiEvent::from_loop_event(&event));
        }
        // join() → Err means the loop thread panicked; `.ok()` collapses that to
        // None so the outer match surfaces an error turn. On a clean join,
        // reconstruct the final source excluding user-rejected edits.
        join_handle.join().ok().map(|loop_result| {
            // Clone under the lock then release, so a panic in reconstruct can't
            // poison the shared mutex.
            let rejected_set = rejected.lock().expect("rejected lock").clone();
            let stop_reason = stop_reason_str(&loop_result.stop_reason);
            let final_source = reconstruct_final_source(&loop_result.events, source, &rejected_set);
            (stop_reason, final_source)
        })
    });

    let (stop_reason, final_source) = match std::panic::catch_unwind(bridge) {
        Ok(Some((sr, fs))) => (sr, fs),
        // Loop thread panicked (join → None) or the bridge itself caught a
        // panic: surface an error turn seeded from the initial source so the
        // frontend ends streaming cleanly.
        _ => ("error".to_string(), source.to_string()),
    };

    on_event(AiEvent::Done {
        stop_reason: stop_reason.clone(),
        final_source: final_source.clone(),
    });
    TurnOutcome {
        stop_reason,
        final_source,
    }
}

// ---------------------------------------------------------------------------
// IPC commands
// ---------------------------------------------------------------------------

/// The provider-config view returned to the frontend. Mirrors the
/// stored [`ProviderConfig`] WITHOUT the api_key — the secret never crosses
/// IPC toward the frontend (only `has_key`). `base_url`/`model` are the STORED
/// overrides (`None` → the provider's defaults, resolved client-side for
/// display).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderConfigInfo {
    pub provider: Provider,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub has_key: bool,
}

/// `ai_has_api_key` — ask the [`SecretStore`] whether a key is set.
#[tauri::command]
pub fn ai_has_api_key(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.secret.get_key().map_err(|e| e.to_string())?.is_some())
}

/// `ai_get_provider_config` — the stored provider config WITHOUT the key.
/// Never errors on an empty store (returns the Anthropic-default info with
/// `has_key: false`); a legacy bare key reads as Anthropic + defaults.
#[tauri::command]
pub fn ai_get_provider_config(state: State<'_, AppState>) -> Result<ProviderConfigInfo, String> {
    state.get_provider_config()
}

/// `ai_set_provider_config` — store the full provider credential: provider +
/// api_key + optional base_url/model overrides (OpenRouter / Azure / local
/// LLMs). The ONLY credential write path (it replaced the older
/// `ai_set_api_key` — one writer, no bare-key/code-config divergence; existing stored bare keys
/// keep working via `ProviderConfig::from_stored_string`).
#[tauri::command]
pub fn ai_set_provider_config(
    provider: Provider,
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.set_provider_config(provider, &api_key, base_url, model)
}

/// `ai_remove_api_key` — delete the key from the [`SecretStore`] (idempotent).
#[tauri::command]
pub fn ai_remove_api_key(state: State<'_, AppState>) -> Result<(), String> {
    state.secret.remove_key().map_err(|e| e.to_string())
}

/// `ai_turn` — run one agent turn and stream [`AiEvent`]s to the frontend.
///
/// 1. Reads the API key from the [`SecretStore`] (errs `"no API key set"` if
///    absent) and builds the [`Client`] (`client_from_secret`).
/// 2. Creates a [`CancellationToken`], stores it in [`AppState`] for
///    [`ai_cancel`].
/// 3. Spawns a [`tauri::async_runtime::spawn_blocking`] task that calls
///    [`run_turn_blocking`] with the [`Channel`] as the event callback. The loop
///    runs on its own OS thread (via `run_on_thread`); the command never calls
///    the sync transport. On completion the task writes the reconstructed final
///    source to [`AppState`] and [`AiEvent::Done`] is forwarded over the channel.
///
/// Returns `Ok(())` immediately — all results arrive via `on_event`.
#[tauri::command]
pub async fn ai_turn(
    request: AiTurnRequest,
    on_event: Channel<AiEvent>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 1. Build the client from the stored provider config (ONE store
    //    read — the stored ProviderConfig drives provider/key/base_url/model;
    //    the default config supplies only max_tokens/timeout).
    let client: Client = client_from_secret(state.secret.as_ref(), ClientConfig::default())
        .map_err(|e| {
            if matches!(e, morphe_ai::SecretError::NotFound) {
                "no API key set".to_string()
            } else {
                e.to_string()
            }
        })?;

    // 1b + 2. Guard + claim the in-flight slot atomically (see
    //     `AppState::try_begin_turn`); the guard moves into the bridge task and
    //     releases the slot when the turn ends. Reset the per-turn rejected set.
    let (cancel, slot_guard) = state.try_begin_turn()?;
    state.rejected.lock().expect("rejected lock").clear();

    // 3. Run the turn on a blocking-pool thread, forwarding AiEvents over the
    //    Channel as they arrive. run_turn_blocking never calls the sync
    //    transport — only rx.recv() and join_handle.join() — so no async-runtime
    //    worker blocks.
    //
    //    Derive the project's .noos dir from the open file (the
    //    Dispatcher's query_noos tool will shell to `noos query` against it).
    //    None when no file is open → query_noos degrades to a useful message.
    let noos_dir = state.opened_path().as_deref().and_then(noos_dir_for_path);
    let opened_path = state.opened_path().as_deref().map(std::path::PathBuf::from);
    // Build the session-start memory digest from the project's .noos
    // sidecar (morphe-ai reads morphe-events.jsonl directly — no noos
    // subprocess). It renders into the system prompt's `## Project memory`
    // section so the AI cites prior design constraints unprompted.
    // None when no file is open / no sidecar → no section renders.
    let opened_filename = state
        .opened_path()
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let prior_memory = noos_dir.as_deref().and_then(|d| {
        morphe_ai::build_session_digest(d, opened_filename.as_deref(), WORKSPACE_PATH)
    });
    let source_slot = state.source_arc();
    let rejected_slot = state.rejected_arc();
    let initial_source = request.source.clone();
    let design_intent = request.design_intent.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Clears the in-flight slot when this task ends (normally or by
        // unwinding), so the next turn can start.
        let _slot_guard = slot_guard;
        let outcome = run_turn_blocking(
            client,
            &initial_source,
            design_intent.as_deref(),
            prior_memory.as_deref(),
            cancel,
            rejected_slot,
            noos_dir,
            opened_path,
            |ev| {
                let _ = on_event.send(ev);
            },
        );
        // Persist the final source so accept/reject + the next turn see it.
        *source_slot.lock().expect("source lock") = outcome.final_source;
    });

    Ok(())
}

/// `ai_cancel` — set the current turn's [`CancellationToken`]. The loop checks
/// it at the top of each round and after each transport call, so cancellation
/// takes effect within one round (it cannot interrupt a blocking `send`
/// mid-flight). No-op if no turn is running.
#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.cancel.lock().expect("cancel lock").as_ref() {
        cancel.cancel();
    }
    Ok(())
}

/// `ai_accept_edit` — acknowledge that an `edit_code` patch is accepted. The
/// loop already applied it to the workspace (and `Done` carried the final
/// source), so this returns the current source buffer for the frontend to sync
/// its editor to. No buffer mutation (the edit is already applied).
///
/// Every accepted edit also emits a [`morphe_ai::DesignEvent`]
/// `Decision { path, old, new }` into the opened project's `.noos/` via
/// [`morphe_ai::write_event`], so each accept becomes a queryable design-memory
/// node. The noos dir is derived from [`AppState::opened_path`] (the disk path
/// the user opened via [`open_file_cmd`]); the `path` arg here is the
/// `edit_code` patch path (always the relative [`WORKSPACE_PATH`] — the
/// logical edit target, carried as the Decision payload). When no file is open
/// (`opened_path == None`), the emit is skipped + logged. The emit is
/// **best-effort**: a memory-write failure is logged and swallowed so the
/// accept's primary job (returning the source) survives — the
/// editor sync must not break on a memory-write failure.
///
/// This command is a thin shell over [`accept_edit_inner`]; tests drive the
/// inner directly with a constructed [`AppState`] (no Tauri runtime).
///
/// `async` + `spawn_blocking`: the memory write takes an exclusive `flock` on
/// the project's `.noos/write.lock` (no timeout — a long external `noos scan`
/// can hold it), so it must not run on the main thread like a sync command.
#[tauri::command]
pub async fn ai_accept_edit(
    path: String,
    old: String,
    new: String,
    app: tauri::AppHandle,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        accept_edit_inner(path, old, new, app.state::<AppState>().inner())
    })
    .await
    .map_err(|e| format!("internal error: accept task failed: {e}"))
}

/// Pure inner of [`ai_accept_edit`]: derive the noos dir from the open file's
/// disk path ([`AppState::opened_path`], set by [`open_file_cmd`]), emit a
/// `Decision` event for the accepted edit, and return the current source
/// buffer. Extracted as a pub(crate) seam so tests drive the real
/// opened_path → noos_dir flow without a Tauri runtime.
///
/// `path` is the `edit_code` patch path (always the relative
/// [`WORKSPACE_PATH`]); it is carried as the Decision payload's `path` field
/// (the logical edit target) and is NOT used to derive the noos dir. The
/// production emit path is alive only when the user has opened a file
/// (`opened_path == Some(abs_disk_path)`); otherwise the emit is skipped +
/// logged.
pub(crate) fn accept_edit_inner(
    path: String,
    old: String,
    new: String,
    state: &AppState,
) -> String {
    // Capture opened_path ONCE: reading state.opened_path() twice would let a
    // file-switch between the two lock acquisitions write to the old
    // project's .noos with the new file's filename. Capturing once removes
    // the race.
    let opened = state.opened_path();
    match opened.as_deref().and_then(noos_dir_for_path) {
        Some(noos_dir) => {
            // Record the REAL opened filename as the Decision
            // path, not the edit_code patch path (always WORKSPACE_PATH
            // "main.scad"). The real name disambiguates files that share a
            // project dir. Fall back to the patch path only if file_name is
            // somehow None (preserves the prior behavior in that edge case).
            let real_path = opened
                .as_deref()
                .and_then(|p| Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| path.clone());
            emit_decision_event(&real_path, &old, &new, &noos_dir)
        }
        None => eprintln!(
            "[morphe] ai_accept_edit: no opened file (opened_path is None or has no derivable \
             .noos) — skipping event emit"
        ),
    }
    state.source()
}

/// Derive the per-project `.noos` dir for an opened-file disk `path`.
///
/// `noos_dir = path.parent()/.noos`. Returns `None` when:
///   - `path` is empty (no file open / in-memory only),
///   - `path` is a relative bare filename with an empty parent (the
///     [`WORKSPACE_PATH`] "main.scad" case — single-file workspace, no
///     per-project dir), or
///   - `path`'s parent is relative (defense-in-depth: the OS file dialog
///     always returns an absolute disk path, so a relative parent can only
///     reach here if a caller bypasses [`open_file_cmd`] — refuse rather than
///     write into an unintended cwd/.noos).
///
/// In every `None` case the caller skips the emit + logs.
///
/// Pure (no I/O) so tests drive it directly. The Tauri command is a thin shell
/// over this + [`emit_decision_event`].
pub(crate) fn noos_dir_for_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let parent = Path::new(path).parent()?;
    if parent.as_os_str().is_empty() {
        return None; // relative bare filename → WORKSPACE_PATH case
    }
    if !parent.is_absolute() {
        return None; // defense-in-depth: OS dialog always returns absolute
    }
    Some(parent.join(".noos"))
}

/// Pure emit seam: build a `Decision` event from the accepted edit and write it
/// into `noos_dir` (sidecar line + graph node). Best-effort — a
/// [`morphe_ai::WriteError`] is logged and swallowed so the accept's primary
/// job (returning the source) survives a memory-write failure.
///
/// **Retry queue:** (1) drain any backlog first via
/// [`morphe_ai::flush_queue`] (best-effort — failures logged, do not break the
/// emit); (2) attempt the live [`morphe_ai::write_event`]; (3) on Err,
/// [`morphe_ai::enqueue_event`] the DesignEvent into
/// `<noos_dir>/morphe-queue.jsonl` and log the new queue depth (sustained
/// failure must not be fully silent). The next emit / open re-attempts the
/// backlog, so a transient memory-write failure does not silently lose data.
///
/// `noos_dir` must already be derivable (caller ran [`noos_dir_for_path`]);
/// `write_event` / `enqueue_event` create the dir + the single-writer lockfile
/// as needed.
fn emit_decision_event(path: &str, old: &str, new: &str, noos_dir: &Path) {
    let event = morphe_ai::DesignEvent::Decision {
        path: path.to_string(),
        old: old.to_string(),
        new: new.to_string(),
    };

    // (1) Drain backlog FIRST (best-effort). A prior emit that failed left
    // events in the queue; try to land them before recording this one so the
    // queue doesn't grow when the substrate is healthy again.
    if let Err(e) = morphe_ai::flush_queue(noos_dir) {
        eprintln!(
            "[morphe] ai_accept_edit: flush_queue failed for {}: {e} \
             (will retry on next emit/open; the accept itself is unaffected)",
            noos_dir.display()
        );
    }

    // (2) Attempt the live write. On failure, (3) enqueue for retry.
    if let Err(e) = morphe_ai::write_event(&event, noos_dir) {
        eprintln!(
            "[morphe] ai_accept_edit: noos write_event failed for {}: {e} \
             (enqueuing for retry; the accept itself is unaffected)",
            noos_dir.display()
        );
        match morphe_ai::enqueue_event(&event, noos_dir) {
            Ok(depth) if depth > 0 => eprintln!(
                "[morphe] ai_accept_edit: event enqueued for retry at <noos>/morphe-queue.jsonl \
                 (queue depth: {depth})"
            ),
            // depth == 0 only happens if a concurrent flush drained between
            // the write_event failure and enqueue — still success, no log.
            Ok(_) => (),
            Err(qe) => eprintln!(
                "[morphe] ai_accept_edit: enqueue ALSO failed for {}: {qe} \
                 (this event is lost; the accept itself is unaffected)",
                noos_dir.display()
            ),
        }
    }
}

/// `ai_reject_edit` — reject a single `edit_code` patch.
///
/// Records the tool-use `id` in the per-turn rejected set FIRST, so the
/// bridge's `final_source` reconstruction skips it whenever the reject lands
/// before the turn finishes. Then, best-effort, re-applies `new`→`old`
/// on the backend source buffer. That buffer only holds the agent's final
/// source after `Done`, so mid-turn the revert usually finds nothing to undo —
/// that is expected and NOT an error (previously the command errored mid-turn
/// before recording the id, so the rejected edit leaked into the final source
/// and the frontend button hung). The editor buffer is the frontend's and is
/// never touched by a reject. Returns the (possibly reverted) source buffer.
/// `path` is carried for API symmetry (single-file workspace).
#[tauri::command]
pub fn ai_reject_edit(
    path: String,
    old: String,
    new: String,
    id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    Ok(reject_edit_inner(&path, &old, &new, id, state.inner()))
}

/// Pure inner of [`ai_reject_edit`] (testable without a Tauri runtime).
pub fn reject_edit_inner(path: &str, old: &str, new: &str, id: String, state: &AppState) -> String {
    let _ = path; // single-file workspace; path is informational.
    state.rejected.lock().expect("rejected lock").insert(id);
    let mut src = state.source.lock().expect("source lock");
    if let Ok(reverted) = revert_patch(&src, old, new) {
        *src = reverted;
    }
    src.clone()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn revert_patch_succeeds_when_new_text_present_once() {
        let src = "cube(2);\n";
        let out = revert_patch(src, "cube(1);", "cube(2);").expect("revert");
        assert_eq!(out, "cube(1);\n");
    }

    #[test]
    fn revert_patch_errors_when_new_text_absent() {
        let src = "cube(1);\n";
        let err = revert_patch(src, "cube(1);", "cube(3);").expect_err("absent");
        assert!(err.contains("not found"));
    }

    #[test]
    fn revert_patch_errors_when_new_text_matches_multiple_times() {
        let src = "cube(2);\ncube(2);\n";
        let err = revert_patch(src, "cube(1);", "cube(2);").expect_err("ambiguous");
        assert!(err.contains("2 times"));
    }

    #[test]
    fn reconstruct_final_source_applies_successful_edit_patches_in_order() {
        // Two successful edit_code patches: cube(1)→cube(2), then ;→; // done.
        let events = vec![
            LoopEvent::ToolCall {
                id: "t1".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "cube(1);", "new": "cube(2);"}),
            },
            LoopEvent::ToolResult {
                id: "t1".into(),
                content: "edited main.scad".into(),
                is_error: false,
            },
            LoopEvent::ToolCall {
                id: "t2".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "cube(2);", "new": "cube(2); // done"}),
            },
            LoopEvent::ToolResult {
                id: "t2".into(),
                content: "edited main.scad".into(),
                is_error: false,
            },
        ];
        let out = reconstruct_final_source(&events, "cube(1);\n", &HashSet::new());
        assert_eq!(out, "cube(2); // done\n");
    }

    #[test]
    fn reconstruct_final_source_skips_failed_edits() {
        // A failed edit (is_error=true) does not apply.
        let events = vec![
            LoopEvent::ToolCall {
                id: "t1".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "missing", "new": "x"}),
            },
            LoopEvent::ToolResult {
                id: "t1".into(),
                content: "`old` not found".into(),
                is_error: true,
            },
        ];
        let out = reconstruct_final_source(&events, "cube(1);\n", &HashSet::new());
        assert_eq!(out, "cube(1);\n", "failed edit leaves source unchanged");
    }

    #[test]
    fn reconstruct_final_source_ignores_non_edit_tools() {
        // compile/get_geometry_stats do not change the source.
        let events = vec![
            LoopEvent::ToolCall {
                id: "t1".into(),
                name: "compile".into(),
                input: json!({"source": "cube(1);"}),
            },
            LoopEvent::ToolResult {
                id: "t1".into(),
                content: "{\"ok\":true}".into(),
                is_error: false,
            },
        ];
        let out = reconstruct_final_source(&events, "cube(1);\n", &HashSet::new());
        assert_eq!(out, "cube(1);\n");
    }

    #[test]
    fn reconstruct_final_source_skips_rejected_edits() {
        // An edit_code the user rejected must NOT apply to the reconstructed
        // final source — otherwise the bridge silently overwrites the reject and
        // the next turn's seed (AppState.source) diverges from the editor.
        let events = vec![
            LoopEvent::ToolCall {
                id: "t1".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "cube(1);", "new": "cube(2);"}),
            },
            LoopEvent::ToolResult {
                id: "t1".into(),
                content: "edited main.scad".into(),
                is_error: false,
            },
            LoopEvent::ToolCall {
                id: "t2".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "cube(2);", "new": "cube(3);"}),
            },
            LoopEvent::ToolResult {
                id: "t2".into(),
                content: "edited main.scad".into(),
                is_error: false,
            },
        ];
        // Reject only t1: the source stays at cube(1) (t1 skipped) and t2's old
        // ("cube(2);") no longer matches, so it's a no-op too → source unchanged.
        let rejected: HashSet<String> = ["t1".to_string()].into_iter().collect();
        let out = reconstruct_final_source(&events, "cube(1);\n", &rejected);
        assert_eq!(
            out, "cube(1);\n",
            "rejected edit is skipped — source reflects the reject"
        );

        // Sanity: with NO rejects, both edits apply (reject handling doesn't
        // regress the happy path).
        let out_all = reconstruct_final_source(&events, "cube(1);\n", &HashSet::new());
        assert_eq!(out_all, "cube(3);\n");
    }

    #[test]
    fn reconstruct_final_source_applies_independent_edit_after_reject() {
        // Once the replay has diverged (t1 rejected), an INDEPENDENT later
        // edit (t2) whose `old` IS present in the initial source must still apply.
        // This pins the post-divergence successful-apply branch
        // (`replay_diverged == true` AND `first_match == Some`), which the
        // reject-cascade test above doesn't cover (there t2's `old` was absent
        // because it depended on t1).
        let events = vec![
            LoopEvent::ToolCall {
                id: "t1".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "cube(1);", "new": "cube(2);"}),
            },
            LoopEvent::ToolResult {
                id: "t1".into(),
                content: "edited main.scad".into(),
                is_error: false,
            },
            LoopEvent::ToolCall {
                id: "t2".into(),
                name: "edit_code".into(),
                input: json!({"path": "main.scad", "old": "// header", "new": "// header v2"}),
            },
            LoopEvent::ToolResult {
                id: "t2".into(),
                content: "edited main.scad".into(),
                is_error: false,
            },
        ];
        // Reject only t1: the replay diverges (cube(1) stays), but t2 is
        // independent — `// header` is present in the initial source — so it
        // still applies even after the divergence flag is set.
        let rejected: HashSet<String> = ["t1".to_string()].into_iter().collect();
        let out = reconstruct_final_source(&events, "// header\ncube(1);\n", &rejected);
        assert_eq!(
            out, "// header v2\ncube(1);\n",
            "independent edit after a rejected edit still applies post-divergence"
        );
    }

    #[test]
    fn ai_event_maps_assistant_text() {
        let event = LoopEvent::AssistantText("hello".to_string());
        let ai = AiEvent::from_loop_event(&event);
        match ai {
            AiEvent::Text { text } => assert_eq!(text, "hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn ai_event_maps_tool_call() {
        let event = LoopEvent::ToolCall {
            id: "toolu_1".to_string(),
            name: "edit_code".to_string(),
            input: json!({"path": "main.scad", "old": "a", "new": "b"}),
        };
        let ai = AiEvent::from_loop_event(&event);
        match ai {
            AiEvent::ToolCall { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "edit_code");
                assert_eq!(input["old"], "a");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn ai_event_maps_tool_result() {
        let event = LoopEvent::ToolResult {
            id: "toolu_1".to_string(),
            content: "{\"ok\":true}".to_string(),
            is_error: false,
        };
        let ai = AiEvent::from_loop_event(&event);
        match ai {
            AiEvent::ToolResult {
                id,
                content,
                is_error,
            } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(content, "{\"ok\":true}");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn stop_reason_str_is_stable() {
        assert_eq!(stop_reason_str(&LoopStopReason::EndTurn), "end_turn");
        assert_eq!(stop_reason_str(&LoopStopReason::Cancelled), "cancelled");
        assert_eq!(stop_reason_str(&LoopStopReason::RoundCap), "round_cap");
        assert_eq!(stop_reason_str(&LoopStopReason::MaxTokens), "max_tokens");
        assert_eq!(stop_reason_str(&LoopStopReason::Refusal), "refusal");
        assert_eq!(
            stop_reason_str(&LoopStopReason::Error("boom".to_string())),
            "error"
        );
    }

    #[test]
    fn build_turn_request_carries_source_and_design_intent() {
        // The client's config drives model/max_tokens (regression guard: if
        // these were hardcoded from ClientConfig::default(), a stored OpenAI
        // model override would never reach the wire).
        let config = ClientConfig {
            model: "gpt-4o-mini".to_string(),
            max_tokens: 2048,
            ..ClientConfig::default()
        };
        let req = build_turn_request("cube(1);", Some("a 10mm cube"), None, &config);
        assert_eq!(req.tools.len(), 6, "all six tools attached");
        assert_eq!(req.model, "gpt-4o-mini", "model from the client config");
        assert_eq!(req.max_tokens, 2048, "max_tokens from the client config");
        assert!(!req.system.is_empty(), "system prompt assembled");
        // The volatile system block renders the current code + design intent.
        let vol = &req.system[1].text;
        assert!(
            vol.contains("cube(1);"),
            "volatile block carries current code"
        );
        assert!(
            vol.contains("10mm cube"),
            "volatile block carries design intent"
        );
        // One user message carrying the design intent.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn build_turn_request_threads_prior_memory_into_prompt() {
        // prior_memory must reach the RENDERED system prompt, not merely exist
        // as a parameter: a digest that build_turn_request accepts but drops
        // would fail here — the `## Project memory` section and the
        // constraint content must appear in the volatile block.
        let req = build_turn_request(
            "cube(1);",
            None,
            Some("### Constraints\n- wall_thickness: 2mm"),
            &ClientConfig::default(),
        );
        let vol = &req.system[1].text;
        assert!(
            vol.contains("## Project memory"),
            "prior_memory must render a dedicated '## Project memory' section; got:\n{vol}"
        );
        assert!(
            vol.contains("wall_thickness") && vol.contains("2mm"),
            "the digest content must reach the system prompt; got:\n{vol}"
        );
    }

    #[test]
    fn app_state_source_roundtrips() {
        let state = AppState::new_in_memory();
        assert_eq!(state.source(), "", "empty by default");
        state.set_source("cube(2);".to_string());
        assert_eq!(state.source(), "cube(2);");
    }

    #[test]
    fn app_state_secret_roundtrips_via_in_memory_store() {
        let state = AppState::new_in_memory();
        // The in-memory store starts empty.
        assert!(state.secret.get_key().expect("get").is_none());
        state.secret.set_key("test-key").expect("set on app");
        assert_eq!(state.secret.get_key().unwrap().as_deref(), Some("test-key"));
        state.secret.remove_key().expect("remove");
        assert!(state.secret.get_key().unwrap().is_none());
    }

    #[test]
    fn ai_event_serializes_with_kind_tag() {
        // The frontend routes on the `kind` discriminant; pin the wire shape.
        let ev = AiEvent::Text {
            text: "hi".to_string(),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["kind"], "text");
        assert_eq!(v["text"], "hi");

        let done = AiEvent::Done {
            stop_reason: "end_turn".to_string(),
            final_source: "cube(1);".to_string(),
        };
        let v = serde_json::to_value(&done).unwrap();
        assert_eq!(v["kind"], "done");
        assert_eq!(v["stop_reason"], "end_turn");
        assert_eq!(v["final_source"], "cube(1);");
    }

    // --- A panic in the loop thread must surface as a Done{error}, not a hang

    /// A transport that panics on `send` — injects a panic into the loop's OS
    /// thread to prove the bridge's `catch_unwind` converts it to an error turn.
    struct PanicTransport;
    impl morphe_ai::Transport for PanicTransport {
        fn send(
            &self,
            _req: &Request,
        ) -> Result<morphe_ai::RawResponse, morphe_ai::TransportError> {
            panic!("injected transport panic");
        }
    }

    #[test]
    fn run_turn_blocking_emits_error_done_on_loop_panic() {
        // A panic in the loop thread (here via a panicking transport) must
        // NOT hang — the bridge emits a terminal Done{stop_reason:"error"} and
        // returns a TurnOutcome seeded from the initial source. Without the
        // catch_unwind the panic would be silently swallowed by Tauri's dropped
        // spawn_blocking JoinHandle and ChatPane would stay streaming=true forever.
        let client =
            Client::with_transport(std::sync::Arc::new(PanicTransport), ClientConfig::default());
        let cancel = CancellationToken::new();
        let rejected = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
        let mut events: Vec<AiEvent> = Vec::new();
        let outcome = run_turn_blocking(
            client,
            "cube(1);",
            None,
            None,
            cancel,
            rejected,
            None,
            None,
            |ev| events.push(ev),
        );

        // stop_reason + final_source reflect the error turn.
        assert_eq!(outcome.stop_reason, "error");
        assert_eq!(outcome.final_source, "cube(1);");

        // The terminal streamed event is a Done{error} carrying the initial
        // source — the frontend ends streaming cleanly.
        match events.last() {
            Some(AiEvent::Done {
                stop_reason,
                final_source,
            }) => {
                assert_eq!(stop_reason, "error");
                assert_eq!(final_source, "cube(1);");
            }
            other => panic!("expected terminal Done{{error}}, got {other:?}"),
        }
    }

    #[test]
    fn app_state_rejected_is_cleared_per_turn() {
        // AppState exposes the rejected set; ai_turn clears it at the start
        // of each turn. Here we drive the accessor + clear path directly (the
        // Tauri command wrapper is a thin shell over this).
        let state = AppState::new_in_memory();
        state
            .rejected
            .lock()
            .expect("rejected lock")
            .insert("t1".to_string());
        assert_eq!(
            state.rejected.lock().expect("rejected lock").len(),
            1,
            "reject recorded"
        );
        // ai_turn clears it (simulated).
        state.rejected.lock().expect("rejected lock").clear();
        assert!(
            state.rejected.lock().expect("rejected lock").is_empty(),
            "rejected cleared for the next turn"
        );
    }

    // --- noos_dir derivation + Decision emit ---

    #[test]
    fn noos_dir_derived_from_absolute_path_parent() {
        // Real opened-file disk path → noos_dir = parent/.noos.
        let dir = noos_dir_for_path("/home/me/project/bracket.scad");
        assert_eq!(
            dir,
            Some(std::path::PathBuf::from("/home/me/project/.noos")),
            "absolute path → parent/.noos"
        );
    }

    #[test]
    fn noos_dir_skipped_for_workspace_path_no_parent() {
        // WORKSPACE_PATH ("main.scad") is a relative bare filename with an
        // EMPTY parent → no derivable noos_dir → skip + log.
        assert_eq!(
            noos_dir_for_path(WORKSPACE_PATH),
            None,
            "WORKSPACE_PATH has no parent → no noos_dir"
        );
    }

    #[test]
    fn noos_dir_skipped_for_empty_path() {
        assert_eq!(noos_dir_for_path(""), None, "empty path → no noos_dir");
    }

    #[test]
    fn noos_dir_skipped_for_relative_path_with_parent() {
        // Defense-in-depth: noos_dir_for_path requires
        // an ABSOLUTE parent. The OS file dialog always returns an absolute
        // disk path, so a relative parent can only reach here if a caller
        // bypasses open_file_cmd — refuse rather than write into an unintended
        // cwd/.noos. Without this, a stray relative path like "proj/main.scad"
        // would derive "proj/.noos" relative to the process cwd, silently
        // polluting whatever directory the app was launched from.
        assert_eq!(
            noos_dir_for_path("proj/main.scad"),
            None,
            "relative parent must skip (defense-in-depth; OS dialog always returns absolute)"
        );
    }

    /// Build a fresh temp dir + an absolute `.scad` path inside it, so the emit
    /// seam writes into a real per-project `.noos`. Returns (project_dir,
    /// scad_abs_path).
    fn temp_project(tag: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "morphe-desktop-ai-accept-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir project");
        let scad = dir.join("bracket.scad");
        std::fs::write(&scad, "// seed\n").expect("seed scad");
        (dir, scad.to_string_lossy().into_owned())
    }

    /// Read morphe-events.jsonl under `<project>/.noos` and return its lines.
    fn read_events_sidecar(project: &std::path::Path) -> Vec<serde_json::Value> {
        let path = project.join(".noos").join("morphe-events.jsonl");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read sidecar {}: {e}", path.display()));
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("line parses"))
            .collect()
    }

    #[test]
    fn ai_accept_edit_emits_decision_event_into_project_noos() {
        // Real flow: ai_accept_edit writes a DesignEvent::Decision
        // {path, old, new} into the OPENED FILE's project's `.noos/`, derived
        // from AppState.opened_path (NOT the patch path arg, which is always
        // the relative WORKSPACE_PATH). Regression test: the agent loop always
        // emits edit_code with path = WORKSPACE_PATH, so deriving noos_dir from
        // that path would always yield None and the emit would never happen.
        // noos_dir comes from AppState.opened_path (set by open_file_cmd); the
        // patch path is carried as the Decision payload.
        let (project, scad_abs) = temp_project("emit");
        let state = AppState::new_in_memory();
        state.set_source("cube(2);".to_string());
        // REAL FLOW: open_file_cmd sets opened_path on a successful open.
        state.set_opened_path(Some(scad_abs.clone()));

        // Drive the real ai_accept_edit body. The patch path is WORKSPACE_PATH
        // (always — the agent's logical edit target); the noos_dir comes from
        // state.opened_path via the inner.
        let returned = accept_edit_inner(
            WORKSPACE_PATH.to_string(),
            "cube(1);".to_string(),
            "cube(2);".to_string(),
            &state,
        );
        assert_eq!(returned, "cube(2);", "accept returns the source buffer");

        // Sidecar gained exactly one decision line. payload.path is the REAL
        // opened filename (file_name of scad_abs), NOT the WORKSPACE_PATH alias
        // "main.scad" — so files sharing a project dir don't intermingle.
        let lines = read_events_sidecar(&project);
        assert_eq!(lines.len(), 1, "exactly one sidecar line");
        let line = &lines[0];
        assert_eq!(line["kind"], "decision", "kind discriminant");
        assert_eq!(
            line["payload"]["path"], "bracket.scad",
            "payload.path = the real opened filename, not the WORKSPACE_PATH alias"
        );
        assert_eq!(line["payload"]["old"], "cube(1);", "payload.old");
        assert_eq!(line["payload"]["new"], "cube(2);", "payload.new");

        // Graph gained an event node tagged kind:decision. The node id's subject is derived from the payload
        // path (WORKSPACE_PATH).
        let graph_text =
            std::fs::read_to_string(project.join(".noos").join("graph.json")).expect("read graph");
        let graph: serde_json::Value = serde_json::from_str(&graph_text).expect("parse graph");
        let event_nodes: Vec<&serde_json::Value> = graph["nodes"]
            .as_array()
            .expect("nodes array")
            .iter()
            .filter(|n| n.get("node_type").and_then(serde_json::Value::as_str) == Some("event"))
            .collect();
        assert_eq!(event_nodes.len(), 1, "exactly one event node");
        assert!(
            event_nodes[0]["tags"]
                .as_array()
                .expect("tags")
                .iter()
                .any(|t| t == "kind:decision"),
            "event node tagged kind:decision"
        );

        // The cwd's `.noos/` (where WORKSPACE_PATH's parent would resolve)
        // must NOT gain an event — that's the dead-path bug. The event landed
        // in the opened project's `.noos/`, not the cwd's.
        assert!(
            !std::path::Path::new(".noos/morphe-events.jsonl").exists(),
            "the relative WORKSPACE_PATH must not produce a stray cwd/.noos event"
        );
    }

    #[test]
    fn ai_accept_edit_skips_emit_when_no_file_open() {
        // Real flow: when no file is open (opened_path == None), the
        // emit is skipped + logged + the accept still returns source. This is
        // the dormancy branch — memory activates only when a file is open.
        let state = AppState::new_in_memory();
        state.set_source("cube(2);".to_string());
        // opened_path stays None (no open_file_cmd called).

        // Drive the real inner with a temp project nearby to PROVE no event
        // is written anywhere reachable from this accept.
        let (project, _scad_abs) = temp_project("dormant");
        let before = std::fs::read_dir(&project).expect("read project").count();

        let returned = accept_edit_inner(
            WORKSPACE_PATH.to_string(),
            "cube(1);".to_string(),
            "cube(2);".to_string(),
            &state,
        );
        assert_eq!(returned, "cube(2);", "accept still returns source");

        // No .noos created in the project dir (the accept did not emit there).
        assert!(
            !project.join(".noos").exists(),
            "no .noos/ created when opened_path is None"
        );
        let after = std::fs::read_dir(&project).expect("read project").count();
        assert_eq!(before, after, "project dir unchanged");
    }

    #[test]
    fn ai_accept_edit_best_effort_swallows_write_failure_and_returns_source() {
        // The accept's primary job is returning the source buffer; a
        // memory-write failure (e.g. corrupt pre-existing graph.json) must NOT
        // break the editor sync. Drives the real accept_edit_inner so the
        // best-effort swallow covers the whole flow (opened_path → noos_dir →
        // write_event fails → control returns). Tightened from "didn't panic"
        // to: graph.json byte-identical pre/post (write_event aborts at
        // read_graph before any graph mutation) + AppState.source unchanged.
        let (project, scad_abs) = temp_project("best_effort");
        let state = AppState::new_in_memory();
        state.set_source("cube(2);".to_string());
        state.set_opened_path(Some(scad_abs));

        // Seed a corrupt graph.json so write_event fails inside the lock at
        // read_graph. (The sidecar append happens before read_graph in
        // write_event — that's the durability guarantee: the sidecar is the
        // source of truth and a later rehydrate will surface the orphan line
        // once the graph is repaired. The assertion here is on the GRAPH: write_event
        // must abort at read_graph before mutating anything.)
        let noos_dir = project.join(".noos");
        std::fs::create_dir_all(&noos_dir).expect("mkdir .noos");
        let corrupt = "{ NOT JSON";
        std::fs::write(noos_dir.join("graph.json"), corrupt).expect("seed bad graph");

        let graph_pre = std::fs::read(noos_dir.join("graph.json")).expect("read pre");

        // No panic — the helper logs and returns the source buffer.
        let returned = accept_edit_inner(
            WORKSPACE_PATH.to_string(),
            "a".to_string(),
            "b".to_string(),
            &state,
        );
        assert_eq!(
            returned, "cube(2);",
            "best-effort: source returned unchanged"
        );
        assert_eq!(
            state.source(),
            "cube(2);",
            "AppState.source unchanged after best-effort swallow"
        );

        // Graph byte-identical pre/post: read_graph failed BEFORE any mutation,
        // so the corrupt bytes are untouched.
        let graph_post = std::fs::read(noos_dir.join("graph.json")).expect("read post");
        assert_eq!(
            graph_pre, graph_post,
            "graph.json byte-identical: write_event aborted at read_graph, no mutation"
        );
    }

    #[test]
    fn ai_accept_edit_skips_emit_when_noos_dir_not_derivable() {
        // Unit-level guard: noos_dir_for_path returns None for the cases
        // the inner's skip branch must handle (WORKSPACE_PATH, empty). Kept as
        // a focused unit test alongside the flow-level
        // ai_accept_edit_skips_emit_when_no_file_open test above.
        assert!(noos_dir_for_path(WORKSPACE_PATH).is_none());
        assert!(noos_dir_for_path("").is_none());
    }

    // --- Emit path enqueues on write_event failure + drains on recovery

    /// Read `<project>/.noos/morphe-queue.jsonl` as a list of trimmed non-blank
    /// lines. Returns an empty Vec when the file does not exist.
    fn read_queue(project: &std::path::Path) -> Vec<String> {
        let path = project.join(".noos").join("morphe-queue.jsonl");
        match std::fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Emit-path level: when `write_event` fails (real
    /// corrupt graph.json in the opened project's `.noos`), the accept's emit
    /// path enqueues the DesignEvent into `<noos_dir>/morphe-queue.jsonl`. The
    /// accept's primary job (returning the source) survives. This drives the REAL
    /// `accept_edit_inner` (which calls the real `write_event` → real
    /// `enqueue_event`) — not a synthesized seam.
    #[test]
    fn ai_accept_edit_enqueues_to_jsonl_when_write_event_fails() {
        let (project, scad_abs) = temp_project("enqueue");
        let state = AppState::new_in_memory();
        state.set_source("cube(2);".to_string());
        state.set_opened_path(Some(scad_abs));

        // REAL failure seed: corrupt graph.json. The sidecar is absent — the
        // live write_event appends one sidecar line before failing at
        // read_graph (its sidecar-first durability guarantee). The QUEUE must
        // ALSO gain a line so the retry path can drain it later.
        let noos_dir = project.join(".noos");
        std::fs::create_dir_all(&noos_dir).expect("mkdir .noos");
        std::fs::write(noos_dir.join("graph.json"), "{ NOT JSON").expect("seed bad graph");

        // Pre: queue is empty (no backlog).
        assert!(read_queue(&project).is_empty(), "pre: queue empty");

        // Drive the real accept path.
        let returned = accept_edit_inner(
            WORKSPACE_PATH.to_string(),
            "cube(1);".to_string(),
            "cube(2);".to_string(),
            &state,
        );
        assert_eq!(returned, "cube(2);", "accept returns source");
        assert_eq!(
            state.source(),
            "cube(2);",
            "AppState.source unchanged (best-effort)"
        );

        // The queue gained exactly one line that round-trips to the Decision.
        // No silent data loss: the failed event is
        // recoverable from the queue file.
        let lines = read_queue(&project);
        assert_eq!(lines.len(), 1, "queue gained exactly one line");
        let parsed: morphe_ai::DesignEvent =
            serde_json::from_str(&lines[0]).expect("queue line deserializes as DesignEvent");
        let expected = morphe_ai::DesignEvent::Decision {
            // The real opened filename, not WORKSPACE_PATH.
            path: "bracket.scad".to_string(),
            old: "cube(1);".to_string(),
            new: "cube(2);".to_string(),
        };
        assert_eq!(
            parsed, expected,
            "enqueued the exact Decision event (real filename)"
        );
    }

    /// Emit-path level: the NEXT accept on a now-healthy
    /// substrate drains the backlog FIRST (via `flush_queue`), then writes the
    /// new event. The prior session's queued event lands in sidecar+graph; the
    /// queue is empty after.
    #[test]
    fn ai_accept_edit_drains_backlog_first_when_substrate_recovered() {
        let (project, scad_abs) = temp_project("drain");
        let state = AppState::new_in_memory();
        state.set_source("cube(2);".to_string());
        state.set_opened_path(Some(scad_abs));

        // Seed a backlog by hand: enqueue a Decision directly into the queue
        // file (simulating a prior session's failed emit).
        let noos_dir = project.join(".noos");
        std::fs::create_dir_all(&noos_dir).expect("mkdir .noos");
        let queued = morphe_ai::DesignEvent::Decision {
            path: WORKSPACE_PATH.to_string(),
            old: "// old".to_string(),
            new: "// new".to_string(),
        };
        morphe_ai::enqueue_event(&queued, &noos_dir).expect("seed backlog via enqueue");
        assert_eq!(read_queue(&project).len(), 1, "pre: backlog of 1");
        assert!(
            !noos_dir.join("morphe-events.jsonl").exists(),
            "pre: sidecar absent"
        );

        // Substrate is HEALTHY for this accept (no corrupt graph; write_event
        // bootstraps). The emit path's flush-first drains the backlog, then
        // write_event lands the new event.
        let returned = accept_edit_inner(
            WORKSPACE_PATH.to_string(),
            "cube(1);".to_string(),
            "cube(2);".to_string(),
            &state,
        );
        assert_eq!(returned, "cube(2);", "accept returns source");

        // Queue drained; sidecar gained the backlog event AND the live event.
        assert!(
            read_queue(&project).is_empty(),
            "queue drained by flush-first"
        );
        let sidecar_lines: Vec<serde_json::Value> =
            std::fs::read_to_string(noos_dir.join("morphe-events.jsonl"))
                .expect("read sidecar")
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str(l).expect("line parses"))
                .collect();
        assert_eq!(
            sidecar_lines.len(),
            2,
            "sidecar has backlog event + live event"
        );
        // The first sidecar line is the drained backlog (sequence 1); the
        // second is the live event (sequence 2 — same subject, different seq).
        assert_eq!(
            sidecar_lines[0]["payload"]["old"], "// old",
            "backlog line first"
        );
        assert_eq!(
            sidecar_lines[1]["payload"]["old"], "cube(1);",
            "live event second"
        );
    }
}
