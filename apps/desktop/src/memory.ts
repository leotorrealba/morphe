// Memory-panel IPC wrappers + the `DesignEvent` wire type.
//
// `DesignEvent` is a discriminated union on `kind`, mirroring the Rust enum
// (crates/morphe-ai/src/design_event.rs, `#[serde(tag = "kind", rename_all =
// "snake_case")]`). The wire-shape snapshot tests
// (apps/desktop/src-tauri/tests/wire_shape.rs + __tests__/wire-shape.test.ts)
// pin it bidirectionally — drift on either side fails. `DesignEvent` crosses
// the IPC boundary via the `memory_list` command, which returns structured
// events to the panel.

import { invoke } from "@tauri-apps/api/core";

/**
 * A design event from the project's `.noos` sidecar. Mirrors the Rust
 * `DesignEvent` enum (internally tagged on a snake_case `kind` discriminant).
 */
export type DesignEvent =
  | { kind: "intent"; statement: string }
  | { kind: "decision"; path: string; old: string; new: string }
  | { kind: "constraint"; name: string; value: string }
  | { kind: "print_setting"; key: string; value: string }
  | { kind: "constraint_retired"; name: string };

/** A constraint still active (not retired) — derived view for the panel. */
export type ActiveConstraint = { kind: "constraint"; name: string; value: string };

/**
 * The constraints still active after replaying `events` IN ORDER: per name,
 * the last `constraint` or `constraint_retired` event wins. So a constraint
 * re-added after being retired is active again, a re-retired one stays
 * hidden, and an updated constraint shows only its latest value. Mirrors the
 * backend's session-digest replay.
 */
export function activeConstraints(events: DesignEvent[]): ActiveConstraint[] {
  const latest = new Map<string, ActiveConstraint | null>();
  for (const e of events) {
    if (e.kind === "constraint") {
      latest.delete(e.name); // re-insert so order follows the latest event
      latest.set(e.name, e);
    } else if (e.kind === "constraint_retired") {
      latest.set(e.name, null);
    }
  }
  return [...latest.values()].filter((c): c is ActiveConstraint => c !== null);
}

export const MEMORY_LIST_CMD = "memory_list_cmd" as const;
export const RETIRE_CONSTRAINT_CMD = "retire_constraint_cmd" as const;

/**
 * Fetch the project's design events for the memory panel. Empty when no file is
 * open or the project has no sidecar.
 */
export async function listMemory(): Promise<DesignEvent[]> {
  return invoke<DesignEvent[]>(MEMORY_LIST_CMD);
}

/** Retire a constraint by name (emits a `ConstraintRetired` tombstone). */
export async function retireConstraint(name: string): Promise<void> {
  return invoke<void>(RETIRE_CONSTRAINT_CMD, { name });
}
