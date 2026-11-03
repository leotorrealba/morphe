// Status bar.
//
// Surfaces compile time, manifold status (when present), and a compiling
// indicator. Pure presentational component — all state is passed in.
import type { GeometryStats } from "../ipc";

export interface StatusBarProps {
  stats: GeometryStats | null;
  compiling: boolean;
  lastMs: number | null;
  errorCount: number;
}

export default function StatusBar({ stats, compiling, lastMs, errorCount }: StatusBarProps) {
  return (
    <footer
      data-testid="status-bar"
      style={{
        display: "flex",
        alignItems: "center",
        gap: "var(--morphe-space-4)",
        padding: "var(--morphe-space-1) var(--morphe-space-3)",
        background: "var(--morphe-surface)",
        color: "var(--morphe-text-dim)",
        borderTop: "1px solid var(--morphe-border)",
        fontFamily: "var(--morphe-font-mono)",
        fontSize: "var(--morphe-font-size-sm)",
        flexShrink: 0,
      }}
    >
      <span data-testid="status-compile">
        {compiling ? "compiling…" : lastMs != null ? `${lastMs}ms` : "ready"}
      </span>
      {stats && (
        <>
          <span>manifold: {stats.is_manifold ? "yes" : "no"}</span>
          <span>tris: {stats.triangle_count}</span>
          <span>verts: {stats.vertex_count}</span>
        </>
      )}
      {errorCount > 0 && (
        <span style={{ color: "var(--morphe-error)" }}>{errorCount} error{errorCount > 1 ? "s" : ""}</span>
      )}
    </footer>
  );
}
