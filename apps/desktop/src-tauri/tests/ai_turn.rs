//! Integration tests for the `ai_*` IPC backing functions.
//!
//! These drive the **pure** [`run_turn_blocking`] seam directly — no Tauri
//! runtime, no real network — invoking the IPC backing functions directly as
//! the backend seam. The
//! `#[tauri::command]` wrapper in `lib.rs` is a thin shell over this function.
//!
//! A scripted [`Transport`] (the morphe-ai test seam) returns recorded SSE
//! responses so the loop runs deterministically without the Anthropic API. The
//! [`AppState`] secret/source roundtrip uses [`InMemorySecretStore`].

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use morphe_desktop_lib::morphe_ai::{
    CancellationToken, Client, ClientConfig, RawResponse, Request, Transport, TransportError,
};
use morphe_desktop_lib::{
    reconstruct_final_source, revert_patch, run_turn_blocking, AiEvent, AppState,
    ProviderConfigInfo, TurnOutcome,
};
use serde_json::{json, Value};

// Re-exported for the provider-config tests below.
use morphe_desktop_lib::morphe_ai::Provider;

// ---------------------------------------------------------------------------
// Scripted transport (the morphe-ai test seam)
// ---------------------------------------------------------------------------

/// A transport that returns a scripted sequence of canned SSE responses (one
/// per `send`, in order). Returns an error if exhausted. `Clone` shares the
/// response queue so it can be wrapped in `Arc` for the `Client`.
struct ScriptedTransport {
    responses: Arc<Mutex<VecDeque<String>>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }
}

impl Transport for ScriptedTransport {
    fn send(&self, _req: &Request) -> Result<RawResponse, TransportError> {
        let body = self
            .responses
            .lock()
            .expect("scripted transport lock")
            .pop_front()
            .ok_or_else(|| TransportError::Http("ScriptedTransport exhausted".to_string()))?;
        Ok(RawResponse { status: 200, body })
    }
}

fn client_with(responses: Vec<String>) -> Client {
    Client::with_transport(
        Arc::new(ScriptedTransport::new(responses)),
        ClientConfig::default(),
    )
}

/// An empty per-turn rejected-id set (no edits rejected). `run_turn_blocking`
/// reads it after the loop to skip rejected edits when reconstructing the final
/// source.
fn empty_rejected() -> Arc<Mutex<HashSet<String>>> {
    Arc::new(Mutex::new(HashSet::new()))
}

// ---------------------------------------------------------------------------
// SSE fixture builders
// ---------------------------------------------------------------------------

/// An SSE body with one text block and `stop_reason: end_turn`.
fn sse_end_turn(text: &str) -> String {
    let start = json!({
        "type":"message_start",
        "message":{"id":"msg_end","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":1}}
    })
    .to_string();
    let block_start =
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})
            .to_string();
    let delta =
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})
            .to_string();
    let block_stop = json!({"type":"content_block_stop","index":0}).to_string();
    let msg_delta = json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}).to_string();
    let msg_stop = json!({"type":"message_stop"}).to_string();
    format!(
        "event: message_start\ndata: {start}\n\n\
         event: content_block_start\ndata: {block_start}\n\n\
         event: content_block_delta\ndata: {delta}\n\n\
         event: content_block_stop\ndata: {block_stop}\n\n\
         event: message_delta\ndata: {msg_delta}\n\n\
         event: message_stop\ndata: {msg_stop}\n"
    )
}

