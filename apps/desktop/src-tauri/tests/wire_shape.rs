//! The Rust↔TS IPC wire-shape snapshot test.
//!
//! Tests on each side of the IPC boundary can pass for the wrong reason: the
//! Rust side and the TS side each assume a wire shape that neither verifies.
//! This file is the Rust half of a machine-checkable wire-shape contract that
//! closes that hole.
//!
//! It pins the Rust→TS IPC wire shape for the three `#[serde(tag = "kind")]`
//! enums crossing the boundary (`ExportError`, `FileError`, `AiEvent`):
//! [`build_snapshot`] constructs a canonical instance of every variant and
//! serializes it via `serde_json::to_string`, producing the exact bytes the
//! frontend will see. [`wire_shape_snapshot_matches`] compares that against the
//! checked-in `wire-shape.expected.json`, which the TS half (in
//! `apps/desktop/src/__tests__/wire-shape.test.ts`) also reads. The two tests
//! meet at the snapshot file: Rust owns the bytes, TS owns the union's declared
//! `kind` set, and drift on either side fails closed.
//!
//! If [`wire_shape_snapshot_matches`] fails, a variant was renamed or a serde
//! attribute changed (field rename, tag rename, field-order change). Run
//! `cargo test -p morphe-desktop wire_shape_write_snapshot -- --ignored`
//! to regenerate the snapshot, then update the TS `EXPECTED` map to match.
//!
//! Variant-set completeness is COMPILE-CHECKED: each `*_entries()` helper
//! constructs one canonical instance per variant AND pattern-matches it with an
//! exhaustive `match`. Adding a NEW variant to a pinned enum without handling
//! it here is a COMPILE ERROR (non-exhaustive match), so an unregistered
//! variant cannot slip through silently (closes the variant-registry gap a
//! hand-curated list would leave open).
//!
//! `CompileError.kind` is pinned here as an `ErrorKind` enum (one entry per
//! variant); the enum bound means Rust cannot produce a kind outside the
//! declared set, so the wire-shape contract pins it bidirectionally.

use serde_json::{Map, Value};

use morphe_desktop_lib::{
    AiEvent, CompileError, ErrorKind, ExportError, ExportStats, FileError, OpenFileResult,
    ProviderConfigInfo, SaveFileResult,
};
// DesignEvent lives in morphe-ai, re-exported through `pub use morphe_ai;` in the
// desktop lib. It crosses the IPC boundary (memory_list returns structured
// events to the panel), so it is part of the wire-shape contract.
use morphe_desktop_lib::morphe_ai::DesignEvent;

/// Build the wire-shape snapshot: `{ enum -> { variant -> serde_json::to_string(canonical_instance) } }`.
///
/// Canonical instances use empty strings, empty vecs, `Value::Null` for the
/// `input` field, and `false` for bools — the point is to pin the `kind`
/// discriminant spelling and the serialized field structure/order, not to
/// exercise any particular payload. The variant set per enum comes from the
/// exhaustive-match-driven `*_entries()` helpers below.
fn build_snapshot() -> Value {
    let mut snapshot = Map::new();
    snapshot.insert(
        "ExportError".to_string(),
        entries_map(export_error_entries()),
    );
    snapshot.insert(
        "CompileError".to_string(),
        entries_map(compile_error_entries()),
    );
    snapshot.insert("FileError".to_string(), entries_map(file_error_entries()));
    snapshot.insert("AiEvent".to_string(), entries_map(ai_event_entries()));
    // Provider crosses IPC (ai_get_provider_config returns it,
    // ai_set_provider_config takes it), so it is pinned here. It is a
    // bare-string enum (no tag): the pinned bytes are "anthropic" /
    // "open_ai_compatible" — the join the TS union dispatches on.
    snapshot.insert("Provider".to_string(), entries_map(provider_entries()));
    // ProviderConfigInfo — the RETURN type of ai_get_provider_config — is
    // pinned too. It's a struct, not a
    // tagged enum, so there is no variant set to exhaustively match; the pin
    // is the serialized byte shape (field names + no api_key field), which a
    // serde rename would break loudly. The TS half asserts the field set.
    snapshot.insert(
        "ProviderConfigInfo".to_string(),
        entries_map(provider_config_info_entries()),
    );
    // The remaining IPC return structs — ExportStats (export_stl_cmd),
    // OpenFileResult (open_file_cmd), SaveFileResult (save_file_cmd). The
    // return-type scan test below forces every IPC return type to appear
    // here, so a TS mirror can never exist without a Rust-side byte pin.
    snapshot.insert(
        "ExportStats".to_string(),
        entries_map(export_stats_entries()),
    );
    snapshot.insert(
        "OpenFileResult".to_string(),
        entries_map(open_file_result_entries()),
    );
    snapshot.insert(
        "SaveFileResult".to_string(),
        entries_map(save_file_result_entries()),
    );
    // DesignEvent crosses IPC via memory_list — pin it here so the
    // ConstraintRetired variant (and any future one) can't drift past the
    // Rust<->TS boundary. The exhaustive match below is the compile guard.
    snapshot.insert(
        "DesignEvent".to_string(),
        entries_map(design_event_entries()),
    );
    Value::Object(snapshot)
}

