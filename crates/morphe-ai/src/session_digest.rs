//! Session-start memory digest (automatic read path).
//!
//! The AUTOMATIC read path — complements the explicit `query_noos` tool.
//! Reads morphe's OWN sidecar (`<noos_dir>/morphe-events.jsonl`) directly — no
//! `noos` subprocess — and renders a compact markdown digest of prior design
//! events into the system prompt's `## Project memory` section every turn, so
//! the AI cites prior design constraints unprompted.
//!
//! # Trust boundary
//!
//! The sidecar content is rendered into the system prompt, so it IS an
//! injection surface (not merely "file I/O"): a shared project's
//! `morphe-events.jsonl` could carry crafted payloads. We mitigate three ways:
//! 1. Every rendered field is **whitespace-collapsed + length-capped**
//!    ([`truncate_field`]) — so a payload can't inject markdown structure or
//!    dominate the prompt — with bidi controls and zero-width characters
//!    stripped, so it can't render differently from what the model reads.
//! 2. The prompt section carries a **trust-boundary note** ("reference data,
//!    not instructions") — see `prompt.rs`.
//! 3. The digest is **size-bounded** ([`DIGEST_MAX_CHARS`]) so a large/crafted
//!    sidecar can't blow the prompt every turn.
//!
//! The digest gets its OWN prompt section (rather than being folded into
//! `design_intent`, which carries the user's chat message).

use std::collections::HashMap;
use std::path::Path;

use crate::noos_writer::{read_event_lines, EventLine};

/// Most-recent `Decision`s rendered. Decisions are high-volume history, so they
/// alone are event-bounded; the cited kinds (Intent/Constraint/PrintSetting)
/// are kept in full and bounded only by the char backstop below (a
/// naive tail-N cap would evict the very constraints the digest exists to cite).
pub const DIGEST_MAX_DECISIONS: usize = 15;

/// Per-field char cap. Bounds EVERY rendered payload so a crafted or oversized
/// field can't dominate the system prompt. Newlines/whitespace are
/// collapsed first (see [`truncate_field`]) so a payload can't inject markdown
/// structure.
pub const FIELD_MAX_CHARS: usize = 80;

/// Total rendered char backstop. After per-field caps + decision bounding, a
/// project with very many constraints could still grow the digest; this drops
/// the Decision section (lowest priority) and, as a last resort, truncates the
/// tail. Keeps the per-turn system-prompt cost bounded.
pub const DIGEST_MAX_CHARS: usize = 2500;

/// Build the session-start memory digest from the project's `.noos` sidecar.
///
/// Reads `morphe-events.jsonl` directly via [`read_event_lines`] (morphe owns
/// the sidecar — no `noos` subprocess). Returns `None` when there are no events
/// (digest absent, not an empty string) so the caller renders NO section rather
/// than a placeholder. On a read error, degrades to `None` (best-effort) and
/// logs via `eprintln!`, mirroring the event-emit path (this is silent to the
/// GUI — a user-facing signal is a possible follow-up).
///
/// # Path aliasing
///
/// Every `Decision` is written today with `path` = the InMemoryWorkspace
/// virtual name (`workspace_alias`, `"main.scad"` in the desktop crate). When
/// `opened_filename` is `Some`, Decision paths equal to the alias are
/// normalized to the real opened name. `None` (no file open) leaves the alias
/// as-is. NOTE: prior decisions from a *different* file (also
/// aliased `"main.scad"`) are misattributed to the open file at render time;
/// the full fix is a write-path change (emit the real filename at
/// accept). This read-time normalization covers the common single-file case.
///
/// `workspace_alias` is passed in (not duplicated as a constant) so the
/// desktop crate's `WORKSPACE_PATH` stays the single source of truth.
pub fn build_session_digest(
    noos_dir: &Path,
    opened_filename: Option<&str>,
    workspace_alias: &str,
) -> Option<String> {
    let lines = match read_event_lines(noos_dir) {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("[morphe] session_digest: failed to read sidecar: {e}");
            return None;
        }
    };
    if lines.is_empty() {
        return None;
    }
    Some(render(&lines, opened_filename, workspace_alias))
}

