// Shared IPC payload types for the morphe desktop app.
//
// The TypeScript surface of the compile/export/file commands: the Rust<->TS
// mirror types and the decoders for the binary `compile_source` wire frame +
// mesh buffer. The `ai_*` and memory commands live in ./ai.ts and ./memory.ts.

import { invoke } from "@tauri-apps/api/core";

// ── compile_source ──────────────────────────────────────────────────────────

/** One diagnostic from the compile pipeline. Mirrors Rust `CompileError`. */
export interface CompileError {
  /** 1-based source line. */
  line: number;
  /** 1-based source column. */
  col: number;
  /** Human-readable message (e.g. `"unsupported: minkowski (not yet implemented in morphe)"`). */
  message: string;
  /** `"parse"` | `"unsupported"` | `"general"` | `"resolver"`. */
  kind: "parse" | "unsupported" | "general" | "resolver";
}

/** Geometry stats for a resolved 3D solid. Mirrors Rust `GeometryStats`. */
export interface GeometryStats {
  bbox: [[number, number, number], [number, number, number]] | null;
  bbox_size: [number, number, number] | null;
  volume: number;
  surface_area: number;
  is_manifold: boolean;
  triangle_count: number;
  vertex_count: number;
}

/**
 * Wire-frame metadata JSON prefix. Mirrors the Rust `to_wire` `Meta` struct: it
 * precedes the raw mesh bytes in the binary response (see {@link decodeCompileResponse}).
 */
export interface CompileMeta {
  stats: GeometryStats | null;
  errors: CompileError[];
  warnings: string[];
  /** Byte length of the `MeshBuffer` that follows the metadata JSON. */
  mesh_len: number;
}

/**
 * A decoded mesh buffer — the viewport's upload payload.
 *
 * Binary layout on the wire (little-endian):
 * ```
 * [u32 vertex_count]            V = positions.length / 3
 * [u32 index_count ]            I = indices.length       (3 per triangle)
 * [f32 x V x 3       ]          positions, xyz interleaved
 * [u32 x I           ]          indices, flat (3 per triangle, CCW outward)
 * ```
 * Positions are f32 (narrowed from the kernel's f64 on the Rust side).
 */
export interface MeshBuffer {
  /** Flat xyz positions, length = vertexCount * 3. */
  positions: Float32Array;
  /** Flat triangle indices, length = indexCount (a multiple of 3). */
  indices: Uint32Array;
  vertexCount: number;
  triangleCount: number;
}

/** A fully decoded `compile_source` result. */
export interface CompileResult {
  mesh: MeshBuffer;
  meta: CompileMeta;
}

/**
 * The Tauri command name. The Rust `#[tauri::command]` is `compile_source_cmd`
 * (the pure backing fn is `compile_source`; the `_cmd` suffix avoids a name
 * collision in `lib.rs`).
 */
export const COMPILE_SOURCE_CMD = "compile_source_cmd" as const;

/**
 * Decode the binary wire frame returned by `compile_source_cmd`:
 * `[u32 meta_json_len][meta_json bytes][mesh bytes]`. The mesh is never
 * JSON-encoded — it is the raw `MeshBuffer`.
 */
export function decodeCompileResponse(bytes: ArrayBuffer): CompileResult {
  const view = new DataView(bytes);
  if (bytes.byteLength < 4) {
    throw new Error(`compile_source response too short: ${bytes.byteLength} bytes`);
  }
  const metaLen = view.getUint32(0, true);
  const metaStart = 4;
  const meshStart = metaStart + metaLen;
  if (bytes.byteLength < meshStart) {
    throw new Error(
      `compile_source response truncated: need ${meshStart} bytes, have ${bytes.byteLength}`,
    );
  }

  const metaJson = new TextDecoder().decode(new Uint8Array(bytes, metaStart, metaLen));
  const meta = JSON.parse(metaJson) as CompileMeta;

  return { mesh: decodeMeshBuffer(bytes, meshStart, meta.mesh_len), meta };
}

/** Decode a `MeshBuffer` starting at `offset` for `len` bytes. */
function decodeMeshBuffer(buffer: ArrayBuffer, offset: number, len: number): MeshBuffer {
  // The Rust backend emits an empty mesh (mesh_len === 0) on compile errors,
  // 2D-only top level, and empty top level. A zero-length buffer is legal and
  // must short-circuit before any DataView read.
  if (len === 0) {
    return {
      positions: new Float32Array(0),
      indices: new Uint32Array(0),
      vertexCount: 0,
      triangleCount: 0,
    };
  }
  if (len < 8) {
    throw new Error(
      `mesh buffer too short: need >= 8 bytes for header, have ${len}`,
    );
  }
  const view = new DataView(buffer, offset, len);
  const vertexCount = view.getUint32(0, true);
  const indexCount = view.getUint32(4, true);

  const positionsOffset = 8;
  const positionsBytes = vertexCount * 3 * 4;
  const indicesOffset = positionsOffset + positionsBytes;

  // slice() copies, detaching the result from the (large) response buffer so
  // the GC can reclaim it once the mesh is uploaded to the GPU.
  const positions = new Float32Array(buffer.slice(offset + positionsOffset, offset + indicesOffset));
  const indices = new Uint32Array(buffer.slice(offset + indicesOffset, offset + len));

  return {
    positions,
    indices,
    vertexCount,
    triangleCount: indexCount / 3,
  };
}

