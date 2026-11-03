// CodeMirror 6 language support + error-gutter extension for OpenSCAD (.scad).
//
// There is no official scad CodeMirror language package, so this defines a
// small StreamLanguage highlighter over the scad keyword set. StreamLanguage
// maps returned token names ("keyword", "number", "comment", ...) onto CodeMirror's
// built-in tag table, so the editor's default theme highlights them with no
// extra style wiring. This keeps the dependency surface to @codemirror/language
// (no @codemirror/legacy-modes clike fork) and gives precise keyword control.
//
// The error gutter is exposed as three testable pieces:
//   - ErrorGutterMarker (a GutterMarker whose DOM carries data-error-line)
//   - errorMarkersForLines (a pure RangeSet builder over a Text doc)
//   - errorLineGutter (the Extension wired into the editor)
// so the line-3 marker behavior can be asserted without depending on
// CodeMirror's viewport-driven gutter painting, which is not deterministic in
// jsdom (line DOM is only emitted for the measured viewport, which is degenerate
// without a real layout engine).
import { StreamLanguage, LanguageSupport, type StringStream } from "@codemirror/language";
import { Extension, RangeSet, Range, Text } from "@codemirror/state";
import { EditorView, GutterMarker, gutter } from "@codemirror/view";

import type { CompileError } from "../ipc";

// ── scad StreamLanguage ─────────────────────────────────────────────────────

const SCAD_KEYWORDS = new Set([
  "module",
  "function",
  "for",
  "if",
  "else",
  "let",
  "use",
  "include",
  "return",
]);

interface ScadState {
  /** True while inside an open block comment (until its closing delimiter). */
  blockComment: boolean;
}

const scadStreamParser = {
  name: "scad",
  startState: (): ScadState => ({ blockComment: false }),
  copyState: (s: ScadState): ScadState => ({ blockComment: s.blockComment }),
  token(stream: StringStream, state: ScadState): string | null {
    // Continue an open block comment until its closing delimiter.
    if (state.blockComment) {
      if (stream.match("*/")) {
        state.blockComment = false;
        return "comment";
      }
      stream.next();
      return "comment";
    }
    if (stream.eatSpace()) return null;
    // line comment
    if (stream.match("//")) {
      stream.skipToEnd();
      return "comment";
    }
    // block comment open
    if (stream.match("/*")) {
      state.blockComment = true;
      return "comment";
    }
    // double-quoted string (single-line; unterminated tolerated)
    if (stream.match(/^"(?:[^"\\]|\\.)*"?/)) return "string";
    // number — decimal, leading-dot, or exponent form
    if (stream.match(/^[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?/) || stream.match(/^\.[0-9]+/)) {
      return "number";
    }
    // special variables: $fn, $fa, $fs, $t, $vpr, $vpt, $vpd, ...
    if (stream.match(/^\$[a-zA-Z_]+/)) return "atom";
    // identifier or keyword (match returns RegExpMatchArray when it consumes)
    const wordMatch = stream.match(/^[a-zA-Z_][a-zA-Z0-9_]*/);
    if (wordMatch) {
      const word = Array.isArray(wordMatch) ? wordMatch[0] : "";
      return SCAD_KEYWORDS.has(word) ? "keyword" : "variable";
    }
    // any other single character (operators / punctuation) — no special class
    stream.next();
    return null;
  },
};

const scadStream = StreamLanguage.define(scadStreamParser);

/** CodeMirror 6 LanguageSupport for `.scad`. */
export function scadLanguage(): LanguageSupport {
  return new LanguageSupport(scadStream);
}

// ── error gutter ────────────────────────────────────────────────────────────

/**
 * Gutter marker for a compile-error line. Renders a small deterministic DOM
 * cell carrying `data-error-line` and `data-error-kind` so a test can assert
 * "the marker for line N exists" without driving CodeMirror's viewport.
 */
export class ErrorGutterMarker extends GutterMarker {
  constructor(
    readonly line: number,
    readonly message: string,
    readonly kind: CompileError["kind"],
  ) {
    super();
  }

  eq(other: ErrorGutterMarker): boolean {
    return this.line === other.line && this.kind === other.kind;
  }

  toDOM(_view: EditorView): HTMLElement {
    const el = document.createElement("div");
    el.className = "cm-scad-error-gutter";
    el.dataset.errorLine = String(this.line);
    el.dataset.errorKind = this.kind;
    el.title = this.message;
    el.textContent = "●";
    return el;
  }
}

/**
 * Build the RangeSet of error gutter markers for a given doc. Pure: takes a
 * Text (the editor document) rather than a live EditorView so it is testable
 * in isolation. One marker per error line (deduped — first error wins).
 */
export function errorMarkersForLines(errors: CompileError[], doc: Text): RangeSet<ErrorGutterMarker> {
  const byLine = new Map<number, CompileError>();
  for (const e of errors) {
    if (e.line >= 1 && e.line <= doc.lines && !byLine.has(e.line)) {
      byLine.set(e.line, e);
    }
  }
  const ranges: Range<ErrorGutterMarker>[] = [];
  for (const e of byLine.values()) {
    ranges.push(new ErrorGutterMarker(e.line, e.message, e.kind).range(doc.line(e.line).from));
  }
  return RangeSet.of(ranges, true);
}

/** Gutter extension rendering `errors` as per-line markers. */
export function errorLineGutter(errors: CompileError[]): Extension {
  return gutter({
    class: "cm-scad-errors",
    markers: (view) => errorMarkersForLines(errors, view.state.doc),
  });
}
