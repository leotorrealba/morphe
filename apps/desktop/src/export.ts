// STL export orchestration.
//
// Factored as a pure fn with an injected `exportStl` dep so the export flow
// can be unit-tested without the Tauri runtime. The component wires the real
// dep — the IPC `exportStl`, which itself opens the native `.stl` save dialog
// inside the backend command.
//
// Security note: the destination path is chosen entirely inside the
// backend process via the OS save dialog — it never crosses the IPC trust
// boundary as a frontend-supplied value. The frontend only learns whether the
// export succeeded, was cancelled, or failed. Returns `null` when the user
// cancels so the caller can stay quiet.
import type { ExportStats } from "./ipc";

/** The side effect `runExport` depends on, isolated for test injection. */
export interface ExportDeps {
  /** Compile + open the OS save dialog + write STL. Mirrors `exportStl` from ./ipc. */
  exportStl: (source: string, ascii: boolean) => Promise<ExportStats>;
}

/** Success/failure outcome. `null` means the user cancelled (no toast). */
export interface ExportResult {
  ok: boolean;
  message: string;
}

/**
 * Format an `ExportError` rejection into a human-readable message. The real
 * `exportStl` IPC rejects with the tagged-union `ExportError` — a plain
 * serialized object, NOT an `Error` instance — so `instanceof Error` is false
 * on every real failure path and would otherwise yield `"[object Object]"`. The
 * union is decoded by its `kind` discriminant here; the `instanceof Error`
 * branch is kept only as defense in depth for any non-union rejection.
 */
function formatExportError(err: unknown): string {
  if (err && typeof err === "object" && "kind" in err) {
    const e = err as { kind: string; message?: string; errors?: { message: string }[] };
    switch (e.kind) {
      case "io":
        return e.message ?? "I/O error";
      case "not_3d":
        return "Top-level object is not 3D";
      case "compile":
        return e.errors?.[0]?.message ?? "Compile failed";
      default:
        return `Export failed (${e.kind})`;
    }
  }
  return err instanceof Error ? err.message : String(err);
}

/**
 * Run the STL export via the backend, which opens its own OS save dialog.
 * Resolves `null` when the user cancels the save dialog.
 */
export async function runExport(
  source: string,
  deps: ExportDeps,
): Promise<ExportResult | null> {
  try {
    const stats = await deps.exportStl(source, false);
    return {
      ok: true,
      message: `Exported ${stats.triangle_count} triangles (${stats.format})`,
    };
  } catch (err) {
    if (
      err &&
      typeof err === "object" &&
      "kind" in err &&
      (err as { kind: string }).kind === "cancelled"
    ) {
      return null; // user cancelled the OS save dialog — stay quiet
    }
    return { ok: false, message: formatExportError(err) };
  }
}
