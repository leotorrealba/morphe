// App shell wiring.
//
// Holds the live `source` state and threads it through the debounced
// recompile hook (→ mesh/errors/stats) into the Editor and Viewport. The
// toolbar exposes open + export (⌘E / Ctrl+E) + theme toggle; the status bar
// reports compile time + manifold status; the toast surfaces export/open
// outcomes. The export destination is chosen entirely inside the backend
// command (the native save dialog) — no frontend path reaches the STL writer.
//
// Opening a `.scad` file via the Open button activates design-memory
// — the backend records the opened disk path in AppState so ai_accept_edit
// can derive the per-project `.noos/` dir for memory writes. The frontend
// tracks the opened path in `openedPath` (for the toolbar filename display +
// to drive the open flow); the backend's `opened_path` is the source of
// truth for the noos-dir derivation.
//
// Save (⌘S / Ctrl+S, toolbar button) writes the buffer back to the opened
// file — or asks for a destination for a new buffer — and ⇧⌘S is Save As.
// `savedSource` tracks what is on disk so the toolbar can flag unsaved
// changes, and opening another file asks before discarding them.
import { useCallback, useEffect, useMemo, useState } from "react";

import Editor from "./components/Editor";
import Viewport from "./components/Viewport";
import ChatPane from "./components/ChatPane";
import MemoryPanel from "./components/MemoryPanel";
import StatusBar from "./components/StatusBar";
import Toolbar from "./components/Toolbar";
import Toast, { type ToastMsg } from "./components/Toast";
import { exportStl, openFile, saveFile } from "./ipc";
import { runExport, type ExportDeps } from "./export";
import { useDebouncedRecompile } from "./hooks/useDebouncedRecompile";
import { useTheme } from "./hooks/useTheme";

const DEFAULT_SOURCE = "cube([10, 10, 10]);\n";

/** Real export deps: the backend command opens its own OS save dialog. */
const REAL_EXPORT_DEPS: ExportDeps = {
  exportStl,
};

/** The basename of `path` (the last segment), or `null` if `path` is null/empty.
 * Used for the toolbar's "what's loaded" cue — display only, never crosses IPC. */
function basename(path: string | null): string | null {
  if (!path) return null;
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] || null;
}

/** Ask before discarding unsaved changes. `window.confirm` may be missing in
 * some webviews/test environments — fall back to allowing the action. */
function confirmDiscard(): boolean {
  try {
    return typeof window.confirm === "function"
      ? window.confirm("You have unsaved changes. Discard them and open another file?")
      : true;
  } catch {
    return true;
  }
}

export default function App() {
  const [source, setSource] = useState(DEFAULT_SOURCE);
  // The disk path of the file the user opened (null = in-memory buffer). The
  // backend mirrors this in AppState.opened_path via open_file_cmd; the
  // frontend copy drives the toolbar filename display. Kept in sync on every
  // successful open.
  const [openedPath, setOpenedPath] = useState<string | null>(null);
  // The source as last loaded from / written to disk; `source !== savedSource`
  // means there are unsaved changes.
  const [savedSource, setSavedSource] = useState(DEFAULT_SOURCE);
  const dirty = source !== savedSource;
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const { theme, toggle } = useTheme();
  const { mesh, errors, stats, compiling, lastMs } = useDebouncedRecompile(source);

  const doExport = useCallback(async () => {
    const res = await runExport(source, REAL_EXPORT_DEPS);
    if (!res) return; // user cancelled the save dialog — stay quiet
    setToast(
      res.ok
        ? { kind: "success", message: res.message }
        : { kind: "error", message: res.message },
    );
  }, [source]);

  const doOpen = useCallback(async () => {
    // openFile surfaces user-cancel as a rejecting FileError; the toast is the
    // existing error idiom. On success: load the source into the editor AND
    // record the opened disk path so the toolbar shows it + the backend's
    // AppState.opened_path is set server-side (so ai_accept_edit can derive
    // the per-project `.noos/` dir).
    if (dirty && !confirmDiscard()) return;
    try {
      const result = await openFile();
      setSource(result.source);
      setSavedSource(result.source);
      setOpenedPath(result.path);
    } catch (e) {
      const err = e as { kind?: string; message?: string };
      // `kind === "cancelled"` is the user closing the dialog — stay quiet,
      // matching the save-dialog cancel idiom in doExport.
      if (err?.kind === "cancelled") return;
      const message = err?.message ?? "Open failed";
      setToast({ kind: "error", message });
    }
  }, [dirty]);

  const doSave = useCallback(
    async (saveAs: boolean) => {
      const snapshot = source;
      try {
        const result = await saveFile(snapshot, saveAs);
        setSavedSource(snapshot);
        setOpenedPath(result.path);
        setToast({ kind: "success", message: `Saved ${basename(result.path) ?? result.path}` });
      } catch (e) {
        const err = e as { kind?: string; message?: string };
        if (err?.kind === "cancelled") return;
        setToast({ kind: "error", message: err?.message ?? "Save failed" });
      }
    },
    [source],
  );

  // ⌘E (mac) / Ctrl+E → export.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const key = e.key.toLowerCase();
      if (key === "e") {
        e.preventDefault();
        void doExport();
      } else if (key === "s") {
        // ⌘S → Save (write back to the opened file); ⇧⌘S → Save As.
        e.preventDefault();
        void doSave(e.shiftKey);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [doExport, doSave]);

  const layout = useMemo<React.CSSProperties>(
    () => ({
      display: "flex",
      flexDirection: "column",
      height: "100vh",
      overflow: "hidden",
      background: "var(--morphe-bg)",
    }),
    [],
  );
  const paneRow: React.CSSProperties = {
    display: "flex",
    flex: 1,
    minHeight: 0,
    overflow: "hidden",
  };
  const editorPane: React.CSSProperties = {
    flex: 1,
    minWidth: 0,
    borderRight: "1px solid var(--morphe-border)",
    background: "var(--morphe-surface)",
  };
  const viewportPane: React.CSSProperties = {
    flex: 1,
    minWidth: 0,
    background: "var(--morphe-bg)",
  };
  const chatPane: React.CSSProperties = {
    width: "360px",
    flexShrink: 0,
    minWidth: 0,
  };

  return (
    <main data-testid="app" style={layout}>
      <Toolbar
        onExport={doExport}
        onOpen={doOpen}
        onSave={() => void doSave(false)}
        dirty={dirty}
        openedFilename={basename(openedPath) ?? (dirty ? "untitled.scad" : null)}
        theme={theme}
        onToggleTheme={toggle}
      />
      <div style={paneRow}>
        <div style={editorPane}>
          <Editor source={source} onSourceChange={setSource} errors={errors} theme={theme} />
        </div>
        <div style={viewportPane}>
          <Viewport mesh={mesh} />
        </div>
        <div style={chatPane}>
          <ChatPane source={source} onSourceChange={setSource} />
        </div>
      </div>
      <MemoryPanel openedPath={openedPath} />
      <StatusBar
        stats={stats}
        compiling={compiling}
        lastMs={lastMs}
        errorCount={errors.length}
      />
      <Toast toast={toast} onDismiss={() => setToast(null)} />
    </main>
  );
}
