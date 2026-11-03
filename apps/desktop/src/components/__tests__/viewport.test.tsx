// Test: viewport_renders_mesh_buffer.
//
// Asserts on the BufferGeometry produced by the pure `buildMeshGeometry` (not a
// WebGL render — jsdom has no WebGL). Also smoke-renders <Viewport> to confirm
// the component guards its renderer setup and mounts the container in jsdom.
import { expect, test } from "vitest";
import { render } from "@testing-library/react";
import * as THREE from "three";

import Viewport, { buildMeshGeometry } from "../Viewport";
import type { MeshBuffer } from "../../ipc";

const FIXTURE_MESH: MeshBuffer = {
  // one CCW triangle in the XY plane
  positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]),
  indices: new Uint32Array([0, 2, 1]),
  vertexCount: 3,
  triangleCount: 1,
};

test("viewport_renders_mesh_buffer", () => {
  const geo = buildMeshGeometry(FIXTURE_MESH);

  // position attribute mirrors the uploaded Float32Array (BufferAttribute wraps
  // the same reference — assert identity + element count).
  const position = geo.getAttribute("position") as THREE.BufferAttribute;
  expect(position).toBeInstanceOf(THREE.BufferAttribute);
  expect(position.itemSize).toBe(3);
  expect(position.count).toBe(3);
  expect(position.array).toBe(FIXTURE_MESH.positions);

  // index mirrors the uploaded Uint32Array.
  const index = geo.getIndex();
  expect(index).not.toBeNull();
  expect(index!.count).toBe(3);
  expect(index!.array).toBe(FIXTURE_MESH.indices);

  // normals computed by the builder (lit shading).
  expect(geo.getAttribute("normal")).toBeDefined();

  // empty mesh -> empty geometry, no index.
  const empty = buildMeshGeometry({
    positions: new Float32Array(0),
    indices: new Uint32Array(0),
    vertexCount: 0,
    triangleCount: 0,
  });
  expect((empty.getAttribute("position") as THREE.BufferAttribute).count).toBe(0);
  expect(empty.getIndex()).toBeNull();

  // smoke: the component renders a container and no-ops WebGL setup in jsdom.
  const { container, rerender } = render(<Viewport mesh={FIXTURE_MESH} />);
  expect(container.querySelector('[data-testid="viewport"]')).toBeTruthy();
  // re-render with a null mesh must not throw (mesh-rebuild effect handles null).
  rerender(<Viewport mesh={null} />);
  expect(container.querySelector('[data-testid="viewport"]')).toBeTruthy();
});
