//! The six morphe tool schemas as serde types.
//!
//! Each tool is a [`ToolDef`] — `{name, description, input_schema}` — matching
//! the Anthropic Messages-API `tools` wire shape exactly. [`tool_definitions`]
//! returns the full list for the `tools` request field.
//!
//! Descriptions are **prescriptive**: they state *when* to call the tool, not
//! just what it does (Opus 4.8 / Sonnet 5 reach for tools more conservatively,
//! so trigger conditions in the description give measurable lift). The
//! str_replace match-count validation (0 or >1 → error) is the dispatcher's
//! job; the schema here only declares the fields.
//!
//! The dispatcher wires `compile` / `get_geometry_stats` to
//! morphe-eval/morphe-kernel; `query_noos` shells to `noos query` against the
//! project's design-intent memory; `screenshot_viewport` stubs its result.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A single tool definition for the Messages-API `tools` field.
///
/// Serializes to `{"name","description","input_schema"}` — the exact shape
/// Anthropic expects. `input_schema` is held as a `serde_json::Value` (a JSON
/// Schema object) so the schema is expressed directly rather than via a
/// schema-builder DSL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// The six morphe tools, in canonical order, for the `tools` request field.
///
/// Order matters for prompt-cache stability (tools render at position 0; a
/// reordered list invalidates the cache). Keep this order stable across
/// requests within a session.
#[must_use]
pub fn tool_definitions() -> Vec<ToolDef> {
    vec![
        read_code(),
        edit_code(),
        compile(),
        get_geometry_stats(),
        screenshot_viewport(),
        query_noos(),
    ]
}

/// `read_code` — read a `.scad` source file from the agent's workspace.
fn read_code() -> ToolDef {
    ToolDef {
        name: "read_code".to_string(),
        description: "Read a .scad source file from the agent's workspace. Call this BEFORE editing a file whenever you need to inspect its current contents, or to understand an existing geometry definition before modifying it. Returns the file's text.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path to the .scad file to read."
                }
            },
            "required": ["path"]
        }),
    }
}

/// `edit_code` — patch-based string replacement in a `.scad` file.
fn edit_code() -> ToolDef {
    ToolDef {
        name: "edit_code".to_string(),
        description: "Apply a single string replacement to a .scad source file (str_replace semantics). Call this to make a targeted edit; `old` must appear exactly once in the file (0 or >1 matches is an error — widen `old` to disambiguate). Do NOT use this to create a file; use it only to change existing text.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path to the .scad file to edit."
                },
                "old": {
                    "type": "string",
                    "description": "The exact text to replace. Must match exactly one occurrence in the file."
                },
                "new": {
                    "type": "string",
                    "description": "The replacement text."
                }
            },
            "required": ["path", "old", "new"]
        }),
    }
}

/// `compile` — compile morphe/OpenSCAD source to a mesh + stats + errors.
fn compile() -> ToolDef {
    ToolDef {
        name: "compile".to_string(),
        description: "Compile morphe/OpenSCAD source into a mesh and return geometry stats plus any compile/eval errors. Call this after editing code to verify the program parses and evaluates, and to inspect the resulting geometry (triangle/vertex counts, bounding box, volume). Returns structured stats and an error list — do not assume success.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "The full morphe/OpenSCAD source to compile."
                }
            },
            "required": ["source"]
        }),
    }
}

/// `get_geometry_stats` — measure a source's geometry without full export.
fn get_geometry_stats() -> ToolDef {
    ToolDef {
        name: "get_geometry_stats".to_string(),
        description: "Evaluate morphe/OpenSCAD source and return geometry statistics only (bounding box, volume, surface area, triangle/vertex counts, manifoldness) — cheaper than a full compile when you only need to check sizes or verify the body is valid. Call this when you need to reason about dimensions or validate geometry without exporting a mesh.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "The full morphe/OpenSCAD source to measure."
                }
            },
            "required": ["source"]
        }),
    }
}

/// `screenshot_viewport` — capture the current viewport (currently a stub).
fn screenshot_viewport() -> ToolDef {
    ToolDef {
        name: "screenshot_viewport".to_string(),
        description: "Capture a screenshot of the current 3D viewport. Call this when you need to visually verify a geometry change, inspect the rendered model, or debug a visual issue. Takes no parameters.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

/// `query_noos` — query the project's design-intent memory (noos).
fn query_noos() -> ToolDef {
    ToolDef {
        name: "query_noos".to_string(),
        description: "Query this project's design-intent memory (the noos knowledge base): past design decisions, constraints, print settings, and rationale recorded for the open project. Call this when the user references past work or design rationale, or when you need project context that is not in the source files. Returns ranked, token-bounded matches wrapped as untrusted data, or a short note when there are no hits or no project file is open.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language query describing the design intent or context to retrieve."
                }
            },
            "required": ["query"]
        }),
    }
}

