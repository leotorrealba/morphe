// three.js viewport pane for a compiled mesh.
//
// The mesh -> geometry step is factored into the pure exported
// `buildMeshGeometry` so it can be tested without a WebGL renderer (jsdom has
// no WebGL). The component sets up a scene + camera + orbit controls + grid +
// axes gizmo, and rebuilds the mesh geometry whenever the `mesh` prop changes.
// The renderer setup no-ops in any environment without WebGL (jsdom).
import { useEffect, useRef } from "react";
import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";

import type { MeshBuffer } from "../ipc";

const MESH_NAME = "morphe-scad-mesh";

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

export interface ViewportProps {
  mesh: MeshBuffer | null;
}

/** Internal handle stashed by the setup effect for the mesh-rebuild effect. */
interface SceneApi {
  scene: THREE.Scene;
  camera: THREE.PerspectiveCamera;
  controls: OrbitControls;
}

export default function Viewport({ mesh }: ViewportProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const apiRef = useRef<SceneApi | null>(null);

  // One-time renderer/scene/camera setup for the component's lifetime. Guarded
  // so it no-ops where WebGL is unavailable (jsdom test env, headless canvas).
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const probe = document.createElement("canvas");
    const gl = probe.getContext("webgl2") || probe.getContext("webgl");
    if (!gl) return; // jsdom: no WebGL — bail before constructing a renderer.

    let renderer: THREE.WebGLRenderer;
    try {
      renderer = new THREE.WebGLRenderer({ antialias: true });
    } catch {
      return;
    }

    const scene = new THREE.Scene();
    scene.background = new THREE.Color(0x1e1e1e);

    const camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
    camera.position.set(60, 60, 60);
    camera.lookAt(0, 0, 0);

    const controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;

    scene.add(new THREE.AmbientLight(0xffffff, 0.6));
    const key = new THREE.DirectionalLight(0xffffff, 1.0);
    key.position.set(1, 1.2, 0.8).normalize();
    scene.add(key);

    // Build plate + XYZ axes gizmo.
    scene.add(new THREE.GridHelper(200, 40, 0x8a8a8a, 0x3a3a3a));
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

    apiRef.current = { scene, camera, controls };

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      controls.dispose();
      renderer.dispose();
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
      const mesh = prev as THREE.Mesh;
      mesh.geometry?.dispose();
      // Dispose the material too — each recompile creates a fresh
      // MeshStandardMaterial, so leaving it undisposed leaks GPU memory over
      // a long edit session. `material` may be a single Material or an array.
      const material = mesh.material;
      if (Array.isArray(material)) {
        material.forEach((m) => m.dispose());
      } else if (material) {
        material.dispose();
      }
    }
    if (!mesh || mesh.vertexCount === 0) return;
    const geometry = buildMeshGeometry(mesh);
    const material = new THREE.MeshStandardMaterial({ color: 0xe6a85c, flatShading: false });
    const obj = new THREE.Mesh(geometry, material);
    obj.name = MESH_NAME;
    api.scene.add(obj);
  }, [mesh]);

  return <div ref={containerRef} data-testid="viewport" style={{ width: "100%", height: "100%" }} />;
}
