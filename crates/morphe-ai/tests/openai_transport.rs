//! OpenAI-compatible transport + SSE parser tests.
//!
//! Exercises the provider-agnostic transport against canned OpenAI SSE
//! streams + mapped bodies (no network — the parser + mapping are pure; the
//! HTTP transport is correct-by-construction mirroring ReqwestTransport). One
//! test drives the real Client.send routing with a canned transport.

use std::sync::Arc;

use morphe_ai::client::{Client, ClientConfig, RawResponse, Request, Transport, TransportError};
use morphe_ai::{
    parse_openai_sse_stream, request_to_openai_body, ContentBlock, MessageContent, Provider,
    StopReason, SystemBlock, ToolChoice,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Canned OpenAI Chat Completions SSE streams
// ---------------------------------------------------------------------------

/// A text-only stream: role chunk + two content deltas + finish_reason:stop +
/// usage + [DONE]. Parses to "Hello world", EndTurn, usage 10/2.
fn text_stream() -> String {
    let chunk = |delta: serde_json::Value,
                 finish_reason: Option<&str>,
                 usage: Option<serde_json::Value>|
     -> String {
        let mut choice = json!({"index": 0, "delta": delta});
        if let Some(fr) = finish_reason {
            choice["finish_reason"] = json!(fr);
        }
        let mut obj = json!({"id": "chatcmpl-1", "model": "gpt-4o", "choices": [choice]});
        if let Some(u) = usage {
            obj["usage"] = u; // sibling of choices (top-level chunk field)
        }
        format!("data: {obj}\n\n")
    };
    let mut s = String::new();
    s.push_str(&chunk(json!({"role":"assistant","content":""}), None, None));
    s.push_str(&chunk(json!({"content":"Hello"}), None, None));
    s.push_str(&chunk(json!({"content":" world"}), None, None));
    s.push_str(&chunk(
        json!({}),
        Some("stop"),
        Some(json!({"prompt_tokens":10,"completion_tokens":2})),
    ));
    s.push_str("data: [DONE]\n\n");
    s
}

/// A tool-call stream: the function call's id+name arrive in the first delta;
/// the arguments stream across two deltas; finish_reason:tool_calls.
fn tool_call_stream() -> String {
    let chunk = |delta: &str, fr: Option<&str>| -> String {
        let fr = fr
            .map(|f| format!(",\"finish_reason\":\"{f}\""))
            .unwrap_or_default();
        format!(
            "data: {{\"id\":\"chatcmpl-2\",\"model\":\"gpt-4o\",\"choices\":[{{\"index\":0,\"delta\":{delta}{fr}}}]}}\n\n"
        )
    };
    let mut s = String::new();
    s.push_str(&chunk(
        r#"{"role":"assistant","tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"compile","arguments":""}}]}"#,
        None,
    ));
    s.push_str(&chunk(
        r#"{"tool_calls":[{"index":0,"function":{"arguments":"{\"source\""}}]}"#,
        None,
    ));
    s.push_str(&chunk(
        r#"{"tool_calls":[{"index":0,"function":{"arguments":":\"cube(1);\"}"}}]}"#,
        None,
    ));
    s.push_str(&chunk("{}", Some("tool_calls")));
    s.push_str("data: [DONE]\n\n");
    s
}

// ---------------------------------------------------------------------------
// text-only stream → Text + EndTurn
// ---------------------------------------------------------------------------

#[test]
fn text_stream_parses_to_text_and_end_turn() {
    let msg = parse_openai_sse_stream(&text_stream()).expect("parse");
    assert_eq!(msg.id, "chatcmpl-1");
    assert_eq!(msg.model, "gpt-4o");
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "Hello world".to_string()
        }]
    );
    assert_eq!(msg.usage.input_tokens, 10);
    assert_eq!(msg.usage.output_tokens, 2);
}

// ---------------------------------------------------------------------------
// tool-call stream → ToolUse (id+name+parsed input) + ToolUse stop
// ---------------------------------------------------------------------------

