//! Real-API smoke test for morphe-ai.
//!
//! Verifies the wire shape of [`morphe_ai::Client`] against the real Anthropic
//! Messages API: a minimal end-turn roundtrip with no tools and adaptive
//! thinking. SKIPPED unless `MORPHE_ANTHROPIC_API_KEY` is set, so CI (which has
//! no key) never runs it. Run locally:
//!
//! ```sh
//! MORPHE_ANTHROPIC_API_KEY=<your-key> cargo test -p morphe-ai --test smoke_real_api
//! ```
//!
//! This is the only test in the crate that touches the network; everything
//! else runs against recorded SSE fixtures via `MockTransport`/`SeqTransport`.

use morphe_ai::{
    Client, ClientConfig, ContentBlock, InputContentBlock, InputMessage, MessageContent, Request,
    StopReason, ThinkingConfig,
};

/// A single end-turn roundtrip against the real Messages API.
///
/// Asserts the client can serialize a minimal request, the API returns a
/// well-formed SSE stream, the parser accumulates it into a [`Message`], and
/// `stop_reason == end_turn` with at least one text block.
#[test]
fn real_api_end_turn_roundtrip() {
    // Eager skip when no key is present — CI never sets this, so the test
    // exits 0 there instead of failing.
    let key = match std::env::var("MORPHE_ANTHROPIC_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            eprintln!(
                "[smoke_real_api] MORPHE_ANTHROPIC_API_KEY not set — skipping real-API smoke test"
            );
            return;
        }
    };

    let config = ClientConfig {
        api_key: key,
        // Tight budget is intentional for the smoke test: a trivial prompt +
        // adaptive thinking should still end_turn within 64 tokens on Sonnet 5.5.
        max_tokens: 64,
        ..ClientConfig::default()
    };
    let client = Client::new(config);

    let request = Request {
        model: "claude-sonnet-5-5".to_string(),
        max_tokens: 64,
        system: vec![],
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![InputContentBlock::Text {
                text: "Reply with exactly the word: pong".to_string(),
            }]),
        }],
        tools: vec![],
        tool_choice: None,
        thinking: ThinkingConfig::adaptive(),
        stream: true,
    };

    let message = client
        .send(&request)
        .expect("real-API request should parse into a Message");

    assert_eq!(
        message.stop_reason,
        StopReason::EndTurn,
        "expected end_turn, got {:?}: {:?}",
        message.stop_reason,
        message.content
    );
    let has_text = message
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. }));
    assert!(has_text, "expected at least one text block: {message:?}");
}
