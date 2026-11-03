// Memory panel.
//
// The USER-FACING read/retire surface over the `.noos` substrate. Browses the
// project's design memory (active constraints / intents / print settings /
// recent decisions) read from the sidecar, and retires a stale constraint
// (emits a `ConstraintRetired` tombstone via the shared event write path).
// Self-contained: fetches on open + after retire, surfaces retire failures as
// an error cue. Active constraints come from an in-order replay of constraint / `constraint_retired`
// tombstones, so retiring a constraint visibly removes it from the active list.
import { useEffect, useState } from "react";

import { activeConstraints, listMemory, retireConstraint, type DesignEvent } from "../memory";

export interface MemoryPanelProps {
  /** The absolute path of the open `.scad` file, or null when none. The panel
   * refetches when this changes. */
  openedPath: string | null;
}

export default function MemoryPanel({ openedPath }: MemoryPanelProps) {
  const [events, setEvents] = useState<DesignEvent[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!openedPath) {
      setEvents([]);
      setError(null);
      return;
    }
    setError(null);
    listMemory()
      .then(setEvents)
      .catch((e) => setError(String(e)));
  }, [openedPath]);

  const onRetire = async (name: string) => {
    setError(null);
    try {
      await retireConstraint(name);
      // Refresh after retire — the backend wrote a ConstraintRetired tombstone,
      // so the re-fetch includes it and the client-side filter below drops the
      // retired constraint from the active list.
      setEvents(await listMemory());
    } catch (e) {
      // Write-failure UI cue — the retire error surfaces to the user
      // (closes the "eprintln invisible to GUI" gap for user-initiated writes).
      setError(String(e));
    }
  };

  // Active constraints: replay the event stream in order, last event per name
  // wins (a constraint re-added after being retired is active again).
  const constraints = activeConstraints(events);
  const intents = events.filter(
    (e): e is Extract<DesignEvent, { kind: "intent" }> => e.kind === "intent",
  );
  const printSettings = events.filter(
    (e): e is Extract<DesignEvent, { kind: "print_setting" }> =>
      e.kind === "print_setting",
  );
  const decisions = events.filter(
    (e): e is Extract<DesignEvent, { kind: "decision" }> => e.kind === "decision",
  );

  const empty =
    constraints.length === 0 &&
    intents.length === 0 &&
    printSettings.length === 0 &&
    decisions.length === 0;

  return (
    <section
      data-testid="memory-panel"
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "var(--morphe-space-2)",
        padding: "var(--morphe-space-3)",
        background: "var(--morphe-surface)",
        color: "var(--morphe-text)",
        borderTop: "1px solid var(--morphe-border)",
        fontFamily: "var(--morphe-font-sans)",
        fontSize: "var(--morphe-font-size-sm)",
        overflow: "auto",
      }}
    >
      <header style={{ fontWeight: 600 }}>Design memory</header>

      {error && (
        <div
          role="alert"
          data-testid="memory-error"
          style={{ color: "var(--morphe-danger, #c00)" }}
        >
          Couldn't update memory: {error}
        </div>
      )}

      {empty && !openedPath && <p>Open a project to see its design memory.</p>}
      {empty && openedPath && <p>No design memory yet.</p>}

      {constraints.length > 0 && (
        <div>
          <h4 style={{ margin: "0 0 var(--morphe-space-1)" }}>Constraints</h4>
          <ul style={{ margin: 0, paddingLeft: "var(--morphe-space-3)" }}>
            {constraints.map((c) => (
              <li key={`c-${c.name}`} data-testid={`constraint-${c.name}`}>
                <code>{c.name}</code>: {c.value}{" "}
                <button
                  type="button"
                  data-testid={`retire-${c.name}`}
                  onClick={() => onRetire(c.name)}
                  title={`Retire constraint ${c.name}`}
                >
                  Retire
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      {intents.length > 0 && (
        <div>
          <h4 style={{ margin: "0 0 var(--morphe-space-1)" }}>Intent</h4>
          <ul style={{ margin: 0, paddingLeft: "var(--morphe-space-3)" }}>
            {intents.map((i, idx) => (
              <li key={`i-${idx}`}>{i.statement}</li>
            ))}
          </ul>
        </div>
      )}

      {printSettings.length > 0 && (
        <div>
          <h4 style={{ margin: "0 0 var(--morphe-space-1)" }}>Print settings</h4>
          <ul style={{ margin: 0, paddingLeft: "var(--morphe-space-3)" }}>
            {printSettings.map((p, idx) => (
              <li key={`p-${idx}`}>
                <code>{p.key}</code>: {p.value}
              </li>
            ))}
          </ul>
        </div>
      )}

      {decisions.length > 0 && (
        <div>
          <h4 style={{ margin: "0 0 var(--morphe-space-1)" }}>Recent decisions</h4>
          <ul style={{ margin: 0, paddingLeft: "var(--morphe-space-3)" }}>
            {decisions.map((d, idx) => (
              <li key={`d-${idx}`}>
                <code>{d.path}</code>: {d.new.split("\n")[0]?.slice(0, 60)}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
