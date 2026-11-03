// The TS half of the Rust↔TS IPC wire-shape snapshot test.
//
// Tests on each side of the IPC boundary can pass for the wrong reason: the
// Rust side and the TS side each assume a wire shape that neither verifies.
// This file is the TS half of a machine-checkable wire-shape contract that
// closes that hole.
//
// It reads the same checked-in snapshot as the Rust test
// (`apps/desktop/src-tauri/tests/wire_shape.rs`), parses each variant's
// serialized JSON string, and asserts its `kind` discriminant is one the TS
// union declares. The Rust test owns the bytes; this test owns the TS union's
// declared `kind` set. The two meet at `wire-shape.expected.json`: drift on
// either side fails closed.
//
// If this test fails, either Rust drifted (regenerate the snapshot via the
// ignored Rust write test) or the TS union changed (update EXPECTED here).
//
// `CompileError.kind` is pinned here as an `ErrorKind` enum (one entry per
// variant); the enum bound means Rust cannot produce a kind outside the
// declared set, so the wire-shape contract pins it bidirectionally.

import { expect, test } from "vitest";

import snapshot from "../../wire-shape.expected.json";

// The set of `kind` discriminants each TS union declares. Derived from
// `src/ipc.ts` (ExportError, FileError), `src/ai.ts` (AiEvent), and
// `src/memory.ts` (DesignEvent, crosses IPC via memory_list). If
// Rust emits a kind not listed here, OR the TS union lists a kind Rust doesn't
// emit, this test fails (bidirectional).
const EXPECTED: Record<string, string[]> = {
  ExportError: ["compile", "not_3d", "io", "cancelled"],
  FileError: ["cancelled", "io"],
  AiEvent: ["text", "tool_call", "tool_result", "done"],
  CompileError: ["parse", "unsupported", "general", "resolver"],
  DesignEvent: [
    "intent",
    "decision",
    "constraint",
    "print_setting",
    "constraint_retired",
  ],
  // Provider crosses IPC (ai_get/ai_set_provider_config). It is a
  // bare-string enum — the pinned values ARE the union's members.
  Provider: ["anthropic", "open_ai_compatible"],
};

// Snapshot is typed as a top-level JSON import: `{ <enum>: { <variant>: <json string> } }`.
type Snapshot = Record<string, Record<string, string>>;

const snapshotTyped = snapshot as Snapshot;

test("ExportError wire-shape kinds match the TS union", () => {
  const kinds = Object.values(snapshotTyped.ExportError)
    .map((serialized) => {
      const parsed = JSON.parse(serialized) as { kind: string };
      return parsed.kind;
    })
    .sort();
  expect(kinds).toEqual([...EXPECTED.ExportError].sort());
});

test("FileError wire-shape kinds match the TS union", () => {
  const kinds = Object.values(snapshotTyped.FileError)
    .map((serialized) => {
      const parsed = JSON.parse(serialized) as { kind: string };
      return parsed.kind;
    })
    .sort();
  expect(kinds).toEqual([...EXPECTED.FileError].sort());
});

test("AiEvent wire-shape kinds match the TS union", () => {
  const kinds = Object.values(snapshotTyped.AiEvent)
    .map((serialized) => {
      const parsed = JSON.parse(serialized) as { kind: string };
      return parsed.kind;
    })
    .sort();
  expect(kinds).toEqual([...EXPECTED.AiEvent].sort());
});

test("CompileError wire-shape kinds match the TS union", () => {
  const kinds = Object.values(snapshotTyped.CompileError)
    .map((serialized) => {
      const parsed = JSON.parse(serialized) as { kind: string };
      return parsed.kind;
    })
    .sort();
  expect(kinds).toEqual([...EXPECTED.CompileError].sort());
});

test("DesignEvent wire-shape kinds match the TS union", () => {
  // DesignEvent crosses the IPC boundary (memory_list returns
  // structured events to the panel). The Rust half pins the serialized bytes;
  // this asserts the TS union (src/memory.ts) declares the same `kind` set.
  const kinds = Object.values(snapshotTyped.DesignEvent)
    .map((serialized) => {
      const parsed = JSON.parse(serialized) as { kind: string };
      return parsed.kind;
    })
    .sort();
  expect(kinds).toEqual([...EXPECTED.DesignEvent].sort());
});

test("Provider wire-shape values match the TS union", () => {
  // Provider crosses IPC via the provider-config commands. Unlike the tagged enums above, Provider serializes as a BARE
  // string — JSON.parse yields the union member directly. The Rust half pins
  // the bytes; this asserts the TS union (src/ai.ts Provider) matches exactly.
  const values = Object.values(snapshotTyped.Provider)
    .map((serialized) => JSON.parse(serialized) as string)
    .sort();
  expect(values).toEqual([...EXPECTED.Provider].sort());
});

test("ProviderConfigInfo wire shape matches the TS interface", () => {
  // ProviderConfigInfo — the ai_get_provider_config return
  // shape — is pinned here as well as Rust-side. The Rust half owns the bytes;
  // this asserts the TS interface (src/ai.ts ProviderConfigInfo) declares the
  // same field set with the same spellings (snake_case) — a serde rename on
  // either side fails this or the Rust snapshot test.
  const serialized = snapshotTyped.ProviderConfigInfo.Instance;
  const parsed = JSON.parse(serialized) as Record<string, unknown>;
  expect(Object.keys(parsed).sort()).toEqual(
    ["provider", "base_url", "model", "has_key"].sort(),
  );
  // And the field the UI reads actually arrives with those names.
  expect(typeof parsed.provider).toBe("string");
  expect(typeof parsed.has_key).toBe("boolean");
});

test("ExportStats wire shape matches the TS interface", () => {
  // export_stl_cmd's success payload (src/ipc.ts ExportStats).
  const parsed = JSON.parse(snapshotTyped.ExportStats.Instance) as Record<string, unknown>;
  expect(Object.keys(parsed).sort()).toEqual(
    ["triangle_count", "vertex_count", "format", "bytes_written"].sort(),
  );
  // typeof parity (matching the ProviderConfigInfo test).
  expect(typeof parsed.triangle_count).toBe("number");
  expect(typeof parsed.bytes_written).toBe("number");
  expect(typeof parsed.format).toBe("string");
});

test("OpenFileResult wire shape matches the TS interface", () => {
  // open_file_cmd's success payload (src/ipc.ts OpenFileResult).
  const parsed = JSON.parse(snapshotTyped.OpenFileResult.Instance) as Record<string, unknown>;
  expect(Object.keys(parsed).sort()).toEqual(["path", "source"].sort());
  expect(typeof parsed.path).toBe("string");
  expect(typeof parsed.source).toBe("string");
});

test("SaveFileResult wire shape matches the TS interface", () => {
  // save_file_cmd's success payload (src/ipc.ts SaveFileResult).
  const parsed = JSON.parse(snapshotTyped.SaveFileResult.Instance) as Record<string, unknown>;
  expect(Object.keys(parsed).sort()).toEqual(["path"].sort());
  expect(typeof parsed.path).toBe("string");
});