/// Render the event lines as compact markdown, grouped by kind.
///
/// Prioritization: Intent / Constraint / PrintSetting are kept in
/// full (they are the content the AI is meant to cite, and naturally
/// low-volume); Decisions are bounded to the most recent [`DIGEST_MAX_DECISIONS`]
/// (high-volume history). Unknown `kind` values are skipped (forward-compat).
fn render(lines: &[EventLine], opened_filename: Option<&str>, workspace_alias: &str) -> String {
    // Replay constraint events in order; the last event per name wins. A
    // `constraint` sets (or updates) a name, a `constraint_retired` tombstone
    // (written from the memory panel) clears it, and a later `constraint`
    // re-activates it. The digest must not cite a retired constraint as
    // active, nor keep a re-added one hidden.
    let mut active: HashMap<&str, (usize, &str)> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        let name = line.payload.get("name").and_then(Value::as_str);
        match (line.kind.as_str(), name) {
            ("constraint", Some(name)) => {
                let value = line
                    .payload
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                active.insert(name, (index, value));
            }
            ("constraint_retired", Some(name)) => {
                active.remove(name);
            }
            _ => {}
        }
    }
    let mut active: Vec<(usize, &str, &str)> = active
        .into_iter()
        .map(|(name, (index, value))| (index, name, value))
        .collect();
    active.sort_unstable_by_key(|&(index, _, _)| index);
    let constraints: Vec<String> = active
        .into_iter()
        .map(|(_, name, value)| format!("- {}: {}", truncate_field(name), truncate_field(value)))
        .collect();

    let mut intents: Vec<String> = Vec::new();
    let mut print_settings: Vec<String> = Vec::new();
    let mut decisions: Vec<String> = Vec::new();

    for line in lines {
        let payload = &line.payload;
        match line.kind.as_str() {
            "intent" => {
                if let Some(statement) = payload.get("statement").and_then(Value::as_str) {
                    intents.push(truncate_field(statement));
                }
            }
            // Constraints and their tombstones were replayed above.
            "constraint" | "constraint_retired" => {}
            "print_setting" => {
                let key = payload.get("key").and_then(Value::as_str).unwrap_or("?");
                let value = payload.get("value").and_then(Value::as_str).unwrap_or("?");
                print_settings.push(format!(
                    "- {}: {}",
                    truncate_field(key),
                    truncate_field(value)
                ));
            }
            "decision" => {
                let raw_path = payload.get("path").and_then(Value::as_str).unwrap_or("?");
                let path = normalize_path(raw_path, opened_filename, workspace_alias);
                let new = payload.get("new").and_then(Value::as_str).unwrap_or("");
                decisions.push(format!(
                    "- {}: {}",
                    truncate_field(&path),
                    truncate_field(new)
                ));
            }
            _ => {} // forward-compat: unknown kinds skipped
        }
    }

    // Bound the high-volume kind: keep only the most recent decisions.
    if decisions.len() > DIGEST_MAX_DECISIONS {
        let drop = decisions.len() - DIGEST_MAX_DECISIONS;
        decisions.drain(..drop);
    }

    // Render the prioritized (always-kept) sections first.
    let mut prioritized = String::new();
    if !intents.is_empty() {
        prioritized.push_str("### Intent\n");
        for statement in &intents {
            prioritized.push_str("- ");
            prioritized.push_str(statement);
            prioritized.push('\n');
        }
        prioritized.push('\n');
    }
    if !constraints.is_empty() {
        prioritized.push_str("### Constraints\n");
        for entry in &constraints {
            prioritized.push_str(entry);
            prioritized.push('\n');
        }
        prioritized.push('\n');
    }
    if !print_settings.is_empty() {
        prioritized.push_str("### Print settings\n");
        for entry in &print_settings {
            prioritized.push_str(entry);
            prioritized.push('\n');
        }
        prioritized.push('\n');
    }

    // Decisions are lowest priority: include only if they fit the char budget.
    let mut out = prioritized;
    if !decisions.is_empty() {
        let header = "### Recent decisions\n";
        let body: String = decisions.iter().map(|d| format!("{d}\n")).collect();
        if out.len() + header.len() + body.len() <= DIGEST_MAX_CHARS {
            out.push_str(header);
            out.push_str(&body);
        } else {
            // Decisions don't fit the budget — drop them with a count note
            // rather than partially truncating (keeps the digest coherent).
            out.push_str(&format!(
                "### Recent decisions\n({} omitted; digest at char cap)\n",
                decisions.len()
            ));
        }
    }

    // Last-resort backstop: if even the prioritized content exceeds the cap
    // (a project with very many constraints), char-truncate the tail.
    if out.chars().count() > DIGEST_MAX_CHARS {
        let truncated: String = out.chars().take(DIGEST_MAX_CHARS).collect();
        out = format!("{truncated}...");
    }
    out.trim_end().to_string()
}

