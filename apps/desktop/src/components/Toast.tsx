// Minimal toast.
//
// A single toast for open/save/export outcomes. Kept deliberately small — no
// queue, no portal; the parent owns the message and the dismiss call.
//
// Accessibility:
// - Errors use `role="alert"` (assertive: announced immediately); successes
//   use `role="status"` (polite).
// - Successes auto-dismiss after 4 s, errors after 10 s, so there's time to
//   read them; hovering or focusing the toast pauses the timer, and Escape or
//   the close button dismisses it.
import { useEffect, useRef, useState } from "react";

export interface ToastMsg {
  kind: "success" | "error";
  message: string;
}

export interface ToastProps {
  toast: ToastMsg | null;
  onDismiss: () => void;
}

/** Auto-dismiss delay per kind (ms). Exported for tests. */
export const TOAST_DURATION_MS: Record<ToastMsg["kind"], number> = {
  success: 4000,
  error: 10000,
};

export default function Toast({ toast, onDismiss }: ToastProps) {
  // Hover/focus pauses auto-dismiss (WCAG 2.2.1, timing adjustable).
  const [paused, setPaused] = useState(false);
  // The parent passes a fresh `onDismiss` on every render (e.g. each keystroke
  // in the editor); reading it through a ref keeps those re-renders from
  // restarting the timer.
  const dismissRef = useRef(onDismiss);
  dismissRef.current = onDismiss;

  useEffect(() => {
    if (!toast || paused) return;
    const t = setTimeout(() => dismissRef.current(), TOAST_DURATION_MS[toast.kind]);
    return () => clearTimeout(t);
  }, [toast, paused]);

  // A new message starts unpaused.
  useEffect(() => setPaused(false), [toast]);

  useEffect(() => {
    if (!toast) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") dismissRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toast]);

  if (!toast) return null;
  const isError = toast.kind === "error";
  return (
    <div
      role={isError ? "alert" : "status"}
      aria-live={isError ? "assertive" : "polite"}
      data-testid="toast"
      data-kind={toast.kind}
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onFocus={() => setPaused(true)}
      onBlur={() => setPaused(false)}
      style={{
        position: "fixed",
        bottom: "var(--morphe-space-5)",
        left: "50%",
        transform: "translateX(-50%)",
        display: "flex",
        alignItems: "center",
        gap: "var(--morphe-space-3)",
        background: "var(--morphe-surface-2)",
        color: "var(--morphe-text)",
        border: `1px solid ${isError ? "var(--morphe-error)" : "var(--morphe-success)"}`,
        borderRadius: "var(--morphe-radius)",
        padding: "var(--morphe-space-2) var(--morphe-space-4)",
        boxShadow: "var(--morphe-shadow)",
        maxWidth: "70vw",
      }}
    >
      <span>{toast.message}</span>
      <button
        type="button"
        data-testid="toast-dismiss"
        aria-label="Dismiss notification"
        onClick={onDismiss}
        style={{
          background: "none",
          border: "none",
          color: "var(--morphe-text-dim)",
          cursor: "pointer",
          fontSize: "var(--morphe-font-size)",
          padding: 0,
        }}
      >
        ×
      </button>
    </div>
  );
}
