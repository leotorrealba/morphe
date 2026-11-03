// Tests for ChatPane streaming + interrupt.
//
// `../ai` is mocked: only the IPC wrappers are overridden (streamAiTurn feeds a
// fixture AiEvent stream; cancelAiTurn is a spy). The pure helper `isEditPatch`
// is kept from the real module so the tool-call rendering branch stays real.
import { expect, test, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import ChatPane from "../ChatPane";
import {
  cancelAiTurn,
  hasApiKey,
  streamAiTurn,
  type AiEvent,
} from "../../ai";

vi.mock("../../ai", async (importOriginal) => {
  // Keep the pure helpers (isEditPatch) real; override only the IPC wrappers.
  const actual = await importOriginal<typeof import("../../ai")>();
  return {
    ...actual,
    hasApiKey: vi.fn(),
    streamAiTurn: vi.fn(),
    cancelAiTurn: vi.fn(async () => undefined),
  };
});

beforeEach(() => {
  vi.mocked(hasApiKey).mockClear();
  vi.mocked(streamAiTurn).mockClear();
  vi.mocked(cancelAiTurn).mockClear();
});

/** A fixture AiEvent stream: text → tool_call → tool_result → done. */
const FIXTURE: AiEvent[] = [
  { kind: "text", text: "I will check the model geometry." },
  {
    kind: "tool_call",
    id: "t1",
    name: "get_geometry_stats",
    input: { source: "cube([10,10,10]);" },
  },
  {
    kind: "tool_result",
    id: "t1",
    content: "volume=1000 manifold=true",
    is_error: false,
  },
  { kind: "done", stop_reason: "end_turn", final_source: "cube([10,10,10]);" },
];

test("chat_streams_tool_calls", async () => {
  // Key is set → chat surface renders immediately (no onboarding gate).
  vi.mocked(hasApiKey).mockResolvedValue(true);
  // Feed the fixture stream synchronously on send.
  vi.mocked(streamAiTurn).mockImplementation(
    async (_source, _intent, onEvent) => {
      for (const ev of FIXTURE) onEvent(ev);
    },
  );

  render(<ChatPane source="cube([10,10,10]);" onSourceChange={() => {}} />);

  // Wait for the gate to clear and the chat surface to render.
  await waitFor(() => expect(screen.getByTestId("chat-input")).toBeInTheDocument());

  // Type + send.
  fireEvent.change(screen.getByTestId("chat-input"), {
    target: { value: "inspect the model" },
  });
  fireEvent.click(screen.getByTestId("chat-send"));

  // The assistant text (markdown → <p>) renders.
  await waitFor(() =>
    expect(screen.getByTestId("chat-text")).toHaveTextContent(
      "I will check the model geometry.",
    ),
  );
  // The tool-call collapsible renders.
  expect(screen.getByTestId("chat-tool-call")).toBeInTheDocument();
  // The tool_result attached to it.
  await waitFor(() =>
    expect(screen.getByTestId("chat-tool-result")).toHaveTextContent(
      "volume=1000 manifold=true",
    ),
  );
  // The done summary.
  await waitFor(() => expect(screen.getByTestId("chat-done")).toBeInTheDocument());
  expect(screen.getByTestId("chat-done")).toHaveTextContent("end_turn");

  // streamAiTurn received the current editor source + the design intent.
  expect(streamAiTurn).toHaveBeenCalledWith(
    "cube([10,10,10]);",
    "inspect the model",
    expect.any(Function),
  );
});

test("interrupt_cancels_loop", async () => {
  vi.mocked(hasApiKey).mockResolvedValue(true);
  // A turn that never resolves — the user interrupts mid-flight.
  vi.mocked(streamAiTurn).mockImplementation(() => new Promise(() => {}));

  render(<ChatPane source="cube([10,10,10]);" onSourceChange={() => {}} />);

  await waitFor(() => expect(screen.getByTestId("chat-input")).toBeInTheDocument());

  // Send → streaming starts → the Send button is replaced by Stop.
  fireEvent.change(screen.getByTestId("chat-input"), {
    target: { value: "do something long" },
  });
  fireEvent.click(screen.getByTestId("chat-send"));

  const stopBtn = await screen.findByTestId("chat-stop");
  fireEvent.click(stopBtn);

  // cancelAiTurn was called (the interrupt reaches the backend).
  await waitFor(() => expect(cancelAiTurn).toHaveBeenCalled());
});

test("assistant_markdown_is_sanitized_against_xss", async () => {
  // Assistant text is model-controlled and prompt-injectable (a
  // tool_result from read_code on an untrusted .scad can coerce the model to
  // emit an <img onerror=...>). The Markdown renderer MUST sanitize marked's
  // output with DOMPurify so an injected handler never reaches the DOM (and,
  // through it, the Tauri IPC). This test feeds a classic XSS
  // payload as assistant text and asserts no onerror survives sanitization.
  vi.mocked(hasApiKey).mockResolvedValue(true);
  const xssPayload = `<img src=x onerror="alert(1)">`;
  vi.mocked(streamAiTurn).mockImplementation(async (_source, _intent, onEvent) => {
    onEvent({ kind: "text", text: xssPayload });
    onEvent({ kind: "done", stop_reason: "end_turn", final_source: "" });
  });

  render(<ChatPane source="" onSourceChange={() => {}} />);
  await waitFor(() => expect(screen.getByTestId("chat-input")).toBeInTheDocument());

  fireEvent.change(screen.getByTestId("chat-input"), { target: { value: "x" } });
  fireEvent.click(screen.getByTestId("chat-send"));

  const text = await screen.findByTestId("chat-text");
  // No element may carry an onerror attribute, and the raw attribute string
  // must be absent from the rendered HTML.
  expect(text.querySelector("[onerror]")).toBeNull();
  expect(text.innerHTML).not.toContain("onerror");
});

test("stop_remains_reachable_until_done_event", async () => {
  // Regression for the streaming-flag desync: ai_turn
  // resolves immediately after dispatching the turn, while AiEvents stream
  // asynchronously afterward. streaming must stay true until the `done` event
  // — otherwise the Stop button vanishes before the user can interrupt. The
  // OLD code reset streaming in the invoke `finally` (flipped false on
  // resolve) and would fail the first assertion below.
  vi.mocked(hasApiKey).mockResolvedValue(true);
  // Production timing: invoke resolves immediately, events stream async.
  vi.mocked(streamAiTurn).mockImplementation(async (_source, _intent, onEvent) => {
    setTimeout(() => {
      for (const ev of FIXTURE) onEvent(ev);
    }, 5);
  });

  render(<ChatPane source="cube([10,10,10]);" onSourceChange={() => {}} />);
  await waitFor(() => expect(screen.getByTestId("chat-input")).toBeInTheDocument());

  fireEvent.change(screen.getByTestId("chat-input"), {
    target: { value: "inspect" },
  });
  fireEvent.click(screen.getByTestId("chat-send"));

  // Before the `done` event fires, streaming is still true → Stop reachable.
  await waitFor(() => expect(screen.getByTestId("chat-stop")).toBeInTheDocument());

  // After the `done` event streams, streaming ends → Send button returns.
  await waitFor(() => expect(screen.getByTestId("chat-send")).toBeInTheDocument());
  // And the done summary rendered.
  expect(screen.getByTestId("chat-done")).toBeInTheDocument();
});

test("chat_does_not_send_an_empty_prompt", async () => {
  vi.mocked(hasApiKey).mockResolvedValue(true);
  vi.mocked(streamAiTurn).mockClear();
  render(<ChatPane source="cube(1);" onSourceChange={() => {}} />);
  await waitFor(() => expect(screen.getByTestId("chat-input")).toBeInTheDocument());
  fireEvent.change(screen.getByTestId("chat-input"), { target: { value: "   " } });
  fireEvent.click(screen.getByTestId("chat-send"));
  fireEvent.keyDown(screen.getByTestId("chat-input"), { key: "Enter" });
  expect(streamAiTurn).not.toHaveBeenCalled();
});

test("chat_links_in_assistant_markdown_are_inert", async () => {
  vi.mocked(hasApiKey).mockResolvedValue(true);
  vi.mocked(streamAiTurn).mockImplementation(async (_s, _i, onEvent) => {
    onEvent({ kind: "text", text: "see [this](https://evil.example/phish)" });
    onEvent({ kind: "done", stop_reason: "end_turn", final_source: "" });
  });
  render(<ChatPane source="cube(1);" onSourceChange={() => {}} />);
  await waitFor(() => expect(screen.getByTestId("chat-input")).toBeInTheDocument());
  fireEvent.change(screen.getByTestId("chat-input"), { target: { value: "hi" } });
  fireEvent.click(screen.getByTestId("chat-send"));
  const link = await waitFor(() => {
    const a = screen.getByTestId("chat-text").querySelector("a");
    expect(a).not.toBeNull();
    return a as HTMLAnchorElement;
  });
  const click = new MouseEvent("click", { bubbles: true, cancelable: true });
  link.dispatchEvent(click);
  expect(click.defaultPrevented).toBe(true);
});
