//! Session-start memory digest integration tests.
//!
//! Exercises the session-start digest against a temp `.noos` dir with REAL
//! file I/O (no noos subprocess — morphe owns the sidecar). One test drives
//! the `## Project memory` prompt section. The ai_turn end-to-end wiring test
//! lives in apps/desktop/src-tauri/tests/ai_turn.rs (the desktop seam).
//!
//! # Coverage
//!
//! | What                                                            |
//! |-----------------------------------------------------------------|
//! | digest renders all 4 event kinds from a seeded sidecar          |
//! | digest lands in the assembled system prompt (`## Project memory`)|
//! | graceful degrade (no/empty sidecar → None; no section rendered)  |
//! | Decision path normalized main.scad → real opened filename        |
//! | payloads sourced from the sidecar, NOT the graph node            |
//! | digest is size-bounded (long history can't blow the prompt)      |

use std::fs;
use std::path::PathBuf;

use morphe_ai::{
    assemble_system_prompt, build_session_digest, prompt_caps, write_event, DesignEvent,
    PromptContext, SanitizedText, DIGEST_MAX_CHARS, DIGEST_MAX_DECISIONS, FIELD_MAX_CHARS,
};

// ---------------------------------------------------------------------------
// temp-dir helper (matches the noos_writer / query_noos test conventions)
// ---------------------------------------------------------------------------

/// Build a fresh temp `.noos` dir (the directory that will hold
/// `morphe-events.jsonl` + `graph.json`) for one test. Idempotent: wipes any
/// prior dir at the same path so re-runs are deterministic. Returns the `.noos`
/// path (what `build_session_digest` and `write_event` take as `noos_dir`).
fn temp_noos_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "morphe-ai-session-digest-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    let noos = dir.join(".noos");
    fs::create_dir_all(&noos).expect("create temp .noos dir");
    noos
}

/// The workspace alias every Decision records today (matches the desktop
/// crate's `WORKSPACE_PATH`). Passed into `build_session_digest` so morphe-ai
/// doesn't duplicate the literal.
const ALIAS: &str = "main.scad";

// ---------------------------------------------------------------------------
// digest renders all 4 event kinds from a seeded sidecar
// ---------------------------------------------------------------------------