/// An SSE body with a text block + one tool_use block, `stop_reason: tool_use`.
fn sse_text_plus_tool(text: &str, tool_id: &str, tool_name: &str, input: &Value) -> String {
    let start = json!({
        "type":"message_start",
        "message":{"id":"msg_tool","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":1}}
    })
    .to_string();
    let text_start =
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})
            .to_string();
    let text_delta =
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})
            .to_string();
    let text_stop = json!({"type":"content_block_stop","index":0}).to_string();
    let tool_start = json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":tool_id,"name":tool_name,"input":{}}}).to_string();
    let tool_input = input.to_string();
    let tool_delta = json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":tool_input}}).to_string();
    let tool_stop = json!({"type":"content_block_stop","index":1}).to_string();
    let msg_delta = json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":20}}).to_string();
    let msg_stop = json!({"type":"message_stop"}).to_string();
    format!(
        "event: message_start\ndata: {start}\n\n\
         event: content_block_start\ndata: {text_start}\n\n\
         event: content_block_delta\ndata: {text_delta}\n\n\
         event: content_block_stop\ndata: {text_stop}\n\n\
         event: content_block_start\ndata: {tool_start}\n\n\
         event: content_block_delta\ndata: {tool_delta}\n\n\
         event: content_block_stop\ndata: {tool_stop}\n\n\
         event: message_delta\ndata: {msg_delta}\n\n\
         event: message_stop\ndata: {msg_stop}\n"
    )
}

// ---------------------------------------------------------------------------
// Acceptance tests
// ---------------------------------------------------------------------------

/// A turn that compiles then ends streams Text → ToolCall(compile) →
/// ToolResult → Done(end_turn). This is the primary end-to-end check: the
/// sync→async bridge plumbing produces the expected event sequence without the
/// real API.
#[test]
fn run_turn_streams_text_toolcall_toolresult_done() {
    let compile_input = json!({"source": "cube([10,10,10]);"});
    let round1 = sse_text_plus_tool("Let me compile.", "toolu_1", "compile", &compile_input);
    let round2 = sse_end_turn("The cube compiled with 12 triangles.");
    let client = client_with(vec![round1, round2]);

    let cancel = CancellationToken::new();
    let mut events: Vec<AiEvent> = Vec::new();
    let outcome = run_turn_blocking(
        client,
        "cube([10,10,10]);",
        None,
        None, // prior_memory: ai_turn.rs tests don't seed memory (the cross-crate
        // tests/session_digest.rs exercises build_session_digest directly).
        cancel,
        empty_rejected(),
        // noos_dir=None: ai_turn.rs tests don't exercise query_noos (the
        // cross-crate tests/query_noos.rs does that against a real graph).
        None,
        // opened_path=None: no file open in these tests.
        None,
        |ev| events.push(ev),
    );

    assert_eq!(outcome.stop_reason, "end_turn");

    // The event stream must include Text, ToolCall(compile), ToolResult, Done.
    let has_text = events
        .iter()
        .any(|e| matches!(e, AiEvent::Text { text } if text == "Let me compile."));
    assert!(has_text, "expected AssistantText event: {events:?}");

    let has_compile_call = events
        .iter()
        .any(|e| matches!(e, AiEvent::ToolCall { name, .. } if name == "compile"));
    assert!(has_compile_call, "expected compile ToolCall: {events:?}");

    let compile_result = events
        .iter()
        .find_map(|e| match e {
            AiEvent::ToolResult { id, content, .. } if id == "toolu_1" => Some(content.clone()),
            _ => None,
        })
        .expect("compile ToolResult");
    let v: Value = serde_json::from_str(&compile_result).expect("result is JSON");
    assert_eq!(v["ok"], true, "compile succeeded: {v}");
    assert_eq!(v["stats"]["is_manifold"], true);

    let done = events
        .iter()
        .find_map(|e| match e {
            AiEvent::Done { stop_reason, .. } => Some(stop_reason.clone()),
            _ => None,
        })
        .expect("a Done event");
    assert_eq!(done, "end_turn");

    // No edit_code in this turn → final_source equals the seeded source.
    assert_eq!(outcome.final_source, "cube([10,10,10]);");
}