/// Build a per-enum snapshot map from `(variant_name, serialized_json)` pairs.
/// `serde_json::Map` sorts keys alphabetically, so insertion order is
/// irrelevant — the snapshot bytes depend only on the key + value set.
fn entries_map(entries: Vec<(&'static str, String)>) -> Value {
    let mut map = Map::new();
    for (name, json) in entries {
        map.insert(name.to_string(), Value::String(json));
    }
    Value::Object(map)
}

/// Canonical `Provider` instances — one per variant. The `match` is
/// exhaustive over `Provider`: a new provider without an arm here is a compile
/// error, forcing registration + snapshot coverage (the TS union in `ai.ts`
/// must then follow or the TS wire-shape test fails).
fn provider_entries() -> Vec<(&'static str, String)> {
    use morphe_desktop_lib::morphe_ai::Provider;

    [Provider::Anthropic, Provider::OpenAiCompatible]
        .into_iter()
        .map(|v| {
            // Exhaustive — NEVER add a `_ =>` arm (variant-registry guard).
            let name = match v {
                Provider::Anthropic => "Anthropic",
                Provider::OpenAiCompatible => "OpenAiCompatible",
            };
            (
                name,
                serde_json::to_string(&v).expect("Provider serializes"),
            )
        })
        .collect()
}

/// Canonical `ProviderConfigInfo` — the one IPC return shape for the provider
/// config (a struct, not an enum: a single canonical instance; the pin is the
/// byte shape, so every field carries a non-null value — a dropped field or a
/// serde rename changes the pinned bytes).
fn provider_config_info_entries() -> Vec<(&'static str, String)> {
    use morphe_desktop_lib::morphe_ai::Provider;

    let info = ProviderConfigInfo {
        provider: Provider::OpenAiCompatible,
        base_url: Some(String::new()),
        model: Some(String::new()),
        has_key: true,
    };
    vec![(
        "Instance",
        serde_json::to_string(&info).expect("ProviderConfigInfo serializes"),
    )]
}

/// Canonical `ExportStats` (export_stl_cmd success payload).
/// Field NAMES are in the pinned bytes, so a rename/drop changes them;
/// non-String fields carry non-default values.
fn export_stats_entries() -> Vec<(&'static str, String)> {
    let stats = ExportStats {
        triangle_count: 1,
        vertex_count: 2,
        format: String::new(),
        bytes_written: 3,
    };
    vec![(
        "Instance",
        serde_json::to_string(&stats).expect("ExportStats serializes"),
    )]
}

/// Canonical `OpenFileResult` (open_file_cmd success payload).
fn open_file_result_entries() -> Vec<(&'static str, String)> {
    let result = OpenFileResult {
        path: String::new(),
        source: String::new(),
    };
    vec![(
        "Instance",
        serde_json::to_string(&result).expect("OpenFileResult serializes"),
    )]
}

/// Canonical `SaveFileResult` (save_file_cmd success payload).
fn save_file_result_entries() -> Vec<(&'static str, String)> {
    let result = SaveFileResult {
        path: String::new(),
    };
    vec![(
        "Instance",
        serde_json::to_string(&result).expect("SaveFileResult serializes"),
    )]
}

/// Canonical `ExportError` instances — one per variant. The `match` is
/// exhaustive over `ExportError`: adding a variant without an arm here is a
/// compile error, forcing registration (and thus snapshot coverage).
fn export_error_entries() -> Vec<(&'static str, String)> {
    [
        ExportError::Compile {
            errors: Vec::new(),
            warnings: Vec::new(),
        },
        ExportError::Not3D,
        ExportError::Io {
            message: String::new(),
        },
        ExportError::Cancelled,
    ]
    .into_iter()
    .map(|v| {
        // Exhaustive — NEVER add a `_ =>` arm: it would silently drop a new
        // variant past both this compile guard and the snapshot byte-check
        // (variant-registry guard).
        let name = match &v {
            ExportError::Compile { .. } => "Compile",
            ExportError::Not3D => "Not3D",
            ExportError::Io { .. } => "Io",
            ExportError::Cancelled => "Cancelled",
        };
        (
            name,
            serde_json::to_string(&v).expect("ExportError variant serializes"),
        )
    })
    .collect()
}

/// Canonical `CompileError` instances — one per `ErrorKind` variant. The `match`
/// is exhaustive over `ErrorKind`.
fn compile_error_entries() -> Vec<(&'static str, String)> {
    [
        ErrorKind::Parse,
        ErrorKind::Unsupported,
        ErrorKind::General,
        ErrorKind::Resolver,
    ]
    .into_iter()
    .map(|kind| {
        // Exhaustive — NEVER add a `_ =>` arm (would silently drop a new
        // ErrorKind variant; variant-registry guard).
        let name = match kind {
            ErrorKind::Parse => "Parse",
            ErrorKind::Unsupported => "Unsupported",
            ErrorKind::General => "General",
            ErrorKind::Resolver => "Resolver",
        };
        let instance = CompileError {
            line: 0,
            col: 0,
            message: String::new(),
            kind,
        };
        (
            name,
            serde_json::to_string(&instance).expect("CompileError variant serializes"),
        )
    })
    .collect()
}

/// Canonical `FileError` instances — one per variant. The `match` is exhaustive.
fn file_error_entries() -> Vec<(&'static str, String)> {
    [
        FileError::Cancelled,
        FileError::Io {
            message: String::new(),
        },
    ]
    .into_iter()
    .map(|v| {
        // Exhaustive — NEVER add a `_ =>` arm (variant-registry guard).
        let name = match &v {
            FileError::Cancelled => "Cancelled",
            FileError::Io { .. } => "Io",
        };
        (
            name,
            serde_json::to_string(&v).expect("FileError variant serializes"),
        )
    })
    .collect()
}

/// Canonical `AiEvent` instances — one per variant. The `match` is exhaustive.
fn ai_event_entries() -> Vec<(&'static str, String)> {
    [
        AiEvent::Text {
            text: String::new(),
        },
        AiEvent::ToolCall {
            id: String::new(),
            name: String::new(),
            input: Value::Null,
        },
        AiEvent::ToolResult {
            id: String::new(),
            content: String::new(),
            is_error: false,
        },
        AiEvent::Done {
            stop_reason: String::new(),
            final_source: String::new(),
        },
    ]
    .into_iter()
    .map(|v| {
        // Exhaustive — NEVER add a `_ =>` arm (variant-registry guard).
        let name = match &v {
            AiEvent::Text { .. } => "Text",
            AiEvent::ToolCall { .. } => "ToolCall",
            AiEvent::ToolResult { .. } => "ToolResult",
            AiEvent::Done { .. } => "Done",
        };
        (
            name,
            serde_json::to_string(&v).expect("AiEvent variant serializes"),
        )
    })
    .collect()
}

/// Canonical `DesignEvent` instances — one per variant. The `match` is
/// exhaustive. DesignEvent crosses the IPC boundary (memory_list returns
/// structured events to the panel), so it is pinned alongside
/// ExportError/FileError/AiEvent/CompileError. The exhaustive match is the
/// compile guard for every variant, including ConstraintRetired.
fn design_event_entries() -> Vec<(&'static str, String)> {
    [
        DesignEvent::Intent {
            statement: String::new(),
        },
        DesignEvent::Decision {
            path: String::new(),
            old: String::new(),
            new: String::new(),
        },
        DesignEvent::Constraint {
            name: String::new(),
            value: String::new(),
        },
        DesignEvent::PrintSetting {
            key: String::new(),
            value: String::new(),
        },
        DesignEvent::ConstraintRetired {
            name: String::new(),
        },
    ]
    .into_iter()
    .map(|v| {
        // Exhaustive — NEVER add a `_ =>` arm: it would silently drop a new
        // DesignEvent variant past both this compile guard and the snapshot
        // byte-check (variant-registry guard).
        let name = match &v {
            DesignEvent::Intent { .. } => "Intent",
            DesignEvent::Decision { .. } => "Decision",
            DesignEvent::Constraint { .. } => "Constraint",
            DesignEvent::PrintSetting { .. } => "PrintSetting",
            DesignEvent::ConstraintRetired { .. } => "ConstraintRetired",
        };
        (
            name,
            serde_json::to_string(&v).expect("DesignEvent variant serializes"),
        )
    })
    .collect()
}

/// The snapshot path. `CARGO_MANIFEST_DIR` is `apps/desktop/src-tauri/`, so one
/// `..` lands at `apps/desktop/` — the same file `include_bytes!` (relative to
/// this source file at `apps/desktop/src-tauri/tests/`) resolves to, and the
/// same file the TS test imports via `../../wire-shape.expected.json`.
fn snapshot_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wire-shape.expected.json")
}

