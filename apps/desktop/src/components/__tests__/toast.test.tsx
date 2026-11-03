import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Toast, { TOAST_DURATION_MS } from "../Toast";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

test("errors are announced assertively and successes politely", () => {
  const { rerender } = render(
    <Toast toast={{ kind: "error", message: "Save failed" }} onDismiss={() => {}} />,
  );
  expect(screen.getByTestId("toast").getAttribute("role")).toBe("alert");
  rerender(<Toast toast={{ kind: "success", message: "Saved" }} onDismiss={() => {}} />);
  expect(screen.getByTestId("toast").getAttribute("role")).toBe("status");
});

test("errors stay up longer than successes before auto-dismissing", () => {
  const onDismiss = vi.fn();
  render(<Toast toast={{ kind: "error", message: "Save failed" }} onDismiss={onDismiss} />);
  act(() => vi.advanceTimersByTime(TOAST_DURATION_MS.success));
  expect(onDismiss).not.toHaveBeenCalled();
  act(() => vi.advanceTimersByTime(TOAST_DURATION_MS.error - TOAST_DURATION_MS.success));
  expect(onDismiss).toHaveBeenCalledTimes(1);
});

test("hovering pauses the auto-dismiss timer", () => {
  const onDismiss = vi.fn();
  render(<Toast toast={{ kind: "success", message: "Saved" }} onDismiss={onDismiss} />);
  fireEvent.mouseEnter(screen.getByTestId("toast"));
  act(() => vi.advanceTimersByTime(TOAST_DURATION_MS.success * 3));
  expect(onDismiss).not.toHaveBeenCalled();
  fireEvent.mouseLeave(screen.getByTestId("toast"));
  act(() => vi.advanceTimersByTime(TOAST_DURATION_MS.success));
  expect(onDismiss).toHaveBeenCalledTimes(1);
});

test("the close button and Escape dismiss the toast", () => {
  const onDismiss = vi.fn();
  render(<Toast toast={{ kind: "error", message: "Save failed" }} onDismiss={onDismiss} />);
  fireEvent.click(screen.getByTestId("toast-dismiss"));
  fireEvent.keyDown(window, { key: "Escape" });
  expect(onDismiss).toHaveBeenCalledTimes(2);
});

test("parent re-renders with a new onDismiss do not restart the timer", () => {
  const calls: number[] = [];
  const toast = { kind: "success" as const, message: "Saved" };
  const { rerender } = render(<Toast toast={toast} onDismiss={() => calls.push(1)} />);
  // Typing in the editor re-renders App every ~half second.
  for (let i = 0; i < 10; i++) {
    act(() => vi.advanceTimersByTime(500));
    rerender(<Toast toast={toast} onDismiss={() => calls.push(1)} />);
  }
  expect(calls.length).toBe(1);
});
