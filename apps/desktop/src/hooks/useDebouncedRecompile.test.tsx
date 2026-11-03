// Test: recompile_debounces_at_250ms.
//
// A burst of 5 rapid source changes must collapse to exactly ONE compileSource
// call after the 250ms debounce window. The debounce is the effect-scoped timer
// in useDebouncedRecompile; fake timers make that behavior observable without
// real waiting. compileSource is mocked (the module ../ipc) so the call count
// is the assertion surface — no Tauri/invoke.
import { expect, test, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

import { useDebouncedRecompile } from "./useDebouncedRecompile";
import { compileSource } from "../ipc";
import type { CompileResult } from "../ipc";

vi.mock("../ipc", () => ({
  compileSource: vi.fn(async () => ({
    mesh: {
      positions: new Float32Array(0),
      indices: new Uint32Array(0),
      vertexCount: 0,
      triangleCount: 0,
    },
    meta: { stats: null, errors: [], warnings: [], mesh_len: 0 },
  })),
}));

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(compileSource).mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

test("recompile_debounces_at_250ms", async () => {
  const { rerender } = renderHook(({ src }) => useDebouncedRecompile(src, 250), {
    initialProps: { src: "cube([10,10,10]);" },
  });

  // Burst of 5 rapid edits — each rerender resets the pending timer.
  for (let i = 0; i < 5; i++) {
    rerender({ src: `cube([${i},10,10]);` });
  }

  // Nothing has compiled yet: the timer is still pending.
  expect(compileSource).not.toHaveBeenCalled();

  // Advance past the debounce window; the final timer fires exactly once.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(250);
  });

  expect(compileSource).toHaveBeenCalledTimes(1);
});

test("cancelled_flag_drops_stale_result_when_edit_supersedes_inflight_compile", async () => {
  // Controllable compileSource: each call returns a pending promise whose
  // resolver we capture, so we can order the resolves and prove the
  // stale-result guard (the `cancelled` flag in the effect closure) drops a
  // superseded in-flight compile. Without this guard, the late edit-A resolve
  // would overwrite edit B's result.
  const pending: Array<(v: CompileResult) => void> = [];
  vi.mocked(compileSource).mockImplementation(
    () =>
      new Promise((resolve) => {
        pending.push(resolve);
      }) as Promise<CompileResult>,
  );

  // Two distinguishable results: edit A → 1 triangle, edit B → 2 triangles.
  const resultA: CompileResult = {
    mesh: {
      positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]),
      indices: new Uint32Array([0, 1, 2]),
      vertexCount: 3,
      triangleCount: 1,
    },
    meta: {
      stats: {
        bbox: null,
        bbox_size: null,
        volume: 0,
        surface_area: 0,
        is_manifold: true,
        triangle_count: 1,
        vertex_count: 3,
      },
      errors: [],
      warnings: [],
      mesh_len: 0,
    },
  };
  const resultB: CompileResult = {
    mesh: {
      positions: new Float32Array(18),
      indices: new Uint32Array(6),
      vertexCount: 6,
      triangleCount: 2,
    },
    meta: {
      stats: {
        bbox: null,
        bbox_size: null,
        volume: 0,
        surface_area: 0,
        is_manifold: true,
        triangle_count: 2,
        vertex_count: 6,
      },
      errors: [],
      warnings: [],
      mesh_len: 0,
    },
  };

  const { rerender, result } = renderHook(({ src }) => useDebouncedRecompile(src, 250), {
    initialProps: { src: "cube([1,1,1]);" },
  });

  // Edit A: fire its debounce timer and advance INTO the await. compileSource(A)
  // has been called; pending[0] holds its resolver. We are now stuck at the await.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(250);
  });
  expect(compileSource).toHaveBeenCalledTimes(1);

  // Edit B arrives while A is in flight. Effect A's cleanup runs (sets its
  // `cancelled` flag true); effect B starts a fresh timer.
  act(() => {
    rerender({ src: "cube([2,2,2]);" });
  });

  // Fire edit B's debounce → compileSource(B) called; pending[1] resolves B.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(250);
  });
  expect(compileSource).toHaveBeenCalledTimes(2);

  // Resolve edit A FIRST (out of order — it completed after B was requested).
  // The stale guard must drop it: no state commits, stats stay null.
  await act(async () => {
    pending[0](resultA);
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(result.current.stats).toBeNull();
  expect(result.current.mesh).toBeNull();

  // Resolve edit B. Its result must land; A's must not have leaked through.
  await act(async () => {
    pending[1](resultB);
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(result.current.compiling).toBe(false);
  expect(result.current.stats?.triangle_count).toBe(2);
  expect(result.current.mesh?.triangleCount).toBe(2);
});

// ── a failed compile keeps the last good model ──────────────────────────────

const GOOD: CompileResult = {
  mesh: {
    positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]),
    indices: new Uint32Array([0, 1, 2]),
    vertexCount: 3,
    triangleCount: 1,
  },
  meta: {
    stats: {
      bbox: null,
      bbox_size: null,
      volume: 0,
      surface_area: 0,
      is_manifold: true,
      triangle_count: 1,
      vertex_count: 3,
    },
    errors: [],
    warnings: [],
    mesh_len: 0,
  },
};

