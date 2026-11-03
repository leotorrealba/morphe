//! The agentic loop for morphe-ai.
//!
//! [`AgentLoop::run`] drives the edit → compile → inspect cycle: each round it
//! sends the conversation (system + messages + tools) to Claude via the
//! [`Client`], parses the response, and either stops (`end_turn` / `max_tokens`
//! / `refusal`) or dispatches any `tool_use` blocks via the [`Dispatcher`] and
//! appends `tool_result` blocks back into the conversation for the next round.
//!
//! # Stop conditions
//!
//! - `end_turn` → [`LoopStopReason::EndTurn`] (done).
//! - `max_tokens` / `refusal` → [`LoopStopReason::MaxTokens`] /
//!   [`LoopStopReason::Refusal`].
//! - round counter ≥ `max_rounds` (default 12) → [`LoopStopReason::RoundCap`].
//! - cancellation token set → [`LoopStopReason::Cancelled`].
//!
//! The loop is **synchronous** (the [`Transport`](crate::Transport) trait is
//! sync). Do not call it from inside an async runtime — `ReqwestTransport`
//! drives its own current-thread tokio runtime via `block_on`, and nesting
//! would block that runtime's worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::client::{
    Client, ContentBlock, InputContentBlock, InputMessage, MessageContent, Request, StopReason,
};
use crate::dispatch::Dispatcher;

/// The default round cap. The loop never runs more than this many rounds,
/// preventing an unbounded tool-use cycle (a model that always calls a tool and
/// never returns `end_turn`).
pub const DEFAULT_MAX_ROUNDS: usize = 12;

/// A cancellation token the caller can set to stop the loop promptly. Clone
/// shares the flag, so a signal handler or another thread can cancel a loop
/// running on the agent's thread.
///
/// The loop checks this at the top of each round and after each transport call
/// returns, so cancellation takes effect within one round (it cannot interrupt
/// a blocking `send` mid-flight — the transport is sync).
#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the loop as cancelled. The loop returns
    /// [`LoopStopReason::Cancelled`] at its next check.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Why the loop stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopStopReason {
    /// The model returned `end_turn`.
    EndTurn,
    /// The round cap was reached without `end_turn`.
    RoundCap,
    /// The cancellation token was set.
    Cancelled,
    /// The model returned `max_tokens`.
    MaxTokens,
    /// The model returned `refusal`.
    Refusal,
    /// A transport/client error occurred, or an unrecognized stop reason.
    Error(String),
}

/// An event emitted by the loop for each round. The caller can stream these to
/// a UI as the agent works.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopEvent {
    /// Assistant text content (one per text block).
    AssistantText(String),
    /// The model requested a tool call.
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// The dispatcher produced a tool result for the given tool-use id.
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
}

/// The result of an agent loop run.
#[derive(Debug, Clone)]
pub struct LoopResult {
    pub stop_reason: LoopStopReason,
    pub events: Vec<LoopEvent>,
    /// The final assistant text (the text blocks from the last assistant
    /// response that carried text). Empty if the model never produced text.
    pub final_text: String,
}

/// The agentic loop.
pub struct AgentLoop {
    max_rounds: usize,
}