/// A turn with an edit_code patch streams the ToolCall carrying the patch and
/// reconstructs the final source from the successful edit. This is the
/// accept/reject data path.
#[test]
fn run_turn_reconstructs_final_source_after_edit() {
    let edit_input = json!({"path": "main.scad", "old": "cube(1);", "new": "cube(2);"});
    let round1 = sse_text_plus_tool("Editing.", "toolu_edit", "edit_code", &edit_input);
    let round2 = sse_end_turn("Done editing.");
    let client = client_with(vec![round1, round2]);

    let cancel = CancellationToken::new();
    let mut events: Vec<AiEvent> = Vec::new();
    let outcome = run_turn_blocking(
        client,
        "cube(1);\n",
        None,
        None, // prior_memory (unseeded here; see tests/session_digest.rs)
        cancel,
        empty_rejected(),
        None,
        // opened_path=None: no file open in these tests.
        None,
        |ev| events.push(ev),
    );

    assert_eq!(outcome.stop_reason, "end_turn");
    // The edit_code ToolCall carries the patch (frontend renders a diff).
    let edit_call = events
        .iter()
        .find_map(|e| match e {
            AiEvent::ToolCall { name, input, .. } if name == "edit_code" => Some(input.clone()),
            _ => None,
        })
        .expect("edit_code ToolCall");
    assert_eq!(edit_call["old"], "cube(1);");
    assert_eq!(edit_call["new"], "cube(2);");

    // The reconstructed final source reflects the applied edit.
    assert_eq!(outcome.final_source, "cube(2);\n");
}

/// Cancelling before the run starts yields `stop_reason: "cancelled"` —
/// observable through the bridge without the caller blocking on the sync
/// transport.
#[test]
fn run_turn_returns_cancelled_when_token_set() {
    let client = client_with(vec![sse_end_turn("never reached")]);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let mut events: Vec<AiEvent> = Vec::new();
    let outcome = run_turn_blocking(
        client,
        "cube(1);",
        None,
        None, // prior_memory (unseeded here; see tests/session_digest.rs)
        cancel,
        empty_rejected(),
        None,
        // opened_path=None: no file open in these tests.
        None,
        |ev| events.push(ev),
    );

    assert_eq!(outcome.stop_reason, "cancelled");
    // No rounds ran, so no Text/ToolCall — but Done is still emitted.
    assert!(events
        .iter()
        .any(|e| matches!(e, AiEvent::Done { stop_reason, .. } if stop_reason == "cancelled")));
}

/// `revert_patch` (the reject-edit pure seam) undoes a successful edit so the
/// next turn/compile reflects the revert.
#[test]
fn revert_patch_undoes_applied_edit() {
    // The applied patch was cube(1); → cube(2);. Reverting re-applies the
    // inverse: cube(2); → cube(1);.
    let after_edit = "cube(2);\n";
    let reverted = revert_patch(after_edit, "cube(1);", "cube(2);").expect("revert");
    assert_eq!(reverted, "cube(1);\n");
}

/// `reconstruct_final_source` replays a multi-edit sequence in order.
#[test]
fn reconstruct_final_source_replays_in_order() {
    use morphe_desktop_lib::morphe_ai::LoopEvent;
    let events = vec![
        LoopEvent::ToolCall {
            id: "t1".into(),
            name: "edit_code".into(),
            input: json!({"path": "main.scad", "old": "a", "new": "b"}),
        },
        LoopEvent::ToolResult {
            id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        },
        LoopEvent::ToolCall {
            id: "t2".into(),
            name: "edit_code".into(),
            input: json!({"path": "main.scad", "old": "b", "new": "c"}),
        },
        LoopEvent::ToolResult {
            id: "t2".into(),
            content: "ok".into(),
            is_error: false,
        },
    ];
    assert_eq!(reconstruct_final_source(&events, "a", &HashSet::new()), "c");
}

/// The `AppState` SecretStore seam roundtrips a key: has/set/remove. This is
/// what the `ai_has_api_key` / `ai_set_provider_config` / `ai_remove_api_key`
/// IPC commands drive (`ai_set_provider_config` replaced the bare-key setter).
#[test]
fn app_state_secret_store_roundtrips() {
    let state = AppState::new_in_memory();
    // No key initially.
    assert!(!state.has_api_key(), "no key initially");

    state
        .set_provider_config(Provider::Anthropic, "test-key-12345", None, None)
        .expect("set");
    assert!(state.has_api_key(), "key set");

    state.remove_api_key().expect("remove");
    assert!(!state.has_api_key(), "key removed");
}

// ---------------------------------------------------------------------------
// Provider config IPC seam
// ---------------------------------------------------------------------------

