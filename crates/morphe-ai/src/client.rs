//! Claude Messages-API client for morphe-ai.
//!
//! A bounded agent loop ([`crate::agent`]) calls Claude with morphe tools, iterating
//! edit→compile→inspect. This module provides the client that talks to the
//! Messages API over raw HTTP (there is no official Anthropic Rust SDK).
//!
//! # Design
//!
//! - **`Transport` trait** is the test seam: the client talks to a `Transport`
//!   instead of the network. [`MockTransport`] returns canned/recorded fixtures
//!   (no network in tests); [`ReqwestTransport`] hits the real API, streaming
//!   the SSE body via `reqwest`'s `bytes_stream()`.
//! - **SSE parsing is a pure function** ([`parse_sse_stream`]): the transport
//!   returns the accumulated SSE body as text, and the parser turns it into a
//!   structured [`Message`]. This keeps the tested parsing logic fully
//!   decoupled from the HTTP transport.
//! - The trait is synchronous (a plain `fn send` signature).
//!   `ReqwestTransport` drives the async reqwest streaming call on an internal
//!   `tokio` runtime via `block_on`. This is fine for the sync agent loop; do
//!   not call `ReqwestTransport::send` from inside an existing async
//!   runtime (it would block that runtime's worker).
//!
//! # Wire shape
//!
//! `POST {base_url}/v1/messages` with headers `x-api-key`, `anthropic-version:
//! 2023-06-01`, `content-type: application/json`. The body is a [`Request`]
//! serialized to JSON (`model`, `max_tokens`, `system`, `messages`, `tools`,
//! `tool_choice`, `thinking: {type:"adaptive"}`, `stream: true`). The response
//! is a Server-Sent Events stream accumulated into a [`Message`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::agent::CancellationToken;
pub use crate::http::ResponseMeta;
use crate::http::{join_endpoint, HttpCore};
use crate::tools::ToolDef;

// ---------------------------------------------------------------------------
// Request side (what we send to the API)
// ---------------------------------------------------------------------------

/// The Messages-API request body. Serializes to the exact wire shape Anthropic
/// expects.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<InputMessage>,
    /// Omitted from the JSON when empty (`skip_serializing_if`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    pub thinking: ThinkingConfig,
    /// Always `true` for this client — we stream the SSE response.
    pub stream: bool,
}

/// A system-prompt block. Carries optional `cache_control` so callers can mark
/// cacheable blocks (prompt caching: `tools` → `system` → `messages` render
/// order; a breakpoint on the last system block caches both).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    /// A plain text system block with no cache breakpoint.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: text.into(),
            cache_control: None,
        }
    }

    /// A text system block marked cacheable (ephemeral breakpoint).
    #[must_use]
    pub fn text_cached(text: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }
}

/// `cache_control: {type: "ephemeral"}` — marks a block as a cache breakpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
}

impl CacheControl {
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral".to_string(),
        }
    }
}

/// `thinking: {type: "adaptive"}` — adaptive thinking (the only on-mode for
/// Sonnet 5 / Opus 4.7+; `budget_tokens` is removed and 400s on these models).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
}

impl ThinkingConfig {
    #[must_use]
    pub fn adaptive() -> Self {
        Self {
            thinking_type: "adaptive".to_string(),
        }
    }

    /// `thinking: {type: "disabled"}` — the only mode in which a forced
    /// `tool_choice` (`any` / `tool`) is accepted by thinking-capable models.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            thinking_type: "disabled".to_string(),
        }
    }

    /// Whether this config turns thinking on (anything but `"disabled"`).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.thinking_type != "disabled"
    }
}

/// A message in the request's `messages` array.
#[derive(Debug, Clone, Serialize)]
pub struct InputMessage {
    pub role: String,
    pub content: MessageContent,
}

/// Message content — either a plain string (simple text) or an array of
/// content blocks (`text`, `tool_result`). Serialized via `untagged` so a
/// `MessageContent::Text("hi")` becomes `"hi"` and a `Blocks([...])` becomes
/// the array.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<InputContentBlock>),
}

/// A content block on the request side: `text`, `tool_result` (user messages),
/// or `tool_use` / `thinking` / `redacted_thinking` (assistant messages — the
/// loop echoes the model's turn back into the conversation history so the next
/// request carries the full exchange, including what tool the result answers).
///
/// `thinking` / `redacted_thinking` blocks MUST be echoed back unchanged (with
/// their `signature` / `data`) in the assistant turn of a tool-use loop — the
/// API verifies them.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum InputContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
}

/// `tool_choice` — when Claude may use tools.
///
/// NOTE: thinking-capable Anthropic models reject a forced choice (`Any` /
/// `Tool`) while thinking is enabled. [`Client::send`] therefore downgrades a
/// forced choice to `Auto` on the Anthropic provider whenever
/// [`ThinkingConfig::is_enabled`] — use [`ThinkingConfig::disabled`] to force a
/// tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "tool")]
    Tool { name: String },
    #[serde(rename = "none")]
    None,
}

// ---------------------------------------------------------------------------
// Response side (what the SSE stream accumulates into)
// ---------------------------------------------------------------------------

/// The structured response message, accumulated from the SSE event stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: String,
    pub model: String,
    pub stop_reason: StopReason,
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
}

/// Why generation stopped. `refusal` must be checked before reading `content`
/// (the content array may be empty or partial on a refusal).
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    /// Any unrecognized stop_reason string — forward-compatible.
    Other(String),
}

/// A response content block: assistant text, a tool-use request, or a
/// (redacted) thinking block. Thinking blocks are preserved with their
/// `signature` / `data` so the agent loop can echo them back verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

impl ContentBlock {
    /// The request-side echo of this block for the next assistant turn.
    #[must_use]
    pub fn to_input_block(&self) -> InputContentBlock {
        match self {
            Self::Text { text } => InputContentBlock::Text { text: text.clone() },
            Self::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            Self::Thinking {
                thinking,
                signature,
            } => InputContentBlock::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            Self::RedactedThinking { data } => {
                InputContentBlock::RedactedThinking { data: data.clone() }
            }
        }
    }
}

/// Token usage. `cache_creation_input_tokens` / `cache_read_input_tokens`
/// report prompt-caching activity (verify caching is working by checking
/// `cache_read_input_tokens` is non-zero across repeated identical-prefix
/// requests).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

// ---------------------------------------------------------------------------
// Transport trait + implementations
// ---------------------------------------------------------------------------

/// The raw HTTP response from the transport. For a streaming SSE response,
/// `body` is the accumulated event-stream text.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub body: String,
}

/// Error from the transport layer (network/HTTP/serialization failures).
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("HTTP transport error: {0}")]
    Http(String),
    #[error("request serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The configured base URL failed [`crate::http::validate_base_url`]; the
    /// request (and the API key) was never sent.
    #[error("refusing to send: invalid base URL: {0}")]
    InvalidBaseUrl(String),
    /// The streamed response body exceeded the buffer cap.
    #[error("response body exceeded the {limit}-byte limit")]
    BodyTooLarge { limit: usize },
}

