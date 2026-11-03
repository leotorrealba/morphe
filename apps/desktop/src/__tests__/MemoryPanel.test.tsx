// Memory-panel component tests: render + retire interaction, and the
// write-failure UI cue.
//
// Mocks `../memory` (listMemory / retireConstraint) — the Rust half is covered
// by apps/desktop/src-tauri/tests/memory.rs. The retire mock appends a
// `constraint_retired` tombstone to the shared event stream, mirroring the
// backend, so the panel's client-side active-filter visibly drops the retired
// constraint on refresh.
import { expect, test, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import MemoryPanel from "../components/MemoryPanel";
import { listMemory, retireConstraint, type DesignEvent } from "../memory";

// The shared event stream the mocks read/mutate. Module-scoped so the retire
// mock's mutation is visible to the next listMemory call (mirrors the backend).
let events: DesignEvent[] = [];

vi.mock("../memory", async (importOriginal) => ({
  // Keep the real pure helpers (activeConstraints); mock only the IPC calls.
  ...(await importOriginal<typeof import("../memory")>()),
  listMemory: vi.fn(async (): Promise<DesignEvent[]> => [...events]),
  retireConstraint: vi.fn(async (name: string): Promise<void> => {
    events.push({ kind: "constraint_retired", name });
  }),
}));

beforeEach(() => {
  events = [
    { kind: "constraint", name: "wall_thickness", value: "2mm" },
    { kind: "constraint", name: "hole_diameter", value: "5mm" },
    { kind: "intent", statement: "a 4-hole bracket" },
  ];
  vi.mocked(listMemory).mockClear();
  vi.mocked(retireConstraint).mockClear();
});

test("renders the fetched memory with a retire control per constraint", async () => {
  render(<MemoryPanel openedPath="/tmp/bracket.scad" />);

  // Constraints render (with retire buttons), intent renders.
  await waitFor(() => expect(screen.getByTestId("constraint-wall_thickness")).toBeTruthy());
  expect(screen.getByTestId("constraint-hole_diameter")).toBeTruthy();
  expect(screen.getByText("a 4-hole bracket")).toBeTruthy();
  expect(screen.getByTestId("retire-wall_thickness")).toBeTruthy();
});

test("retire click calls retireConstraint then refreshes, dropping the retired constraint", async () => {
  render(<MemoryPanel openedPath="/tmp/bracket.scad" />);
  await waitFor(() => expect(screen.getByTestId("constraint-wall_thickness")).toBeTruthy());

  const initialListCalls = vi.mocked(listMemory).mock.calls.length;

  fireEvent.click(screen.getByTestId("retire-wall_thickness"));

  // The retire IPC fired with the constraint name.
  await waitFor(() =>
    expect(retireConstraint).toHaveBeenCalledWith("wall_thickness"),
  );
  // The panel refreshed (a second listMemory call after the retire).
  await waitFor(() =>
    expect(vi.mocked(listMemory).mock.calls.length).toBeGreaterThan(initialListCalls),
  );
  // The retired constraint is gone from the active list (client-side filter
  // applied the constraint_retired tombstone the mock appended).
  await waitFor(() =>
    expect(screen.queryByTestId("constraint-wall_thickness")).toBeNull(),
  );
  // The other constraint remains.
  expect(screen.getByTestId("constraint-hole_diameter")).toBeTruthy();
});

test("a retire failure surfaces an error cue", async () => {
  vi.mocked(retireConstraint).mockRejectedValueOnce(new Error("sidecar unwritable"));

  render(<MemoryPanel openedPath="/tmp/bracket.scad" />);
  await waitFor(() => expect(screen.getByTestId("constraint-wall_thickness")).toBeTruthy());

  fireEvent.click(screen.getByTestId("retire-wall_thickness"));

  // The error surfaces to the user (closes "eprintln invisible to GUI" for
  // user-initiated writes).
  await waitFor(() =>
    expect(screen.getByTestId("memory-error")).toBeTruthy(),
  );
  expect(screen.getByTestId("memory-error").textContent).toContain(
    "sidecar unwritable",
  );
});

test("no file open shows the placeholder, not an error", async () => {
  render(<MemoryPanel openedPath={null} />);
  // No invoke fired when there's no file open (the panel short-circuits).
  expect(vi.mocked(listMemory)).not.toHaveBeenCalled();
  expect(screen.getByText(/Open a project/i)).toBeTruthy();
  expect(screen.queryByTestId("memory-error")).toBeNull();
});