/// The wire-shape kill switch. If Rust's serialization of any pinned variant
/// drifts (new variant, renamed, serde attribute change), this fails. The
/// snapshot file is checked in alongside this test and shared with the TS half.
#[test]
fn wire_shape_snapshot_matches() {
    let bytes = include_bytes!("../../wire-shape.expected.json");
    let expected: Value = serde_json::from_slice(bytes).expect(
        "wire-shape.expected.json must parse; if absent, run \
         `cargo test -p morphe-desktop wire_shape_write_snapshot -- --ignored` \
         to generate it",
    );
    assert_eq!(
        build_snapshot(),
        expected,
        "Rust wire-shape serialization drifted; regenerate the snapshot with \
         `cargo test -p morphe-desktop wire_shape_write_snapshot -- --ignored` \
         then update the TS EXPECTED in apps/desktop/src/__tests__/wire-shape.test.ts"
    );
}

/// Bless the snapshot. Regenerates `wire-shape.expected.json` from
/// [`build_snapshot`], pretty-printed. Run after intentionally changing a
/// pinned enum (new variant, rename, serde attribute edit) — then update the TS
/// `EXPECTED` to match.
#[test]
#[ignore = "regenerate with: cargo test -p morphe-desktop wire_shape_write_snapshot -- --ignored"]
fn wire_shape_write_snapshot() {
    let pretty =
        serde_json::to_string_pretty(&build_snapshot()).expect("snapshot serializes pretty");
    let path = snapshot_path();
    std::fs::write(&path, pretty).expect("write wire-shape.expected.json");
    println!("wrote {}", path.display());
}