#[cfg(test)]
mod tests {
    //! Golden serialization tests — each tool definition must serialize to the
    //! exact JSON Anthropic expects: `{"name","description","input_schema"}`.
    //! If a description or schema changes, the test's recorded expected value
    //! must be updated consciously (this is the golden-test discipline).

    use super::*;

    #[test]
    fn tool_definitions_returns_six_tools_in_canonical_order() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "read_code",
                "edit_code",
                "compile",
                "get_geometry_stats",
                "screenshot_viewport",
                "query_noos"
            ]
        );
    }

    #[test]
    fn read_code_serializes_to_expected_json() {
        let v = serde_json::to_value(read_code()).unwrap();
        let expected = json!({
            "name": "read_code",
            "description": "Read a .scad source file from the agent's workspace. Call this BEFORE editing a file whenever you need to inspect its current contents, or to understand an existing geometry definition before modifying it. Returns the file's text.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path to the .scad file to read."
                    }
                },
                "required": ["path"]
            }
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn edit_code_serializes_to_expected_json() {
        let v = serde_json::to_value(edit_code()).unwrap();
        let expected = json!({
            "name": "edit_code",
            "description": "Apply a single string replacement to a .scad source file (str_replace semantics). Call this to make a targeted edit; `old` must appear exactly once in the file (0 or >1 matches is an error — widen `old` to disambiguate). Do NOT use this to create a file; use it only to change existing text.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path to the .scad file to edit."
                    },
                    "old": {
                        "type": "string",
                        "description": "The exact text to replace. Must match exactly one occurrence in the file."
                    },
                    "new": {
                        "type": "string",
                        "description": "The replacement text."
                    }
                },
                "required": ["path", "old", "new"]
            }
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn compile_serializes_to_expected_json() {
        let v = serde_json::to_value(compile()).unwrap();
        let expected = json!({
            "name": "compile",
            "description": "Compile morphe/OpenSCAD source into a mesh and return geometry stats plus any compile/eval errors. Call this after editing code to verify the program parses and evaluates, and to inspect the resulting geometry (triangle/vertex counts, bounding box, volume). Returns structured stats and an error list — do not assume success.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "The full morphe/OpenSCAD source to compile."
                    }
                },
                "required": ["source"]
            }
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn get_geometry_stats_serializes_to_expected_json() {
        let v = serde_json::to_value(get_geometry_stats()).unwrap();
        let expected = json!({
            "name": "get_geometry_stats",
            "description": "Evaluate morphe/OpenSCAD source and return geometry statistics only (bounding box, volume, surface area, triangle/vertex counts, manifoldness) — cheaper than a full compile when you only need to check sizes or verify the body is valid. Call this when you need to reason about dimensions or validate geometry without exporting a mesh.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "The full morphe/OpenSCAD source to measure."
                    }
                },
                "required": ["source"]
            }
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn screenshot_viewport_serializes_to_expected_json() {
        let v = serde_json::to_value(screenshot_viewport()).unwrap();
        let expected = json!({
            "name": "screenshot_viewport",
            "description": "Capture a screenshot of the current 3D viewport. Call this when you need to visually verify a geometry change, inspect the rendered model, or debug a visual issue. Takes no parameters.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn query_noos_serializes_to_expected_json() {
        let v = serde_json::to_value(query_noos()).unwrap();
        let expected = json!({
            "name": "query_noos",
            "description": "Query this project's design-intent memory (the noos knowledge base): past design decisions, constraints, print settings, and rationale recorded for the open project. Call this when the user references past work or design rationale, or when you need project context that is not in the source files. Returns ranked, token-bounded matches wrapped as untrusted data, or a short note when there are no hits or no project file is open.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language query describing the design intent or context to retrieve."
                    }
                },
                "required": ["query"]
            }
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn tool_definitions_round_trips_through_serde() {
        // The whole list must round-trip unchanged (catches any serialization
        // drift across the set, not just per-tool).
        let tools = tool_definitions();
        let json = serde_json::to_string(&tools).unwrap();
        let back: Vec<ToolDef> = serde_json::from_str(&json).unwrap();
        assert_eq!(tools, back);
    }
}
