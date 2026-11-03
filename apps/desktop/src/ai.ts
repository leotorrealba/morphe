// Typed IPC wrappers for the `ai_*` commands.
//
// Mirrors the Rust `AiEvent` serde shape and the `ai_*` Tauri commands defined
// in `src-tauri/src/ai.rs`. Follows the same typed-wrapper style as `./ipc.ts`:
// each command is a thin function over `invoke`, and the wire types are
// mirrored here as TS types so the frontend is accountable to a verifiable
// shape, not loose JSON.
//
// CRITICAL: the Rust `AiEvent` enum derives `#[serde(tag = "kind", rename_all =
// "snake_case")]` and `AiTurnRequest` has NO rename — so the wire fields are
// snake_case (`is_error`, `stop_reason`, `final_source`, `design_intent`). The
// TS union below mirrors that exactly; using camelCase here would silently
// break runtime dispatch.

import { invoke, Channel } from "@tauri-apps/api/core";

// ── AiEvent (mirrors Rust `AiEvent`) ─────────────────────────────────────────

/**
 * One streamed event from an AI turn. Tagged union on `kind` (snake_case),
 * mirroring the Rust `#[serde(tag = "kind", rename_all = "snake_case")]` enum.
 *
 * - `text`: assistant text content (one per text block; concatenate for a turn).
 * - `tool_call`: the model requested a tool. For `edit_code`, `input` is the
 *   {@link EditPatch} shape (`{ path, old, new }`).
 * - `tool_result`: the dispatcher's result for the tool-use `id`. `is_error`
 *   flags failures.
 * - `done`: the turn finished. `stop_reason` is one of `end_turn`, `cancelled`,
 *   `round_cap`, `max_tokens`, `refusal`, `error`; `final_source` is the
 *   workspace source after all edits.
 */
export type AiEvent =
  | { kind: "text"; text: string }
  | { kind: "tool_call"; id: string; name: string; input: unknown }
  | { kind: "tool_result"; id: string; content: string; is_error: boolean }
  | { kind: "done"; stop_reason: string; final_source: string };

/**
 * The `input` of an `edit_code` tool call — a deterministic str_replace patch
 * the frontend renders as a diff preview with accept/reject.
 */
export interface EditPatch {
  path: string;
  old: string;
  new: string;
}

/**
 * The `ai_turn` request payload. Mirrors Rust `AiTurnRequest` (snake_case, no
 * serde rename) — `design_intent` crosses the wire verbatim.
 */
export interface AiTurnRequest {
  source: string;
  design_intent: string | null;
}

// ── command names ────────────────────────────────────────────────────────────

const AI_HAS_API_KEY_CMD = "ai_has_api_key" as const;
const AI_GET_PROVIDER_CONFIG_CMD = "ai_get_provider_config" as const;
const AI_SET_PROVIDER_CONFIG_CMD = "ai_set_provider_config" as const;
const AI_REMOVE_API_KEY_CMD = "ai_remove_api_key" as const;
const AI_TURN_CMD = "ai_turn" as const;
const AI_CANCEL_CMD = "ai_cancel" as const;
const AI_ACCEPT_EDIT_CMD = "ai_accept_edit" as const;
const AI_REJECT_EDIT_CMD = "ai_reject_edit" as const;

// ── Provider config ─────────────────────────────────────────────────────────

/**
 * The AI provider, mirroring the Rust `Provider` enum (bare-string serde,
 * snake_case). Pinned by the wire-shape snapshot
 * (`wire-shape.expected.json` → `Provider`) — adding a provider on either
 * side without the other fails the wire-shape tests.
 */
export type Provider = "anthropic" | "open_ai_compatible";

/**
 * The stored provider config WITHOUT the key — what
 * `ai_get_provider_config` returns. `baseUrl`/`model` are the STORED
 * overrides; `null` → the provider's defaults (resolve client-side for
 * display via {@link providerDefaults}). The api_key NEVER crosses IPC toward
 * the frontend.
 */
export interface ProviderConfigInfo {
  provider: Provider;
  base_url: string | null;
  model: string | null;
  has_key: boolean;
}

/**
 * The per-provider defaults, mirroring Rust `Provider::default_base_url` /
 * `default_model` (display-only on this side; the backend resolves the real
 * client config from the stored overrides).
 */
export const providerDefaults: Record<
  Provider,
  { baseUrl: string; model: string }