// ---------------------------------------------------------------------------
// IPC return-type pin
// ---------------------------------------------------------------------------
//
// Every type a `#[tauri::command]` RETURNS crosses the IPC boundary, so every
// non-primitive one must be pinned in wire-shape.expected.json (a serde rename
// would otherwise break the TS side silently). This test scans the command sources, unwraps Result<T, E> (BOTH
// sides cross the wire: the Ok payload and the serialized rejection) and
// Vec<Inner>, and asserts each remaining named type is either in the primitive
// allowlist or a key of the snapshot.

/// Return types that need no snapshot pin: Rust/JSON primitives and Tauri's
/// opaque binary frame (its packing has its own tests: the empty-mesh
/// decode test + the to_wire frame tests).
const IPC_PRIMITIVE_RETURNS: &[&str] = &[
    "bool",
    "String",
    "()",
    "u8",
    "u32",
    "u64",
    "usize",
    "i32",
    "i64",
    "f32",
    "f64",
    "tauri::ipc::Response",
];

/// Extract the named types an IPC return type exposes: unwraps `Result<T, E>`
/// into both sides and `Vec<Inner>` into `Inner`; leaves other generics
/// (none exist today — a future `Option<T>`/`HashMap<..>` return FAILS this
/// scan until explicitly handled here, which is the fail-closed direction).
fn exposed_types(ret: &str) -> Vec<String> {
    let ret = ret.trim();
    let mut out = Vec::new();
    if let Some(args) = ret
        .strip_prefix("Result<")
        .and_then(|r| r.strip_suffix('>'))
    {
        // Split on the top-level comma (no nested generics contain commas
        // today; if one appears, the split yields a non-allowlisted name and
        // the scan fails closed with a readable message).
        if let Some((ok, err)) = args.split_once(", ") {
            out.extend(exposed_types(ok));
            out.extend(exposed_types(err));
            return out;
        }
        // Malformed/unsplit Result — surface the raw text so the failure names it.
        out.push(ret.to_string());
        return out;
    }
    if let Some(inner) = ret.strip_prefix("Vec<").and_then(|r| r.strip_suffix('>')) {
        out.extend(exposed_types(inner));
        return out;
    }
    out.push(ret.to_string());
    out
}

