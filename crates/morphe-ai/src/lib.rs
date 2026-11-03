//! morphe-ai: agentic-AI client for the morphe CAD application.
//!
//! This crate provides:
//! - a Claude Messages-API client over raw HTTP (reqwest), with a `Transport`
//!   trait test seam so tests run against recorded fixtures — never the network
//! - streaming-SSE accumulation into a structured [`client::Message`]
//! - the six morphe tool schemas ([`tools::tool_definitions`]) as serde types
//! - a tool [`dispatch::Dispatcher`] wiring `compile`/`get_geometry_stats` to
//!   the morphe parse → eval → resolve pipeline, plus `read_code`/`edit_code`
//!   over an in-memory workspace
//! - an [`agent::AgentLoop`] driving the edit → compile → inspect cycle with a
//!   round cap, cancellation token, and event stream
//! - system-prompt assembly with a cacheable stable prefix and a volatile
//!   session-state tail
//! - a [`secret::SecretStore`] trait with an in-memory double and an OS
//!   credential-store backend (macOS Keychain, Windows Credential Manager,
//!   Linux Secret Service), plus a [`secret::client_from_secret`] helper

pub mod agent;
pub mod client;
pub mod design_event;
pub mod dispatch;
pub mod event_queue;
pub mod http;
mod noos_fs;
pub mod noos_writer;
pub mod openai;
pub mod prompt;
pub mod secret;
pub mod session_digest;
pub mod tools;
pub mod untrusted;

// Request side (what we send to the API).
pub use client::{
    CacheControl, InputContentBlock, InputMessage, MessageContent, Request, SystemBlock,
    ThinkingConfig, ToolChoice,
};
// Response side (what the SSE stream accumulates into).
pub use client::Provider;
pub use client::{ContentBlock, Message, StopReason, Usage};
pub use openai::{parse_openai_sse_stream, request_to_openai_body, OpenAiTransport};
// Transport seam + client.
pub use client::{
    parse_sse_stream, Client, ClientConfig, ClientError, MockTransport, RawResponse,
    ReqwestTransport, RetryPolicy, Transport, TransportError, RETRYABLE_STATUSES,
};
// Transport hardening: base-URL validation + endpoint joining (shared by both
// provider transports).
pub use http::{
    join_endpoint, validate_base_url, ResponseMeta, CONNECT_TIMEOUT, MAX_SSE_BODY_BYTES,
};
// Trust-boundary wrapper for file/memory-originated text.
pub use untrusted::{untrusted_inner, wrap_untrusted};
// Tool schemas.
pub use tools::{tool_definitions, ToolDef};
// Tool dispatcher.
pub use dispatch::{CompileError, Dispatcher, InMemoryWorkspace, ToolResult, Workspace};
// noos query tool.
pub use dispatch::{noos_query_argv, QUERY_BUDGET};
// Agent loop.
pub use agent::{
    run_on_thread, AgentLoop, CancellationToken, LoopEvent, LoopResult, LoopStopReason,
    DEFAULT_MAX_ROUNDS,
};
// System prompt assembly.
pub use prompt::{assemble_system_prompt, prompt_caps, PromptContext, SanitizedText};
// Secret store.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub use secret::KeychainSecretStore;
pub use secret::{
    client_from_secret, InMemorySecretStore, ProviderConfig, SecretError, SecretStore,
};
// Design-event vocabulary.
pub use design_event::DesignEvent;

pub use session_digest::{
    build_session_digest, DIGEST_MAX_CHARS, DIGEST_MAX_DECISIONS, FIELD_MAX_CHARS,
};
// noos writer.
pub use noos_writer::{
    event_from_line, read_event_lines, rehydrate_events, write_event, EventLine, WriteError,
};
// jsonl retry queue.
pub use event_queue::{enqueue_event, flush_queue, QueueError};
