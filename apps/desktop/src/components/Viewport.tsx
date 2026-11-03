// three.js viewport pane for a compiled mesh.
//
// The pure pieces are exported so they can be tested without a WebGL renderer
// (jsdom has no WebGL): `buildMeshGeometry` (mesh -> BufferGeometry),
// `disposeObject` (free GPU resources of a subtree), `shouldReframe` and
// `frameBox` (camera framing). The component sets up a scene + camera + orbit
// controls + build plate + axes gizmo, and rebuilds the mesh whenever the
// `mesh` prop changes. The renderer setup no-ops in any environment without
// WebGL (jsdom).
//
// Coordinates: OpenSCAD is Z-up. The scene keeps the model's coordinates as
// they are and makes Z the camera's up axis, with the build plate in the XY
// plane, so a `cube([10, 10, 30])` stands upright instead of lying on its side.
import { useEffect, useRef } from "react";
import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";

import type { MeshBuffer } from "../ipc";

const MESH_NAME = "morphe-scad-mesh";

/** The scene's up axis: OpenSCAD models are Z-up. */
export const UP = new THREE.Vector3(0, 0, 1);

/** Direction the camera looks FROM when framing (an isometric-ish 3/4 view). */
const VIEW_DIRECTION = new THREE.Vector3(1, -1.2, 0.9).normalize();

/**
 * Convert a decoded MeshBuffer into a three.js BufferGeometry: a `position`
 * attribute from the flat xyz positions and an index from the triangle indices.
 * Vertex normals are computed so lit shading works. Pure — no scene/renderer.
 */
export function buildMeshGeometry(mesh: MeshBuffer): THREE.BufferGeometry {
  const geo = new THREE.BufferGeometry();
  geo.setAttribute("position", new THREE.BufferAttribute(mesh.positions, 3));
  if (mesh.indices.length > 0) {
    geo.setIndex(new THREE.BufferAttribute(mesh.indices, 1));
  }
  geo.computeVertexNormals();
  return geo;
}

/**
 * Dispose the geometry and material(s) of every mesh/line in `root`'s subtree
 * (three.js never frees GPU buffers on its own). Safe to call on any Object3D.
 */
export function disposeObject(root: THREE.Object3D): void {
  root.traverse((node) => {
    const withGpu = node as THREE.Object3D & {
      geometry?: THREE.BufferGeometry;
      material?: THREE.Material | THREE.Material[];
    };
    withGpu.geometry?.dispose();
    const material = withGpu.material;
    if (Array.isArray(material)) {
      material.forEach((m) => m.dispose());
    } else {
      material?.dispose();
    }
  });
}

/**
 * Whether the camera should re-frame for a model whose bounding box changed
 * from `previous` to `next`. Re-framing on every recompile would fight the
 * user's orbiting while they edit, so only re-frame for the first model, or
 * when the model's size changes by more than 2x either way (it would no
 * longer fit, or would shrink to a speck).
 */
export function shouldReframe(previous: THREE.Box3 | null, next: THREE.Box3): boolean {
  if (next.isEmpty()) return false;
  if (!previous || previous.isEmpty()) return true;
  const a = previous.getSize(new THREE.Vector3()).length();
  const b = next.getSize(new THREE.Vector3()).length();
  if (a === 0 || b === 0) return a !== b;
  const ratio = b / a;
  return ratio > 2 || ratio < 0.5;
}

/** Camera placement that frames a bounding box. */
export interface Framing {
  position: THREE.Vector3;
  target: THREE.Vector3;
  near: number;
  far: number;
}

/**
 * Where to put a perspective camera (vertical field of view `fovDeg`, aspect
 * `aspect`) so the whole of `box` is visible, looking at its centre from
 * {@link VIEW_DIRECTION} with a small margin. Pure.
 */
export function frameBox(box: THREE.Box3, fovDeg: number, aspect: number): Framing {
  const sphere = box.getBoundingSphere(new THREE.Sphere());
  const radius = Math.max(sphere.radius, 1e-3);
  const vfov = THREE.MathUtils.degToRad(fovDeg);
  // The narrower of the two fields of view decides the distance.
  const hfov = 2 * Math.atan(Math.tan(vfov / 2) * Math.max(aspect, 1e-3));
  const fov = Math.min(vfov, hfov);
  const distance = (radius / Math.sin(fov / 2)) * 1.15;
  const position = sphere.center.clone().addScaledVector(VIEW_DIRECTION, distance);
  return {
    position,
    target: sphere.center.clone(),
    near: Math.max(distance / 1000, 0.01),
    far: distance * 100,
  };
}

/** Release a probe canvas's WebGL context (browsers cap live contexts). */
function releaseProbeContext(gl: WebGLRenderingContext | WebGL2RenderingContext) {
  gl.getExtension("WEBGL_lose_context")?.loseContext();
}

export interface ViewportProps {
  mesh: MeshBuffer | null;
}

/** Internal handle stashed by the setup effect for the mesh-rebuild effect. */
interface SceneApi {
  scene: THREE.Scene;
  camera: THREE.PerspectiveCamera;
  controls: OrbitControls;
  /** The box the camera was last framed on (null before the first model). */
  framedBox: THREE.Box3 | null;
}