/// Test seam: the client talks to a [`Transport`] instead of the network.
///
/// `send` serializes the [`Request`] (the mock captures the serialized form so
/// tests can assert the wire shape) and returns the raw response. The real
/// [`ReqwestTransport`] posts to the API; [`MockTransport`] returns a canned
/// fixture.
pub trait Transport: Send + Sync {
    fn send(&self, req: &Request) -> Result<RawResponse, TransportError>;

    /// [`Transport::send`] plus response metadata (the `retry-after` header).
    /// The default delegates to `send` with empty metadata, so test doubles
    /// only implement `send`; the real transports override this.
    fn send_with_meta(&self, req: &Request) -> Result<(RawResponse, ResponseMeta), TransportError> {
        self.send(req).map(|r| (r, ResponseMeta::default()))
    }
}

/// A mock transport for tests — returns a canned [`RawResponse`] and records
/// the serialized form of the last request it received.
///
/// Clone shares the recorded-request cell (via `Arc<Mutex<…>>`), so a test can
/// hold a clone to inspect what the client sent after the call.
#[derive(Clone)]
pub struct MockTransport {
    canned: RawResponse,
    last_request: Arc<Mutex<Option<Value>>>,
}

impl MockTransport {
    #[must_use]
    pub fn new(canned: RawResponse) -> Self {
        Self {
            canned,
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// The serialized JSON the client last sent through this transport
    /// (`Value::Null` if nothing has been sent yet).
    #[must_use]
    pub fn last_request(&self) -> Value {
        self.last_request
            .lock()
            .expect("mock transport lock poisoned")
            .clone()
            .unwrap_or(Value::Null)
    }
}

impl Transport for MockTransport {
    fn send(&self, req: &Request) -> Result<RawResponse, TransportError> {
        let val = serde_json::to_value(req)?;
        *self
            .last_request
            .lock()
            .expect("mock transport lock poisoned") = Some(val);
        Ok(self.canned.clone())
    }
}

/// The real Messages-API transport: posts the request to `{base_url}/v1/messages`
/// and streams the SSE response body via reqwest's `bytes_stream()`.
///
/// The endpoint is built with [`crate::http::join_endpoint`], which validates
/// the base URL ([`crate::http::validate_base_url`]); an invalid base URL makes
/// every `send` fail with [`TransportError::InvalidBaseUrl`] before anything
/// (including the API key) leaves the process.
///
/// Timeouts: [`crate::http::CONNECT_TIMEOUT`] for the connect phase and
/// [`ClientConfig::timeout`] as a per-read idle timeout — no whole-request
/// deadline, so long streamed (thinking) turns are not cut off. The buffered
/// body is capped at [`crate::http::MAX_SSE_BODY_BYTES`].
///
/// The trait is sync; the async reqwest call is driven on an internal
/// current-thread `tokio` runtime via `block_on`. Do not call this from inside
/// another async runtime — see the module docs.
pub struct ReqwestTransport {
    core: HttpCore,
    endpoint: Result<String, String>,
    api_key: String,
}

impl ReqwestTransport {
    #[must_use]
    pub fn new(config: &ClientConfig) -> Self {
        Self {
            core: HttpCore::new(config.timeout),
            endpoint: join_endpoint(&config.base_url, "/v1/messages"),
            api_key: config.api_key.clone(),
        }
    }

    /// The resolved `…/v1/messages` URL, or the base-URL validation error.
    pub fn endpoint(&self) -> Result<&str, &str> {
        self.endpoint.as_deref().map_err(String::as_str)
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, req: &Request) -> Result<RawResponse, TransportError> {
        self.send_with_meta(req).map(|(r, _)| r)
    }

    fn send_with_meta(&self, req: &Request) -> Result<(RawResponse, ResponseMeta), TransportError> {
        // Fail closed BEFORE serializing or sending: the API key never goes to
        // an unvalidated base URL.
        let url = self
            .endpoint
            .as_ref()
            .map_err(|e| TransportError::InvalidBaseUrl(e.clone()))?;
        let body = serde_json::to_vec(req)?;
        self.core.post(
            url,
            &[
                ("x-api-key", self.api_key.clone()),
                ("anthropic-version", "2023-06-01".to_string()),
                ("content-type", "application/json".to_string()),
            ],
            body,
        )
    }
}

// ---------------------------------------------------------------------------
// Client + config
// ---------------------------------------------------------------------------

/// Client configuration. The model and base URL are configurable (do not
/// hardcode); the API key is plumbed through to the `x-api-key` header. The
/// api key is normally supplied from a `SecretStore` via
/// [`crate::client_from_secret`].
///
/// The LLM provider. Selects the transport + SSE parser in [`Client::new`] /
/// [`Client::send`]. `OpenAiCompatible` covers any Chat Completions endpoint
/// (OpenAI.com, Azure, OpenRouter, local LLMs via base_url).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    OpenAiCompatible,
}

impl Provider {
    /// Default base URL for this provider.
    #[must_use]
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com",
            Self::OpenAiCompatible => "https://api.openai.com",
        }
    }

    /// Default model id for this provider.
    #[must_use]
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => "claude-sonnet-5-5",
            Self::OpenAiCompatible => "gpt-4o",
        }
    }
}

/// `Debug` is implemented by hand so the `api_key` is never surfaced (it would
/// leak into logs, panic messages, and tracing spans) — see the impl below.
#[derive(Clone)]
pub struct ClientConfig {
    pub provider: Provider,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Read (idle) timeout for the underlying reqwest client: the longest the
    /// transport waits for the NEXT chunk of the streamed response. `Some(d)`
    /// applies `reqwest::ClientBuilder::read_timeout(d)` (reset after every
    /// received chunk); `None` disables it. This is deliberately NOT a
    /// whole-request deadline — a long streamed thinking turn keeps receiving
    /// chunks/pings and is never cut off. The connect phase is separately
    /// bounded by [`crate::http::CONNECT_TIMEOUT`]. Defaults to 60s — see
    /// [`ClientConfig::default`].
    pub timeout: Option<Duration>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never surface the api_key in debug output. Show a redaction sentinel
        // so logs and panic messages cannot leak the credential.
        let redacted = if self.api_key.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        f.debug_struct("ClientConfig")
            .field("provider", &self.provider)
            .field("api_key", &redacted)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Anthropic,
            api_key: String::new(),
            base_url: Provider::Anthropic.default_base_url().to_string(),
            model: Provider::Anthropic.default_model().to_string(),
            max_tokens: 4096,
            timeout: Some(Duration::from_secs(60)),
        }
    }
}