impl AgentLoop {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }

    /// Build a loop with a custom round cap (for tests).
    #[must_use]
    pub fn with_max_rounds(max_rounds: usize) -> Self {
        Self { max_rounds }
    }

    /// Run the loop to completion (or until a stop condition fires).
    ///
    /// `request` is the initial request (system + first user message + tools);
    /// its `messages` grow as the conversation progresses (assistant responses
    /// and `tool_result` user messages are appended). `client` sends each
    /// round; `dispatcher` executes tool calls; `cancel` stops the loop.
    pub fn run(
        &self,
        request: Request,
        client: &Client,
        dispatcher: &mut Dispatcher,
        cancel: &CancellationToken,
    ) -> LoopResult {
        self.run_observed(request, client, dispatcher, cancel, &mut |_| {})
    }

    /// Like [`run`](Self::run), but also calls `on_event` with each
    /// [`LoopEvent`] the moment it is produced (assistant text, tool call,
    /// tool result), so a UI sees progress live instead of only after the
    /// whole loop finishes. The returned [`LoopResult::events`] holds the same
    /// events in the same order.
    pub fn run_observed(
        &self,
        mut request: Request,
        client: &Client,
        dispatcher: &mut Dispatcher,
        cancel: &CancellationToken,
        on_event: &mut dyn FnMut(&LoopEvent),
    ) -> LoopResult {
        let mut events = Vec::new();
        let mut final_text = String::new();

        for _ in 0..self.max_rounds {
            // Check cancellation at the top of each round (before the transport
            // call) — catches a cancel set during the previous round's dispatch.
            if cancel.is_cancelled() {
                return LoopResult {
                    stop_reason: LoopStopReason::Cancelled,
                    events,
                    final_text,
                };
            }

            // send_cancellable: transient overloads are retried inside the
            // client (fresh request, before any tool of this round runs) and
            // the backoff aborts promptly on cancel.
            let msg = match client.send_cancellable(&request, cancel) {
                Ok(m) => m,
                Err(_) if cancel.is_cancelled() => {
                    return LoopResult {
                        stop_reason: LoopStopReason::Cancelled,
                        events,
                        final_text,
                    };
                }
                Err(e) => {
                    return LoopResult {
                        stop_reason: LoopStopReason::Error(e.to_string()),
                        events,
                        final_text,
                    };
                }
            };

            // Check cancellation after the transport call returns — catches a
            // cancel set during the (potentially blocking) send, before we
            // dispatch tools or continue.
            if cancel.is_cancelled() {
                return LoopResult {
                    stop_reason: LoopStopReason::Cancelled,
                    events,
                    final_text,
                };
            }

            let has_tool_use = msg
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            // Tools run ONLY when the model actually stopped to call them. A
            // tool_use block under any other stop_reason (notably max_tokens,
            // where the input JSON may be truncated) is never dispatched.
            if has_tool_use && msg.stop_reason == StopReason::ToolUse {
                // Tool-use round: emit events, dispatch each tool_use, append
                // the assistant message (with thinking + tool_use blocks
                // echoed, in order) and a user message of tool_result blocks.
                let mut assistant_blocks: Vec<InputContentBlock> = Vec::new();
                let mut tool_results: Vec<InputContentBlock> = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            emit(
                                &mut events,
                                on_event,
                                LoopEvent::AssistantText(text.clone()),
                            );
                            final_text = text.clone();
                            assistant_blocks.push(InputContentBlock::Text { text: text.clone() });
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            emit(
                                &mut events,
                                on_event,
                                LoopEvent::ToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: input.clone(),
                                },
                            );
                            // Echo the tool_use block into the assistant
                            // message so the API can match the tool_result.
                            assistant_blocks.push(InputContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            });
                            let result = dispatcher.dispatch(name, input);
                            emit(
                                &mut events,
                                on_event,
                                LoopEvent::ToolResult {
                                    id: id.clone(),
                                    content: result.content.clone(),
                                    is_error: result.is_error,
                                },
                            );
                            tool_results.push(InputContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: result.content,
                                is_error: if result.is_error { Some(true) } else { None },
                            });
                        }
                        // Thinking blocks are echoed back verbatim (with
                        // signature) — the API requires it in tool-use loops.
                        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                            assistant_blocks.push(block.to_input_block());
                        }
                    }
                }

                request.messages.push(InputMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Blocks(assistant_blocks),
                });
                request.messages.push(InputMessage {
                    role: "user".to_string(),
                    content: MessageContent::Blocks(tool_results),
                });
                // Continue to the next round.
            } else {
                // No tool calls — the model is done (or stopped). Emit any
                // text and return based on the stop reason.
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        emit(
                            &mut events,
                            on_event,
                            LoopEvent::AssistantText(text.clone()),
                        );
                        final_text = text.clone();
                    }
                }
                let stop_reason = match msg.stop_reason {
                    StopReason::MaxTokens if has_tool_use => LoopStopReason::Error(
                        "response hit max_tokens in the middle of a tool call; the (possibly \
                         truncated) tool call was NOT executed — raise max_tokens or ask for a \
                         smaller step"
                            .to_string(),
                    ),
                    StopReason::EndTurn => LoopStopReason::EndTurn,
                    StopReason::MaxTokens => LoopStopReason::MaxTokens,
                    StopReason::Refusal => LoopStopReason::Refusal,
                    StopReason::ToolUse => LoopStopReason::Error(
                        "stop_reason was tool_use but no tool_use blocks were present".to_string(),
                    ),
                    StopReason::Other(s) => {
                        LoopStopReason::Error(format!("unrecognized stop_reason: {s}"))
                    }
                };
                return LoopResult {
                    stop_reason,
                    events,
                    final_text,
                };
            }
        }

        // Exceeded the round cap without end_turn.
        LoopResult {
            stop_reason: LoopStopReason::RoundCap,
            events,
            final_text,
        }
    }
}

