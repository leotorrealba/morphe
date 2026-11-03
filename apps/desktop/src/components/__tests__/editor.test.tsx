// Test: editor_marks_error_line.
//
// CodeMirror 6's gutter painting is viewport-driven — line DOM is only emitted
// for lines inside the measured viewport, which is degenerate in jsdom (no real
// layout, zero-height containers). So the line-3 marker behavior is asserted on
// the pure gutter pieces rather than a rendered `.cm-gutter` cell:
//   1. ErrorGutterMarker.toDOM carries data-error-line / data-error-kind.
//   2. errorMarkersForLines places a marker whose `.line === 3` over a 4-line doc.
//   3. The <Editor> mounts without throwing (render smoke — the gutter + scad
//      language extension are wired in).
import { expect, test } from "vitest";
import { render } from "@testing-library/react";
import { Text } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";

import Editor from "../Editor";
import { ErrorGutterMarker, errorMarkersForLines } from "../scad";
import type { CompileError } from "../../ipc";

test("editor_marks_error_line", () => {
  const errors: CompileError[] = [{ line: 3, col: 5, message: "unsupported: minkowski", kind: "unsupported" }];

  // 1. The gutter marker DOM is deterministic and tagged with the line/kind.
  const dom = new ErrorGutterMarker(3, "unsupported: minkowski", "unsupported").toDOM({} as EditorView);
  expect(dom.getAttribute("data-error-line")).toBe("3");
  expect(dom.getAttribute("data-error-kind")).toBe("unsupported");
  expect(dom.className).toContain("cm-scad-error-gutter");

  // 2. errorMarkersForLines emits a marker at line 3 over a 4-line document.
  const doc = Text.of(["module foo() {", "  cube(1);", "  bar()", "}"]);
  const set = errorMarkersForLines(errors, doc);
  const markedLines: number[] = [];
  set.between(0, doc.length, (_from, _to, value) => {
    markedLines.push(value.line);
  });
  expect(markedLines).toContain(3);
  // Out-of-range error lines are dropped.
  const onlyOutOfRange = errorMarkersForLines([{ line: 99, col: 1, message: "x", kind: "general" }], doc);
  const allLines: number[] = [];
  onlyOutOfRange.between(0, doc.length, (_f, _t, v) => {
    allLines.push(v.line);
  });
  expect(allLines).toEqual([]);

  // 3. The editor mounts with the scad language + error gutter attached.
  const { container } = render(
    <Editor source={"module foo() {\n  cube(1);\n  bar()\n}"} errors={errors} onSourceChange={() => {}} theme="dark" />,
  );
  expect(container.querySelector(".cm-editor")).toBeTruthy();
});
