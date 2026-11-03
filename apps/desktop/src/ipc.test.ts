// Pure-decoder tests for the `compile_source` wire frame.
//
// `decodeCompileResponse` is the first thing the frontend runs on the IPC bytes
// — including the error path, where the Rust backend emits an empty mesh. These
// tests build hand-crafted frames (no Tauri/invoke) so the decoder behavior is
// exercised in CI, not just typechecked. The empty-mesh case is a regression
// test: a `RangeError` once escaped before `meta.errors` reached the caller,
// deadening frontend error reporting.

import { expect, test } from "vitest";

import { decodeCompileResponse, type CompileMeta } from "./ipc";

/** Encode the `[u32 meta_len][meta_json][mesh]` wire frame (little-endian). */
function encodeFrame(meta: CompileMeta, meshBytes: Uint8Array): ArrayBuffer {
  const metaBytes = new TextEncoder().encode(JSON.stringify(meta));
  const metaLen = metaBytes.length;
  const buf = new ArrayBuffer(4 + metaLen + meshBytes.length);
  const view = new DataView(buf);
  view.setUint32(0, metaLen, true);
  new Uint8Array(buf, 4, metaLen).set(metaBytes);
  new Uint8Array(buf, 4 + metaLen, meshBytes.length).set(meshBytes);
  return buf;
}

/**
 * Encode the `MeshBuffer` layout: `[u32 vcount][u32 icount][f32 positions][u32 indices]`.
 * `positions` is a flat xyz list (length must be a multiple of 3).
 */
function encodeMesh(positions: number[], indices: number[]): Uint8Array {
  const vertexCount = positions.length / 3;
  const indexCount = indices.length;
  const buf = new ArrayBuffer(8 + positions.length * 4 + indices.length * 4);
  const view = new DataView(buf);
  view.setUint32(0, vertexCount, true);
  view.setUint32(4, indexCount, true);
  let off = 8;
  for (const p of positions) {
    view.setFloat32(off, p, true);
    off += 4;
  }
  for (const i of indices) {
    view.setUint32(off, i, true);
    off += 4;
  }
  return new Uint8Array(buf);
}

test("decodeCompileResponse handles an empty mesh on the error path", () => {
  // Mirrors the Rust backend's compile-error output: mesh_len === 0.
  const meta: CompileMeta = {
    stats: null,
    errors: [{ line: 1, col: 1, message: "bad", kind: "parse" }],
    warnings: [],
    mesh_len: 0,
  };
  const frame = encodeFrame(meta, new Uint8Array(0));

  // Must NOT throw — a RangeError on this path was a past bug.
  const result = decodeCompileResponse(frame);
  expect(result.meta.errors[0].kind).toBe("parse");
  expect(result.mesh.vertexCount).toBe(0);
  expect(result.mesh.triangleCount).toBe(0);
  expect(result.mesh.positions.length).toBe(0);
  expect(result.mesh.indices.length).toBe(0);
});

test("decodeCompileResponse decodes a valid mesh frame with matching counts", () => {
  const positions = [0, 0, 0, 1, 0, 0, 0, 1, 0]; // 3 verts, xyz interleaved
  const indices = [0, 2, 1]; // 1 triangle
  const meshBytes = encodeMesh(positions, indices);
  const meta: CompileMeta = {
    stats: null,
    errors: [],
    warnings: [],
    mesh_len: meshBytes.length,
  };
  const frame = encodeFrame(meta, meshBytes);

  const result = decodeCompileResponse(frame);
  expect(result.meta.errors.length).toBe(0);
  expect(result.mesh.vertexCount).toBe(3);
  expect(result.mesh.triangleCount).toBe(1);
  expect(result.mesh.positions.length).toBe(9);
  expect(result.mesh.indices.length).toBe(3);
  expect(Array.from(result.mesh.positions)).toEqual(positions);
  expect(Array.from(result.mesh.indices)).toEqual(indices);
});