/// set → get round-trips provider + overrides (never the key).
#[test]
fn provider_config_set_get_roundtrip() {
    let state = AppState::new_in_memory();
    state
        .set_provider_config(
            Provider::OpenAiCompatible,
            "sk-oai-test",
            Some("https://openrouter.example/api".to_string()),
            Some("gpt-4o-mini".to_string()),
        )
        .expect("set");
    let info = state.get_provider_config().expect("get");
    assert_eq!(
        info,
        ProviderConfigInfo {
            provider: Provider::OpenAiCompatible,
            base_url: Some("https://openrouter.example/api".to_string()),
            model: Some("gpt-4o-mini".to_string()),
            has_key: true,
        }
    );
    // The stored string is the ProviderConfig JSON (not a bare key).
    let stored = state
        .stored_secret_for_tests()
        .expect("stored")
        .expect("some");
    assert!(stored.contains("\"provider\":\"open_ai_compatible\""));

    // Anthropic + no overrides → defaults shape.
    state
        .set_provider_config(Provider::Anthropic, "sk-ant-test", None, None)
        .expect("set");
    assert_eq!(
        state.get_provider_config().expect("get"),
        ProviderConfigInfo {
            provider: Provider::Anthropic,
            base_url: None,
            model: None,
            has_key: true,
        }
    );
}

/// A legacy bare key reads as Anthropic + defaults (no migration).
#[test]
fn provider_config_bare_key_backward_compat() {
    let state = AppState::new_in_memory();
    // Simulate a legacy install: the OLD command stored a bare string.
    state
        .set_stored_secret_for_tests("sk-ant-legacy-key")
        .expect("set");
    assert_eq!(
        state.get_provider_config().expect("get"),
        ProviderConfigInfo {
            provider: Provider::Anthropic,
            base_url: None,
            model: None,
            has_key: true,
        }
    );
}

/// Empty store → default info with has_key=false, never an error.
#[test]
fn provider_config_empty_store_returns_default_info() {
    let state = AppState::new_in_memory();
    assert_eq!(
        state.get_provider_config().expect("get on empty"),
        ProviderConfigInfo {
            provider: Provider::Anthropic,
            base_url: None,
            model: None,
            has_key: false,
        }
    );
}