impl Default for AgentLoop {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the sync [`AgentLoop`] on a dedicated OS thread, bridging events back to
/// the caller over a std channel.
///
/// The agent loop and its [`Transport`](crate::Transport) are synchronous;
/// [`ReqwestTransport`](crate::ReqwestTransport) drives an internal tokio
/// runtime via `block_on`. Calling that from inside the Tauri async runtime
/// would block a worker. This helper moves the whole loop onto a bare OS
/// thread so the runtime stays free — the channel is the sync→async bridge.
///
/// Each [`LoopEvent`] produced by the run is sent through the returned
/// [`std::sync::mpsc::Receiver`]; the [`LoopResult`] comes back via the
/// [`std::thread::JoinHandle`]. Cancel from any thread by setting the shared
/// [`CancellationToken`] — the loop checks it at the top of each round and
/// after each transport call.
///
/// The loop stays synchronous internally; the `Transport` trait is unchanged.
#[allow(clippy::type_complexity)]
pub fn run_on_thread(
    request: Request,
    client: Client,
    mut dispatcher: Dispatcher,
    cancel: CancellationToken,
) -> (
    std::sync::mpsc::Receiver<LoopEvent>,
    std::thread::JoinHandle<LoopResult>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<LoopEvent>();
    let handle = std::thread::spawn(move || {
        // Forward each event the moment the loop produces it (previously all
        // events were sent only after the whole run finished, so the UI saw
        // nothing for up to `max_rounds` rounds and a mid-turn reject could
        // never land in time). A send error means the receiver was dropped —
        // the loop still completes and the result is returned via the join
        // handle. `tx` is dropped when this closure returns, which ends the
        // caller's `recv` loop.
        AgentLoop::new().run_observed(request, &client, &mut dispatcher, &cancel, &mut |event| {
            let _ = tx.send(event.clone());
        })
    });
    (rx, handle)
}

/// Push `event` into `events` and hand it to the live observer.
fn emit(events: &mut Vec<LoopEvent>, on_event: &mut dyn FnMut(&LoopEvent), event: LoopEvent) {
    on_event(&event);
    events.push(event);
}

#[cfg(test)]
mod tests {
    //! Agent-loop tests. All tests drive the loop with
    //! in-process transports (no network): [`SeqTransport`] returns a scripted
    //! sequence of SSE responses; [`CancelAfterFirst`] cancels mid-run.

    use super::*;
    use crate::client::{
        ClientConfig, RawResponse, ThinkingConfig, ToolChoice, Transport, TransportError,
    };
    use crate::dispatch::{Dispatcher, InMemoryWorkspace};
    use crate::tools::tool_definitions;
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    // --- Test transports ---

    /// A transport that returns a scripted sequence of canned responses (one
    /// per `send`, in order). Captures the last request sent so tests can
    /// assert the wire shape (e.g. the tool_result was fed back). Returns an
    /// error if exhausted (the loop asked for more responses than scripted).
    #[derive(Clone)]
    struct SeqTransport {
        responses: Arc<Mutex<VecDeque<RawResponse>>>,
        last_request: Arc<Mutex<Option<Value>>>,
    }

    impl SeqTransport {
        fn new(responses: Vec<RawResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                last_request: Arc::new(Mutex::new(None)),
            }
        }

        fn last_request(&self) -> Value {
            self.last_request
                .lock()
                .expect("seq lock")
                .clone()
                .unwrap_or(Value::Null)
        }
    }

    impl Transport for SeqTransport {
        fn send(&self, req: &Request) -> Result<RawResponse, TransportError> {
            let val = serde_json::to_value(req)?;
            *self.last_request.lock().expect("seq lock") = Some(val);
            self.responses
                .lock()
                .expect("seq lock")
                .pop_front()
                .ok_or_else(|| TransportError::Http("SeqTransport exhausted".to_string()))
        }
    }

    /// A transport that returns one canned response on the first `send` and
    /// sets the cancellation token (so the loop stops before the next round).
    /// A second `send` is an error (the loop should not reach it).
    struct CancelAfterFirst {
        response: RawResponse,
        cancel: CancellationToken,
        calls: AtomicUsize,
    }

