// Debounced live-recompile hook.
//
// Watches the `source` string and, after a 250ms quiet window, calls
// `compileSource` and surfaces `{ mesh, errors, stats, compiling, lastMs }`.
// The debounce is an effect-scoped `setTimeout` whose cleanup clears the timer
// on every source change, so a burst of edits collapses to exactly one compile
// call. An in-flight compile that is superseded by a newer edit is dropped via
// the `cancelled` flag (stale-result guard).
//
// Factored as a standalone hook so the single-call behavior can be asserted
// with fake timers (see useDebouncedRecompile.test.tsx) without mounting the
// full editor/viewport.
import { useEffect, useState } from "react";

import { compileSource, type CompileError, type GeometryStats, type MeshBuffer } from "../ipc";

export interface RecompileState {
  mesh: MeshBuffer | null;
  errors: CompileError[];
  stats: GeometryStats | null;
  compiling: boolean;
  /** Wall-clock duration of the most recent compile (ms), or null before first. */
  lastMs: number | null;
}

const INITIAL: RecompileState = {
  mesh: null,
  errors: [],
  stats: null,
  compiling: false,
  lastMs: null,
};

/**
 * Debounce window in milliseconds. Exported so tests can pass the same value
 * explicitly rather than relying on the default.
 */
export const DEFAULT_DEBOUNCE_MS = 250;

/**
 * Recompile `source` through {@link compileSource} `ms` after the last edit.
 * Each new source value resets the timer and marks any pending result stale.
 */
export function useDebouncedRecompile(source: string, ms = DEFAULT_DEBOUNCE_MS): RecompileState {
  const [state, setState] = useState<RecompileState>(INITIAL);

  useEffect(() => {
    let cancelled = false;
    const handle = setTimeout(async () => {
      setState((s) => ({ ...s, compiling: true }));
      const t0 = performance.now();
      try {
        const res = await compileSource(source);
        if (cancelled) return;
        setState({
          mesh: res.mesh,
          errors: res.meta.errors,
          stats: res.meta.stats,
          compiling: false,
          lastMs: Math.round(performance.now() - t0),
        });
      } catch (err) {
        if (cancelled) return;
        setState({
          mesh: null,
          errors: [
            {
              line: 0,
              col: 0,
              message: err instanceof Error ? err.message : String(err),
              kind: "general",
            },
          ],
          stats: null,
          compiling: false,
          lastMs: Math.round(performance.now() - t0),
        });
      }
    }, ms);
    return () => {
      cancelled = true;
      clearTimeout(handle);
    };
  }, [source, ms]);

  return state;
}
