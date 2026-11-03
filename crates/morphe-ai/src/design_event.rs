//! Design-event vocabulary for morphe.
//!
//! Self-describing, internally-tagged serde shape for the design events morphe
//! records as the user and agent work on a project: intent statements, accepted
//! decisions, dimension constraints, and slicer print settings. This is
//! morphe's INTERNAL typed event model — the noos bridge ([`crate::noos_writer`]) and
//! the jsonl fallback queue ([`crate::event_queue`]) translate and consume at the boundary.
//!
//! Every event serializes to a JSON object tagged on a `kind` discriminant
//! whose value is the snake_case variant name, matching the existing IPC enum
//! convention (`AiEvent`, `ExportError`). For example:
//!
//! ```json
//! {"kind": "intent", "statement": "..."}
//! {"kind": "decision", "path": "...", "old": "...", "new": "..."}
//! {"kind": "constraint", "name": "wall_thickness", "value": "2mm"}
//! {"kind": "print_setting", "key": "layer_height", "value": "0.2mm"}
//! ```
//!
//! Payloads are deliberately minimal — no id/timestamp/provenance fields.
//! Those are added by the noos writer at write time, where the noos
//! node-identity model is known.

use serde::{Deserialize, Serialize};

/// A typed design event recorded by morphe. Serialized as an internally-tagged
/// JSON object via `#[serde(tag = "kind", rename_all = "snake_case")]`, so the
/// `kind` discriminant is a peer key alongside each variant's payload — the
/// wire shape the noos bridge and the jsonl fallback queue
/// consume line-by-line. See the [module docs](self) for each variant's shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesignEvent {
    /// A natural-language statement of design intent — what the user wants this
    /// part to achieve or satisfy. The agent may quote the user or paraphrase.
    Intent {
        /// The intent statement itself.
        statement: String,
    },
    /// A decision accepted into the source: the `edit_code` str_replace the
    /// agent proposed and the user accepted (or the agent applied unattended).
    /// For str_replace the accepted-edit request and the applied patch coincide,
    /// so a single `{path, old, new}` triple captures both — mirroring the
    /// `edit_code` tool input shape (`AiEvent::ToolCall` payload in `ai.rs`).
    Decision {
        /// Workspace-relative path of the edited `.scad` file.
        path: String,
        /// The exact text replaced.
        old: String,
        /// The replacement text.
        new: String,
    },
    /// A named dimension or physical constraint on the part
    /// (e.g. `wall_thickness = 2mm`). `value` is a string so units ride along
    /// cleanly; structured parsing is the consumer's job.
    Constraint {
        /// Constraint name (e.g. `wall_thickness`).
        name: String,
        /// Constraint value, units included (e.g. `2mm`).
        value: String,
    },
    /// A slicer print setting (e.g. `layer_height = 0.2mm`). `value` is a string
    /// so units ride along cleanly; structured parsing is the consumer's job.
    PrintSetting {
        /// Slicer setting key (e.g. `layer_height`).
        key: String,
        /// Setting value, units included (e.g. `0.2mm`).
        value: String,
    },
    /// A constraint retired from the active set. Retirement is a
    /// tombstone, not a delete — the prior [`Constraint`](Self::Constraint)
    /// stays in history (the digest/panel show both); `name` correlates to the
    /// retired constraint so consumers stop treating it as active. Emitted from
    /// the memory panel via the normal event write path.
    ConstraintRetired {
        /// The name of the constraint being retired (matches a prior
        /// `Constraint { name }`).
        name: String,
    },
}
