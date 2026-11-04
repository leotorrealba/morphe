// Top toolbar.
//
// Holds the open + export affordances (buttons; ⌘E/Ctrl+E for export is
// handled in App) and the theme toggle. Pure presentational component.
//
// The Open button: opening a `.scad` file is the user action that
// activates design-memory (the backend records the opened disk path in
// AppState so ai_accept_edit can derive the per-project `.noos/` dir). The
// opened filename is shown next to the brand mark as a minimal "what's loaded"
// affordance — no full file-management UX (recent files, save-back, etc.).
import type { Theme } from "../hooks/useTheme";
import morpheMark from "../assets/morphe-mark.png";

export interface ToolbarProps {
  onExport: () => void;
  /** Invoked when the user clicks Open. App.tsx wires this to `openFile` and
   * records the opened disk path so the backend can derive the `.noos/` dir
   * for memory writes. */
  onOpen: () => void;
  /** Invoked when the user clicks Save (⌘S/Ctrl+S is handled in App). Writes
   * back to the opened file, or asks for a destination for a new buffer. */
  onSave?: () => void;
  /** True when the editor holds changes that are not saved to disk. */
  dirty?: boolean;
  /** The basename of the opened file, or `null` when the editor holds an
   * in-memory buffer (no file open). Shown next to the brand mark as a minimal
   * "what's loaded" cue. */
  openedFilename: string | null;
  theme: Theme;
  onToggleTheme: () => void;
}

export default function Toolbar({
  onExport,
  onOpen,
  onSave,
  dirty = false,
  openedFilename,
  theme,
  onToggleTheme,
}: ToolbarProps) {
  return (
    <header
      data-testid="toolbar"
      style={{
        display: "flex",
        alignItems: "center",
        gap: "var(--morphe-space-3)",
        padding: "var(--morphe-space-2) var(--morphe-space-4)",
        background: "var(--morphe-surface)",
        borderBottom: "1px solid var(--morphe-border)",
        flexShrink: 0,
      }}
    >
      <img
        src={morpheMark}
        alt=""
        aria-hidden="true"
        data-testid="toolbar-mark"
        width={24}
        height={24}
        style={{ display: "block" }}
      />
      <span style={{ fontWeight: 600, color: "var(--morphe-accent)", letterSpacing: "0.04em" }}>
        morphe
      </span>
      {openedFilename !== null && (
        <span
          data-testid="toolbar-opened-filename"
          style={{
            color: "var(--morphe-text-dim)",
            fontFamily: "var(--morphe-font-mono)",
            fontSize: "var(--morphe-font-size-sm)",
          }}
        >
          {openedFilename}
          {dirty && (
            <span data-testid="toolbar-dirty" title="Unsaved changes" aria-label="Unsaved changes">
              {" ●"}
            </span>
          )}
        </span>
      )}
      <button
        type="button"
        onClick={onOpen}
        data-testid="open-button"
        style={btnStyle}
      >
        Open…
      </button>
      {onSave && (
        <button type="button" onClick={onSave} data-testid="save-button" style={btnStyle}>
          Save ⌘S
        </button>
      )}
      <div style={{ flex: 1 }} />
      <button
        type="button"
        onClick={onExport}
        data-testid="export-button"
        style={btnStyle}
      >
        Export STL ⌘E
      </button>
      <button
        type="button"
        onClick={onToggleTheme}
        data-testid="theme-toggle"
        aria-label="Toggle theme"
        title={theme === "dark" ? "Switch to light" : "Switch to dark"}
        style={btnStyle}
      >
        {theme === "dark" ? "☀" : "☾"}
      </button>
    </header>
  );
}

const btnStyle: React.CSSProperties = {
  background: "var(--morphe-surface-2)",
  color: "var(--morphe-text)",
  border: "1px solid var(--morphe-border)",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-1) var(--morphe-space-3)",
  fontFamily: "var(--morphe-font)",
  fontSize: "var(--morphe-font-size)",
  cursor: "pointer",
};