#[test]
fn tool_call_stream_accumulates_to_tool_use() {
    let msg = parse_openai_sse_stream(&tool_call_stream()).expect("parse");
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    assert_eq!(msg.content.len(), 1, "one tool-use block");
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_abc");
            assert_eq!(name, "compile");
            assert_eq!(
                input,
                &json!({"source": "cube(1);"}),
                "streamed arguments parsed"
            );
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// finish_reason mapping + [DONE] terminator
// ---------------------------------------------------------------------------

#[test]
fn finish_reason_maps_and_done_terminates() {
    let one = |fr: &str| -> String {
        format!(
            "data: {{\"id\":\"x\",\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"{fr}\"}}]}}\n\ndata: [DONE]\n\n"
        )
    };
    for (fr, expected) in [
        ("stop", StopReason::EndTurn),
        ("tool_calls", StopReason::ToolUse),
        ("length", StopReason::MaxTokens),
        ("content_filter", StopReason::Refusal),
    ] {
        let msg = parse_openai_sse_stream(&one(fr)).expect("parse");
        assert_eq!(msg.stop_reason, expected, "finish_reason {fr}");
    }
    // [DONE] with no prior finish_reason → Other("unknown") (no panic).
    let msg = parse_openai_sse_stream("data: [DONE]\n\n").expect("parse");
    assert!(matches!(msg.stop_reason, StopReason::Other(_)));
}

// ---------------------------------------------------------------------------
// request_to_openai_body: Request → valid OpenAI Chat Completions body
// ---------------------------------------------------------------------------

#[test]
fn request_maps_to_openai_body() {
    let req = Request {
        model: "gpt-4o".to_string(),
        max_tokens: 1024,
        system: vec![SystemBlock::text("you are morphe")],
        messages: vec![morphe_ai::client::InputMessage {
            role: "user".to_string(),
            content: MessageContent::Text("cube(1);".to_string()),
        }],
        tools: vec![],
        tool_choice: Some(ToolChoice::Auto),
        thinking: morphe_ai::client::ThinkingConfig::adaptive(),
        stream: true,
    };
    let body = request_to_openai_body(&req);

    // system → a leading {role:system} message.
    let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
    assert_eq!(messages.len(), 2, "system + user");
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("system")
    );
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("you are morphe")
    );
    assert_eq!(
        messages[1].get("role").and_then(|v| v.as_str()),
        Some("user")
    );

    // thinking OMITTED (OpenAI has no thinking field).
    assert!(
        body.get("thinking").is_none(),
        "thinking must be omitted from the OpenAI body"
    );
    // stream true, model, max_tokens carried.
    assert_eq!(body.get("stream").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(body.get("model").and_then(|v| v.as_str()), Some("gpt-4o"));
    assert_eq!(body.get("max_tokens").and_then(|v| v.as_u64()), Some(1024));
    // No tools → no tools/tool_choice fields.
    assert!(body.get("tools").is_none());
}

#[test]
fn request_maps_assistant_tool_use_and_user_tool_result() {
    use morphe_ai::client::{InputContentBlock, InputMessage};
    let req = Request {
        model: "gpt-4o".to_string(),
        max_tokens: 1024,
        system: vec![],
        messages: vec![
            InputMessage {
                role: "assistant".to_string(),
                content: MessageContent::Blocks(vec![InputContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "compile".to_string(),
                    input: json!({"source": "cube(1);"}),
                }]),
            },
            InputMessage {
                role: "user".to_string(),
                content: MessageContent::Blocks(vec![InputContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                }]),
            },
        ],
        tools: vec![morphe_ai::ToolDef {
            name: "compile".to_string(),
            description: "compile".to_string(),
            input_schema: json!({"type": "object"}),
        }],
        tool_choice: Some(ToolChoice::Auto),
        thinking: morphe_ai::client::ThinkingConfig::adaptive(),
        stream: true,
    };
    let body = request_to_openai_body(&req);
    let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
    // assistant tool_use → {role:assistant, tool_calls:[...]}.
    let asst = &messages[0];
    assert_eq!(asst.get("role").and_then(|v| v.as_str()), Some("assistant"));
    let tc = asst
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .expect("assistant has tool_calls");
    assert_eq!(tc[0].get("id").and_then(|v| v.as_str()), Some("call_1"));
    assert_eq!(
        tc[0]
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str()),
        Some("compile")
    );
    // user tool_result → {role:tool, tool_call_id}.
    let tool_msg = &messages[1];
    assert_eq!(tool_msg.get("role").and_then(|v| v.as_str()), Some("tool"));
    assert_eq!(
        tool_msg.get("tool_call_id").and_then(|v| v.as_str()),
        Some("call_1")
    );
    // tools → [{type:function, function:{name, description, parameters}}].
    let tools = body.get("tools").and_then(|v| v.as_array()).unwrap();
    assert_eq!(
        tools[0].get("type").and_then(|v| v.as_str()),
        Some("function")
    );
    assert_eq!(
        tools[0]
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str()),
        Some("compile")
    );
    // tool_choice → "auto" (Auto with tools present).
    assert_eq!(
        body.get("tool_choice").and_then(|v| v.as_str()),
        Some("auto")
    );
}

// ---------------------------------------------------------------------------
// OpenAiTransport constructs + is a usable Transport (HTTP is
// correct-by-construction, mirroring ReqwestTransport; the body it sends is
// request_to_openai_body, covered by the body-mapping tests).
// ---------------------------------------------------------------------------

