// Chat surface for the morphe-ai agent loop.
//
// Consumes the `ai_*` IPC: streams an AiEvent turn into a live message list
// (assistant text as markdown, tool calls as collapsibles with a diff preview
// for `edit_code`), with send + interrupt controls. Reads the current editor
// `source` as context and writes accepted edits back via `onSourceChange`
// (which feeds the debounced recompile → viewport).
//
// Gated on the API key: until a key is set, shows {@link ApiKeyOnboarding}.
import { useEffect, useRef, useState } from "react";
import DOMPurify from "dompurify";
import { marked } from "marked";

import {
  cancelAiTurn,
  hasApiKey,
  isEditPatch,
  streamAiTurn,
  type AiEvent,
  type EditPatch,
} from "../ai";
import ApiKeyOnboarding from "./ApiKeyOnboarding";
import DiffPreview from "./DiffPreview";

export interface ChatPaneProps {
  /** The current `.scad` source from the editor (sent as turn context). */
  source: string;
  /** Editor buffer setter — accepted edits apply old→new through this. */
  onSourceChange: (s: string) => void;
}

/** A rendered chat item derived from the AiEvent stream. */
type ChatItem =
  | { type: "text"; key: string; text: string }
  | {
      type: "tool_call";
      key: string;
      id: string;
      name: string;
      input: unknown;
      result?: { content: string; isError: boolean };
    }
  | { type: "done"; key: string; stopReason: string };