/// Error from the client: transport failure, non-2xx HTTP status, or an SSE
/// parse failure.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    /// An error event delivered INSIDE an HTTP-200 stream (Anthropic `event:
    /// error`, or an OpenAI-compatible `data: {"error": ...}` chunk), e.g.
    /// `overloaded_error`.
    #[error("API stream error ({error_type}): {message}")]
    Stream { error_type: String, message: String },
    #[error("SSE parse error: {0}")]
    Parse(String),
}

impl ClientError {
    /// Whether this error is a transient overload/rate-limit that is safe to
    /// retry with a fresh identical request.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api { status, .. } => RETRYABLE_STATUSES.contains(status),
            Self::Stream { error_type, .. } => error_type == "overloaded_error",
            _ => false,
        }
    }
}

/// HTTP statuses retried by [`Client::send`]: 429 (rate limited), 503
/// (unavailable), 529 (Anthropic overloaded).
pub const RETRYABLE_STATUSES: [u16; 3] = [429, 503, 529];

/// Bounded retry-with-backoff policy for [`Client::send`].
///
/// A retry re-sends the SAME request. Each agent round is a fresh request and
/// tools are dispatched only after a successful response, so a retry never
/// repeats a tool side effect. The delay before retry `n` (0-based) is
/// `base_delay * 2^n`, raised to the server's `retry-after` when larger, and
/// always capped at `max_delay` — total sleep is bounded by
/// `max_retries * max_delay`.
#[derive(Clone)]
pub struct RetryPolicy {
    /// Retries after the first attempt (default 3 → at most 4 attempts).
    pub max_retries: u32,
    /// First backoff delay (default 1s → 1s / 2s / 4s).
    pub base_delay: Duration,
    /// Cap for any single delay, including an honored `retry-after`
    /// (default 10s).
    pub max_delay: Duration,
    sleeper: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_retries", &self.max_retries)
            .field("base_delay", &self.base_delay)
            .field("max_delay", &self.max_delay)
            .finish_non_exhaustive()
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
            sleeper: Arc::new(std::thread::sleep),
        }
    }
}

impl RetryPolicy {
    /// No retries at all.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Replace the sleep function (tests inject a recorder / no-op).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Fn(Duration) + Send + Sync>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// The delay before retry number `retry` (0-based).
    #[must_use]
    pub fn delay_for(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        let factor = 1u32.checked_shl(retry).unwrap_or(u32::MAX);
        let backoff = self
            .base_delay
            .checked_mul(factor)
            .unwrap_or(self.max_delay);
        backoff
            .max(retry_after.unwrap_or_default())
            .min(self.max_delay)
    }

    /// Sleep for `total` in short slices; returns `true` (early) once `cancel`
    /// is set.
    fn wait(&self, total: Duration, cancel: Option<&CancellationToken>) -> bool {
        const SLICE: Duration = Duration::from_millis(100);
        let cancelled = || cancel.is_some_and(CancellationToken::is_cancelled);
        let mut remaining = total;
        while !remaining.is_zero() {
            if cancelled() {
                return true;
            }
            let step = remaining.min(SLICE);
            (self.sleeper)(step);
            remaining -= step;
        }
        cancelled()
    }
}

/// The Claude Messages-API client. Takes a [`Transport`] so tests inject a
/// [`MockTransport`]; production uses [`ReqwestTransport`].
pub struct Client {
    transport: Arc<dyn Transport>,
    config: ClientConfig,
    retry: RetryPolicy,
}

impl Client {
    /// Build a production client backed by the transport matching
    /// [`ClientConfig::provider`] (Anthropic → [`ReqwestTransport`];
    /// OpenAI-compatible → [`crate::openai::OpenAiTransport`]).
    #[must_use]
    pub fn new(config: ClientConfig) -> Self {
        let transport: Arc<dyn Transport> = match config.provider {
            Provider::Anthropic => Arc::new(ReqwestTransport::new(&config)),
            Provider::OpenAiCompatible => Arc::new(crate::openai::OpenAiTransport::new(
                &config.base_url,
                &config.api_key,
                config.timeout,
            )),
        };
        Self::with_transport(transport, config)
    }

    /// Build a client with an injected transport (for tests).
    #[must_use]
    pub fn with_transport(transport: Arc<dyn Transport>, config: ClientConfig) -> Self {
        Self {
            transport,
            config,
            retry: RetryPolicy::default(),
        }
    }

    /// Replace the retry policy (tests inject zero delays / a sleep recorder).
    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Send a request and return the accumulated response [`Message`].
    ///
    /// Serializes the request through the transport, checks the HTTP status,
    /// then parses the SSE body. A non-2xx status yields [`ClientError::Api`];
    /// an in-stream error event yields [`ClientError::Stream`]. Transient
    /// failures ([`ClientError::is_retryable`]: HTTP 429/503/529, or an
    /// `overloaded_error` stream event) are retried per the client's
    /// [`RetryPolicy`] (bounded total sleep).
    ///
    /// On the Anthropic provider a forced `tool_choice` (`any`/`tool`) is
    /// downgraded to `auto` when thinking is enabled — thinking-capable models
    /// reject that combination with a 400.
    pub fn send(&self, req: &Request) -> Result<Message, ClientError> {
        self.send_inner(req, None)
    }

    /// [`Client::send`], but retry backoff is abandoned promptly once `cancel`
    /// is set (the last error is returned).
    pub fn send_cancellable(
        &self,
        req: &Request,
        cancel: &CancellationToken,
    ) -> Result<Message, ClientError> {
        self.send_inner(req, Some(cancel))
    }

    fn send_inner(
        &self,
        req: &Request,
        cancel: Option<&CancellationToken>,
    ) -> Result<Message, ClientError> {
        let normalized = self.normalize_request(req);
        let req = normalized.as_ref().unwrap_or(req);
        let mut retry = 0;
        loop {
            let (result, retry_after) = self.send_once(req);
            match result {
                Err(e) if e.is_retryable() && retry < self.retry.max_retries => {
                    let delay = self.retry.delay_for(retry, retry_after);
                    if self.retry.wait(delay, cancel) {
                        return Err(e);
                    }
                    retry += 1;
                }
                other => return other,
            }
        }
    }

    /// One attempt: transport → status check → provider-specific parse.
    fn send_once(&self, req: &Request) -> (Result<Message, ClientError>, Option<Duration>) {
        let (raw, meta) = match self.transport.send_with_meta(req) {
            Ok(r) => r,
            Err(e) => return (Err(e.into()), None),
        };
        if raw.status >= 400 {
            let err = ClientError::Api {
                status: raw.status,
                body: raw.body,
            };
            return (Err(err), meta.retry_after);
        }
        // Route the SSE parser by provider: transports are
        // HTTP-thin; the provider-specific SSE→Message parsing happens here.
        let parsed = match self.config.provider {
            Provider::Anthropic => parse_sse_stream(&raw.body),
            Provider::OpenAiCompatible => crate::openai::parse_openai_sse_stream(&raw.body),
        };
        (parsed, meta.retry_after)
    }