/// Scan the crate sources for `#[tauri::command]` functions and return
/// (file, fn_name, return_type_source) triples.
///
/// The marker matches the PREFIX `#[tauri::command` and accepts `]` (plain) or
/// `(` (attribute args like `rename_all`) as the next char — a command with
/// attribute arguments cannot become invisible to the pin. The
/// source collection RECURSES into subdirectories: a future
/// `src/commands/export.rs` layout is scanned, not escaped.
///
/// KNOWN FRAGILITY: a doc comment BETWEEN the attribute and the
/// `fn` containing the text `fn ` or `->` could mis-anchor the scan. All 14
/// current sites place doc comments BEFORE the attribute, so the scan is
/// correct today; if a future command interleaves them, the failure mode is a
/// wrong/missing signature read — which surfaces as an unpinned-name failure
/// (fail-closed), never a silent pass.
fn command_returns() -> Vec<(String, String, String)> {
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources: Vec<std::path::PathBuf> = Vec::new();
    collect_rs_files(&src_dir, &mut sources);
    sources.sort();

    let mut cmds = Vec::new();
    for path in sources {
        let text = std::fs::read_to_string(&path).expect("command source readable");
        // Every #[tauri::command] followed (same-item) by a fn whose signature
        // ends in `-> <type>` before the body brace. Hand-rolled scan — no
        // regex/syn dependency.
        let mut rest = text.as_str();
        while let Some(idx) = rest.find("#[tauri::command") {
            let after_marker = &rest[idx + "#[tauri::command".len()..];
            // `]` = plain form; `(` = attribute-args form. Anything
            // longer (`#[tauri::command_x`) is NOT the marker — skip it so the
            // find always progresses.
            if !(after_marker.starts_with(']') || after_marker.starts_with('(')) {
                rest = after_marker;
                continue;
            }
            rest = after_marker;
            // Find the fn signature: skip attributes/doc comments to `fn`.
            let Some(fn_at) = rest.find("fn ") else {
                continue;
            };
            let sig_start = &rest[fn_at..];
            // The signature runs to the opening brace of the body.
            let Some(brace) = sig_start.find('{') else {
                continue;
            };
            let sig = &sig_start[..brace];
            let Some(arrow) = sig.find("->") else {
                continue;
            };
            let name = sig
                .split_whitespace()
                .nth(1)
                .unwrap_or("?")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string();
            let ret = sig[arrow + 2..].trim().to_string();
            cmds.push((
                path.file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned(),
                name,
                ret,
            ));
            rest = &sig_start[brace..];
        }
    }
    cmds
}

/// Whether one exposed type is adequately covered: primitive-allowlisted or
/// a snapshot key. Shared by the integration test and the kill-switch so the
/// kill-switch tests the REAL predicate.
fn is_covered(ty: &str, snapshot: &Map<String, Value>) -> bool {
    IPC_PRIMITIVE_RETURNS.contains(&ty) || snapshot.contains_key(ty)
}

