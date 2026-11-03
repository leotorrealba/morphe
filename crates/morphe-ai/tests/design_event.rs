//! Design-event vocabulary round-trip tests.
//!
//! Pins the [`morphe_ai::DesignEvent`] enum's serde shape — the self-describing
//! wire format the noos bridge, the jsonl fallback queue, the session
//! digest, and the memory panel all consume.
//! Each variant must round-trip through `serde_json` with byte-for-byte equality,
//! and every serialized event must be an internally-tagged object whose `kind`
//! discriminant is the snake_case variant name.

use morphe_ai::DesignEvent;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Intent round-trips
// ---------------------------------------------------------------------------

#[test]
fn design_event_intent_round_trips_through_serde_json() {
    let event = DesignEvent::Intent {
        statement: "The bracket must mount to a 2020 extrusion.".to_string(),
    };
    let serialized = serde_json::to_value(&event).expect("serialize Intent");
    let back: DesignEvent = serde_json::from_value(serialized).expect("deserialize Intent");
    assert_eq!(back, event);
}

// ---------------------------------------------------------------------------
// Decision round-trips (accepted-edit request + applied patch)
// ---------------------------------------------------------------------------

#[test]
fn design_event_decision_round_trips_through_serde_json() {
    // The {path, old, new} triple mirrors the `edit_code` tool input shape
    // (apps/desktop/src-tauri/src/ai.rs AiEvent::ToolCall payload). For
    // str_replace the accepted-edit request and the applied patch coincide,
    // so a single triple captures both.
    let event = DesignEvent::Decision {
        path: "bracket.scad".to_string(),
        old: "wall_thickness = 1.5;".to_string(),
        new: "wall_thickness = 2.0;".to_string(),
    };
    let serialized = serde_json::to_value(&event).expect("serialize Decision");
    let back: DesignEvent = serde_json::from_value(serialized).expect("deserialize Decision");
    assert_eq!(back, event);
}

// ---------------------------------------------------------------------------
// Constraint round-trips (name + value)
// ---------------------------------------------------------------------------

#[test]
fn design_event_constraint_round_trips_through_serde_json() {
    let event = DesignEvent::Constraint {
        name: "wall_thickness".to_string(),
        value: "2mm".to_string(),
    };
    let serialized = serde_json::to_value(&event).expect("serialize Constraint");
    let back: DesignEvent = serde_json::from_value(serialized).expect("deserialize Constraint");
    assert_eq!(back, event);
}

// ---------------------------------------------------------------------------
// PrintSetting round-trips (key + value)
// ---------------------------------------------------------------------------

#[test]
fn design_event_print_setting_round_trips_through_serde_json() {
    let event = DesignEvent::PrintSetting {
        key: "layer_height".to_string(),
        value: "0.2mm".to_string(),
    };
    let serialized = serde_json::to_value(&event).expect("serialize PrintSetting");
    let back: DesignEvent = serde_json::from_value(serialized).expect("deserialize PrintSetting");
    assert_eq!(back, event);
}

// ---------------------------------------------------------------------------
// Internally-tagged on a snake_case "kind" discriminant, all 4 variants
// ---------------------------------------------------------------------------

#[test]
fn design_event_serde_is_internally_tagged_on_snake_case_kind() {
    // Each variant must serialize to a JSON object whose `kind` discriminant is
    // the snake_case variant name. This pins the wire shape the noos writer and
    // retry queue consume.
    let cases: Vec<(DesignEvent, &str)> = vec![
        (
            DesignEvent::Intent {
                statement: "intent statement".to_string(),
            },
            "intent",
        ),
        (
            DesignEvent::Decision {
                path: "p.scad".to_string(),
                old: "old text".to_string(),
                new: "new text".to_string(),
            },
            "decision",
        ),
        (
            DesignEvent::Constraint {
                name: "dimension".to_string(),
                value: "1mm".to_string(),
            },
            "constraint",
        ),
        (
            DesignEvent::PrintSetting {
                key: "infill".to_string(),
                value: "20%".to_string(),
            },
            "print_setting",
        ),
        (
            DesignEvent::ConstraintRetired {
                name: "retired_dim".to_string(),
            },
            "constraint_retired",
        ),
    ];

    for (event, expected_kind) in cases {
        let v = serde_json::to_value(&event).expect("serialize event");
        let obj = v.as_object().expect("event serialized as JSON object");
        assert!(
            obj.contains_key("kind"),
            "missing `kind` discriminant on {expected_kind}: {v}"
        );
        let kind = obj.get("kind").and_then(Value::as_str).unwrap_or_else(|| {
            panic!("`kind` discriminant is not a string on {expected_kind}: {v}")
        });
        assert_eq!(
            kind, expected_kind,
            "snake_case discriminant mismatch for variant"
        );
    }
}

/// The exact serialized shape of one variant, asserted as a golden value so a
/// drift in the wire shape (extra fields, renamed discriminant, different
/// tagging style) breaks the test loudly. Intent is the smallest variant.
#[test]
fn design_event_intent_wire_shape_is_internally_tagged_kind() {
    let event = DesignEvent::Intent {
        statement: "s".to_string(),
    };
    let v = serde_json::to_value(&event).expect("serialize Intent");
    // Internally-tagged: `kind` is a peer key alongside the payload fields, not
    // a wrapping object. If the tagging style changes (e.g. adjacently-tagged),
    // this golden breaks.
    assert_eq!(v, json!({"kind": "intent", "statement": "s"}));
}

// ---------------------------------------------------------------------------
// ConstraintRetired round-trips (constraint retirement)
// ---------------------------------------------------------------------------

#[test]
fn design_event_constraint_retired_round_trips_through_serde_json() {
    let event = DesignEvent::ConstraintRetired {
        name: "wall_thickness".to_string(),
    };
    let serialized = serde_json::to_value(&event).expect("serialize ConstraintRetired");
    let back: DesignEvent =
        serde_json::from_value(serialized).expect("deserialize ConstraintRetired");
    assert_eq!(back, event);
}

/// The exact serialized shape of ConstraintRetired — golden so a wire-shape
/// drift (rename, re-tag) breaks loudly.
#[test]
fn design_event_constraint_retired_wire_shape() {
    let event = DesignEvent::ConstraintRetired {
        name: "wall_thickness".to_string(),
    };
    let v = serde_json::to_value(&event).expect("serialize ConstraintRetired");
    assert_eq!(
        v,
        json!({"kind": "constraint_retired", "name": "wall_thickness"})
    );
}