const BROKEN: CompileResult = {
  mesh: { positions: new Float32Array(0), indices: new Uint32Array(0), vertexCount: 0, triangleCount: 0 },
  meta: {
    stats: null,
    errors: [{ line: 1, col: 5, message: "expected ';'", kind: "parse" }],
    warnings: [],
    mesh_len: 0,
  },
};

async function compileWith(
  rerender: (p: { src: string }) => void,
  src: string,
  outcome: CompileResult | Error,
) {
  if (outcome instanceof Error) {
    vi.mocked(compileSource).mockRejectedValueOnce(outcome);
  } else {
    vi.mocked(compileSource).mockResolvedValueOnce(outcome);
  }
  act(() => rerender({ src }));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(250);
  });
}

test("a_failed_compile_keeps_the_last_good_mesh_and_marks_it_stale", async () => {
  vi.mocked(compileSource).mockResolvedValueOnce(GOOD);
  const { rerender, result } = renderHook(({ src }) => useDebouncedRecompile(src, 250), {
    initialProps: { src: "cube(1);" },
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(250);
  });
  expect(result.current.mesh?.triangleCount).toBe(1);
  expect(result.current.stale).toBe(false);

  // A typo: errors are reported, but the viewport keeps the previous model.
  await compileWith(rerender, "cube(1)", BROKEN);
  expect(result.current.errors).toHaveLength(1);
  expect(result.current.mesh?.triangleCount).toBe(1);
  expect(result.current.stats?.triangle_count).toBe(1);
  expect(result.current.stale).toBe(true);

  // The IPC call itself failing behaves the same way.
  await compileWith(rerender, "cube(1)  ", new Error("worker crashed"));
  expect(result.current.errors[0].message).toBe("worker crashed");
  expect(result.current.mesh?.triangleCount).toBe(1);
  expect(result.current.stale).toBe(true);

  // Fixing the typo replaces the model and clears the stale flag.
  await compileWith(rerender, "cube(2);", GOOD);
  expect(result.current.errors).toHaveLength(0);
  expect(result.current.stale).toBe(false);
});

test("a_failed_first_compile_has_no_model_and_is_not_stale", async () => {
  vi.mocked(compileSource).mockResolvedValueOnce(BROKEN);
  const { result } = renderHook(({ src }) => useDebouncedRecompile(src, 250), {
    initialProps: { src: "cube(" },
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(250);
  });
  expect(result.current.errors).toHaveLength(1);
  expect(result.current.mesh).toBeNull();
  expect(result.current.stale).toBe(false);
});