/// Recursively collect `*.rs` files under `dir` (a future
/// `src/commands/` module layout is scanned, not escaped). Unreadable dirs
/// panic — fail-closed.
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("iterate {dir:?}: {e}"));
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn ipc_return_types_are_pinned() {
    let snapshot: Map<String, Value> = match serde_json::from_str(
        &std::fs::read_to_string(snapshot_path()).expect("snapshot readable"),
    )
    .expect("snapshot parses")
    {
        Value::Object(map) => map,
        _ => panic!("snapshot must be an object"),
    };

    let cmds = command_returns();
    assert!(
        !cmds.is_empty(),
        "the command scan found nothing — a silent no-op scan must fail"
    );

    let mut unpinned: Vec<String> = Vec::new();
    for (file, name, ret) in &cmds {
        for ty in exposed_types(ret) {
            if !is_covered(&ty, &snapshot) {
                unpinned.push(format!("{file} `{name}` returns {ty:?}"));
            }
        }
    }
    assert!(
        unpinned.is_empty(),
        "IPC return types not pinned in wire-shape.expected.json \
         (add a canonical instance to build_snapshot + regenerate):\n{}",
        unpinned.join("\n")
    );
}

#[test]
fn kill_switch_unpinned_return_type_is_flagged() {
    // A synthetic command returning an unpinned type must be flagged using
    // the SAME predicate as the integration test (one shared fn, not a
    // re-encoding). Snapshot with one known key, no NotPinned.
    let mut snapshot = Map::new();
    snapshot.insert("ExportStats".to_string(), Value::String(String::new()));

    assert!(!is_covered("NotPinned", &snapshot));
    assert!(is_covered("ExportStats", &snapshot));
    assert!(is_covered("bool", &snapshot)); // primitive allowlist
    assert!(is_covered("tauri::ipc::Response", &snapshot));

    // And the synthetic command WOULD be flagged end-to-end.
    for (file, name, ret) in [("x.rs", "evil_cmd", "Result<NotPinned, String>")] {
        for ty in exposed_types(ret) {
            if !is_covered(&ty, &snapshot) {
                assert_eq!(file, "x.rs");
                assert_eq!(name, "evil_cmd");
                assert_eq!(ty, "NotPinned");
            }
        }
    }

    // Result unwrapping checks BOTH sides (the serialized rejection crosses too).
    let both = exposed_types("Result<ExportStats, ExportError>");
    assert_eq!(both, ["ExportStats", "ExportError"]);
    // Vec unwraps to the element type.
    assert_eq!(exposed_types("Vec<DesignEvent>"), ["DesignEvent"]);
    // Primitives pass through untouched.
    assert_eq!(exposed_types("bool"), ["bool"]);
}

#[test]
fn kill_switch_attribute_args_and_nested_dirs_are_scanned() {
    // A command with ATTRIBUTE ARGS (`rename_all`) must be found by
    // the same marker logic command_returns uses — it cannot silently escape
    // the pin. This exercises the marker-prefix rule on synthetic sources
    // (the repo's 14 commands use the plain form today).
    let plain = "#[tauri::command]\npub fn a() -> Result<bool, String> { todo!() }";
    let attr_args =
        "#[tauri::command(rename_all = \"camelCase\")]\npub fn b() -> Result<bool, String> { todo!() }";
    let not_marker = "#[tauri::command_x]\npub fn c() -> Result<bool, String> { todo!() }";

    let count_markers = |src: &str| {
        let mut rest = src;
        let mut n = 0;
        while let Some(idx) = rest.find("#[tauri::command") {
            let after = &rest[idx + "#[tauri::command".len()..];
            if after.starts_with(']') || after.starts_with('(') {
                n += 1;
            }
            rest = after;
        }
        n
    };
    assert_eq!(count_markers(plain), 1);
    assert_eq!(count_markers(attr_args), 1, "attr-args form IS a command");
    assert_eq!(
        count_markers(not_marker),
        0,
        "longer paths are NOT the marker"
    );

    // The collector recurses — a nested commands/ dir is scanned.
    let tmp = std::env::temp_dir().join(format!("morphe-ws-nested-{}", std::process::id()));
    let nested = tmp.join("src").join("commands");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(nested.join("export.rs"), "// empty").expect("write");
    let mut out = Vec::new();
    collect_rs_files(&tmp.join("src"), &mut out);
    assert_eq!(out.len(), 1, "nested .rs file collected");
    let _ = std::fs::remove_dir_all(&tmp);
}
