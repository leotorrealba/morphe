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

// ── framing, disposal, orientation (pure helpers) ───────────────────────────

import { vi } from "vitest";
import { disposeObject, frameBox, shouldReframe, UP } from "../Viewport";

const box = (min: [number, number, number], max: [number, number, number]) =>
  new THREE.Box3(new THREE.Vector3(...min), new THREE.Vector3(...max));

test("viewport_scene_is_z_up_like_openscad", () => {
  expect(UP.toArray()).toEqual([0, 0, 1]);
});

test("frameBox_keeps_every_corner_of_the_model_in_view", () => {
  for (const aspect of [0.5, 1, 2.5]) {
    const b = box([-5, -5, 0], [5, 5, 30]); // a tall cube([10,10,30]), centred
    const f = frameBox(b, 45, aspect);
    const camera = new THREE.PerspectiveCamera(45, aspect, f.near, f.far);
    camera.up.copy(UP);
    camera.position.copy(f.position);
    camera.lookAt(f.target);
    camera.updateMatrixWorld();
    camera.updateProjectionMatrix();
    for (const x of [b.min.x, b.max.x])
      for (const y of [b.min.y, b.max.y])
        for (const z of [b.min.z, b.max.z]) {
          const p = new THREE.Vector3(x, y, z).project(camera);
          expect(Math.abs(p.x)).toBeLessThanOrEqual(1);
          expect(Math.abs(p.y)).toBeLessThanOrEqual(1);
          expect(p.z).toBeLessThan(1); // in front of the far plane
        }
    expect(f.target.toArray()).toEqual([0, 0, 15]);
  }
});

test("shouldReframe_only_for_first_model_or_a_big_size_change", () => {
  const unit = box([0, 0, 0], [10, 10, 10]);
  expect(shouldReframe(null, unit)).toBe(true);
  // Small edits while orbiting must not yank the camera around.
  expect(shouldReframe(unit, box([0, 0, 0], [12, 10, 10]))).toBe(false);
  expect(shouldReframe(unit, box([5, 5, 0], [15, 15, 10]))).toBe(false);
  // Grew or shrank by more than 2x → re-frame.
  expect(shouldReframe(unit, box([0, 0, 0], [30, 30, 30]))).toBe(true);
  expect(shouldReframe(unit, box([0, 0, 0], [2, 2, 2]))).toBe(true);
  // An empty box never triggers framing.
  expect(shouldReframe(unit, new THREE.Box3())).toBe(false);
});

test("disposeObject_frees_every_geometry_and_material_in_the_subtree", () => {
  const root = new THREE.Group();
  const geoA = new THREE.BufferGeometry();
  const matA = new THREE.MeshStandardMaterial();
  const geoB = new THREE.BufferGeometry();
  const matsB = [new THREE.MeshBasicMaterial(), new THREE.MeshBasicMaterial()];
  const child = new THREE.Mesh(geoB, matsB);
  root.add(new THREE.Mesh(geoA, matA));
  root.add(new THREE.Group().add(child));
  root.add(new THREE.GridHelper(10, 2));
  const spies = [geoA, matA, geoB, ...matsB].map((o) => vi.spyOn(o, "dispose"));
  disposeObject(root);
  spies.forEach((s) => expect(s).toHaveBeenCalledTimes(1));
});

test("fit_button_is_disabled_without_a_model", () => {
  const { getByTestId, rerender } = render(<Viewport mesh={null} />);
  expect((getByTestId("viewport-fit") as HTMLButtonElement).disabled).toBe(true);
  rerender(<Viewport mesh={FIXTURE_MESH} />);
  expect((getByTestId("viewport-fit") as HTMLButtonElement).disabled).toBe(false);
});
