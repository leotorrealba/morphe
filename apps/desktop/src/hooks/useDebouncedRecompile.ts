// Debounced live-recompile hook.
//
// Watches the `source` string and, after a 250ms quiet window, calls
// `compileSource` and surfaces `{ mesh, errors, stats, compiling, lastMs }`.
// The debounce is an effect-scoped `setTimeout` whose cleanup clears the timer
// on every source change, so a burst of edits collapses to exactly one compile
// call. An in-flight compile that is superseded by a newer edit is dropped via
// the `cancelled` flag (stale-result guard).
//
// When a compile fails (parse/eval/resolver errors, or the IPC call itself),
// the LAST GOOD mesh and stats are kept and `stale` is set, so a typo
// mid-edit doesn't blank the viewport; the errors are still reported. A
// compile that succeeds with no 3D result (a 2D-only or empty program) clears
// the mesh as before.
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
  /** True when `mesh`/`stats` are from an earlier successful compile because
   * the latest one failed. */
  stale: boolean;
}

const INITIAL: RecompileState = {
  mesh: null,
  errors: [],
  stats: null,
  compiling: false,
  lastMs: null,
  stale: false,
};

/** A failed compile: report `errors` but keep the previous mesh and stats
 * (marked stale when there was one). */
function keepLastGood(
  prev: RecompileState,
  errors: CompileError[],
  lastMs: number,
): RecompileState {
  return {
    mesh: prev.mesh,
    stats: prev.stats,
    errors,
    compiling: false,
    lastMs,
    stale: prev.mesh !== null,
  };
}

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
        const lastMs = Math.round(performance.now() - t0);
        if (res.meta.errors.length > 0) {
          setState((s) => keepLastGood(s, res.meta.errors, lastMs));
        } else {
          setState({
            mesh: res.mesh,
            errors: [],
            stats: res.meta.stats,
            compiling: false,
            lastMs,
            stale: false,
          });
        }
      } catch (err) {
        if (cancelled) return;
        const error: CompileError = {
          line: 0,
          col: 0,
          message: err instanceof Error ? err.message : String(err),
          kind: "general",
        };
        setState((s) => keepLastGood(s, [error], Math.round(performance.now() - t0)));
      }
    }, ms);
    return () => {
      cancelled = true;
      clearTimeout(handle);
    };
  }, [source, ms]);

  return state;
}
