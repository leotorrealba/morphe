// Minimal toast.
//
// A single, auto-dismissing toast for export success/failure. Kept deliberately
// small — no queue, no portal. The component auto-dismisses 3s after the
// current message appears; the parent owns the message and the dismiss call.
import { useEffect } from "react";

export interface ToastMsg {
  kind: "success" | "error";
  message: string;
}

export interface ToastProps {
  toast: ToastMsg | null;
  onDismiss: () => void;
}

export default function Toast({ toast, onDismiss }: ToastProps) {
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(onDismiss, 3000);
    return () => clearTimeout(t);
  }, [toast, onDismiss]);

  if (!toast) return null;
  return (
    <div
      role="status"
      data-testid="toast"
      data-kind={toast.kind}
      style={{
        position: "fixed",
        bottom: "var(--morphe-space-5)",
        left: "50%",
        transform: "translateX(-50%)",
        background: "var(--morphe-surface-2)",
        color: "var(--morphe-text)",
        border: `1px solid ${toast.kind === "success" ? "var(--morphe-success)" : "var(--morphe-error)"}`,
        borderRadius: "var(--morphe-radius)",
        padding: "var(--morphe-space-2) var(--morphe-space-4)",
        boxShadow: "var(--morphe-shadow)",
        maxWidth: "70vw",
      }}
    >
      {toast.message}
    </div>
  );
}
