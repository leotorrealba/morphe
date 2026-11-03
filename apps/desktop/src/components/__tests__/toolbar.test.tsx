// Tests for the Toolbar (including the Open button).
//
// The Toolbar is pure presentational: clicking Open / Export / theme-toggle
// fires the corresponding callback; the opened-filename cue renders when a
// file is open and stays absent when the editor holds an in-memory buffer.
// These tests pin that the Open click reaches `onOpen` and that the filename
// cue's presence depends on `openedFilename !== null` (dormancy: no filename
// shown when no
// file is open, mirroring the backend's `opened_path == None` memory-dormancy
// branch).
import { expect, test, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import Toolbar from "../Toolbar";

test("open_button_fires_onOpen_when_clicked", () => {
  // The Open button is the user action that activates design-memory: the
  // App wires onOpen → openFile → records the disk path in backend AppState.
  const onOpen = vi.fn();
  render(
    <Toolbar
      onExport={() => {}}
      onOpen={onOpen}
      openedFilename={null}
      theme="dark"
      onToggleTheme={() => {}}
    />,
  );

  fireEvent.click(screen.getByTestId("open-button"));
  expect(onOpen).toHaveBeenCalledTimes(1);
});

test("opened_filename_renders_when_a_file_is_open", () => {
  // The basename cue shows next to the brand mark — a minimal "what's loaded"
  // affordance. Required for the user to know memory is active (a file is
  // open → ai_accept_edit will emit a Decision event into the project's .noos).
  render(
    <Toolbar
      onExport={() => {}}
      onOpen={() => {}}
      openedFilename="bracket.scad"
      theme="dark"
      onToggleTheme={() => {}}
    />,
  );

  const cue = screen.getByTestId("toolbar-opened-filename");
  expect(cue.textContent).toBe("bracket.scad");
});

test("opened_filename_is_absent_when_no_file_is_open", () => {
  // When openedFilename is null (in-memory buffer), the cue MUST be absent —
  // not just empty — so the dormancy state is unambiguous to the user.
  // (Mirrors the backend's opened_path == None skip-and-log branch in
  // ai_accept_edit.)
  render(
    <Toolbar
      onExport={() => {}}
      onOpen={() => {}}
      openedFilename={null}
      theme="dark"
      onToggleTheme={() => {}}
    />,
  );

  expect(screen.queryByTestId("toolbar-opened-filename")).toBeNull();
});

test("export_and_theme_buttons_still_fire_their_callbacks", () => {
  // Regression guard: adding the Open button must not break the existing
  // export / theme-toggle callbacks.
  const onExport = vi.fn();
  const onToggleTheme = vi.fn();
  render(
    <Toolbar
      onExport={onExport}
      onOpen={() => {}}
      openedFilename={null}
      theme="dark"
      onToggleTheme={onToggleTheme}
    />,
  );

  fireEvent.click(screen.getByTestId("export-button"));
  expect(onExport).toHaveBeenCalledTimes(1);

  fireEvent.click(screen.getByTestId("theme-toggle"));
  expect(onToggleTheme).toHaveBeenCalledTimes(1);
});

test("save_button_fires_onSave_and_dirty_marker_shows_unsaved_changes", () => {
  const onSave = vi.fn();
  const { rerender } = render(
    <Toolbar
      onExport={() => {}}
      onOpen={() => {}}
      onSave={onSave}
      dirty={false}
      openedFilename="part.scad"
      theme="dark"
      onToggleTheme={() => {}}
    />,
  );
  expect(screen.queryByTestId("toolbar-dirty")).toBeNull();
  fireEvent.click(screen.getByTestId("save-button"));
  expect(onSave).toHaveBeenCalledTimes(1);

  rerender(
    <Toolbar
      onExport={() => {}}
      onOpen={() => {}}
      onSave={onSave}
      dirty={true}
      openedFilename="part.scad"
      theme="dark"
      onToggleTheme={() => {}}
    />,
  );
  expect(screen.getByTestId("toolbar-dirty")).toBeInTheDocument();
});
