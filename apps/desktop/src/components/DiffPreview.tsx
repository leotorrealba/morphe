// edit_code diff preview with accept/reject.
//
// Renders an `edit_code` patch (`{ path, old, new }`) as an inline diff (old
// lines red, new lines green) with Accept + Reject buttons.
//
// Accept applies the patch to the CURRENT editor buffer (read through
// `getSource` when provided, so two quick accepts compose instead of the
// second overwriting the first with a stale render-time `source`), and only
// when the patch still applies unambiguously: the `old` text must occur
// EXACTLY once (overlapping occurrences counted) — the same rule the backend
// dispatcher enforced against the agent's copy. If the user edited the buffer
// since, or an earlier edit this one depends on was not accepted, the accept
// is refused with a visible message instead of landing at a different match
// or silently doing nothing. Only after the buffer change is applied does it
// call the `acceptEdit` IPC, which records the decision in project memory —
// so memory never records an edit that was not applied.
//
// Reject calls the `rejectEdit` IPC (records the tool-use id so the bridge's
// final-source reconstruction skips it) and leaves the buffer
// untouched. IPC failures are surfaced, and the buttons are disabled while a
// call is in flight.
//
// The patch insertion uses a function replacer `source.replace(old, () => new)`
// so `new` is inserted LITERALLY (JS `$&`/`$$`/`` $` ``/`$'` are not
// expanded), mirroring the backend dispatcher's literal `push_str(new)`
// exactly.
import { useState } from "react";

import { acceptEdit, rejectEdit, type EditPatch } from "../ai";

export interface DiffPreviewProps {
  /** The patch to render + act on. */
  patch: EditPatch;
  /** The tool-use id of this `edit_code` call — passed to `rejectEdit` so the
   * backend can skip it when reconstructing the final source. */
  id: string;
  /** The editor source at render time (fallback when `getSource` is absent). */
  source: string;
  /** Reads the LATEST editor source at click time (preferred over `source`). */
  getSource?: () => string;
  /** Applied old→new on accept; triggers the live recompile in the parent. */
  onSourceChange: (s: string) => void;
}

/** Count occurrences of `needle` in `haystack`, INCLUDING overlapping ones
 * ("aa" occurs twice in "aaa"), so an ambiguous patch is never treated as
 * unique. An empty needle never applies. */
export function countOccurrences(haystack: string, needle: string): number {
  if (needle.length === 0) return 0;
  let count = 0;
  let from = 0;
  for (;;) {
    const at = haystack.indexOf(needle, from);
    if (at < 0) return count;
    count += 1;
    from = at + 1;
  }
}

type Status = "pending" | "busy" | "accepted" | "rejected";

function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  return String(err);
}

export default function DiffPreview({ patch, id, source, getSource, onSourceChange }: DiffPreviewProps) {
  const [status, setStatus] = useState<Status>("pending");
  const [error, setError] = useState<string | null>(null);

  const onAccept = async () => {
    if (status !== "pending") return;
    const current = getSource ? getSource() : source;
    const matches = countOccurrences(current, patch.old);
    if (matches !== 1) {
      setError(
        matches === 0
          ? "This edit no longer applies: the code it changes is not in the editor (it was modified, or depends on an edit you did not accept)."
          : `This edit is ambiguous now: the code it changes appears ${matches} times in the editor.`,
      );
      return;
    }
    setError(null);
    setStatus("busy");
    // `String.replace` with a string pattern replaces the FIRST (here: the
    // only) match; the function replacer keeps `new` literal.
    onSourceChange(current.replace(patch.old, () => patch.new));
    setStatus("accepted");
    try {
      await acceptEdit(patch);
    } catch (err) {
      setError(`Applied, but recording it in project memory failed: ${errorMessage(err)}`);
    }
  };

  const onReject = async () => {
    if (status !== "pending") return;
    setError(null);
    setStatus("busy");
    try {
      await rejectEdit(patch, id);
      setStatus("rejected");
    } catch (err) {
      setStatus("pending");
      setError(`Reject failed: ${errorMessage(err)}`);
    }
  };

  return (
    <div
      data-testid="diff-preview"
      data-status={status}
      style={{
        margin: "var(--morphe-space-2) 0",
        border: "1px solid var(--morphe-border)",
        borderRadius: "var(--morphe-radius-sm)",
        overflow: "hidden",
        fontFamily: "var(--morphe-font-mono)",
        fontSize: "var(--morphe-font-size-sm)",
      }}
    >
      <div
        style={{
          padding: "var(--morphe-space-1) var(--morphe-space-2)",
          background: "var(--morphe-surface-2)",
          color: "var(--morphe-text-dim)",
          borderBottom: "1px solid var(--morphe-border)",
        }}
      >
        edit · {patch.path}
      </div>
      <pre data-testid="diff-old" style={removedBlockStyle}>
        {patch.old
          .split("\n")
          .map((l, i) => (
            <span key={i} style={removedLineStyle}>
              {"− " + l + "\n"}
            </span>
          ))}
      </pre>
      <pre data-testid="diff-new" style={addedBlockStyle}>
        {patch.new
          .split("\n")
          .map((l, i) => (
            <span key={i} style={addedLineStyle}>
              {"+ " + l + "\n"}
            </span>
          ))}
      </pre>
      {(status === "pending" || status === "busy") && (
        <div style={{ display: "flex", gap: "var(--morphe-space-2)", padding: "var(--morphe-space-2)" }}>
          <button
            type="button"
            data-testid="diff-accept"
            onClick={onAccept}
            disabled={status === "busy"}
            style={acceptBtnStyle}
          >
            Accept
          </button>
          <button
            type="button"
            data-testid="diff-reject"
            onClick={onReject}
            disabled={status === "busy"}
            style={rejectBtnStyle}
          >
            Reject
          </button>
        </div>
      )}
      {error !== null && (
        <div data-testid="diff-error" role="alert" style={errorStyle}>
          {error}
        </div>
      )}
    </div>
  );
}

const removedBlockStyle: React.CSSProperties = {
  margin: 0,
  padding: "var(--morphe-space-1) var(--morphe-space-2)",
  background: "rgba(229, 112, 122, 0.08)",
  whiteSpace: "pre-wrap",
  wordBreak: "break-word",
};
const addedBlockStyle: React.CSSProperties = {
  margin: 0,
  padding: "var(--morphe-space-1) var(--morphe-space-2)",
  background: "rgba(126, 196, 126, 0.08)",
  whiteSpace: "pre-wrap",
  wordBreak: "break-word",
};
const errorStyle: React.CSSProperties = {
  padding: "var(--morphe-space-1) var(--morphe-space-2)",
  color: "var(--morphe-error)",
  fontFamily: "var(--morphe-font)",
};
const removedLineStyle: React.CSSProperties = { color: "var(--morphe-error)" };
const addedLineStyle: React.CSSProperties = { color: "var(--morphe-success)" };
const acceptBtnStyle: React.CSSProperties = {
  background: "var(--morphe-success)",
  color: "#1a1a1c",
  border: "none",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-1) var(--morphe-space-3)",
  cursor: "pointer",
  fontWeight: 600,
};
const rejectBtnStyle: React.CSSProperties = {
  background: "var(--morphe-surface-2)",
  color: "var(--morphe-text)",
  border: "1px solid var(--morphe-border)",
  borderRadius: "var(--morphe-radius-sm)",
  padding: "var(--morphe-space-1) var(--morphe-space-3)",
  cursor: "pointer",
};
