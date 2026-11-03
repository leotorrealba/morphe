// Tests for DiffPreview accept/reject wiring.
//
// The DiffPreview owns the accept/reject IPC calls and the editor-buffer patch
// on accept. `../ai` is mocked so the assertions are the IPC calls (accept
// vs reject called) and the editor-buffer effect (onSourceChange called only on
// accept, with old→new applied — which feeds useDebouncedRecompile → viewport).
import { expect, test, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import DiffPreview from "../DiffPreview";
import {
  acceptEdit,
  rejectEdit,
  type EditPatch,
} from "../../ai";

vi.mock("../../ai", () => ({
  acceptEdit: vi.fn(async () => "cube([20,20,20]);"),
  rejectEdit: vi.fn(async () => "cube([10,10,10]);"),
}));

const patch: EditPatch = {
  path: "main.scad",
  old: "cube([10,10,10]);",
  new: "cube([20,20,20]);",
};

beforeEach(() => {
  vi.mocked(acceptEdit).mockClear();
  vi.mocked(rejectEdit).mockClear();
});

test("diff_reject_leaves_buffer_untouched", async () => {
  const onSourceChange = vi.fn();
  const source = "cube([10,10,10]);\n";
  render(
    <DiffPreview
      patch={patch}
      id="toolu_reject_1"
      source={source}
      onSourceChange={onSourceChange}
    />,
  );

  fireEvent.click(screen.getByTestId("diff-reject"));
  // rejectEdit carries the tool-use id so the backend can skip
  // this edit when reconstructing the final source (otherwise the bridge would
  // overwrite the reject and the next turn's seed would diverge from the editor).
  await waitFor(() => expect(rejectEdit).toHaveBeenCalledWith(patch, "toolu_reject_1"));

  // Accept never fired, and the editor buffer is untouched (no recompile).
  expect(acceptEdit).not.toHaveBeenCalled();
  expect(onSourceChange).not.toHaveBeenCalled();
});

test("diff_accept_updates_buffer_and_recompiles", async () => {
  const onSourceChange = vi.fn();
  const source = "cube([10,10,10]);\n";
  render(
    <DiffPreview
      patch={patch}
      id="toolu_accept_1"
      source={source}
      onSourceChange={onSourceChange}
    />,
  );

  fireEvent.click(screen.getByTestId("diff-accept"));
  await waitFor(() => expect(acceptEdit).toHaveBeenCalled());

  // The editor buffer has old→new applied; onSourceChange feeds the debounced
  // recompile in the parent (the "recompile" effect is implicit in App).
  expect(onSourceChange).toHaveBeenCalledWith("cube([20,20,20]);\n");
  expect(rejectEdit).not.toHaveBeenCalled();
});

test("diff_accept_applies_new_literally_without_dollar_expansion", async () => {
  const onSourceChange = vi.fn();
  // `new` contains `$&` — JS String.replace with a STRING replacement would
  // expand `$&` to the matched substring ("cube"); the backend dispatcher
  // inserts `new` literally (dispatch.rs `edit`'s `push_str(new)`). The
  // function-replacer must keep `$&` literal so the editor buffer matches the
  // backend workspace byte-for-byte.
  const dollarPatch: EditPatch = {
    path: "main.scad",
    old: "cube",
    new: "$&X", // must stay literal "$&X", NOT expand to "cubeX"
  };
  const source = "cube([10,10,10]);\n";
  render(
    <DiffPreview
      patch={dollarPatch}
      id="toolu_v6"
      source={source}
      onSourceChange={onSourceChange}
    />,
  );

  fireEvent.click(screen.getByTestId("diff-accept"));
  await waitFor(() => expect(acceptEdit).toHaveBeenCalled());

  // Literal: "$&X" survives — the buffer matches what the Rust dispatcher wrote.
  expect(onSourceChange).toHaveBeenCalledWith("$&X([10,10,10]);\n");
});

// ── review-hardening: accept must apply to the CURRENT buffer, exactly once ──

test("diff_accept_refuses_when_old_text_no_longer_in_buffer", async () => {
  const onSourceChange = vi.fn();
  render(
    <DiffPreview
      patch={patch}
      id="toolu_stale"
      source={"sphere(5);\n"}
      onSourceChange={onSourceChange}
    />,
  );
  fireEvent.click(screen.getByTestId("diff-accept"));
  await waitFor(() => expect(screen.getByTestId("diff-error")).toBeInTheDocument());
  expect(screen.getByTestId("diff-error").textContent).toMatch(/no longer applies/);
  // Nothing applied, and nothing recorded in project memory.
  expect(onSourceChange).not.toHaveBeenCalled();
  expect(acceptEdit).not.toHaveBeenCalled();
  expect(screen.getByTestId("diff-preview").getAttribute("data-status")).toBe("pending");
});

test("diff_accept_refuses_ambiguous_match_including_overlaps", async () => {
  const onSourceChange = vi.fn();
  const overlap: EditPatch = { path: "main.scad", old: "aa", new: "b" };
  render(
    <DiffPreview patch={overlap} id="toolu_amb" source={"aaa"} onSourceChange={onSourceChange} />,
  );
  fireEvent.click(screen.getByTestId("diff-accept"));
  await waitFor(() => expect(screen.getByTestId("diff-error")).toBeInTheDocument());
  expect(screen.getByTestId("diff-error").textContent).toMatch(/appears 2 times/);
  expect(onSourceChange).not.toHaveBeenCalled();
  expect(acceptEdit).not.toHaveBeenCalled();
});

test("diff_accept_reads_latest_buffer_via_getSource", async () => {
  // The render-time `source` is stale; getSource returns the live buffer
  // (e.g. after a previous accept in the same tick). The patch applies to it.
  const onSourceChange = vi.fn();
  const live = "translate([1,0,0]) cube([10,10,10]);\n";
  render(
    <DiffPreview
      patch={patch}
      id="toolu_live"
      source={"stale"}
      getSource={() => live}
      onSourceChange={onSourceChange}
    />,
  );
  fireEvent.click(screen.getByTestId("diff-accept"));
  await waitFor(() => expect(acceptEdit).toHaveBeenCalled());
  expect(onSourceChange).toHaveBeenCalledWith("translate([1,0,0]) cube([20,20,20]);\n");
});

test("diff_reject_failure_is_surfaced_and_buttons_stay", async () => {
  vi.mocked(rejectEdit).mockRejectedValueOnce("backend down");
  render(
    <DiffPreview patch={patch} id="toolu_rf" source={"x"} onSourceChange={vi.fn()} />,
  );
  fireEvent.click(screen.getByTestId("diff-reject"));
  await waitFor(() => expect(screen.getByTestId("diff-error")).toBeInTheDocument());
  expect(screen.getByTestId("diff-error").textContent).toMatch(/Reject failed: backend down/);
  expect(screen.getByTestId("diff-reject")).toBeInTheDocument();
});