    impl CancelAfterFirst {
        fn new(response: RawResponse, cancel: CancellationToken) -> Self {
            Self {
                response,
                cancel,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Transport for CancelAfterFirst {
        fn send(&self, req: &Request) -> Result<RawResponse, TransportError> {
            let _ = serde_json::to_value(req)?;
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First (and only) call: return the response, then cancel so
                // the loop stops before dispatching / continuing.
                self.cancel.cancel();
                Ok(self.response.clone())
            } else {
                Err(TransportError::Http(
                    "CancelAfterFirst: should not be reached (loop did not stop)".to_string(),
                ))
            }
        }
    }

    // --- SSE fixture helpers ---

    /// Build an SSE body with one text block and `stop_reason: end_turn`.
    fn sse_end_turn(text: &str) -> String {
        let start = json!({
            "type":"message_start",
            "message":{"id":"msg_01","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":1}}
        })
        .to_string();
        let block_start = json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string();
        let delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}).to_string();
        let block_stop = json!({"type":"content_block_stop","index":0}).to_string();
        let msg_delta = json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":10}}).to_string();
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

    /// Build an SSE body with a text block + a tool_use block, ending with
    /// `stop_reason: tool_use`. The tool input JSON is sent in one
    /// `input_json_delta` (the parser accumulates it).
    fn sse_tool_use(text: &str, tool_id: &str, tool_name: &str, input: &Value) -> String {
        let start = json!({
            "type":"message_start",
            "message":{"id":"msg_02","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":1}}
        })
        .to_string();
        let text_start = json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string();
        let text_delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}).to_string();
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

    /// A minimal initial request for the loop (system + one user message + tools).
    fn sample_request() -> Request {
        Request {
            model: "claude-sonnet-5".to_string(),
            max_tokens: 1024,
            system: vec![crate::client::SystemBlock::text(
                "You are a morphe CAD assistant.",
            )],
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

    fn client_with(transport: Arc<dyn Transport>) -> Client {
        Client::with_transport(transport, ClientConfig::default())
    }

    // --- Acceptance tests ---

    #[test]
    fn loop_stops_at_round_cap() {
        // A transport that ALWAYS returns a tool_use (never end_turn). The loop
        // must stop at the 12-round cap, not loop forever. Uses a stub tool
        // (screenshot_viewport) so no real compilation runs each round.
        let body = sse_tool_use("", "toolu_cap", "screenshot_viewport", &json!({}));
        let responses = vec![RawResponse { status: 200, body }; 15];
        let transport = Arc::new(SeqTransport::new(responses));
        let client = client_with(transport.clone());
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let cancel = CancellationToken::new();

        let result = AgentLoop::new().run(sample_request(), &client, &mut dispatcher, &cancel);

        assert_eq!(result.stop_reason, LoopStopReason::RoundCap);
        // Exactly 12 tool calls (one per round) — not 15.
        let tool_calls = result
            .events
            .iter()
            .filter(|e| matches!(e, LoopEvent::ToolCall { .. }))
            .count();
        assert_eq!(
            tool_calls, DEFAULT_MAX_ROUNDS,
            "loop ran exactly max_rounds rounds"
        );
    }

    #[test]
    fn loop_cancels_before_first_round() {
        // Cancel set before run → the loop stops immediately, no events.
        let transport = Arc::new(SeqTransport::new(vec![RawResponse {
            status: 200,
            body: sse_end_turn("done"),
        }]));
        let client = client_with(transport);
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = AgentLoop::new().run(sample_request(), &client, &mut dispatcher, &cancel);

        assert_eq!(result.stop_reason, LoopStopReason::Cancelled);
        assert!(result.events.is_empty(), "no rounds should run");
    }

    #[test]
    fn loop_cancels_mid_round() {
        // The transport cancels on the first send; the loop checks cancellation
        // after send returns (before dispatching tools) and stops promptly.
        let body = sse_tool_use("working", "toolu_1", "screenshot_viewport", &json!({}));
        let cancel = CancellationToken::new();
        let transport = Arc::new(CancelAfterFirst::new(
            RawResponse { status: 200, body },
            cancel.clone(),
        ));
        let client = client_with(transport);
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);

        let result = AgentLoop::new().run(sample_request(), &client, &mut dispatcher, &cancel);

        assert_eq!(result.stop_reason, LoopStopReason::Cancelled);
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn loop_runs_to_completion_on_end_turn() {
        // One response: text + end_turn. The loop returns the final text and an
        // AssistantText event.
        let transport = Arc::new(SeqTransport::new(vec![RawResponse {
            status: 200,
            body: sse_end_turn("The cube is ready."),
        }]));
        let client = client_with(transport);
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let cancel = CancellationToken::new();

        let result = AgentLoop::new().run(sample_request(), &client, &mut dispatcher, &cancel);

        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        assert_eq!(result.final_text, "The cube is ready.");
        assert!(result.events.iter().any(|e| matches!(
            e,
            LoopEvent::AssistantText(t) if t == "The cube is ready."
        )));
    }

    #[test]
    fn loop_dispatches_tools_and_feeds_results() {
        // Round 1: tool_use for compile → the loop dispatches it via the
        // dispatcher and feeds the tool_result back. Round 2: end_turn.
        // Assert: the tool was dispatched, the result was produced, and the
        // second request carries a tool_result block for the tool_use id.
        let compile_input = json!({"source": "cube([10,10,10]);"});
        let first = sse_tool_use("Let me compile.", "toolu_1", "compile", &compile_input);
        let second = sse_end_turn("The cube compiled with 12 triangles.");
        let transport = Arc::new(SeqTransport::new(vec![
            RawResponse {
                status: 200,
                body: first,
            },
            RawResponse {
                status: 200,
                body: second,
            },
        ]));
        let client = client_with(transport.clone());
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let cancel = CancellationToken::new();

        let result = AgentLoop::new().run(sample_request(), &client, &mut dispatcher, &cancel);

        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        assert_eq!(result.final_text, "The cube compiled with 12 triangles.");

        // The compile tool was dispatched.
        let tool_call = result
            .events
            .iter()
            .find_map(|e| match e {
                LoopEvent::ToolCall { name, .. } if name == "compile" => Some(name.clone()),
                _ => None,
            })
            .expect("compile tool call");
        assert_eq!(tool_call, "compile");

        // The tool result was produced and carries the compile stats.
        let tool_result_content = result
            .events
            .iter()
            .find_map(|e| match e {
                LoopEvent::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("tool result");
        let result_v: Value = serde_json::from_str(&tool_result_content).expect("result is JSON");
        assert_eq!(result_v["ok"], true);
        assert_eq!(result_v["stats"]["triangle_count"], 12);

        // The second request (captured as the last request) must feed the
        // tool_result back as a user message block for toolu_1.
        let last_req = transport.last_request();
        let messages = last_req["messages"].as_array().expect("messages array");
        let has_tool_result = messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == "toolu_1")
                    })
                    .unwrap_or(false)
        });
        assert!(
            has_tool_result,
            "the second request must carry the tool_result block for toolu_1"
        );

        // And the assistant message must echo the tool_use block.
        let has_tool_use_echo = messages.iter().any(|m| {
            m["role"] == "assistant"
                && m["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b["type"] == "tool_use" && b["id"] == "toolu_1")
                    })
                    .unwrap_or(false)
        });
        assert!(
            has_tool_use_echo,
            "the assistant message must echo the tool_use block for the API to match the result"
        );
    }

    #[test]
    fn loop_stops_on_max_tokens() {
        let body = sse_with_stop_reason("max_tokens");
        let transport = Arc::new(SeqTransport::new(vec![RawResponse { status: 200, body }]));
        let client = client_with(transport);
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let cancel = CancellationToken::new();

        let result = AgentLoop::new().run(sample_request(), &client, &mut dispatcher, &cancel);

        assert_eq!(result.stop_reason, LoopStopReason::MaxTokens);
    }

    // --- live event streaming (run_on_thread) ---

    /// A transport whose SECOND send blocks until the test opens a gate, so
    /// the test can prove round-1 events were delivered while the loop was
    /// still running.
    struct GatedTransport {
        first: Mutex<Option<RawResponse>>,
        second: Mutex<Option<RawResponse>>,
        gate: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl Transport for GatedTransport {
        fn send(&self, _req: &Request) -> Result<RawResponse, TransportError> {
            if let Some(r) = self.first.lock().expect("lock").take() {
                return Ok(r);
            }
            self.gate
                .lock()
                .expect("lock")
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("test opens the gate");
            Ok(self.second.lock().expect("lock").take().expect("second"))
        }
    }

    #[test]
    fn run_on_thread_streams_events_before_the_loop_finishes() {
        let (open_gate, gate) = std::sync::mpsc::channel::<()>();
        let transport = Arc::new(GatedTransport {
            first: Mutex::new(Some(RawResponse {
                status: 200,
                body: sse_tool_use("looking", "toolu_1", "screenshot_viewport", &json!({})),
            })),
            second: Mutex::new(Some(RawResponse {
                status: 200,
                body: sse_end_turn("done"),
            })),
            gate: Mutex::new(gate),
        });
        let client = client_with(transport);
        let dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let (rx, handle) = run_on_thread(
            sample_request(),
            client,
            dispatcher,
            CancellationToken::new(),
        );

        // Round 1's events must arrive while round 2's send is still blocked.
        let timeout = std::time::Duration::from_secs(10);
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|e| matches!(e, LoopEvent::ToolResult { .. }))
        {
            seen.push(
                rx.recv_timeout(timeout)
                    .expect("event streamed before loop end"),
            );
        }
        assert!(!handle.is_finished(), "the loop must still be running");

        open_gate.send(()).expect("open gate");
        let rest: Vec<LoopEvent> = rx.iter().collect();
        let result = handle.join().expect("loop thread");
        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        seen.extend(rest);
        assert_eq!(seen, result.events, "streamed events == recorded events");
    }

    // --- stop_reason gates tool dispatch ---

    fn no_tool_results(events: &[LoopEvent]) -> bool {
        !events
            .iter()
            .any(|e| matches!(e, LoopEvent::ToolResult { .. }))
    }

    #[test]
    fn tool_use_block_under_end_turn_is_not_dispatched() {
        let body = sse_tool_use("done", "toolu_x", "edit_code", &json!({"path": "a"})).replace(
            "\"stop_reason\":\"tool_use\"",
            "\"stop_reason\":\"end_turn\"",
        );
        let transport = Arc::new(SeqTransport::new(vec![RawResponse { status: 200, body }]));
        let client = client_with(transport);
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let result = AgentLoop::new().run(
            sample_request(),
            &client,
            &mut dispatcher,
            &CancellationToken::new(),
        );
        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        assert!(no_tool_results(&result.events), "{:?}", result.events);
    }

    #[test]
    fn max_tokens_with_truncated_tool_call_is_an_error_and_runs_no_tools() {
        // The tool input JSON is cut off mid-string by max_tokens.
        let start = json!({"type":"message_start","message":{"id":"m","model":"x","usage":{"input_tokens":1,"output_tokens":1}}}).to_string();
        let tool_start = json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_t","name":"edit_code","input":{}}}).to_string();
        let tool_delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.scad\",\"old\":\"cu"}}).to_string();
        let msg_delta = json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":10}}).to_string();
        let body = format!(
            "event: message_start\ndata: {start}\n\n\
             event: content_block_start\ndata: {tool_start}\n\n\
             event: content_block_delta\ndata: {tool_delta}\n\n\
             event: message_delta\ndata: {msg_delta}\n\n"
        );
        let transport = Arc::new(SeqTransport::new(vec![RawResponse { status: 200, body }]));
        let client = client_with(transport);
        let mut dispatcher = Dispatcher::new(
            Box::new(InMemoryWorkspace::with_files([("a.scad", "cube(1);")])),
            None,
        );
        let result = AgentLoop::new().run(
            sample_request(),
            &client,
            &mut dispatcher,
            &CancellationToken::new(),
        );
        match &result.stop_reason {
            LoopStopReason::Error(msg) => {
                assert!(msg.contains("max_tokens"), "{msg}");
                assert!(msg.contains("NOT executed"), "{msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(no_tool_results(&result.events), "{:?}", result.events);
    }

    #[test]
    fn thinking_blocks_are_echoed_back_first_with_signature() {
        let start = json!({"type":"message_start","message":{"id":"m","model":"x","usage":{"input_tokens":1,"output_tokens":1}}}).to_string();
        let th_start = json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}).to_string();
        let th_delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}).to_string();
        let sig_delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}).to_string();
        let tool_start = json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"screenshot_viewport","input":{}}}).to_string();
        let msg_delta = json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}).to_string();
        let first = format!(
            "event: message_start\ndata: {start}\n\n\
             event: content_block_start\ndata: {th_start}\n\n\
             event: content_block_delta\ndata: {th_delta}\n\n\
             event: content_block_delta\ndata: {sig_delta}\n\n\
             event: content_block_start\ndata: {tool_start}\n\n\
             event: message_delta\ndata: {msg_delta}\n\n"
        );
        let transport = Arc::new(SeqTransport::new(vec![
            RawResponse {
                status: 200,
                body: first,
            },
            RawResponse {
                status: 200,
                body: sse_end_turn("ok"),
            },
        ]));
        let client = client_with(transport.clone());
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let result = AgentLoop::new().run(
            sample_request(),
            &client,
            &mut dispatcher,
            &CancellationToken::new(),
        );
        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        let last = transport.last_request();
        let assistant = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant turn echoed");
        let blocks = assistant["content"].as_array().unwrap();
        assert_eq!(
            blocks[0],
            json!({"type": "thinking", "thinking": "plan", "signature": "SIG"})
        );
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn overloaded_then_success_is_retried_inside_one_round() {
        let overloaded = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n".to_string();
        let transport = Arc::new(SeqTransport::new(vec![
            RawResponse {
                status: 529,
                body: String::new(),
            },
            RawResponse {
                status: 200,
                body: overloaded,
            },
            RawResponse {
                status: 200,
                body: sse_end_turn("done"),
            },
        ]));
        let client = client_with(transport).with_retry_policy(
            crate::client::RetryPolicy::default().with_sleeper(Arc::new(|_| {})),
        );
        let mut dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let result = AgentLoop::new().run(
            sample_request(),
            &client,
            &mut dispatcher,
            &CancellationToken::new(),
        );
        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        assert_eq!(result.final_text, "done");
    }

    // --- run_on_thread ---

    #[test]
    fn run_on_thread_streams_events_and_returns_end_turn() {
        // SeqTransport end_turn: the loop runs one round, emits an
        // AssistantText event, and returns EndTurn. Events must arrive through
        // the channel AND the join handle must return EndTurn.
        let transport = Arc::new(SeqTransport::new(vec![RawResponse {
            status: 200,
            body: sse_end_turn("The cube is ready."),
        }]));
        let client = client_with(transport);
        let dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);
        let cancel = CancellationToken::new();

        let (rx, handle) = run_on_thread(sample_request(), client, dispatcher, cancel);

        let result = handle.join().expect("loop thread should not panic");
        assert_eq!(result.stop_reason, LoopStopReason::EndTurn);
        assert_eq!(result.final_text, "The cube is ready.");

        // Every event the loop produced is forwarded through the channel.
        let events: Vec<LoopEvent> = rx.iter().collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                LoopEvent::AssistantText(t) if t == "The cube is ready."
            )),
            "expected AssistantText event via channel: {events:?}"
        );
    }

    #[test]
    fn run_on_thread_cancels_from_another_thread() {
        // CancelAfterFirst sets the token during the first send (simulating an
        // external cancel from another thread). The loop checks cancellation
        // after send returns and stops with Cancelled — observable via the
        // join handle without the caller blocking on the sync transport.
        let body = sse_tool_use("working", "toolu_1", "screenshot_viewport", &json!({}));
        let cancel = CancellationToken::new();
        let transport = Arc::new(CancelAfterFirst::new(
            RawResponse { status: 200, body },
            cancel.clone(),
        ));
        let client = client_with(transport);
        let dispatcher = Dispatcher::new(Box::new(InMemoryWorkspace::new()), None);

        let (rx, handle) = run_on_thread(sample_request(), client, dispatcher, cancel);

        let result = handle.join().expect("loop thread should not panic");
        assert_eq!(result.stop_reason, LoopStopReason::Cancelled);
        // Drain the receiver; cancellation timing means the channel may carry
        // zero events, but dropping the receiver must not deadlock the join.
        drop(rx);
    }

    /// Minimal SSE body with no content blocks and the given stop_reason.
    fn sse_with_stop_reason(stop_reason: &str) -> String {
        let start = json!({
            "type":"message_start",
            "message":{"id":"msg_03","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":1}}
        })
        .to_string();
        let delta = json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"output_tokens":10}}).to_string();
        let stop = json!({"type":"message_stop"}).to_string();
        format!(
            "event: message_start\ndata: {start}\n\n\
             event: message_delta\ndata: {delta}\n\n\
             event: message_stop\ndata: {stop}\n"
        )
    }
}