    /// Provider-specific request fix-ups; `None` when `req` is sent as-is (the
    /// common case — no clone).
    ///
    /// Anthropic + thinking enabled + forced `tool_choice` → `auto`. We
    /// DOWNGRADE rather than error: the model still sees every tool and the
    /// turn proceeds, whereas the API would 400 the whole request.
    fn normalize_request(&self, req: &Request) -> Option<Request> {
        let forced = matches!(
            req.tool_choice,
            Some(ToolChoice::Any) | Some(ToolChoice::Tool { .. })
        );
        if self.config.provider == Provider::Anthropic && forced && req.thinking.is_enabled() {
            let mut fixed = req.clone();
            fixed.tool_choice = Some(ToolChoice::Auto);
            return Some(fixed);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

/// A single parsed SSE event: the `event:` type and the concatenated `data:`
/// payload (one JSON object per event in Anthropic's stream).
#[derive(Default, Clone)]
struct SseEvent {
    event: String,
    data: String,
}

/// Split a raw SSE body into `(event_type, data)` pairs.
///
/// Handles `\r\n` line endings and multiple `data:` lines per event
/// (concatenated with `\n`, per the SSE spec). Blank lines delimit events.
fn split_sse_events(body: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current = SseEvent::default();
    let mut has_content = false;
    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if has_content {
                events.push(std::mem::take(&mut current));
                has_content = false;
            }
        } else if let Some(rest) = line.strip_prefix("event:") {
            current.event = rest.trim().to_string();
            has_content = true;
        } else if let Some(rest) = line.strip_prefix("data:") {
            let d = rest.trim_start();
            if !current.data.is_empty() {
                current.data.push('\n');
            }
            current.data.push_str(d);
            has_content = true;
        }
        // Other SSE fields (id:, retry:, comments starting ':') are ignored —
        // Anthropic's stream does not use them.
    }
    if has_content {
        events.push(current);
    }
    events
}

/// Accumulating builder for a content block. Text blocks append `text_delta`s;
/// tool-use blocks accumulate `input_json_delta.partial_json` strings and parse
/// the JSON at finalization.
enum BlockBuilder {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input_json: String,
    },
    /// `thinking` text accumulates from `thinking_delta`s; the `signature`
    /// arrives in a `signature_delta` (or, rarely, on content_block_start).
    Thinking {
        thinking: String,
        signature: String,
    },
    /// `redacted_thinking` carries its opaque `data` on content_block_start.
    RedactedThinking(String),
}

#[derive(Default)]
struct SseAccumulator {
    id: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    blocks: BTreeMap<usize, BlockBuilder>,
    usage: Usage,
}

