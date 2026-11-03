// Per-test setup for the vitest jsdom environment.
//
// Registers @testing-library/jest-dom's matchers against vitest's expect, and
// polyfills requestAnimationFrame (jsdom does not ship it) so CodeMirror 6 —
// which schedules viewport measuring via rAF — mounts without throwing.
import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// Register auto-cleanup. vitest runs without `globals: true` here, so
// @testing-library/react's own auto-registration (which probes for a global
// `afterEach`) never binds — renders would leak across tests in the same file.
// This is the standard vitest+testing-library seam.
afterEach(() => {
  cleanup();
});

// jsdom does not ship requestAnimationFrame; CodeMirror 6 schedules viewport
// measuring via rAF. Polyfill it (through `unknown` to bridge the Node/Bun
// setTimeout return type vs the DOM lib's `number`).
if (typeof globalThis.requestAnimationFrame !== "function") {
  globalThis.requestAnimationFrame = (((cb: FrameRequestCallback) =>
    setTimeout(() => cb(Date.now()), 0)) as unknown) as typeof globalThis.requestAnimationFrame;
  globalThis.cancelAnimationFrame = (((id: number) =>
    clearTimeout(id)) as unknown) as typeof globalThis.cancelAnimationFrame;
}