export default function Viewport({ mesh }: ViewportProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const apiRef = useRef<SceneApi | null>(null);
  // The current model's bounds, for the "Fit view" button.
  const boxRef = useRef<THREE.Box3 | null>(null);

  const fitView = (box: THREE.Box3 | null) => {
    const api = apiRef.current;
    if (!api || !box || box.isEmpty()) return;
    const f = frameBox(box, api.camera.fov, api.camera.aspect);
    api.camera.position.copy(f.position);
    api.camera.near = f.near;
    api.camera.far = f.far;
    api.camera.updateProjectionMatrix();
    api.controls.target.copy(f.target);
    api.controls.update();
    api.framedBox = box.clone();
  };

  // One-time renderer/scene/camera setup for the component's lifetime. Guarded
  // so it no-ops where WebGL is unavailable (jsdom test env, headless canvas).
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const probe = document.createElement("canvas");
    const gl = probe.getContext("webgl2") || probe.getContext("webgl");
    if (!gl) return; // jsdom: no WebGL — bail before constructing a renderer.
    releaseProbeContext(gl);

    let renderer: THREE.WebGLRenderer;
    try {
      renderer = new THREE.WebGLRenderer({ antialias: true });
    } catch {
      return;
    }

    const scene = new THREE.Scene();
    scene.background = new THREE.Color(0x1e1e1e);

    const camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
    // Z-up must be set BEFORE OrbitControls is constructed: the controls
    // derive their orbit axis from camera.up at construction time.
    camera.up.copy(UP);
    camera.position.set(60, -72, 54);
    camera.lookAt(0, 0, 0);

    const controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;

    scene.add(new THREE.AmbientLight(0xffffff, 0.6));
    const key = new THREE.DirectionalLight(0xffffff, 1.0);
    key.position.set(0.8, -1, 1.2).normalize();
    scene.add(key);

    // Build plate in the XY plane (GridHelper is built in XZ, so tip it up)
    // and the XYZ axes gizmo (red X, green Y, blue Z — up).
    const grid = new THREE.GridHelper(200, 40, 0x8a8a8a, 0x3a3a3a);
    grid.rotation.x = Math.PI / 2;
    scene.add(grid);
    scene.add(new THREE.AxesHelper(25));

    container.appendChild(renderer.domElement);

    const resize = () => {
      const w = container.clientWidth || 1;
      const h = container.clientHeight || 1;
      renderer.setSize(w, h, false);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(container);

    let raf = 0;
    const animate = () => {
      raf = requestAnimationFrame(animate);
      controls.update();
      renderer.render(scene, camera);
    };
    animate();

    apiRef.current = { scene, camera, controls, framedBox: null };

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      controls.dispose();
      // Free everything the scene still holds: the current model, the build
      // plate and the axes gizmo — then the renderer and its GL context.
      disposeObject(scene);
      scene.clear();
      renderer.dispose();
      renderer.forceContextLoss();
      if (renderer.domElement.parentNode === container) {
        container.removeChild(renderer.domElement);
      }
      apiRef.current = null;
    };
  }, []);

  // Rebuild the mesh geometry whenever the `mesh` prop changes.
  useEffect(() => {
    const api = apiRef.current;
    if (!api) return;
    const prev = api.scene.getObjectByName(MESH_NAME);
    if (prev) {
      api.scene.remove(prev);
      // Each recompile creates a fresh geometry + material; free the old ones
      // or GPU memory grows over a long edit session.
      disposeObject(prev);
    }
    if (!mesh || mesh.vertexCount === 0) {
      boxRef.current = null;
      return;
    }
    const geometry = buildMeshGeometry(mesh);
    geometry.computeBoundingBox();
    const material = new THREE.MeshStandardMaterial({ color: 0xe6a85c, flatShading: false });
    const obj = new THREE.Mesh(geometry, material);
    obj.name = MESH_NAME;
    api.scene.add(obj);

    const box = geometry.boundingBox ? geometry.boundingBox.clone() : null;
    boxRef.current = box;
    if (box && shouldReframe(api.framedBox, box)) {
      fitView(box);
    }
  }, [mesh]);

  return (
    <div style={{ position: "relative", width: "100%", height: "100%" }}>
      <div ref={containerRef} data-testid="viewport" style={{ width: "100%", height: "100%" }} />
      <button
        type="button"
        data-testid="viewport-fit"
        onClick={() => fitView(boxRef.current)}
        disabled={!mesh || mesh.vertexCount === 0}
        title="Fit the model in view"
        aria-label="Fit the model in view"
        style={fitBtnStyle}
      >
        Fit
      </button>
    </div>
  );
}

const fitBtnStyle: React.CSSProperties = {
  position: "absolute",
  top: "var(--morphe-space-2)",
  right: "var(--morphe-space-2)",
  background: "var(--morphe-surface-2)",
  color: "var(--morphe-text)",
  border: "1px solid var(--morphe-border)",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-1) var(--morphe-space-3)",
  fontFamily: "var(--morphe-font)",
  fontSize: "var(--morphe-font-size-sm)",
  cursor: "pointer",
};
