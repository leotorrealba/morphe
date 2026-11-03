// CodeMirror 6 editor pane for `.scad` source.
//
// Wraps @uiw/react-codemirror with the scad StreamLanguage highlighter (see
// ./scad.ts) and the error gutter driven by the `errors` prop. Recompile,
// debounce and export live elsewhere (useDebouncedRecompile, App).
import { useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";

import type { CompileError } from "../ipc";
import { errorLineGutter, scadLanguage } from "./scad";

export interface EditorProps {
  source: string;
  onSourceChange: (s: string) => void;
  errors: CompileError[];
  /** The app theme (dark/light) — drives CodeMirror's theme so the editor
   * background matches the app. Without it CodeMirror defaults to its light
   * theme (white bg), unreadable inside morphe's dark app. */
  theme: "dark" | "light";
}

export default function Editor({ source, onSourceChange, errors, theme }: EditorProps) {
  const extensions = useMemo(
    () => [scadLanguage(), errorLineGutter(errors), EditorView.lineWrapping],
    [errors],
  );

  return (
    <CodeMirror
      value={source}
      onChange={onSourceChange}
      extensions={extensions}
      theme={theme}
      data-testid="scad-editor"
    />
  );
}
