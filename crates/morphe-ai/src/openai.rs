//! OpenAI-compatible Chat Completions transport + SSE parser.
//!
//! The provider-agnostic counterpart to [`crate::client`]'s Anthropic Messages
//! client. Same architecture: a thin HTTP [`OpenAiTransport`] (posts to
//! `{base_url}/v1/chat/completions` with `Authorization: Bearer`) + a PURE
//! [`parse_openai_sse_stream`] that turns the SSE body into a [`Message`]. The
//! [`crate::client::Request`] is the shared neutral IR; [`request_to_openai_body`]
//! maps it to the OpenAI Chat Completions body shape. The agent loop is
//! unchanged — provider-agnosticism lives at the [`crate::client::Client`] seam.
//!
//! "OpenAI-compatible" = any endpoint speaking Chat Completions (OpenAI.com,
//! Azure, OpenRouter, local LLMs via base_url).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::client::{
    stream_error_from_json, ClientError, ContentBlock, InputContentBlock, Message, MessageContent,
    RawResponse, Request, StopReason, ToolChoice, Transport, TransportError, Usage,
};
use crate::http::{join_endpoint, HttpCore, ResponseMeta};
use crate::tools::ToolDef;

// ---------------------------------------------------------------------------
// Request → OpenAI Chat Completions body (the pure testable mapping seam)
// ---------------------------------------------------------------------------

/// Map the neutral [`Request`] IR to an OpenAI Chat Completions request body.
///
/// Differences from the Anthropic body (which serializes `Request` directly):
/// - `system` → ONE `{role:"system"}` message prepended (OpenAI has no separate
///   `system` field).
/// - `messages` → OpenAI messages: user text → `{role:"user"}`; assistant
///   tool_use → `{role:"assistant", tool_calls:[...]}`; user tool_result →
///   `{role:"tool", tool_call_id}`.
/// - `tools` → `[{type:"function", function:{name, description, parameters}}]`.
/// - `tool_choice` → `"auto"` | `"none"` | `{type:"function",function:{name}}`
///   (OpenAI has no `"any"` — mapped to `"auto"`).
/// - `thinking` is OMIT (OpenAI has no thinking field).
pub fn request_to_openai_body(req: &Request) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    // system → one {role:system} message (concatenate block texts).
    let system_text: String = req
        .system
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    if !system_text.is_empty() {
        messages.push(json!({"role": "system", "content": system_text}));
    }

    for msg in &req.messages {
        match &msg.content {
            MessageContent::Text(s) => {
                messages.push(json!({"role": msg.role, "content": s}));
            }
            MessageContent::Blocks(blocks) => {
                let has_tool_use = blocks
                    .iter()
                    .any(|b| matches!(b, InputContentBlock::ToolUse { .. }));
                let has_tool_result = blocks
                    .iter()
                    .any(|b| matches!(b, InputContentBlock::ToolResult { .. }));
                if has_tool_use {
                    // assistant: text (concatenated, null if empty) + tool_calls.
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            InputContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    let tool_calls: Vec<Value> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            InputContentBlock::ToolUse { id, name, input } => Some(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                                }
                            })),
                            _ => None,
                        })
                        .collect();
                    messages.push(json!({
                        "role": msg.role,
                        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                        "tool_calls": tool_calls,
                    }));
                } else if has_tool_result {
                    // each tool_result → its own {role:tool} message.
                    for b in blocks {
                        if let InputContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = b
                        {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content,
                            }));
                        }
                    }
                } else {
                    // text blocks → one message with concatenated text.
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            InputContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    messages.push(json!({"role": msg.role, "content": text}));
                }
            }
        }
    }

    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t: &ToolDef| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect();

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = match &req.tool_choice {
            Some(ToolChoice::None) => json!("none"),
            Some(ToolChoice::Tool { name }) => {
                json!({"type": "function", "function": {"name": name}})
            }
            // Auto / Any / None-omitted → "auto" (OpenAI has no "any"; default is auto).
            _ => json!("auto"),
        };
    }
    // Request usage in the final chunk: without this OpenAI streams
    // carry NO usage → Message.usage would always be (0,0). The final chunk
    // then carries {usage: {prompt_tokens, completion_tokens}} which the parser
    // reads. (The canned test hardcodes a usage chunk, which masked the absence.)
    body["stream_options"] = json!({"include_usage": true});
    // thinking intentionally omitted — OpenAI has no thinking field.
    body
}

// ---------------------------------------------------------------------------
// OpenAI SSE → Message (the pure testable parser seam)
// ---------------------------------------------------------------------------

