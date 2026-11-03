//! Transform module calls mapping to typed CSG transform nodes.
//!
//! Every rule and matrix here is transcribed from upstream openscad
//! (master, fetched 2026-07-03):
//!
//! - `src/core/TransformNode.cc` — `builtin_translate`/`scale`/`rotate`/
//!   `mirror`/`multmatrix`. translate/scale use `getVec3(x, y, z, pad)`
//!   (a vec2 is padded with `z = 0` for translate, `z = 1` for scale);
//!   scale also accepts a scalar. rotate with a VECTOR `a` builds the Euler
//!   matrix `Rz(az)·Ry(ay)·Rx(ax)`; with a scalar `a` it rotates about axis
//!   `v` (default Z) via `angle_axis_degrees`. mirror is the Householder
//!   reflection across the plane with normal `v` (default `[1, 0, 0]`); a
//!   zero normal is the identity. multmatrix fills a 4x4 identity from the
//!   first 4x4 of `m` and divides by `m[3][3]` when that is not 1.
//! - `src/utils/degree_trig.cc` — `sin_degrees`/`cos_degrees` (exact at
//!   30/45/60/90, so 90-degree rotations land on clean 0/±1 entries) and the
//!   `angle_axis_degrees` Rodrigues construction.
//! - `src/core/CgalAdvNode.cc` — `builtin_resize`: `newsize` (first three
//!   components, missing default 0) and `auto` (vec of bools or a single
//!   bool). Deferred: the kernel needs the child bbox, so params are stored.
//! - `src/core/ColorNode.cc` + `ColorUtil.cc` + `WebColors.h` —
//!   `builtin_color`: vec RGBA (missing channels default 1.0), named web
//!   colors, `#hex`, and an `alpha` override. Default is the inherit
//!   sentinel `[-1, -1, -1, -1]`.

use morphe_eval::csg::{Affine, CsgNode, Primitive, TransformKind};
use morphe_eval::eval::Evaluator;
use morphe_lang::parser::parse_program;

fn eval(src: &str) -> (CsgNode, Evaluator) {
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    let program = parse_program(src).expect("program should parse");
    let root = evaluator.eval_program(&program);
    (root, evaluator)
}

fn group_children(node: &CsgNode) -> &[CsgNode] {
    match node {
        CsgNode::Group { children, .. } => children,
        other => panic!("expected a group node, got {other:?}"),
    }
}

/// The first transform node directly under the root group: its kind and its
/// child subtree.
fn transform(src: &str) -> (TransformKind, CsgNode) {
    let (root, ev) = eval(src);
    assert!(
        ev.errors().is_empty(),
        "unexpected errors: {:?}",
        ev.errors()
    );
    match &group_children(&root)[0] {
        CsgNode::Transform { kind, child, .. } => (kind.clone(), (**child).clone()),
        other => panic!("expected a transform node, got {other:?}"),
    }
}

/// The affine matrix of the first transform node.
fn affine(src: &str) -> Affine {
    match transform(src).0 {
        TransformKind::Affine(m) => m,
        other => panic!("expected an affine transform, got {other:?}"),
    }
}

fn has_warning(ev: &Evaluator, needle: &str) -> bool {
    ev.warnings().iter().any(|w| w.contains(needle))
}

/// Assert two 4x4 affine matrices are equal within a tight tolerance.
fn assert_affine(actual: Affine, expected: Affine) {
    for r in 0..4 {
        for c in 0..4 {
            assert!(
                (actual[r][c] - expected[r][c]).abs() < 1e-12,
                "m[{r}][{c}] = {} != expected {}\nfull: {actual:?}",
                actual[r][c],
                expected[r][c]
            );
        }
    }
}