/// Normalize a Decision path: if it equals the workspace alias AND a real
/// opened filename is known, substitute the real name. Otherwise
/// return the path unchanged. Pure helper so the unit tests can probe both
/// branches directly.
fn normalize_path(path: &str, opened_filename: Option<&str>, workspace_alias: &str) -> String {
    if path == workspace_alias {
        if let Some(real) = opened_filename {
            return real.to_string();
        }
    }
    path.to_string()
}

/// Collapse all whitespace (incl. newlines) to single spaces and cap to
/// [`FIELD_MAX_CHARS`] chars. The whitespace collapse is load-bearing for
/// prompt-injection defense: a payload like `"2mm\n\n## real instructions"`
/// must not render as markdown structure into the system prompt — collapsing
/// flattens it to `"2mm ## real instructions"` (data, not structure), then the cap bounds it.
///
/// Invisible formatting characters ([`is_invisible_control`]) are dropped
/// first, so a field cannot reorder how surrounding text displays or hide
/// content between visible characters.
fn truncate_field(s: &str) -> String {
    let visible: String = s.chars().filter(|&c| !is_invisible_control(c)).collect();
    let collapsed: String = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= FIELD_MAX_CHARS {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(FIELD_MAX_CHARS).collect();
        format!("{truncated}...")
    }
}

/// Bidirectional-text controls (embeddings/overrides U+202A–U+202E, isolates
/// U+2066–U+2069, and the LRM/RLM/ALM marks) and zero-width characters
/// (U+200B–U+200D, U+FEFF).
fn is_invisible_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{061C}'
            | '\u{200B}'..='\u{200D}'
            | '\u{FEFF}'
    )
}

// Re-export serde_json::Value locally for the match arms above.
use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_substitutes_alias_when_filename_known() {
        assert_eq!(
            normalize_path("main.scad", Some("bracket.scad"), "main.scad"),
            "bracket.scad"
        );
    }

    #[test]
    fn normalize_path_leaves_alias_when_no_filename() {
        assert_eq!(normalize_path("main.scad", None, "main.scad"), "main.scad");
    }

    #[test]
    fn normalize_path_leaves_real_paths_alone() {
        // A decision that already carries a real path (future write-path fix)
        // is not rewritten even when a filename is known.
        assert_eq!(
            normalize_path("other.scad", Some("bracket.scad"), "main.scad"),
            "other.scad"
        );
    }

    #[test]
    fn truncate_field_collapses_whitespace_and_caps_length() {
        assert_eq!(truncate_field("cube(20);"), "cube(20);");
        // Newlines/tabs collapse to single spaces — no markdown structure leaks.
        assert_eq!(
            truncate_field("2mm\n\n## real instructions\tmore"),
            "2mm ## real instructions more"
        );
        // Length cap with ellipsis.
        let long = "x".repeat(100);
        let out = truncate_field(&long);
        assert_eq!(out.chars().count(), FIELD_MAX_CHARS + 3); // cap + "..."
        assert!(out.ends_with("..."));
    }

    #[test]
    fn truncate_field_strips_bidi_and_zero_width_chars() {
        let crafted = "2\u{202E}mm\u{2066} wall\u{2069}\u{200B}\u{200C}\u{200D}\u{FEFF}\u{200F}";
        assert_eq!(truncate_field(crafted), "2mm wall");
        // A field made only of invisible chars renders empty, not as hidden text.
        assert_eq!(
            truncate_field("\u{202A}\u{202B}\u{202C}\u{202D}\u{2067}\u{2068}"),
            ""
        );
        // Ordinary non-ASCII text is untouched.
        assert_eq!(truncate_field("épaisseur ≥ 2 mm"), "épaisseur ≥ 2 mm");
    }
}