export default function ChatPane({ source, onSourceChange }: ChatPaneProps) {
  const [apiKeyReady, setApiKeyReady] = useState<boolean | null>(null);
  const [items, setItems] = useState<ChatItem[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const keyCounter = useRef(0);
  // The LATEST editor source, readable at click time. Accepting two diffs in
  // quick succession must compose: the second accept has to see the first
  // one's result, not the stale `source` captured at render time.
  const sourceRef = useRef(source);
  sourceRef.current = source;
  const getSource = () => sourceRef.current;
  const applySource = (next: string) => {
    sourceRef.current = next;
    onSourceChange(next);
  };

  // Gate: ask the backend whether a key is set on mount.
  useEffect(() => {
    let cancelled = false;
    hasApiKey()
      .then((present) => {
        if (!cancelled) setApiKeyReady(present);
      })
      .catch(() => {
        if (!cancelled) setApiKeyReady(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const nextKey = () => `item-${keyCounter.current++}`;

  const handleEvent = (event: AiEvent) => {
    // The invoke (streamAiTurn) resolves immediately after the backend
    // dispatches the turn; events (incl. the terminal `done`) stream
    // asynchronously afterward. So streaming must end on the `done` event,
    // not on the invoke promise — otherwise the Stop button vanishes before
    // the user can interrupt.
    if (event.kind === "done") setStreaming(false);
    setItems((prev) => {
      switch (event.kind) {
        case "text":
          return [...prev, { type: "text", key: nextKey(), text: event.text }];
        case "tool_call":
          return [
            ...prev,
            {
              type: "tool_call",
              key: nextKey(),
              id: event.id,
              name: event.name,
              input: event.input,
            },
          ];
        case "tool_result":
          return prev.map((it) =>
            it.type === "tool_call" && it.id === event.id
              ? { ...it, result: { content: event.content, isError: event.is_error } }
              : it,
          );
        case "done":
          return [
            ...prev,
            { type: "done", key: nextKey(), stopReason: event.stop_reason },
          ];
      }
    });
  };

  const onSend = async () => {
    const designIntent = input.trim();
    // An empty prompt would send an empty text block (rejected by the API).
    if (streaming || designIntent.length === 0) return;
    setStreaming(true);
    setInput("");
    try {
      await streamAiTurn(source, designIntent, handleEvent);
    } catch (err) {
      setStreaming(false);
      setItems((prev) => [
        ...prev,
        { type: "text", key: nextKey(), text: `**Error:** ${err instanceof Error ? err.message : String(err)}` },
      ]);
    }
    // NOTE: streaming is NOT reset here — streamAiTurn resolves before events
    // finish streaming. setStreaming(false) fires on the `done` event (handleEvent)
    // or on invoke rejection (catch above).
  };

  const onStop = async () => {
    try {
      await cancelAiTurn();
    } catch {
      // Best-effort cancel; the turn will end via the done event.
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void onSend();
    }
  };

  // Still checking the key — render nothing (avoids a flash of the form).
  if (apiKeyReady === null) {
    return <section data-testid="chat-pane" style={paneStyle} />;
  }

  if (!apiKeyReady) {
    return (
      <section data-testid="chat-pane" style={paneStyle}>
        <header style={headerStyle}>
          <span style={{ fontWeight: 600, color: "var(--morphe-accent)" }}>chat</span>
        </header>
        <ApiKeyOnboarding onKeyChange={(hasKey) => setApiKeyReady(hasKey)} />
      </section>
    );
  }

  return (
    <section data-testid="chat-pane" style={paneStyle}>
      <header style={headerStyle}>
        <span style={{ fontWeight: 600, color: "var(--morphe-accent)" }}>chat</span>
        {streaming && (
          <span style={{ color: "var(--morphe-text-dim)", fontSize: "var(--morphe-font-size-sm)" }}>
            thinking…
          </span>
        )}
      </header>

      <div
        data-testid="chat-log"
        role="log"
        aria-live="polite"
        aria-label="Chat with the design agent"
        style={logStyle}
        onClick={blockLinkNavigation}
      >
        {items.length === 0 && (
          <p style={{ color: "var(--morphe-text-dim)", fontSize: "var(--morphe-font-size-sm)" }}>
            Describe a change and the agent will edit your model.
          </p>
        )}
        {items.map((item) => (
          <ChatItemView
            key={item.key}
            item={item}
            source={source}
            getSource={getSource}
            onSourceChange={applySource}
          />
        ))}
      </div>

      <div style={composerStyle}>
        <textarea
          data-testid="chat-input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Design intent (e.g. make the cube taller)"
          aria-label="Design intent"
          rows={2}
          style={textareaStyle}
          disabled={streaming}
        />
        {streaming ? (
          <button type="button" data-testid="chat-stop" onClick={onStop} style={stopBtnStyle}>
            Stop
          </button>
        ) : (
          <button
            type="button"
            data-testid="chat-send"
            onClick={onSend}
            disabled={input.trim().length === 0}
            style={sendBtnStyle}
          >
            Send
          </button>
        )}
      </div>
    </section>
  );
}

// ── item rendering ───────────────────────────────────────────────────────────

function ChatItemView({
  item,
  source,
  getSource,
  onSourceChange,
}: {
  item: ChatItem;
  source: string;
  getSource: () => string;
  onSourceChange: (s: string) => void;
}) {
  if (item.type === "text") {
    return <Markdown text={item.text} />;
  }
  if (item.type === "done") {
    return (
      <div
        data-testid="chat-done"
        style={{
          borderTop: "1px solid var(--morphe-border)",
          marginTop: "var(--morphe-space-2)",
          paddingTop: "var(--morphe-space-2)",
          color: "var(--morphe-text-dim)",
          fontSize: "var(--morphe-font-size-sm)",
        }}
      >
        done · {item.stopReason}
      </div>
    );
  }
  // tool_call
  return (
    <ToolCallView
      item={item}
      source={source}
      getSource={getSource}
      onSourceChange={onSourceChange}
    />
  );
}

function ToolCallView({
  item,
  source,
  getSource,
  onSourceChange,
}: {
  item: Extract<ChatItem, { type: "tool_call" }>;
  source: string;
  getSource: () => string;
  onSourceChange: (s: string) => void;
}) {
  return (
    <details
      data-testid="chat-tool-call"
      data-tool={item.name}
      open
      style={{
        border: "1px solid var(--morphe-border)",
        borderRadius: "var(--morphe-radius-sm)",
        padding: "var(--morphe-space-2)",
        background: "var(--morphe-surface)",
      }}
    >
      <summary style={{ cursor: "pointer", color: "var(--morphe-text)" }}>
        <code>{item.name}</code>
      </summary>
      <div style={{ marginTop: "var(--morphe-space-2)" }}>
        <ToolCallBody
          item={item}
          source={source}
          getSource={getSource}
          onSourceChange={onSourceChange}
        />
      </div>
      {item.result && (
        <div
          data-testid="chat-tool-result"
          data-error={item.result.isError}
          style={{
            marginTop: "var(--morphe-space-2)",
            padding: "var(--morphe-space-2)",
            background: "var(--morphe-bg)",
            borderRadius: "var(--morphe-radius-sm)",
            color: item.result.isError ? "var(--morphe-error)" : "var(--morphe-text-dim)",
            fontFamily: "var(--morphe-font-mono)",
            fontSize: "var(--morphe-font-size-sm)",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
          }}
        >
          {item.result.content}
        </div>
      )}
    </details>
  );
}

function ToolCallBody({
  item,
  source,
  getSource,
  onSourceChange,
}: {
  item: Extract<ChatItem, { type: "tool_call" }>;
  source: string;
  getSource: () => string;
  onSourceChange: (s: string) => void;
}) {
  if (item.name === "edit_code" && isEditPatch(item.input)) {
    return (
      <DiffPreview
        patch={item.input as EditPatch}
        id={item.id}
        source={source}
        getSource={getSource}
        onSourceChange={onSourceChange}
      />
    );
  }
  if (item.name === "compile" || item.name === "get_geometry_stats") {
    const snippet = sourceSnippet(item.input);
    return <pre style={monoBlockStyle}>{snippet}</pre>;
  }
  return <pre style={monoBlockStyle}>{compactJson(item.input)}</pre>;
}

/** Extract a short source snippet from a compile/stats tool input, if present. */
function sourceSnippet(input: unknown): string {
  if (
    typeof input === "object" &&
    input !== null &&
    typeof (input as { source?: unknown }).source === "string"
  ) {
    const src = (input as { source: string }).source;
    const lines = src.split("\n");
    return lines.length > 6 ? lines.slice(0, 6).join("\n") + "\n…" : src;
  }
  return compactJson(input);
}

function compactJson(input: unknown): string {
  try {
    return JSON.stringify(input, null, 2);
  } catch {
    return String(input);
  }
}

// ── links ────────────────────────────────────────────────────────────────────
//
// Assistant markdown is model-controlled (and prompt-injectable). DOMPurify
// keeps `<a href>`, and clicking one would navigate the app's only window to a
// remote page (a phishing surface the user cannot navigate back from). Chat
// links are therefore inert: a click inside the log never follows an anchor.
export function blockLinkNavigation(e: React.MouseEvent) {
  const target = e.target as HTMLElement | null;
  if (target && typeof target.closest === "function" && target.closest("a")) {
    e.preventDefault();
  }
}

// ── markdown ─────────────────────────────────────────────────────────────────
//
// Markdown-renderer choice: `marked` (not react-markdown). Rationale: marked is
// a single small dep with no React peer coupling and resolves cleanly under bun;
// react-markdown pulls the unified ecosystem (heavier, fussier with React 19
// peer deps). The assistant text is the only markdown source.
//
// SECURITY: assistant text is model-controlled and prompt-injectable
// (a tool_result from read_code on an untrusted .scad can coerce the model to
// emit, e.g. <img onerror="invoke('ai_set_provider_config', ...)">). marked passes raw
// HTML through by default, so the output is sanitized with DOMPurify before
// reaching dangerouslySetInnerHTML. The strict CSP (`script-src 'self'`, see
// tauri.conf.json) independently blocks inline event handlers + scripts, so
// DOMPurify is defense-in-depth here — it strips event-handler attributes
// (onerror/onload/…) and script tags, and additionally limits non-script HTML
// attacks (CSS injection via <style>, etc.) the CSP's `style-src 'unsafe-inline'`
// doesn't cover.

function Markdown({ text }: { text: string }) {
  const raw = marked.parse(text, { async: false }) as string;
  const html = DOMPurify.sanitize(raw);
  return (
    <div
      data-testid="chat-text"
      className="chat-markdown"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

// ── styles ───────────────────────────────────────────────────────────────────

const paneStyle: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  height: "100%",
  minWidth: 0,
  background: "var(--morphe-surface)",
  borderLeft: "1px solid var(--morphe-border)",
};
const headerStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--morphe-space-2)",
  padding: "var(--morphe-space-2) var(--morphe-space-3)",
  borderBottom: "1px solid var(--morphe-border)",
  flexShrink: 0,
  letterSpacing: "0.04em",
  fontSize: "var(--morphe-font-size-sm)",
};
const logStyle: React.CSSProperties = {
  flex: 1,
  minHeight: 0,
  overflowY: "auto",
  padding: "var(--morphe-space-3)",
  display: "flex",
  flexDirection: "column",
  gap: "var(--morphe-space-3)",
};
const composerStyle: React.CSSProperties = {
  display: "flex",
  gap: "var(--morphe-space-2)",
  padding: "var(--morphe-space-2) var(--morphe-space-3)",
  borderTop: "1px solid var(--morphe-border)",
  flexShrink: 0,
};
const textareaStyle: React.CSSProperties = {
  flex: 1,
  resize: "none",
  background: "var(--morphe-bg)",
  color: "var(--morphe-text)",
  border: "1px solid var(--morphe-border)",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-2)",
  fontFamily: "var(--morphe-font)",
  fontSize: "var(--morphe-font-size)",
};
const sendBtnStyle: React.CSSProperties = {
  background: "var(--morphe-accent)",
  color: "#1a1a1c",
  border: "none",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-2) var(--morphe-space-3)",
  cursor: "pointer",
  fontWeight: 600,
};
const stopBtnStyle: React.CSSProperties = {
  background: "var(--morphe-error)",
  color: "#fff",
  border: "none",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-2) var(--morphe-space-3)",
  cursor: "pointer",
  fontWeight: 600,
};
const monoBlockStyle: React.CSSProperties = {
  margin: 0,
  padding: "var(--morphe-space-2)",
  background: "var(--morphe-bg)",
  color: "var(--morphe-text-dim)",
  borderRadius: "var(--morphe-radius-sm)",
  fontFamily: "var(--morphe-font-mono)",
  fontSize: "var(--morphe-font-size-sm)",
  whiteSpace: "pre-wrap",
  wordBreak: "break-word",
  maxHeight: "200px",
  overflow: "auto",
};