fn parse_usage(u: &Value) -> Usage {
    Usage {
        input_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_creation_input_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: u
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn merge_usage(acc: &mut Usage, u: &Value) {
    if let Some(v) = u.get("input_tokens").and_then(Value::as_u64) {
        acc.input_tokens = v;
    }
    if let Some(v) = u.get("output_tokens").and_then(Value::as_u64) {
        acc.output_tokens = v;
    }
    if let Some(v) = u.get("cache_creation_input_tokens").and_then(Value::as_u64) {
        acc.cache_creation_input_tokens = v;
    }
    if let Some(v) = u.get("cache_read_input_tokens").and_then(Value::as_u64) {
        acc.cache_read_input_tokens = v;
    }
}

/// Build a [`ClientError::Stream`] from an error payload. Accepts the
/// Anthropic shape `{"type":"error","error":{"type":..,"message":..}}` and the
/// OpenAI shape `{"error":{"type"|"code":..,"message":..}}`; anything
/// unparseable becomes `error_type: "unknown_error"` with the raw text
/// (truncated) as the message.
pub(crate) fn stream_error_from_json(data: &str) -> ClientError {
    const RAW_MAX: usize = 500;
    let v: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let err = v.get("error").unwrap_or(&v);
    let as_string = |x: &Value| match x {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let error_type = err
        .get("type")
        .and_then(as_string)
        .filter(|t| t != "error")
        .or_else(|| err.get("code").and_then(as_string))
        .unwrap_or_else(|| "unknown_error".to_string());
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| data.chars().take(RAW_MAX).collect());
    ClientError::Stream {
        error_type,
        message,
    }
}

/// Parse a streamed SSE response body into a structured [`Message`].
///
/// Recognizes the Anthropic SSE event types: `message_start`,
/// `content_block_start`, `content_block_delta` (`text_delta` /
/// `input_json_delta`), `content_block_stop`, `message_delta` (`stop_reason`,
/// `usage`), `message_stop`, and `ping` (ignored). Tool-use `input` JSON is
/// assembled from the concatenated `partial_json` deltas and parsed at the end.
///
/// Public so tests can exercise the parser directly against recorded fixtures.
pub fn parse_sse_stream(body: &str) -> Result<Message, ClientError> {
    let events = split_sse_events(body);
    let mut acc = SseAccumulator::default();

    for ev in events {
        match ev.event.as_str() {
            "message_start" => {
                let v: Value = serde_json::from_str(&ev.data)
                    .map_err(|e| ClientError::Parse(format!("message_start JSON: {e}")))?;
                if let Some(msg) = v.get("message") {
                    acc.id = msg.get("id").and_then(Value::as_str).map(String::from);
                    acc.model = msg.get("model").and_then(Value::as_str).map(String::from);
                    if let Some(u) = msg.get("usage") {
                        acc.usage = parse_usage(u);
                    }
                }
            }
            "content_block_start" => {
                let v: Value = serde_json::from_str(&ev.data)
                    .map_err(|e| ClientError::Parse(format!("content_block_start JSON: {e}")))?;
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = v.get("content_block").cloned().unwrap_or(Value::Null);
                let builder = match block.get("type").and_then(Value::as_str) {
                    Some("text") => BlockBuilder::Text(String::new()),
                    Some("tool_use") => BlockBuilder::ToolUse {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        input_json: String::new(),
                    },
                    // Thinking blocks are preserved (with signature) so the
                    // agent loop can echo them back unchanged — the API
                    // requires that during tool-use loops.
                    Some("thinking") => BlockBuilder::Thinking {
                        thinking: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    },
                    Some("redacted_thinking") => BlockBuilder::RedactedThinking(
                        block
                            .get("data")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    // Other unknown block types are skipped (forward-compat).
                    _ => continue,
                };
                acc.blocks.insert(index, builder);
            }
            "content_block_delta" => {
                let v: Value = serde_json::from_str(&ev.data)
                    .map_err(|e| ClientError::Parse(format!("content_block_delta JSON: {e}")))?;
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                let delta_type = delta.get("type").and_then(Value::as_str);
                if let Some(builder) = acc.blocks.get_mut(&index) {
                    match (builder, delta_type) {
                        (BlockBuilder::Text(s), Some("text_delta")) => {
                            if let Some(t) = delta.get("text").and_then(Value::as_str) {
                                s.push_str(t);
                            }
                        }
                        (BlockBuilder::ToolUse { input_json, .. }, Some("input_json_delta")) => {
                            if let Some(p) = delta.get("partial_json").and_then(Value::as_str) {
                                input_json.push_str(p);
                            }
                        }
                        (BlockBuilder::Thinking { thinking, .. }, Some("thinking_delta")) => {
                            if let Some(t) = delta.get("thinking").and_then(Value::as_str) {
                                thinking.push_str(t);
                            }
                        }
                        (BlockBuilder::Thinking { signature, .. }, Some("signature_delta")) => {
                            if let Some(s) = delta.get("signature").and_then(Value::as_str) {
                                signature.push_str(s);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                // Nothing to do — the block at `index` is complete. It stays in
                // the map and is finalized at the end.
            }
            "message_delta" => {
                let v: Value = serde_json::from_str(&ev.data)
                    .map_err(|e| ClientError::Parse(format!("message_delta JSON: {e}")))?;
                if let Some(delta) = v.get("delta") {
                    if let Some(sr) = delta.get("stop_reason").and_then(Value::as_str) {
                        acc.stop_reason = Some(sr.to_string());
                    }
                }
                if let Some(u) = v.get("usage") {
                    merge_usage(&mut acc.usage, u);
                }
            }
            "error" => {
                // A mid-stream error on an HTTP-200 response, e.g.
                // {"type":"error","error":{"type":"overloaded_error",
                // "message":"Overloaded"}}. Surface it as a typed error rather
                // than a partial message with an "unknown" stop_reason.
                return Err(stream_error_from_json(&ev.data));
            }
            "message_stop" | "ping" => {}
            _ => {
                // Unknown event type — ignore for forward-compatibility.
            }
        }
    }

    // A stream cut off by max_tokens can end mid-way through a tool call's
    // input JSON; that block is kept (input = null) so the agent loop can see
    // it was truncated, instead of failing the whole parse. Any other
    // malformed tool input is still a hard parse error.
    let truncated_ok = acc.stop_reason.as_deref() == Some("max_tokens");

    // Finalize: convert builders to content blocks, parsing tool-use input JSON.
    let mut content = Vec::with_capacity(acc.blocks.len());
    for (_, builder) in std::mem::take(&mut acc.blocks) {
        match builder {
            BlockBuilder::Text(text) => content.push(ContentBlock::Text { text }),
            BlockBuilder::Thinking {
                thinking,
                signature,
            } => content.push(ContentBlock::Thinking {
                thinking,
                signature,
            }),
            BlockBuilder::RedactedThinking(data) => {
                content.push(ContentBlock::RedactedThinking { data });
            }
            BlockBuilder::ToolUse {
                id,
                name,
                input_json,
            } => {
                let input = if input_json.is_empty() {
                    // No deltas arrived — the model made a tool call with empty
                    // input, which Anthropic represents as `{}` in
                    // content_block_start.
                    Value::Object(serde_json::Map::new())
                } else {
                    match serde_json::from_str(&input_json) {
                        Ok(v) => v,
                        Err(_) if truncated_ok => Value::Null,
                        Err(e) => {
                            return Err(ClientError::Parse(format!("tool_use input JSON: {e}")))
                        }
                    }
                };
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }
    }

    let stop_reason = match acc.stop_reason.as_deref() {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        Some(s) => StopReason::Other(s.to_string()),
        None => StopReason::Other("unknown".to_string()),
    };

    Ok(Message {
        id: acc.id.unwrap_or_default(),
        model: acc.model.unwrap_or_default(),
        stop_reason,
        content,
        usage: acc.usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tool_definitions;
    use serde_json::json;

    /// A realistic SSE fixture: a text block followed by a tool_use block,
    /// ending with `stop_reason: tool_use`. The tool input JSON is split across
    /// two `input_json_delta` chunks to exercise accumulation.
    const SSE_TEXT_PLUS_TOOL_USE: &str = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01ABC\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-5\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":25,\"output_tokens\":1,\"cache_creation_input_tokens\":128,\"cache_read_input_tokens\":0}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"I'll read the file first.\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01XYZ\",\"name\":\"read_code\",\"input\":{}}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"src/cube.scad\\\"}\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":42}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n";

    // --- SSE parsing tests (decode streamed SSE) ---

    #[test]
    fn parse_sse_decodes_text_and_tool_use_blocks() {
        let msg = parse_sse_stream(SSE_TEXT_PLUS_TOOL_USE).expect("parse should succeed");
        assert_eq!(msg.id, "msg_01ABC");
        assert_eq!(msg.model, "claude-sonnet-5");
        assert_eq!(msg.stop_reason, StopReason::ToolUse);
        assert_eq!(
            msg.content,
            vec![
                ContentBlock::Text {
                    text: "I'll read the file first.".to_string()
                },
                ContentBlock::ToolUse {
                    id: "toolu_01XYZ".to_string(),
                    name: "read_code".to_string(),
                    input: json!({"path": "src/cube.scad"}),
                },
            ]
        );
    }

    #[test]
    fn parse_sse_parses_cache_usage_tokens() {
        // message_start carries the input-side usage (a cache write of 128
        // tokens on this first request); message_delta carries only the final
        // cumulative output_tokens. The parser must preserve the message_start
        // cache tokens and take output_tokens from message_delta.
        let msg = parse_sse_stream(SSE_TEXT_PLUS_TOOL_USE).expect("parse should succeed");
        assert_eq!(msg.usage.input_tokens, 25);
        assert_eq!(msg.usage.output_tokens, 42);
        assert_eq!(msg.usage.cache_creation_input_tokens, 128);
        assert_eq!(msg.usage.cache_read_input_tokens, 0);
    }

    /// Build a minimal SSE body with the given stop_reason (no content blocks).
    /// JSON payloads are built with `serde_json::json!` to avoid hand-escaping
    /// braces inside a `format!` string.
    fn sse_with_stop_reason(stop_reason: &str) -> String {
        let start_data = json!({
            "type": "message_start",
            "message": {
                "id": "msg_01",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-5",
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 5, "output_tokens": 1},
            }
        })
        .to_string();
        let delta_data = json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": 10},
        })
        .to_string();
        let stop_data = json!({"type": "message_stop"}).to_string();
        format!(
            "event: message_start\ndata: {start_data}\n\n\
             event: message_delta\ndata: {delta_data}\n\n\
             event: message_stop\ndata: {stop_data}\n"
        )
    }

    #[test]
    fn parse_sse_handles_end_turn_stop_reason() {
        let msg = parse_sse_stream(&sse_with_stop_reason("end_turn")).expect("parse ok");
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn parse_sse_handles_tool_use_stop_reason() {
        let msg = parse_sse_stream(&sse_with_stop_reason("tool_use")).expect("parse ok");
        assert_eq!(msg.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn parse_sse_handles_max_tokens_stop_reason() {
        let msg = parse_sse_stream(&sse_with_stop_reason("max_tokens")).expect("parse ok");
        assert_eq!(msg.stop_reason, StopReason::MaxTokens);
    }

    #[test]
    fn parse_sse_handles_refusal_stop_reason() {
        let msg = parse_sse_stream(&sse_with_stop_reason("refusal")).expect("parse ok");
        assert_eq!(msg.stop_reason, StopReason::Refusal);
    }

    #[test]
    fn parse_sse_handles_crlf_line_endings() {
        // Normalize the fixture to CRLF — the parser must still split events.
        let crlf = SSE_TEXT_PLUS_TOOL_USE.replace('\n', "\r\n");
        let msg = parse_sse_stream(&crlf).expect("parse should succeed");
        assert_eq!(msg.stop_reason, StopReason::ToolUse);
        assert_eq!(msg.content.len(), 2);
    }

    #[test]
    fn parse_sse_handles_ping_events() {
        let body = "\
event: ping\n\
data: {\"type\":\"ping\"}\n\
\n\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"model\":\"claude-sonnet-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n";
        let msg = parse_sse_stream(body).expect("parse ok");
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
    }

    // --- Request wire-shape test (right serialized form) ---

    /// Build a representative request matching what the agent loop will send.
    fn sample_request() -> Request {
        Request {
            model: "claude-sonnet-5".to_string(),
            max_tokens: 1024,
            system: vec![SystemBlock::text_cached("You are a morphe CAD assistant.")],
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: MessageContent::Blocks(vec![InputContentBlock::Text {
                    text: "Design a 10mm cube.".to_string(),
                }]),
            }],
            tools: tool_definitions(),
            tool_choice: Some(ToolChoice::Auto),
            thinking: ThinkingConfig::adaptive(),
            stream: true,
        }
    }

    #[test]
    fn client_serializes_request_with_expected_wire_shape() {
        let canned = RawResponse {
            status: 200,
            body: sse_with_stop_reason("end_turn"),
        };
        let mock = Arc::new(MockTransport::new(canned));
        let client = Client::with_transport(mock.clone(), ClientConfig::default());

        // Send the request through the mock (the response is discarded — this
        // test asserts what the transport *received*).
        let _ = client.send(&sample_request()).expect("send should succeed");

        let captured = mock.last_request();
        let expected = json!({
            "model": "claude-sonnet-5",
            "max_tokens": 1024,
            "system": [{
                "type": "text",
                "text": "You are a morphe CAD assistant.",
                "cache_control": {"type": "ephemeral"}
            }],
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "Design a 10mm cube."}]
            }],
            "tools": serde_json::to_value(tool_definitions()).unwrap(),
            "tool_choice": {"type": "auto"},
            "thinking": {"type": "adaptive"},
            "stream": true
        });
        assert_eq!(captured, expected);
    }

    #[test]
    fn client_returns_message_from_mock_sse_body() {
        // Full integration: client.send → mock returns SSE → parse into Message.
        let canned = RawResponse {
            status: 200,
            body: SSE_TEXT_PLUS_TOOL_USE.to_string(),
        };
        let mock = Arc::new(MockTransport::new(canned));
        let client = Client::with_transport(mock, ClientConfig::default());

        let msg = client.send(&sample_request()).expect("send should succeed");
        assert_eq!(msg.stop_reason, StopReason::ToolUse);
        assert_eq!(msg.content.len(), 2);
        assert_eq!(
            msg.content[1],
            ContentBlock::ToolUse {
                id: "toolu_01XYZ".to_string(),
                name: "read_code".to_string(),
                input: json!({"path": "src/cube.scad"}),
            }
        );
    }

    #[test]
    fn client_surfaces_api_error_on_non_2xx() {
        let canned = RawResponse {
            status: 401,
            body: "{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}".to_string(),
        };
        let mock = Arc::new(MockTransport::new(canned));
        let client = Client::with_transport(mock, ClientConfig::default());

        match client.send(&sample_request()) {
            Err(ClientError::Api { status, body }) => {
                assert_eq!(status, 401);
                assert!(body.contains("authentication_error"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn client_config_defaults_to_sonnet_and_anthropic_base_url() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.model, "claude-sonnet-5-5");
        assert_eq!(cfg.base_url, "https://api.anthropic.com");
        // api_key defaults to empty — SecretStore fills it at runtime.
        assert!(cfg.api_key.is_empty());
    }

    #[test]
    fn client_config_model_is_configurable() {
        let cfg = ClientConfig {
            model: "claude-opus-4-8".to_string(),
            ..ClientConfig::default()
        };
        assert_eq!(cfg.model, "claude-opus-4-8");
    }

    #[test]
    fn client_config_debug_redacts_api_key() {
        let cfg = ClientConfig {
            api_key: "test-key-redact-fixture".to_string(),
            ..ClientConfig::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(
            !dbg.contains("test-key-redact-fixture"),
            "Debug must not leak the api_key: {dbg}"
        );
        assert!(dbg.contains("<redacted>"), "expected <redacted>: {dbg}");
    }

    #[test]
    fn client_config_debug_shows_empty_for_unset_key() {
        let cfg = ClientConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("<empty>"), "expected <empty>: {dbg}");
    }

    #[test]
    fn client_config_timeout_defaults_to_60s() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.timeout, Some(Duration::from_secs(60)));
    }

    #[test]
    fn reqwest_transport_builder_accepts_timeout_config() {
        // Config-presence: the builder is configured with the timeout without
        // panic. The timeout is not exercised over the network here — the
        // real-API smoke test (tests/smoke_real_api.rs) covers wire behavior.
        let cfg = ClientConfig {
            timeout: Some(Duration::from_secs(5)),
            ..ClientConfig::default()
        };
        let _transport = ReqwestTransport::new(&cfg);

        // None (no timeout) must also construct.
        let cfg_none = ClientConfig {
            timeout: None,
            ..ClientConfig::default()
        };
        let _transport_none = ReqwestTransport::new(&cfg_none);
    }

    #[test]
    fn tool_result_block_serializes_with_is_error() {
        let block = InputContentBlock::ToolResult {
            tool_use_id: "toolu_01".to_string(),
            content: "file not found".to_string(),
            is_error: Some(true),
        };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "tool_result",
                "tool_use_id": "toolu_01",
                "content": "file not found",
                "is_error": true
            })
        );
    }

    #[test]
    fn tool_result_block_omits_is_error_when_none() {
        let block = InputContentBlock::ToolResult {
            tool_use_id: "toolu_01".to_string(),
            content: "ok".to_string(),
            is_error: None,
        };
        let v = serde_json::to_value(&block).unwrap();
        assert!(v.get("is_error").is_none());
    }

    #[test]
    fn tool_choice_tool_serializes_with_name() {
        let v = serde_json::to_value(&ToolChoice::Tool {
            name: "compile".to_string(),
        })
        .unwrap();
        assert_eq!(v, json!({"type": "tool", "name": "compile"}));
    }

    #[test]
    fn request_omits_tools_when_empty() {
        let req = Request {
            model: "claude-sonnet-5".to_string(),
            max_tokens: 64,
            system: vec![],
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: MessageContent::Text("hi".to_string()),
            }],
            tools: vec![],
            tool_choice: None,
            thinking: ThinkingConfig::adaptive(),
            stream: true,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("tools").is_none());
        assert!(v.get("tool_choice").is_none());
        // system serializes as an empty array (it's always present in the wire shape)
        assert_eq!(v["system"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn split_sse_handles_multiple_data_lines() {
        // SSE spec allows multiple data: lines per event, concatenated with \n.
        let body = "\
event: message_start\n\
data: {\"type\":\"message_start\",\n\
data:  \"message\":{\"id\":\"msg_01\",\"model\":\"claude-sonnet-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n";
        let msg = parse_sse_stream(body).expect("parse ok");
        assert_eq!(msg.id, "msg_01");
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
    }

    // --- default model (claude-sonnet-5-5) ---

    #[test]
    fn default_anthropic_model_is_sonnet_5_5() {
        assert_eq!(Provider::Anthropic.default_model(), "claude-sonnet-5-5");
        assert_eq!(ClientConfig::default().model, "claude-sonnet-5-5");
    }

    // --- mid-stream SSE error events ---

    /// An HTTP-200 stream that starts normally, then delivers `event: error`.
    const SSE_OVERLOADED_MID_STREAM: &str = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"model\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\
\n\
event: error\n\
data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\
\n";

    #[test]
    fn parse_sse_surfaces_mid_stream_error_event() {
        match parse_sse_stream(SSE_OVERLOADED_MID_STREAM) {
            Err(ClientError::Stream {
                error_type,
                message,
            }) => {
                assert_eq!(error_type, "overloaded_error");
                assert_eq!(message, "Overloaded");
            }
            other => panic!("expected Stream error, got {other:?}"),
        }
        let err = parse_sse_stream(SSE_OVERLOADED_MID_STREAM).unwrap_err();
        assert!(err.is_retryable());
        assert!(err.to_string().contains("overloaded_error"), "{err}");
        assert!(!err.to_string().contains("unrecognized stop_reason"));
    }

    #[test]
    fn parse_sse_non_overloaded_stream_error_is_not_retryable() {
        let body = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad\"}}\n\n";
        let err = parse_sse_stream(body).unwrap_err();
        assert!(
            matches!(&err, ClientError::Stream { error_type, .. } if error_type == "invalid_request_error")
        );
        assert!(!err.is_retryable());
        // Garbage payload still yields a typed error.
        let err = parse_sse_stream("event: error\ndata: not json\n\n").unwrap_err();
        assert!(
            matches!(&err, ClientError::Stream { error_type, .. } if error_type == "unknown_error")
        );
    }

    // --- retry with backoff ---

    /// Returns scripted (response, meta) pairs; counts calls.
    struct ScriptedMeta {
        responses: Mutex<std::collections::VecDeque<(RawResponse, ResponseMeta)>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedMeta {
        fn new(responses: Vec<(RawResponse, ResponseMeta)>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Transport for ScriptedMeta {
        fn send(&self, req: &Request) -> Result<RawResponse, TransportError> {
            self.send_with_meta(req).map(|(r, _)| r)
        }
        fn send_with_meta(
            &self,
            _req: &Request,
        ) -> Result<(RawResponse, ResponseMeta), TransportError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| TransportError::Http("exhausted".to_string()))
        }
    }

    fn status(code: u16) -> (RawResponse, ResponseMeta) {
        (
            RawResponse {
                status: code,
                body: "{\"type\":\"error\"}".to_string(),
            },
            ResponseMeta::default(),
        )
    }

    fn ok_end_turn() -> (RawResponse, ResponseMeta) {
        (
            RawResponse {
                status: 200,
                body: sse_with_stop_reason("end_turn"),
            },
            ResponseMeta::default(),
        )
    }

    /// A policy whose sleeps are recorded (summed) instead of performed.
    fn recording_policy() -> (RetryPolicy, Arc<Mutex<Duration>>) {
        let slept = Arc::new(Mutex::new(Duration::ZERO));
        let s = slept.clone();
        let policy = RetryPolicy::default().with_sleeper(Arc::new(move |d| {
            *s.lock().unwrap() += d;
        }));
        (policy, slept)
    }

    #[test]
    fn retries_429_529_503_then_succeeds_with_backoff() {
        let t = Arc::new(ScriptedMeta::new(vec![
            status(429),
            status(529),
            status(503),
            ok_end_turn(),
        ]));
        let (policy, slept) = recording_policy();
        let client =
            Client::with_transport(t.clone(), ClientConfig::default()).with_retry_policy(policy);
        let msg = client.send(&sample_request()).expect("eventually ok");
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
        assert_eq!(t.calls(), 4);
        // 1s + 2s + 4s backoff.
        assert_eq!(*slept.lock().unwrap(), Duration::from_secs(7));
    }

    #[test]
    fn retry_gives_up_after_max_retries() {
        let t = Arc::new(ScriptedMeta::new(vec![status(529); 10]));
        let (policy, _) = recording_policy();
        let client =
            Client::with_transport(t.clone(), ClientConfig::default()).with_retry_policy(policy);
        match client.send(&sample_request()) {
            Err(ClientError::Api { status, .. }) => assert_eq!(status, 529),
            other => panic!("expected Api 529, got {other:?}"),
        }
        assert_eq!(t.calls(), 4, "1 attempt + 3 retries");
    }

    #[test]
    fn non_retryable_status_is_not_retried() {
        let t = Arc::new(ScriptedMeta::new(vec![status(400), ok_end_turn()]));
        let (policy, slept) = recording_policy();
        let client =
            Client::with_transport(t.clone(), ClientConfig::default()).with_retry_policy(policy);
        assert!(client.send(&sample_request()).is_err());
        assert_eq!(t.calls(), 1);
        assert_eq!(*slept.lock().unwrap(), Duration::ZERO);
    }

    #[test]
    fn overloaded_stream_event_is_retried() {
        let t = Arc::new(ScriptedMeta::new(vec![
            (
                RawResponse {
                    status: 200,
                    body: SSE_OVERLOADED_MID_STREAM.to_string(),
                },
                ResponseMeta::default(),
            ),
            ok_end_turn(),
        ]));
        let (policy, _) = recording_policy();
        let client =
            Client::with_transport(t.clone(), ClientConfig::default()).with_retry_policy(policy);
        let msg = client.send(&sample_request()).expect("retried to success");
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
        assert_eq!(t.calls(), 2);
    }

    #[test]
    fn retry_after_header_is_honored_but_capped() {
        let with_after = |secs: u64| {
            (
                RawResponse {
                    status: 429,
                    body: String::new(),
                },
                ResponseMeta {
                    retry_after: Some(Duration::from_secs(secs)),
                },
            )
        };
        let t = Arc::new(ScriptedMeta::new(vec![
            with_after(3),
            with_after(600),
            ok_end_turn(),
        ]));
        let (policy, slept) = recording_policy();
        let client =
            Client::with_transport(t.clone(), ClientConfig::default()).with_retry_policy(policy);
        client.send(&sample_request()).expect("ok");
        // max(1s, 3s) = 3s, then max(2s, 600s) capped at 10s.
        assert_eq!(*slept.lock().unwrap(), Duration::from_secs(13));
    }

    #[test]
    fn retry_backoff_aborts_on_cancel() {
        let t = Arc::new(ScriptedMeta::new(vec![status(529); 10]));
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let slept = Arc::new(Mutex::new(Duration::ZERO));
        let s = slept.clone();
        // The first sleep slice cancels the run (as a UI Stop button would).
        let policy = RetryPolicy::default().with_sleeper(Arc::new(move |d| {
            *s.lock().unwrap() += d;
            c.cancel();
        }));
        let client =
            Client::with_transport(t.clone(), ClientConfig::default()).with_retry_policy(policy);
        assert!(client.send_cancellable(&sample_request(), &cancel).is_err());
        assert_eq!(t.calls(), 1, "no further attempts after cancel");
        assert!(*slept.lock().unwrap() <= Duration::from_millis(100));
    }

    #[test]
    fn retry_policy_delays_are_bounded() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_for(0, None), Duration::from_secs(1));
        assert_eq!(p.delay_for(1, None), Duration::from_secs(2));
        assert_eq!(p.delay_for(2, None), Duration::from_secs(4));
        assert_eq!(p.delay_for(40, None), p.max_delay, "no overflow");
        assert_eq!(RetryPolicy::none().max_retries, 0);
    }

    // --- forced tool_choice + thinking ---

    #[test]
    fn forced_tool_choice_is_downgraded_to_auto_when_thinking_enabled() {
        let mock = Arc::new(MockTransport::new(RawResponse {
            status: 200,
            body: sse_with_stop_reason("end_turn"),
        }));
        let client = Client::with_transport(mock.clone(), ClientConfig::default());
        for forced in [
            ToolChoice::Any,
            ToolChoice::Tool {
                name: "compile".to_string(),
            },
        ] {
            let req = Request {
                tool_choice: Some(forced),
                ..sample_request()
            };
            client.send(&req).expect("send");
            assert_eq!(mock.last_request()["tool_choice"], json!({"type": "auto"}));
        }
        // With thinking disabled the forced choice is sent as-is.
        let req = Request {
            tool_choice: Some(ToolChoice::Any),
            thinking: ThinkingConfig::disabled(),
            ..sample_request()
        };
        client.send(&req).expect("send");
        assert_eq!(mock.last_request()["tool_choice"], json!({"type": "any"}));
        assert_eq!(mock.last_request()["thinking"], json!({"type": "disabled"}));
    }

    // --- thinking blocks preserved ---

    const SSE_THINKING_THEN_TOOL: &str = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_t\",\"model\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me \"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think.\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-abc\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque==\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_code\",\"input\":{}}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.scad\\\"}\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":2}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n";

    #[test]
    fn parse_sse_preserves_thinking_and_redacted_thinking_blocks() {
        let msg = parse_sse_stream(SSE_THINKING_THEN_TOOL).expect("parse");
        assert_eq!(
            msg.content,
            vec![
                ContentBlock::Thinking {
                    thinking: "Let me think.".to_string(),
                    signature: "sig-abc".to_string(),
                },
                ContentBlock::RedactedThinking {
                    data: "opaque==".to_string()
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "read_code".to_string(),
                    input: json!({"path": "a.scad"}),
                },
            ]
        );
        // And they serialize back to the exact wire shape.
        let echoed: Vec<Value> = msg
            .content
            .iter()
            .map(|b| serde_json::to_value(b.to_input_block()).unwrap())
            .collect();
        assert_eq!(
            echoed[0],
            json!({"type": "thinking", "thinking": "Let me think.", "signature": "sig-abc"})
        );
        assert_eq!(
            echoed[1],
            json!({"type": "redacted_thinking", "data": "opaque=="})
        );
    }

    #[test]
    fn parse_sse_keeps_truncated_tool_input_on_max_tokens() {
        let body = "\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_t\",\"name\":\"edit_code\",\"input\":{}}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.sc\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":9}}\n\
\n";
        let msg = parse_sse_stream(body).expect("truncation is not a parse failure");
        assert_eq!(msg.stop_reason, StopReason::MaxTokens);
        assert!(matches!(
            &msg.content[0],
            ContentBlock::ToolUse {
                input: Value::Null,
                ..
            }
        ));
        // The same truncated JSON under tool_use is still a hard parse error.
        let bad = body.replace("max_tokens", "tool_use");
        assert!(matches!(parse_sse_stream(&bad), Err(ClientError::Parse(_))));
    }

    // --- base URL enforcement in the transport ---

    #[test]
    fn reqwest_transport_refuses_invalid_base_url_before_sending() {
        let cfg = ClientConfig {
            base_url: "http://attacker.example".to_string(),
            api_key: "sk-secret".to_string(),
            ..ClientConfig::default()
        };
        let t = ReqwestTransport::new(&cfg);
        assert!(t.endpoint().is_err());
        match t.send(&sample_request()) {
            Err(TransportError::InvalidBaseUrl(msg)) => assert!(msg.contains("https"), "{msg}"),
            other => panic!("expected InvalidBaseUrl, got {other:?}"),
        }
    }

    #[test]
    fn reqwest_transport_endpoint_joins_robustly() {
        let ep = |base: &str| {
            let t = ReqwestTransport::new(&ClientConfig {
                base_url: base.to_string(),
                ..ClientConfig::default()
            });
            t.endpoint().map(str::to_string).map_err(str::to_string)
        };
        assert_eq!(
            ep("https://api.anthropic.com").unwrap(),
            "https://api.anthropic.com/v1/messages",
            "default base stays byte-identical"
        );
        assert_eq!(
            ep("https://api.anthropic.com/").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            ep("http://localhost:8080/v1").unwrap(),
            "http://localhost:8080/v1/messages"
        );
    }
}