#[test]
fn openai_transport_constructs_and_is_dyn_transport() {
    let t: Arc<dyn Transport> = Arc::new(morphe_ai::OpenAiTransport::new(
        "https://api.openai.com",
        "sk-test",
        None,
    ));
    // Trait object is usable (the production Client::new path). We don't fire a
    // real request here (no network); the URL/auth/body are correct-by-
    // construction + the body-mapping tests cover the body.
    let _ = t;
}

// ---------------------------------------------------------------------------
// Client.send routes the parser by provider (the wire join)
// ---------------------------------------------------------------------------

/// A canned transport returning a fixed RawResponse (no network). Implements
/// the real seam so Client.send can't tell it from the production transport.
struct CannedTransport {
    body: String,
}
impl Transport for CannedTransport {
    fn send(&self, _req: &Request) -> Result<RawResponse, TransportError> {
        Ok(RawResponse {
            status: 200,
            body: self.body.clone(),
        })
    }
}

#[test]
fn client_routes_openai_parser_by_provider() {
    // provider=OpenAiCompatible + an OpenAI SSE body → parsed via the OpenAI
    // parser. Had it routed to parse_sse_stream (Anthropic), the OpenAI-shaped
    // body would parse to empty/garbage (no message_start/content_block events).
    let cfg = ClientConfig {
        provider: Provider::OpenAiCompatible,
        ..ClientConfig::default()
    };
    let client = Client::with_transport(
        Arc::new(CannedTransport {
            body: text_stream(),
        }),
        cfg,
    );
    let req = Request {
        model: "gpt-4o".to_string(),
        max_tokens: 16,
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: None,
        thinking: morphe_ai::client::ThinkingConfig::adaptive(),
        stream: true,
    };
    let msg = client.send(&req).expect("send");
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "Hello world".to_string()
        }],
        "OpenAI body routed to the OpenAI parser"
    );
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
}

// ---------------------------------------------------------------------------
// Provider + ClientConfig defaults
// ---------------------------------------------------------------------------

#[test]
fn provider_defaults_and_clientconfig_default() {
    assert_eq!(
        Provider::Anthropic.default_base_url(),
        "https://api.anthropic.com"
    );
    assert_eq!(Provider::Anthropic.default_model(), "claude-sonnet-5-5");
    assert_eq!(
        Provider::OpenAiCompatible.default_base_url(),
        "https://api.openai.com"
    );
    assert_eq!(Provider::OpenAiCompatible.default_model(), "gpt-4o");
    // Default config stays Anthropic (no behavior change for existing callers).
    assert_eq!(ClientConfig::default().provider, Provider::Anthropic);
}

// ---------------------------------------------------------------------------
// Transport hardening: in-stream error chunks + base-URL joining/validation
// ---------------------------------------------------------------------------

#[test]
fn openai_error_chunk_in_stream_is_a_typed_error() {
    let body = "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"error\":{\"message\":\"The server is overloaded\",\"type\":\"server_error\",\"code\":null}}\n\n";
    match parse_openai_sse_stream(body) {
        Err(morphe_ai::ClientError::Stream {
            error_type,
            message,
        }) => {
            assert_eq!(error_type, "server_error");
            assert_eq!(message, "The server is overloaded");
        }
        other => panic!("expected Stream error, got {other:?}"),
    }
    // A chunk with `"error": null` is not an error.
    let ok = "data: {\"id\":\"c\",\"model\":\"m\",\"error\":null,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    assert_eq!(
        parse_openai_sse_stream(ok).expect("ok").stop_reason,
        StopReason::EndTurn
    );
}

#[test]
fn openai_endpoint_does_not_double_v1_and_trims_slashes() {
    let ep = |base: &str| {
        let t = morphe_ai::OpenAiTransport::new(base, "k", None);
        t.endpoint().map(str::to_string).map_err(str::to_string)
    };
    assert_eq!(
        ep("https://api.openai.com").unwrap(),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        ep("http://localhost:11434/v1").unwrap(),
        "http://localhost:11434/v1/chat/completions"
    );
    assert_eq!(
        ep("https://openrouter.ai/api/v1/").unwrap(),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(
        ep("https://api.openai.com/").unwrap(),
        "https://api.openai.com/v1/chat/completions"
    );
}

#[test]
fn openai_transport_refuses_insecure_base_url_before_sending() {
    let t = morphe_ai::OpenAiTransport::new("http://lan-box.example:11434/v1", "sk-secret", None);
    let req = Request {
        model: "m".to_string(),
        max_tokens: 1,
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: None,
        thinking: morphe_ai::client::ThinkingConfig::adaptive(),
        stream: true,
    };
    match t.send(&req) {
        Err(TransportError::InvalidBaseUrl(msg)) => assert!(msg.contains("https"), "{msg}"),
        other => panic!("expected InvalidBaseUrl, got {other:?}"),
    }
}