/**
 * Compile `.scad` source into a mesh + stats, returning a fully decoded result.
 * The mesh crosses the IPC boundary as raw bytes (not JSON).
 *
 * @param source The `.scad` source to compile.
 * @returns The decoded mesh, geometry stats, and any errors/warnings.
 */
export async function compileSource(source: string): Promise<CompileResult> {
  const bytes = (await invoke(COMPILE_SOURCE_CMD, { source })) as ArrayBuffer;
  return decodeCompileResponse(bytes);
}

// ── export_stl ──────────────────────────────────────────────────────────────

/** Stats for a successful STL export. Mirrors Rust `ExportStats`. */
export interface ExportStats {
  triangle_count: number;
  vertex_count: number;
  /** Which STL variant was written. */
  format: "binary" | "ascii";
  /** Total bytes written to disk. */
  bytes_written: number;
}

/**
 * One failure mode of `export_stl`. Mirrors Rust `ExportError` as a tagged
 * union (`kind` discriminant, snake_case).
 */
export type ExportError =
  | { kind: "compile"; errors: CompileError[]; warnings: string[] }
  | { kind: "not_3d" }
  | { kind: "io"; message: string }
  | { kind: "cancelled" };

/** The Rust `#[tauri::command]` name for the STL export. */
export const EXPORT_STL_CMD = "export_stl_cmd" as const;

/**
 * Compile `source` and write the resolved solid's STL bytes. The destination
 * path is chosen via the native `.stl` save dialog opened *inside* the backend
 * command — it never crosses the IPC trust boundary as a frontend-supplied
 * value. Resolves with {@link ExportStats} on success; rejects with
 * {@link ExportError} on a user cancel, a compile failure, a 2D-only top level,
 * or an I/O error.
 *
 * @param source The `.scad` source to compile + export.
 * @param ascii  Emit ASCII STL instead of the default binary STL.
 */
export async function exportStl(
  source: string,
  ascii: boolean,
): Promise<ExportStats> {
  return invoke(EXPORT_STL_CMD, { source, ascii }) as Promise<ExportStats>;
}

// ── open_file / save_file ───────────────────────────────────────────────────

/** Result of `open_file` — the picked path and the file's `.scad` source. Mirrors Rust `OpenFileResult`. */
export interface OpenFileResult {
  /** The picked path, as a display string. */
  path: string;
  /** The file's contents (UTF-8 `.scad` source). */
  source: string;
}

/** Result of `save_file` — the path the source was written to. Mirrors Rust `SaveFileResult`. */
export interface SaveFileResult {
  /** The picked path, as a display string. */
  path: string;
}

/**
 * One failure mode of `open_file` / `save_file`. Mirrors Rust `FileError` as a
 * tagged union (`kind` discriminant, snake_case).
 */
export type FileError =
  | { kind: "cancelled" }
  | { kind: "io"; message: string };

/** The Rust `#[tauri::command]` name for the open-file dialog. */
export const OPEN_FILE_CMD = "open_file_cmd" as const;

/**
 * Open a native `.scad` open dialog, read the picked file, and return its
 * source. Resolves with {@link OpenFileResult}; rejects with {@link FileError}
 * if the user cancels or the read fails.
 */
export async function openFile(): Promise<OpenFileResult> {
  return invoke(OPEN_FILE_CMD) as Promise<OpenFileResult>;
}

/** The Rust `#[tauri::command]` name for the save-file dialog. */
export const SAVE_FILE_CMD = "save_file_cmd" as const;

/**
 * Save `source`. With `saveAs` (the default) a native `.scad` save dialog
 * picks the destination; with `saveAs: false` the backend writes straight
 * back to the file that is currently open (falling back to the dialog for an
 * in-memory buffer). The destination is always chosen inside the backend — no
 * path crosses IPC. Resolves with {@link SaveFileResult}; rejects with
 * {@link FileError} if the user cancels or the write fails.
 *
 * @param source The `.scad` source to write.
 * @param saveAs Force the save dialog (Save As…). Defaults to `true`.
 */
export async function saveFile(source: string, saveAs = true): Promise<SaveFileResult> {
  return invoke(SAVE_FILE_CMD, { source, saveAs }) as Promise<SaveFileResult>;
}
