// Test: export_stl_cmd opens its own OS save
// dialog in the backend (the destination path never crosses the IPC trust
// boundary).
//
// runExport now depends on a single `exportStl(source, ascii)` seam — the OS
// save dialog moved server-side, so this needs no Tauri runtime. The failure
// cases reject with the REAL ExportError wire shapes (the tagged union from
// ./ipc, a plain serialized object — NOT an Error instance) so
// formatExportError's union branch is what's exercised here, not the
// `instanceof Error` fallback. Covers: confirm → exportStl(source, false) +
// success; cancel → null; not_3d / io / compile rejections → formatted
// messages.
import { expect, test, vi } from "vitest";

import { runExport } from "./export";
import type { ExportStats } from "./ipc";

const STATS_OK: ExportStats = {
  triangle_count: 12,
  vertex_count: 36,
  format: "binary",
  bytes_written: 760,
};

test("confirm path: exportStl(source, false) + success outcome", async () => {
  const exportStl = vi.fn().mockResolvedValue(STATS_OK);

  const res = await runExport("cube([10,10,10]);", { exportStl });

  expect(exportStl).toHaveBeenCalledTimes(1);
  expect(exportStl).toHaveBeenCalledWith("cube([10,10,10]);", false);
  expect(res?.ok).toBe(true);
  expect(res?.message).toContain("12");
});

test("cancel path: null result when backend reports cancelled", async () => {
  const exportCancel = vi.fn().mockRejectedValue({ kind: "cancelled" });

  const res = await runExport("cube(1);", { exportStl: exportCancel });

  expect(res).toBeNull();
  expect(exportCancel).toHaveBeenCalledTimes(1);
});

test("not_3d rejection: formats the union, not [object Object]", async () => {
  const exportFail = vi.fn().mockRejectedValue({ kind: "not_3d" });
  const res = await runExport("circle(5);", { exportStl: exportFail });
  expect(res?.ok).toBe(false);
  expect(res?.message).toBe("Top-level object is not 3D");
});

test("io rejection: surfaces the union's message", async () => {
  const exportFail = vi.fn().mockRejectedValue({ kind: "io", message: "disk full" });
  const res = await runExport("cube(1);", { exportStl: exportFail });
  expect(res?.ok).toBe(false);
  expect(res?.message).toBe("disk full");
});

test("compile rejection: surfaces the first compile error message", async () => {
  const exportFail = vi.fn().mockRejectedValue({
    kind: "compile",
    errors: [
      { line: 3, col: 2, message: "unsupported: minkowski", kind: "unsupported" },
    ],
    warnings: [],
  });
  const res = await runExport("minkowski(){cube(1);sphere(1);}", {
    exportStl: exportFail,
  });
  expect(res?.ok).toBe(false);
  expect(res?.message).toBe("unsupported: minkowski");
});
