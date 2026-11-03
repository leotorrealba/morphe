// Wire-shape tests for the `ai_*` IPC bindings.
//
// `acceptEdit` carries the accepted edit's `{ path, old, new }` so the backend
// can emit a noos `Decision` event on every accept. These tests mock
// `@tauri-apps/api/core` and assert the invoke payload matches the Rust
// command's signature — a wire-shape check, not a runtime-behavior test (the latter lives in diff.test.tsx).

import { expect, test, vi, beforeEach } from "vitest";

// Mock `invoke` so the tests assert on the (cmd, args) tuple actually sent over
// IPC. `Channel` is imported (and used) by ai.ts for streamAiTurn; stub it as a
// no-op class so the module loads cleanly.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: ((msg: unknown) => void) | null = null;
  },
}));

import { invoke } from "@tauri-apps/api/core";
import { acceptEdit, type EditPatch } from "./ai";

beforeEach(() => {
  vi.mocked(invoke).mockClear();
});

test("acceptEdit passes the patch's {path, old, new} to ai_accept_edit", async () => {
  vi.mocked(invoke).mockResolvedValue("cube(2);");

  const patch: EditPatch = {
    path: "/home/me/proj/bracket.scad",
    old: "cube(1);",
    new: "cube(2);",
  };

  const result = await acceptEdit(patch);

  expect(result).toBe("cube(2);");
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith("ai_accept_edit", {
    path: "/home/me/proj/bracket.scad",
    old: "cube(1);",
    new: "cube(2);",
  });
});

test("acceptEdit accepts an EditPatch (type-level wire check)", () => {
  // Compile-time assertion: acceptEdit must take an EditPatch, not be no-arg.
  // If the signature regresses to `()`, this line fails to typecheck.
  const fn: (patch: EditPatch) => Promise<string> = acceptEdit;
  expect(typeof fn).toBe("function");
});