> = {
  anthropic: { baseUrl: "https://api.anthropic.com", model: "claude-sonnet-5-5" },
  open_ai_compatible: { baseUrl: "https://api.openai.com", model: "gpt-4o" },
};

// ── typed wrappers ───────────────────────────────────────────────────────────

/** Ask the backend `SecretStore` whether an API key is set. */
export function hasApiKey(): Promise<boolean> {
  return invoke<boolean>(AI_HAS_API_KEY_CMD);
}

/**
 * Read the stored provider config (provider + overrides + has_key — NEVER the
 * key itself). Empty store → `{provider: "anthropic", base_url: null,
 * model: null, has_key: false}`; a legacy bare key reads as Anthropic +
 * defaults (no migration).
 */
export function getProviderConfig(): Promise<ProviderConfigInfo> {
  return invoke<ProviderConfigInfo>(AI_GET_PROVIDER_CONFIG_CMD);
}

/**
 * Store the full provider credential: provider + key + optional base_url /
 * model overrides (OpenRouter / Azure / local LLMs via
 * `open_ai_compatible`). Empty optional fields should be passed as `null`.
 */
export function setProviderConfig(
  provider: Provider,
  apiKey: string,
  baseUrl: string | null,
  model: string | null,
): Promise<void> {
  return invoke<void>(AI_SET_PROVIDER_CONFIG_CMD, {
    provider,
    apiKey,
    baseUrl,
    model,
  });
}

/** Delete the key from the backend `SecretStore` (idempotent). */
export function removeApiKey(): Promise<void> {
  return invoke<void>(AI_REMOVE_API_KEY_CMD);
}

/**
 * Cancel the in-flight AI turn. The loop checks the cancel token at the top of
 * each round, so cancellation lands within one round. No-op if no turn running.
 */
export function cancelAiTurn(): Promise<void> {
  return invoke<void>(AI_CANCEL_CMD);
}

/**
 * Acknowledge an `edit_code` patch. The backend already applied it to the
 * workspace; this returns the current backend source buffer (the frontend syncs
 * its editor on accept separately via `onSourceChange`).
 *
 * The accepted `patch` (`{ path, old, new }`) crosses the wire so
 * the backend can emit a noos `DesignEvent::Decision` into the opened
 * project's `.noos/` — every accept becomes a queryable design-memory node.
 * When `path` is the in-memory workspace path (no `.noos/` parent) the backend
 * skips the emit + logs; the accept itself is unaffected.
 */
export function acceptEdit(patch: EditPatch): Promise<string> {
  return invoke<string>(AI_ACCEPT_EDIT_CMD, {
    path: patch.path,
    old: patch.old,
    new: patch.new,
  });
}

/**
 * Reject an `edit_code` patch — reverts `new`→`old` in the backend source
 * buffer AND records the tool-use `id` so the backend's final-source
 * reconstruction skips it (without the id, the bridge would overwrite
 * this reject after the loop and the next turn's seed would diverge from the
 * editor). Returns the reverted source. `path` is informational
 * (single-file workspace).
 */
export function rejectEdit(patch: EditPatch, id: string): Promise<string> {
  return invoke<string>(AI_REJECT_EDIT_CMD, {
    path: patch.path,
    old: patch.old,
    new: patch.new,
    id,
  });
}

/**
 * Run one agent turn and stream {@link AiEvent}s to `onEvent` over a Tauri
 * `Channel`. Returns a promise that resolves when the turn completes (the
 * `done` event has been forwarded). The promise rejects on an invocation error
 * (e.g. no API key set); per-tool failures arrive as `tool_result` events with
 * `is_error: true`.
 *
 * @param source       The current `.scad` source (seeds the agent workspace).
 * @param designIntent Optional design-intent context.
 * @param onEvent      Called once per streamed `AiEvent`.
 */
export function streamAiTurn(
  source: string,
  designIntent: string | null,
  onEvent: (event: AiEvent) => void,
): Promise<void> {
  const channel = new Channel<AiEvent>();
  channel.onmessage = onEvent;
  const request: AiTurnRequest = { source, design_intent: designIntent };
  return invoke<void>(AI_TURN_CMD, { request, onEvent: channel });
}

// ── helpers ──────────────────────────────────────────────────────────────────

/** Type guard: is `input` an {@link EditPatch}? */
export function isEditPatch(input: unknown): input is EditPatch {
  return (
    typeof input === "object" &&
    input !== null &&
    typeof (input as EditPatch).path === "string" &&
    typeof (input as EditPatch).old === "string" &&
    typeof (input as EditPatch).new === "string"
  );
}