/// A pure 3x3 rotation embedded in a 4x4 affine (no translation).
fn rot(m: [[f64; 3]; 3]) -> Affine {
    [
        [m[0][0], m[0][1], m[0][2], 0.0],
        [m[1][0], m[1][1], m[1][2], 0.0],
        [m[2][0], m[2][1], m[2][2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

const IDENTITY: Affine = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

// -------------------------------------------------------------- translate

#[test]
fn translate_vec3_sets_translation_column() {
    assert_affine(
        affine("translate([1, 2, 3]) cube(1);"),
        [
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 2.0],
            [0.0, 0.0, 1.0, 3.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn translate_vec2_pads_z_zero() {
    assert_affine(
        affine("translate([4, 5]) cube(1);"),
        [
            [1.0, 0.0, 0.0, 4.0],
            [0.0, 1.0, 0.0, 5.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn translate_scalar_warns_and_is_identity() {
    // getVec3 rejects a scalar, so the matrix is never translated.
    let (root, ev) = eval("translate(1) cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(*kind, TransformKind::Affine(IDENTITY));
    assert!(has_warning(&ev, "Unable to convert translate(1) parameter"));
}

// ------------------------------------------------------------------ scale

#[test]
fn scale_scalar_is_uniform() {
    assert_affine(
        affine("scale(2) cube(1);"),
        [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn scale_vec3_is_per_axis() {
    assert_affine(
        affine("scale([2, 3, 4]) cube(1);"),
        [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, 4.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn scale_vec2_pads_z_one() {
    assert_affine(
        affine("scale([2, 3]) cube(1);"),
        [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn scale_negative_is_stored_without_error() {
    // The zero/negative rangeCheck warning is behind a debug flag (off).
    let (root, ev) = eval("scale([-1, 1, 1]) cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(
        *kind,
        TransformKind::Affine([
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    );
    assert!(ev.errors().is_empty());
}

// ----------------------------------------------------------------- rotate

#[test]
fn rotate_scalar_is_z_axis_90() {
    // angle_axis about default axis [0,0,1].
    assert_affine(
        affine("rotate(90) cube(1);"),
        rot([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
    );
}

#[test]
fn rotate_scalar_45_uses_exact_trig() {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    assert_affine(
        affine("rotate(45) cube(1);"),
        rot([[s, -s, 0.0], [s, s, 0.0], [0.0, 0.0, 1.0]]),
    );
}

#[test]
fn rotate_vector_z_matches_scalar_z() {
    assert_affine(
        affine("rotate([0, 0, 90]) cube(1);"),
        rot([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
    );
}

#[test]
fn rotate_vector_euler_x_90() {
    assert_affine(
        affine("rotate([90, 0, 0]) cube(1);"),
        rot([[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]]),
    );
}

#[test]
fn rotate_vector_euler_y_90() {
    assert_affine(
        affine("rotate([0, 90, 0]) cube(1);"),
        rot([[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]]),
    );
}

#[test]
fn rotate_vector_euler_compose_order_zx() {
    // Rz(90)·Rx(90), hand-computed.
    assert_affine(
        affine("rotate([90, 0, 90]) cube(1);"),
        rot([[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
    );
}

#[test]
fn rotate_axis_angle_about_z() {
    assert_affine(
        affine("rotate(90, [0, 0, 1]) cube(1);"),
        rot([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
    );
}

#[test]
fn rotate_axis_angle_120_about_diagonal_is_cyclic() {
    // 120 degrees about [1,1,1] cyclically permutes the axes.
    assert_affine(
        affine("rotate(120, [1, 1, 1]) cube(1);"),
        rot([[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
    );
}

#[test]
fn rotate_vector_ignores_v_with_warning() {
    let (_, ev) = eval("rotate(a = [0, 0, 90], v = [1, 0, 0]) cube(1);");
    assert!(has_warning(&ev, "v is ignored"));
}

// ----------------------------------------------------------------- mirror

#[test]
fn mirror_x_negates_x() {
    assert_affine(
        affine("mirror([1, 0, 0]) cube(1);"),
        [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn mirror_y_negates_y() {
    assert_affine(
        affine("mirror([0, 1, 0]) cube(1);"),
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn mirror_arbitrary_normal_householder() {
    // normal [1,1,0], a = 2: swaps x and y (reflection across x=y plane).
    assert_affine(
        affine("mirror([1, 1, 0]) cube(1);"),
        [
            [0.0, -1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn mirror_zero_normal_is_identity() {
    assert_affine(affine("mirror([0, 0, 0]) cube(1);"), IDENTITY);
}

#[test]
fn mirror_bad_arg_warns_and_uses_default_x_normal() {
    // TransformNode.cc builtin_mirror inits `x=1,y=0,z=0` BEFORE getVec3.
    // Value::getVec3 returns false without touching x/y/z on a non-vector
    // (Value.cc:744 `if type != VECTOR return false`), so on failure the
    // fallback normal [1,0,0] survives and `if(x!=0||y!=0||z!=0)` still
    // builds the Householder matrix — the result is an x-flip, NOT identity.
    let (root, ev) = eval("mirror(5) cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(
        *kind,
        TransformKind::Affine([
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    );
    assert!(has_warning(&ev, "Unable to convert mirror(5) parameter"));
}

#[test]
fn mirror_no_arg_uses_default_x_normal() {
    // Same fallback path as a bad arg: undef `v` fails getVec3, [1,0,0]
    // survives, so mirror() is an x-flip (not identity).
    let (root, _) = eval("mirror() cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(
        *kind,
        TransformKind::Affine([
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    );
}

// ------------------------------------------- non-finite / degenerate inputs

#[test]
fn translate_non_finite_collapses_to_identity() {
    // TransformNode.cc builtin_translate: `ok &= isfinite(x)&&isfinite(y)&&
    // isfinite(z)` (lines 215-216), so a non-finite component skips
    // matrix.translate and leaves the identity, with a warning.
    let (root, ev) = eval("translate([1/0, 0, 0]) cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(*kind, TransformKind::Affine(IDENTITY));
    assert!(has_warning(&ev, "Unable to convert translate"));
}

#[test]
fn rotate_scalar_non_finite_angle_is_zero_rotation() {
    // TransformNode.cc builtin_rotate scalar branch: `aConverted &= !isinf &&
    // !isnan` then `angle_axis_degrees(aConverted ? a : 0, v)` (lines
    // 147, 151), so a non-finite angle rotates by 0 (identity).
    let (root, _) = eval("rotate(1/0) cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(*kind, TransformKind::Affine(IDENTITY));
}

#[test]
fn rotate_axis_angle_zero_axis_is_identity() {
    // degree_trig.cc angle_axis_degrees: `Matrix3d M{Identity}; if (m > 0)
    // {...}` where m = squaredNorm(v) — a zero-length axis yields identity.
    assert_affine(affine("rotate(90, [0, 0, 0]) cube(1);"), IDENTITY);
}

// ------------------------------------------------------------- multmatrix

#[test]
fn multmatrix_full_4x4_stored_verbatim() {
    assert_affine(
        affine("multmatrix([[1, 0, 0, 5], [0, 1, 0, 6], [0, 0, 1, 7], [0, 0, 0, 1]]) cube(1);"),
        [
            [1.0, 0.0, 0.0, 5.0],
            [0.0, 1.0, 0.0, 6.0],
            [0.0, 0.0, 1.0, 7.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn multmatrix_partial_rows_pad_to_identity() {
    // Only three rows supplied: the fourth stays the identity row.
    assert_affine(
        affine("multmatrix([[2, 0, 0, 0], [0, 2, 0, 0], [0, 0, 2, 0]]) cube(1);"),
        [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

#[test]
fn multmatrix_partial_cols_pad_to_identity() {
    // 3x3 supplied: missing columns/rows keep identity values.
    assert_affine(
        affine("multmatrix([[0, -1, 0], [1, 0, 0], [0, 0, 1]]) cube(1);"),
        rot([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
    );
}

#[test]
fn multmatrix_divides_by_w() {
    // m[3][3] = 2 divides the whole matrix.
    assert_affine(
        affine("multmatrix([[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 2]]) cube(1);"),
        [
            [0.5, 0.0, 0.0, 0.0],
            [0.0, 0.5, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
}

// ----------------------------------------------------------------- resize

#[test]
fn resize_stores_newsize_and_default_auto() {
    assert_eq!(
        transform("resize([10, 20, 30]) cube(1);").0,
        TransformKind::Resize {
            newsize: [10.0, 20.0, 30.0],
            auto: [false, false, false],
        }
    );
}

#[test]
fn resize_bool_auto_applies_to_all_axes() {
    assert_eq!(
        transform("resize([10, 20, 30], true) cube(1);").0,
        TransformKind::Resize {
            newsize: [10.0, 20.0, 30.0],
            auto: [true, true, true],
        }
    );
}

#[test]
fn resize_vector_auto_is_per_axis() {
    assert_eq!(
        transform("resize([10, 0, 0], [true, false, true]) cube(1);").0,
        TransformKind::Resize {
            newsize: [10.0, 0.0, 0.0],
            auto: [true, false, true],
        }
    );
}

#[test]
fn resize_partial_newsize_pads_zero() {
    assert_eq!(
        transform("resize([5]) cube(1);").0,
        TransformKind::Resize {
            newsize: [5.0, 0.0, 0.0],
            auto: [false, false, false],
        }
    );
}

// ------------------------------------------------------------------ color

#[test]
fn color_rgb_vector_defaults_alpha_one() {
    assert_eq!(
        transform("color([1, 0, 0]) cube(1);").0,
        TransformKind::Color([1.0, 0.0, 0.0, 1.0])
    );
}

#[test]
fn color_rgba_vector() {
    assert_eq!(
        transform("color([0.5, 0.25, 0.75, 0.5]) cube(1);").0,
        TransformKind::Color([0.5, 0.25, 0.75, 0.5])
    );
}

#[test]
fn color_named_web_color() {
    assert_eq!(
        transform("color(\"red\") cube(1);").0,
        TransformKind::Color([1.0, 0.0, 0.0, 1.0])
    );
}

#[test]
fn color_named_black() {
    assert_eq!(
        transform("color(\"black\") cube(1);").0,
        TransformKind::Color([0.0, 0.0, 0.0, 1.0])
    );
}

#[test]
fn color_named_case_insensitive() {
    assert_eq!(
        transform("color(\"Blue\") cube(1);").0,
        TransformKind::Color([0.0, 0.0, 1.0, 1.0])
    );
}

#[test]
fn color_string_with_alpha_override() {
    assert_eq!(
        transform("color(\"red\", 0.5) cube(1);").0,
        TransformKind::Color([1.0, 0.0, 0.0, 0.5])
    );
}

#[test]
fn color_hex_long() {
    assert_eq!(
        transform("color(\"#ff0000\") cube(1);").0,
        TransformKind::Color([1.0, 0.0, 0.0, 1.0])
    );
}

#[test]
fn color_hex_short() {
    assert_eq!(
        transform("color(\"#0f0\") cube(1);").0,
        TransformKind::Color([0.0, 1.0, 0.0, 1.0])
    );
}

#[test]
fn color_bad_name_warns_and_keeps_sentinel() {
    let (root, ev) = eval("color(\"notacolour\") cube(1);");
    let CsgNode::Transform { kind, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert_eq!(*kind, TransformKind::Color([-1.0, -1.0, -1.0, -1.0]));
    assert!(has_warning(&ev, "Unable to parse color"));
}

#[test]
fn color_out_of_range_channel_warns() {
    let (_, ev) = eval("color([2, 0, 0]) cube(1);");
    assert!(has_warning(&ev, "expects numbers between 0.0 and 1.0"));
}

// ----------------------------------------------------------- tree structure

#[test]
fn transform_child_is_a_group_of_the_children() {
    let (_, child) = transform("translate([1, 0, 0]) cube(3);");
    let kids = group_children(&child);
    assert_eq!(kids.len(), 1);
    assert!(matches!(
        &kids[0],
        CsgNode::Primitive {
            primitive: Primitive::Cube { .. },
            ..
        }
    ));
}

#[test]
fn transform_over_multiple_children_groups_them() {
    let (_, child) = transform("translate([0, 0, 1]) { cube(1); sphere(1); }");
    assert_eq!(group_children(&child).len(), 2);
}

#[test]
fn nested_transforms_compose_as_child_nodes() {
    // Outer translate's child group holds the inner rotate transform, whose
    // own child group holds the cube.
    let (_, outer_child) = transform("translate([1, 0, 0]) rotate(90) cube(1);");
    let outer_kids = group_children(&outer_child);
    assert_eq!(outer_kids.len(), 1);
    let CsgNode::Transform { kind, child, .. } = &outer_kids[0] else {
        panic!("expected an inner transform node");
    };
    assert_affine(
        match kind {
            TransformKind::Affine(m) => *m,
            other => panic!("expected affine, got {other:?}"),
        },
        rot([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
    );
    assert!(matches!(
        &group_children(child)[0],
        CsgNode::Primitive {
            primitive: Primitive::Cube { .. },
            ..
        }
    ));
}

#[test]
fn dollar_vars_propagate_into_transform_children() {
    // $fn set at the top level must reach the circle under the transform.
    let (_, child) = transform("$fn = 8; translate([1, 0, 0]) circle(1);");
    let CsgNode::Primitive {
        primitive: Primitive::Circle { fragments, .. },
        ..
    } = &group_children(&child)[0]
    else {
        panic!("expected a circle leaf");
    };
    assert_eq!(*fragments, 8);
}

#[test]
fn modifier_prefix_is_recorded_on_transform_node() {
    let (root, _) = eval("#translate([1, 0, 0]) cube(1);");
    let CsgNode::Transform { modifiers, .. } = &group_children(&root)[0] else {
        panic!("expected a transform node");
    };
    assert!(modifiers.debug);
}