/// from_stored_string's fallback semantics pinned at the IPC seam —
/// malformed/scalar/extra-field stored strings never panic and degrade to a
/// defined shape (pinned because the IPC writer makes them load-bearing).
#[test]
fn provider_config_fallback_semantics_pinned() {
    let state = AppState::new_in_memory();

    // (a) malformed JSON missing required fields -> whole string becomes the
    // api_key, Anthropic + defaults (confusing runtime 401, never a panic).
    state
        .set_stored_secret_for_tests(r#"{"api_key":"x"}"#)
        .expect("seed");
    assert_eq!(
        state.get_provider_config().expect("get"),
        ProviderConfigInfo {
            provider: Provider::Anthropic,
            base_url: None,
            model: None,
            has_key: true,
        }
    );
    // The whole JSON string is the key (the legacy-bare-key path).
    assert_eq!(
        state.stored_secret_for_tests().expect("read"),
        Some(r#"{"api_key":"x"}"#.to_string())
    );

    // (b) valid JSON scalars stored verbatim as the key.
    state.set_stored_secret_for_tests("null").expect("seed");
    let info = state.get_provider_config().expect("get");
    assert!(info.has_key);
    assert_eq!(info.provider, Provider::Anthropic);

    // (d) whitespace-padded bare key seeded RAW keeps its whitespace: only the IPC WRITER trims — a hand-seeded/hand-edited
    // keychain entry is passed through verbatim (401 at send time, never a
    // panic, never silently rewritten).
    state
        .set_stored_secret_for_tests("   sk-ant-legacy   ")
        .expect("seed");
    assert_eq!(
        state.stored_secret_for_tests().expect("read"),
        Some("   sk-ant-legacy   ".to_string()),
        "raw-seeded entries are never silently rewritten"
    );
    let info = state.get_provider_config().expect("get");
    assert!(info.has_key);

    // (c) unknown extra fields accepted, dropped on re-store through the
    // IPC writer (set_provider_config writes a clean ProviderConfig).
    state
        .set_stored_secret_for_tests(
            r#"{"provider":"open_ai_compatible","api_key":"sk-x","evil":"leak"}"#,
        )
        .expect("seed");
    let info = state.get_provider_config().expect("get");
    assert_eq!(info.provider, Provider::OpenAiCompatible);
    assert!(info.has_key);
    state
        .set_provider_config(Provider::OpenAiCompatible, "sk-x", None, None)
        .expect("re-store");
    let clean = state
        .stored_secret_for_tests()
        .expect("read")
        .expect("some");
    assert!(
        !clean.contains("evil"),
        "extras dropped on re-store: {clean}"
    );

    // (e) valid JSON with empty api_key -> has_key true, empty key (runtime
    // 401 path — never a panic, never a NotFound).
    state
        .set_stored_secret_for_tests(r#"{"provider":"anthropic","api_key":""}"#)
        .expect("seed");
    let info = state.get_provider_config().expect("get");
    assert!(
        info.has_key,
        "empty-key entry still counts as a key present"
    );
}

/// The IPC writer trims — padded key/base_url/model normalize at
/// the write path (the enforcement point), not only in the UI.
#[test]
fn provider_config_writer_trims_fields() {
    let state = AppState::new_in_memory();
    state
        .set_provider_config(
            Provider::OpenAiCompatible,
            "  sk-oai-padded  ",
            Some("  https://openrouter.example/api  ".to_string()),
            Some("  gpt-4o-mini  ".to_string()),
        )
        .expect("set");
    assert_eq!(
        state.get_provider_config().expect("get"),
        ProviderConfigInfo {
            provider: Provider::OpenAiCompatible,
            base_url: Some("https://openrouter.example/api".to_string()),
            model: Some("gpt-4o-mini".to_string()),
            has_key: true,
        }
    );
    // The stored key is trimmed too.
    let stored = state
        .stored_secret_for_tests()
        .expect("read")
        .expect("some");
    assert!(stored.contains("sk-oai-padded"));
    assert!(!stored.contains("sk-oai-padded \""));

    // Blank strings become None (empty optional fields -> default).
    state
        .set_provider_config(
            Provider::OpenAiCompatible,
            "sk-oai",
            Some("   ".to_string()),
            Some("".to_string()),
        )
        .expect("set");
    let info = state.get_provider_config().expect("get");
    assert_eq!(info.base_url, None);
    assert_eq!(info.model, None);
}

/// Wire half: ProviderConfigInfo serializes to the IPC shape the TS
/// side mirrors — no api_key field anywhere on the wire.
#[test]
fn provider_config_info_serializes_without_key() {
    let info = ProviderConfigInfo {
        provider: Provider::OpenAiCompatible,
        base_url: None,
        model: Some("llama3.1".to_string()),
        has_key: true,
    };
    let wire = serde_json::to_string(&info).expect("serialize");
    assert!(
        !wire.to_lowercase().contains("api_key"),
        "key material or field must never appear: {wire}"
    );
    assert_eq!(
        wire,
        "{\"provider\":\"open_ai_compatible\",\"base_url\":null,\"model\":\"llama3.1\",\"has_key\":true}"
    );
}

/// The `AppState` source buffer roundtrips (the accept/reject substrate).
#[test]
fn app_state_source_roundtrips() {
    let state = AppState::new_in_memory();
    assert_eq!(state.source(), "", "empty by default");
    state.set_source("cube(1);".to_string());
    assert_eq!(state.source(), "cube(1);");
}

/// `run_turn_blocking`'s outcome is deterministic and comparable (TurnOutcome
/// derives PartialEq).
#[test]
fn turn_outcome_is_comparable() {
    let a = TurnOutcome {
        stop_reason: "end_turn".to_string(),
        final_source: "cube(1);".to_string(),
    };
    let b = TurnOutcome {
        stop_reason: "end_turn".to_string(),
        final_source: "cube(1);".to_string(),
    };
    assert_eq!(a, b);
}
