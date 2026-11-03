// Test: theme_tokens_applied.
//
// Asserts the Ember token set is defined with distinct dark + light values
// (parsed from tokens.css so the assertion checks the real artifact, not jsdom's
// partial CSS computation), and that useTheme applies + toggles data-theme on
// the root element and persists the choice.
import { expect, test, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { useTheme, THEME_STORAGE_KEY } from "./useTheme";

// Read the real token artifact from disk so the assertion checks the file that
// ships (not vite's in-memory CSS representation, which is environment-dependent).
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const tokensCssRaw = readFileSync(
  path.resolve(__dirname, "../styles/tokens.css"),
  "utf8",
);

beforeEach(() => {
  document.documentElement.removeAttribute("data-theme");
  try {
    localStorage.clear();
  } catch {
    // ignore (sandbox without localStorage)
  }
});

test("theme_tokens_applied", () => {
  // 1. The Ember token set defines --morphe-bg with distinct dark + light values.
  const bgMatches = tokensCssRaw.match(/--morphe-bg:\s*[^;]+;/g);
  expect(bgMatches).not.toBeNull();
  expect(bgMatches!.length).toBeGreaterThanOrEqual(2);
  expect(bgMatches![0]).not.toBe(bgMatches![1]); // dark !== light
  expect(tokensCssRaw).toContain('[data-theme="light"]');

  // 2. useTheme defaults to dark, reflects data-theme on <html>, and persists.
  const { result } = renderHook(() => useTheme());
  expect(result.current.theme).toBe("dark");
  expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");

  // 3. Toggling swaps the theme end-to-end.
  act(() => result.current.toggle());
  expect(result.current.theme).toBe("light");
  expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
});