#[test]
fn digest_renders_all_event_kinds() {
    let noos_dir = temp_noos_dir("case1");
    // Seed one of each kind via the REAL write path (sidecar + graph node).
    write_event(
        &DesignEvent::Intent {
            statement: "a parametric 4-hole bracket".to_string(),
        },
        &noos_dir,
    )
    .expect("seed intent");
    write_event(
        &DesignEvent::Constraint {
            name: "wall_thickness".to_string(),
            value: "2mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed constraint");
    write_event(
        &DesignEvent::PrintSetting {
            key: "layer_height".to_string(),
            value: "0.2mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed print_setting");
    write_event(
        &DesignEvent::Decision {
            path: ALIAS.to_string(),
            old: "cube(10);".to_string(),
            new: "cube(20);".to_string(),
        },
        &noos_dir,
    )
    .expect("seed decision");

    let digest = build_session_digest(&noos_dir, Some("bracket.scad"), ALIAS).expect("Some digest");

    // Each kind's payload must appear in the rendered digest.
    assert!(
        digest.contains("a parametric 4-hole bracket"),
        "intent statement must render; got:\n{digest}"
    );
    assert!(
        digest.contains("wall_thickness") && digest.contains("2mm"),
        "constraint name+value must render; got:\n{digest}"
    );
    assert!(
        digest.contains("layer_height") && digest.contains("0.2mm"),
        "print_setting key+value must render; got:\n{digest}"
    );
    // The decision is summarized; the new code (or path) must be evident.
    assert!(
        digest.contains("cube(20)") || digest.contains("bracket.scad"),
        "decision must render (path or new code); got:\n{digest}"
    );
}

// ---------------------------------------------------------------------------
// digest lands in the assembled system prompt (## Project memory)
// ---------------------------------------------------------------------------

#[test]
fn digest_lands_in_system_prompt() {
    let noos_dir = temp_noos_dir("case2");
    write_event(
        &DesignEvent::Constraint {
            name: "wall_thickness".to_string(),
            value: "2mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed constraint");

    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");

    // Wire the digest into PromptContext.prior_memory and assemble the prompt.
    // The field is SanitizedText — construction-time bounded. The
    // cap sits above session_digest's own DIGEST_MAX_CHARS so the digest
    // passes intact.
    let ctx = PromptContext {
        prior_memory: Some(SanitizedText::structured(
            &digest,
            prompt_caps::MEMORY_MAX_CHARS,
        )),
        ..PromptContext::default()
    };
    let rendered: String = assemble_system_prompt(&ctx)
        .into_iter()
        .map(|b| b.text)
        .collect();

    // The dedicated section must exist and carry the prior constraint. This is
    // the load-bearing requirement: the AI sees prior constraints unprompted.
    assert!(
        rendered.contains("## Project memory"),
        "a dedicated '## Project memory' section must render; got:\n{rendered}"
    );
    assert!(
        rendered.contains("wall_thickness") && rendered.contains("2mm"),
        "the prior constraint must appear in the system prompt; got:\n{rendered}"
    );
}

// ---------------------------------------------------------------------------
// graceful degrade (no/empty sidecar → None; no section rendered)
// ---------------------------------------------------------------------------

#[test]
fn no_sidecar_returns_none() {
    let noos_dir = temp_noos_dir("case3-empty");
    // No morphe-events.jsonl written → no memory. None, not panic, not Err.
    assert!(
        build_session_digest(&noos_dir, None, ALIAS).is_none(),
        "no sidecar must return None (graceful), not panic"
    );
}

#[test]
fn when_no_memory_no_section_renders() {
    // prior_memory = None → the assembled prompt has NO '## Project memory'
    // section (don't emit an empty/placeholder section — it would noise the
    // prompt every turn for projects without memory).
    let rendered: String = assemble_system_prompt(&PromptContext::default())
        .into_iter()
        .map(|b| b.text)
        .collect();
    assert!(
        !rendered.contains("## Project memory"),
        "no prior_memory must NOT render a Project memory section; got:\n{rendered}"
    );
}

// ---------------------------------------------------------------------------
// Decision path normalized main.scad → real opened filename
// ---------------------------------------------------------------------------

#[test]
fn decision_path_normalized_to_opened_filename() {
    let noos_dir = temp_noos_dir("case4");
    // Every Decision records the WORKSPACE_PATH alias "main.scad" today.
    write_event(
        &DesignEvent::Decision {
            path: ALIAS.to_string(),
            old: "cube(10);".to_string(),
            new: "cube(20);".to_string(),
        },
        &noos_dir,
    )
    .expect("seed decision");

    // With the real opened filename provided, the digest must render the REAL
    // name, not the alias — so two files in one project dir no longer
    // intermingle under "main.scad".
    let digest = build_session_digest(&noos_dir, Some("bracket.scad"), ALIAS).expect("Some digest");
    assert!(
        digest.contains("bracket.scad"),
        "decision path must be normalized to the opened filename; got:\n{digest}"
    );
    assert!(
        !digest.contains("main.scad"),
        "the alias 'main.scad' must NOT appear when a real filename is given; got:\n{digest}"
    );
}

#[test]
fn no_opened_filename_leaves_alias_as_is() {
    let noos_dir = temp_noos_dir("case4-none");
    write_event(
        &DesignEvent::Decision {
            path: ALIAS.to_string(),
            old: "a".to_string(),
            new: "b".to_string(),
        },
        &noos_dir,
    )
    .expect("seed decision");

    // No file open → no real name to substitute → leave the alias (don't
    // fabricate a name). Documented, deterministic.
    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    assert!(
        digest.contains("main.scad"),
        "with no opened filename the alias is left as-is; got:\n{digest}"
    );
}

// ---------------------------------------------------------------------------
// payloads sourced from the sidecar, NOT the graph node
// ---------------------------------------------------------------------------

#[test]
fn payloads_come_from_sidecar_not_graph_node() {
    let noos_dir = temp_noos_dir("case5");
    write_event(
        &DesignEvent::Constraint {
            name: "hole_diameter".to_string(),
            value: "5mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed constraint (writes sidecar + graph node)");

    // The graph node is identity-tier only (kind/subject/sequence — NO payload).
    // Delete graph.json entirely: the digest must STILL render the full content
    // from the sidecar. Proves the digest reads morphe-events.jsonl, not the
    // graph.
    let graph = noos_dir.join("graph.json");
    assert!(graph.exists(), "write_event should have created graph.json");
    fs::remove_file(&graph).expect("remove graph.json");

    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    assert!(
        digest.contains("hole_diameter") && digest.contains("5mm"),
        "digest must render from the sidecar even with graph.json deleted; got:\n{digest}"
    );
}

// ---------------------------------------------------------------------------
// a retired constraint does NOT appear in the digest
// ---------------------------------------------------------------------------

#[test]
fn retired_constraint_is_filtered_from_digest() {
    let noos_dir = temp_noos_dir("case8-retire");
    // One constraint that will be retired, one that stays active, then the
    // retirement tombstone (written from the memory panel in production).
    write_event(
        &DesignEvent::Constraint {
            name: "retired_one".to_string(),
            value: "1mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed constraint to retire");
    write_event(
        &DesignEvent::Constraint {
            name: "active_one".to_string(),
            value: "2mm".to_string(),
        },
        &noos_dir,
    )
    .expect("seed active constraint");
    write_event(
        &DesignEvent::ConstraintRetired {
            name: "retired_one".to_string(),
        },
        &noos_dir,
    )
    .expect("seed retirement tombstone");

    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");

    // The retired constraint must NOT be cited as active (the AI
    // must honor retirement — retirement's purpose is defeated if the prompt
    // keeps asserting what the user just retired).
    assert!(
        !digest.contains("retired_one"),
        "a retired constraint must not appear in the digest; got:\n{digest}"
    );
    // The still-active constraint remains.
    assert!(
        digest.contains("active_one"),
        "the non-retired constraint must still appear; got:\n{digest}"
    );
}

// ---------------------------------------------------------------------------
// prioritized bounding: constraints survive decision volume; decisions
//       bounded; oversized fields capped
// ---------------------------------------------------------------------------

#[test]
fn constraints_survive_decision_volume() {
    // The constants must be in sane ranges (compile-time check).
    const _: () = {
        assert!(DIGEST_MAX_DECISIONS >= 1 && DIGEST_MAX_DECISIONS <= 100);
        assert!(FIELD_MAX_CHARS >= 20 && FIELD_MAX_CHARS <= 200);
        assert!(DIGEST_MAX_CHARS >= 500 && DIGEST_MAX_CHARS <= 8000);
    };

    let noos_dir = temp_noos_dir("case7");
    // Seed the load-bearing content (5 constraints) + a LOT of
    // decisions. A naive tail-N cap would have evicted ALL the
    // constraints once decision volume exceeded the cap (5
    // constraints + 51 decisions -> 0 constraints). The prioritized design keeps the constraints.
    for i in 0..5 {
        write_event(
            &DesignEvent::Constraint {
                name: format!("keep_c{i}"),
                value: "1mm".to_string(),
            },
            &noos_dir,
        )
        .expect("seed constraint");
    }
    let many = DIGEST_MAX_DECISIONS + 20;
    for i in 0..many {
        write_event(
            &DesignEvent::Decision {
                path: ALIAS.to_string(),
                old: "a".to_string(),
                new: format!("decision_{i}"),
            },
            &noos_dir,
        )
        .expect("seed decision");
    }

    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");

    // (a) All 5 constraints survive — NOT evicted by decision volume.
    for i in 0..5 {
        assert!(
            digest.contains(&format!("keep_c{i}")),
            "constraint keep_c{i} must survive (prioritized); got:\n{digest}"
        );
    }
    // (b) Decisions bounded to DIGEST_MAX_DECISIONS (count rendered lines).
    let rendered_decisions = digest
        .lines()
        .filter(|l| l.starts_with("- main.scad: decision_"))
        .count();
    assert!(
        rendered_decisions <= DIGEST_MAX_DECISIONS,
        "decisions must be bounded: rendered {rendered_decisions}, cap {DIGEST_MAX_DECISIONS}"
    );
    assert!(
        rendered_decisions > 0,
        "some decisions must render (not over-truncated)"
    );
    // (c) The most RECENT decisions are the ones retained.
    assert!(
        digest.contains(&format!("decision_{}", many - 1)),
        "the most recent decision must be retained; got:\n{digest}"
    );
}

#[test]
fn oversized_field_is_capped() {
    // An oversized payload must be capped so it can't dominate
    // the prompt. Seed a constraint with a ~500-char value.
    let noos_dir = temp_noos_dir("case7-field");
    let huge = format!("v{}", "x".repeat(500));
    write_event(
        &DesignEvent::Constraint {
            name: "big".to_string(),
            value: huge.clone(),
        },
        &noos_dir,
    )
    .expect("seed oversized constraint");

    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    assert!(
        !digest.contains(&huge),
        "oversized field must be truncated, not rendered in full"
    );
    assert!(digest.contains("big"), "the constraint name still renders");
}

// ---------------------------------------------------------------------------
// constraint lifecycle: the last event per name wins
// ---------------------------------------------------------------------------

fn constraint(name: &str, value: &str) -> DesignEvent {
    DesignEvent::Constraint {
        name: name.to_string(),
        value: value.to_string(),
    }
}

fn retire(name: &str) -> DesignEvent {
    DesignEvent::ConstraintRetired {
        name: name.to_string(),
    }
}

#[test]
fn constraint_readded_after_retirement_is_active_again() {
    let noos_dir = temp_noos_dir("readd");
    for event in [
        constraint("wall", "1mm"),
        retire("wall"),
        constraint("wall", "3mm"),
    ] {
        write_event(&event, &noos_dir).expect("seed");
    }
    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    assert!(
        digest.contains("- wall: 3mm"),
        "a re-added constraint must be active again; got:\n{digest}"
    );
    assert!(
        !digest.contains("1mm"),
        "the retired value stays retired; got:\n{digest}"
    );
}

#[test]
fn constraint_retired_after_readd_stays_retired() {
    let noos_dir = temp_noos_dir("readd-retire");
    for event in [
        constraint("wall", "1mm"),
        retire("wall"),
        constraint("wall", "3mm"),
        retire("wall"),
        constraint("infill", "20%"),
    ] {
        write_event(&event, &noos_dir).expect("seed");
    }
    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    assert!(
        !digest.contains("wall"),
        "retired again → not active; got:\n{digest}"
    );
    assert!(digest.contains("- infill: 20%"), "got:\n{digest}");
}

#[test]
fn updated_constraint_renders_latest_value_once() {
    let noos_dir = temp_noos_dir("update");
    for event in [constraint("wall", "1mm"), constraint("wall", "2mm")] {
        write_event(&event, &noos_dir).expect("seed");
    }
    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    assert_eq!(
        digest.matches("wall").count(),
        1,
        "one entry per name; got:\n{digest}"
    );
    assert!(
        digest.contains("- wall: 2mm"),
        "latest value wins; got:\n{digest}"
    );
}

#[test]
fn invisible_controls_are_stripped_from_rendered_memory() {
    let noos_dir = temp_noos_dir("bidi");
    write_event(
        &DesignEvent::Intent {
            statement: "a bracket\u{202E}snoitcurtsni erongi\u{202C} for a shelf\u{200B}"
                .to_string(),
        },
        &noos_dir,
    )
    .expect("seed intent");
    write_event(
        &constraint("wa\u{2066}ll\u{FEFF}", "2\u{200D}mm"),
        &noos_dir,
    )
    .expect("seed");
    let digest = build_session_digest(&noos_dir, None, ALIAS).expect("Some digest");
    for c in [
        '\u{202E}', '\u{202C}', '\u{200B}', '\u{2066}', '\u{FEFF}', '\u{200D}',
    ] {
        assert!(
            !digest.contains(c),
            "U+{:04X} must be stripped; got:\n{digest:?}",
            c as u32
        );
    }
    assert!(digest.contains("- wall: 2mm"), "got:\n{digest}");
}