/// Parse an OpenAI Chat Completions SSE body into a [`Message`].
///
/// Each `data: {..}` line (except `data: [DONE]`) is a chunk:
/// `{id, model, choices:[{index, delta:{role?, content?, tool_calls?},
/// finish_reason?}], usage?}`. Text `delta.content` strings concatenate into
/// one [`ContentBlock::Text`]; `delta.tool_calls` accumulate BY INDEX (the
/// first delta for an index establishes `id` + `function.name`; subsequent
/// deltas APPEND to `function.arguments` — streamed JSON fragments, like
/// Anthropic's `input_json_delta`) and finalize to [`ContentBlock::ToolUse`].
/// `finish_reason` maps to [`StopReason`]. Malformed lines are skipped
/// (forward-compat), never panic.
pub fn parse_openai_sse_stream(body: &str) -> Result<Message, ClientError> {
    let mut id = String::new();
    let mut model = String::new();
    let mut text = String::new();
    // index → (id, name, accumulated arguments string)
    let mut tool_calls: BTreeMap<usize, (String, String, String)> = BTreeMap::new();
    let mut stop_reason = StopReason::Other("unknown".to_string());
    let mut usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    };

    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Some(data) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(v): Result<Value, _> = serde_json::from_str(data) else {
            continue; // skip malformed chunks (forward-compat)
        };
        // A mid-stream error chunk (`data: {"error": {...}}`, as OpenAI and
        // most compatible servers emit on an HTTP-200 stream) is a typed
        // error, not a partial message.
        if v.get("error").is_some_and(|e| !e.is_null()) {
            return Err(stream_error_from_json(data));
        }

        if id.is_empty() {
            if let Some(s) = v.get("id").and_then(Value::as_str) {
                id = s.to_string();
            }
        }
        if model.is_empty() {
            if let Some(s) = v.get("model").and_then(Value::as_str) {
                model = s.to_string();
            }
        }
        if let Some(choices) = v.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if let Some(c) = delta.get("content").and_then(Value::as_str) {
                        text.push_str(c);
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                        for tc in tcs {
                            let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            let entry = tool_calls
                                .entry(idx)
                                .or_insert_with(|| (String::new(), String::new(), String::new()));
                            if let Some(i) = tc.get("id").and_then(Value::as_str) {
                                entry.0 = i.to_string();
                            }
                            if let Some(f) = tc.get("function") {
                                if let Some(n) = f.get("name").and_then(Value::as_str) {
                                    entry.1 = n.to_string();
                                }
                                if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                                    entry.2.push_str(a);
                                }
                            }
                        }
                    }
                }
                if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                    stop_reason = match fr {
                        "stop" => StopReason::EndTurn,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        "content_filter" => StopReason::Refusal,
                        s => StopReason::Other(s.to_string()),
                    };
                }
            }
        }
        if let Some(u) = v.get("usage") {
            // OpenAI: prompt_tokens / completion_tokens → input/output.
            usage.input_tokens = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
            usage.output_tokens = u
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
    }

    let mut content: Vec<ContentBlock> = Vec::new();
    if !text.is_empty() {
        content.push(ContentBlock::Text { text });
    }
    for (_idx, (tc_id, name, args)) in tool_calls {
        let input: Value = if args.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            // Fail loud on malformed accumulated arguments: mirror
            // parse_sse_stream's ClientError::Parse rather than
            // silently degrading to Null (which would propagate a confusing
            // null-input ToolUse to the dispatcher).
            serde_json::from_str(&args)
                .map_err(|e| ClientError::Parse(format!("openai tool_call arguments JSON: {e}")))?
        };
        content.push(ContentBlock::ToolUse {
            id: tc_id,
            name,
            input,
        });
    }

    Ok(Message {
        id,
        model,
        stop_reason,
        content,
        usage,
    })
}

// ---------------------------------------------------------------------------
// OpenAiTransport (HTTP — correct-by-construction, mirrors ReqwestTransport)
// ---------------------------------------------------------------------------

/// The OpenAI-compatible HTTP transport: posts the mapped body to
/// `{base_url}/v1/chat/completions` with `Authorization: Bearer <api_key>` and
/// streams the SSE body. Mirrors [`crate::client::ReqwestTransport`]'s structure
/// (reqwest client + internal tokio runtime + `block_on`); differs only in URL,
/// auth header, and that the body is [`request_to_openai_body`]-mapped (not the
/// `Request` serialized directly).
///
/// Shares [`crate::http`] with the Anthropic transport: the base URL is
/// validated (https, or http for loopback only) and joined without doubling a
/// trailing `/v1` (`http://localhost:11434/v1` → `…/v1/chat/completions`); an
/// invalid base URL fails every send with [`TransportError::InvalidBaseUrl`]
/// before the key is sent. `timeout` is a per-read idle timeout (plus a fixed
/// connect timeout), not a total deadline; the body is size-capped.
pub struct OpenAiTransport {
    core: HttpCore,
    endpoint: Result<String, String>,
    api_key: String,
}

impl OpenAiTransport {
    /// Build from a [`crate::client::ClientConfig`]. The reqwest client + a
    /// dedicated single-threaded tokio runtime (mirrors `ReqwestTransport`).
    #[must_use]
    pub fn new(base_url: &str, api_key: &str, timeout: Option<Duration>) -> Self {
        Self {
            core: HttpCore::new(timeout),
            endpoint: join_endpoint(base_url, "/v1/chat/completions"),
            api_key: api_key.to_string(),
        }
    }

    /// The resolved `…/chat/completions` URL, or the base-URL validation error.
    pub fn endpoint(&self) -> Result<&str, &str> {
        self.endpoint.as_deref().map_err(String::as_str)
    }
}

impl Transport for OpenAiTransport {
    fn send(&self, req: &Request) -> Result<RawResponse, TransportError> {
        self.send_with_meta(req).map(|(r, _)| r)
    }

    fn send_with_meta(&self, req: &Request) -> Result<(RawResponse, ResponseMeta), TransportError> {
        // Fail closed before the key leaves the process.
        let url = self
            .endpoint
            .as_ref()
            .map_err(|e| TransportError::InvalidBaseUrl(e.clone()))?;
        // Map the neutral Request → OpenAI body (NOT a direct serialize).
        let body = serde_json::to_vec(&request_to_openai_body(req))?;
        self.core.post(
            url,
            &[
                ("Authorization", format!("Bearer {}", self.api_key)),
                ("content-type", "application/json".to_string()),
            ],
            body,
        )
    }
}

// Allow Arc<dyn Transport> construction ergonomics.
#[allow(dead_code)]
fn _assert_transport_send_sync() {
    fn _is_send_sync<T: Transport>(_t: T) {}
    _is_send_sync(OpenAiTransport::new("x", "y", None));
    let _: Arc<dyn Transport> = Arc::new(OpenAiTransport::new("x", "y", None));
}
