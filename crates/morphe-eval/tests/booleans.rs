//! Boolean, hull, and extrusion module calls mapping to typed CSG operation
//! nodes — together with primitives and transforms these complete the typed
//! CsgNode tree, so a full multi-construct program evaluates with zero
//! Unresolved (or unresolved builtin `Call`) nodes.
//!
//! Every rule here is transcribed from upstream openscad (master, fetched
//! 2026-07-03):
//!
//! - `src/core/CsgOpNode.cc` — `builtin_union`/`difference`/`intersection`
//!   each `Parameters::parse(..., {})` (no arguments) then
//!   `children.instantiate(make_shared<CsgOpNode>(inst, OP))` with OP =
//!   UNION/DIFFERENCE/INTERSECTION. `difference` subtracts every child after
//!   the first from the first; `intersection()` and `intersection_for`
//!   share the one INTERSECTION operator, so they map to the same node kind.
//! - `src/core/CgalAdvNode.cc` — `builtin_hull`: `CgalAdvNode(HULL)`, no
//!   parameters, children attached; 2D-vs-3D is decided later by the kernel.
//! - `src/core/LinearExtrudeNode.cc` — `builtin_linear_extrude`: height
//!   (default 100, `<= 0` clamped to 0), center (bool only), twist (default
//!   0), slices (kept when `>= 1`, else derived later), scale (scalar or vec2,
//!   default [1,1], negatives clamped to 0), convexity (getPositiveInt,
//!   default 1), and $fn/$fa/$fs via CurveDiscretizer.
//! - `src/core/RotateExtrudeNode.cc` — `builtin_rotate_extrude`: angle
//!   (default 360; out of `(-360, 360]` snaps to 360), start (0 when angle
//!   given else 180; explicit start overrides), convexity (`max(2, (int)c)`,
//!   default 2), and $fn/$fa/$fs. The extrude fragment trio is stored verbatim
//!   (the final count needs the profile extent, deferred to the kernel).

use morphe_eval::csg::{CsgNode, Fragments, LinearExtrudeParams, Primitive, RotateExtrudeParams};
use morphe_eval::eval::Evaluator;
use morphe_lang::parser::parse_program;

/// Parse and evaluate a whole program, asserting no hard errors, and return
/// the root node together with the evaluator (for its warning sink).
fn eval(src: &str) -> (CsgNode, Evaluator) {
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    let program = parse_program(src).expect("program should parse");
    let root = evaluator.eval_program(&program);
    assert!(
        evaluator.errors().is_empty(),
        "unexpected errors: {:?}",
        evaluator.errors()
    );
    (root, evaluator)
}

/// The children of a group node.
fn group_children(node: &CsgNode) -> &[CsgNode] {
    match node {
        CsgNode::Group { children, .. } => children,
        other => panic!("expected a group node, got {other:?}"),
    }
}

/// The first node directly under the root group.
fn first(root: &CsgNode) -> &CsgNode {
    &group_children(root)[0]
}

/// A primitive leaf's payload, panicking otherwise.
fn as_primitive(node: &CsgNode) -> &Primitive {
    match node {
        CsgNode::Primitive { primitive, .. } => primitive,
        other => panic!("expected a primitive, got {other:?}"),
    }
}

// ---- union ----

#[test]
fn union_groups_children_in_order() {
    let (root, _) = eval("union() { cube(1); sphere(1); }");
    let CsgNode::Union { children, .. } = first(&root) else {
        panic!("expected a union node, got {:?}", first(&root));
    };
    assert_eq!(children.len(), 2);
    assert!(matches!(as_primitive(&children[0]), Primitive::Cube { .. }));
    assert!(matches!(
        as_primitive(&children[1]),
        Primitive::Sphere { .. }
    ));
}

#[test]
fn union_is_distinct_from_group() {
    // union() is an explicit operator node, NOT the passive Group wrapper.
    let (root, _) = eval("union() cube(1);");
    assert!(matches!(first(&root), CsgNode::Union { .. }));
}

#[test]
fn union_with_no_children_is_empty() {
    let (root, _) = eval("union();");
    let CsgNode::Union { children, .. } = first(&root) else {
        panic!("expected a union node");
    };
    assert!(children.is_empty());
}

#[test]
fn union_takes_no_arguments() {
    // An argument to a no-parameter operator warns but still produces the node.
    let (root, ev) = eval("union(5) cube(1);");
    assert!(matches!(first(&root), CsgNode::Union { .. }));
    assert!(ev
        .warnings()
        .iter()
        .any(|w| w.contains("Too many unnamed arguments")));
}

// ---- difference ----

#[test]
fn difference_keeps_children_in_source_order() {
    // children[0] is the minuend; the rest are subtracted (kernel semantics).
    let (root, _) = eval("difference() { cube(4); sphere(2); cube(1); }");
    let CsgNode::Difference { children, .. } = first(&root) else {
        panic!("expected a difference node, got {:?}", first(&root));
    };
    assert_eq!(children.len(), 3);
    let Primitive::Cube { size, .. } = as_primitive(&children[0]) else {
        panic!("first child is the minuend cube");
    };
    assert_eq!(*size, [4.0, 4.0, 4.0]);
    assert!(matches!(
        as_primitive(&children[1]),
        Primitive::Sphere { .. }
    ));
    assert!(matches!(as_primitive(&children[2]), Primitive::Cube { .. }));
}

#[test]
fn difference_with_single_child() {
    // Nothing to subtract — the lone child is the whole result.
    let (root, _) = eval("difference() cube(2);");
    let CsgNode::Difference { children, .. } = first(&root) else {
        panic!("expected a difference node");
    };
    assert_eq!(children.len(), 1);
}

#[test]
fn difference_with_no_children_is_empty() {
    let (root, _) = eval("difference();");
    let CsgNode::Difference { children, .. } = first(&root) else {
        panic!("expected a difference node");
    };
    assert!(children.is_empty());
}

// ---- intersection (reuses the intersection_for Intersection node) ----

#[test]
fn intersection_maps_to_intersection_node() {
    let (root, _) = eval("intersection() { cube(2); sphere(1.5); }");
    let CsgNode::Intersection { children, .. } = first(&root) else {
        panic!("expected an intersection node, got {:?}", first(&root));
    };
    assert_eq!(children.len(), 2);
}

#[test]
fn intersection_and_intersection_for_share_one_node_kind() {
    // Upstream maps intersection() and intersection_for to the same
    // OpenSCADOperator::INTERSECTION, so both produce CsgNode::Intersection.
    let (root_op, _) = eval("intersection() { cube(1); sphere(1); }");
    let (root_for, _) = eval("intersection_for (i = [0:1]) cube(1);");
    assert!(matches!(first(&root_op), CsgNode::Intersection { .. }));
    assert!(matches!(first(&root_for), CsgNode::Intersection { .. }));
}

// ---- hull ----

#[test]
fn hull_collects_children() {
    let (root, _) = eval("hull() { circle(1); translate([5, 0, 0]) circle(1); }");
    let CsgNode::Hull { children, .. } = first(&root) else {
        panic!("expected a hull node, got {:?}", first(&root));
    };
    assert_eq!(children.len(), 2);
    assert!(matches!(
        as_primitive(&children[0]),
        Primitive::Circle { .. }
    ));
    assert!(matches!(&children[1], CsgNode::Transform { .. }));
}

#[test]
fn hull_with_no_children_is_empty() {
    let (root, _) = eval("hull();");
    let CsgNode::Hull { children, .. } = first(&root) else {
        panic!("expected a hull node");
    };
    assert!(children.is_empty());
}

// ---- nested and mixed booleans ----

#[test]
fn difference_of_unions_nests() {
    let (root, _) = eval(
        "difference() {\
            union() { cube(2); sphere(1); }\
            union() { cube(1); }\
         }",
    );
    let CsgNode::Difference { children, .. } = first(&root) else {
        panic!("expected a difference node");
    };
    assert_eq!(children.len(), 2);
    let CsgNode::Union { children: u0, .. } = &children[0] else {
        panic!("first child is a union");
    };
    assert_eq!(u0.len(), 2);
    assert!(matches!(&children[1], CsgNode::Union { .. }));
}

#[test]
fn boolean_over_primitives_and_transforms_mixed() {
    let (root, _) =
        eval("union() { cube(1); translate([1, 0, 0]) sphere(1); rotate(45) square(2); }");
    let CsgNode::Union { children, .. } = first(&root) else {
        panic!("expected a union node");
    };
    assert_eq!(children.len(), 3);
    assert!(matches!(as_primitive(&children[0]), Primitive::Cube { .. }));
    assert!(matches!(&children[1], CsgNode::Transform { .. }));
    assert!(matches!(&children[2], CsgNode::Transform { .. }));
}

#[test]
fn boolean_modifier_prefix_carries_onto_node() {
    // The '#' debug/highlight modifier on the instantiation reaches the node.
    let (root, _) = eval("#difference() { cube(2); sphere(1); }");
    let CsgNode::Difference { modifiers, .. } = first(&root) else {
        panic!("expected a difference node");
    };
    assert!(modifiers.debug);
}

// ---- linear_extrude ----

/// The linear_extrude node's params + child directly under the root.
fn linear(src: &str) -> (LinearExtrudeParams, CsgNode) {
    let (root, _) = eval(src);
    match first(&root) {
        CsgNode::LinearExtrude { params, child, .. } => (*params, (**child).clone()),
        other => panic!("expected a linear_extrude node, got {other:?}"),
    }
}

#[test]
fn linear_extrude_height() {
    assert_eq!(
        linear("linear_extrude(height = 5) square(2);").0.height,
        5.0
    );
}

#[test]
fn linear_extrude_default_height_is_100() {
    assert_eq!(linear("linear_extrude() square(2);").0.height, 100.0);
}

#[test]
fn linear_extrude_positional_height() {
    assert_eq!(linear("linear_extrude(7) square(2);").0.height, 7.0);
}

#[test]
fn linear_extrude_negative_height_clamps_to_zero() {
    assert_eq!(
        linear("linear_extrude(height = -3) square(2);").0.height,
        0.0
    );
}

#[test]
fn linear_extrude_center() {
    assert!(
        linear("linear_extrude(height = 5, center = true) square(2);")
            .0
            .center
    );
    assert!(!linear("linear_extrude(height = 5) square(2);").0.center);
}

#[test]
fn linear_extrude_twist() {
    assert_eq!(
        linear("linear_extrude(height = 5, twist = 90) square(2);")
            .0
            .twist,
        90.0
    );
}

#[test]
fn linear_extrude_slices_set() {
    assert_eq!(
        linear("linear_extrude(height = 5, twist = 90, slices = 12) square(2);")
            .0
            .slices,
        Some(12)
    );
}

#[test]
fn linear_extrude_slices_unset_is_none() {
    // Deferred: the kernel derives the count from twist + fragments.
    assert_eq!(
        linear("linear_extrude(height = 5, twist = 90) square(2);")
            .0
            .slices,
        None
    );
}

#[test]
fn linear_extrude_scale_scalar_sets_both_axes() {
    assert_eq!(
        linear("linear_extrude(height = 5, scale = 3) square(2);")
            .0
            .scale,
        [3.0, 3.0]
    );
}

#[test]
fn linear_extrude_scale_vector() {
    assert_eq!(
        linear("linear_extrude(height = 5, scale = [2, 4]) square(2);")
            .0
            .scale,
        [2.0, 4.0]
    );
}

#[test]
fn linear_extrude_default_scale_is_unit() {
    assert_eq!(
        linear("linear_extrude(height = 5) square(2);").0.scale,
        [1.0, 1.0]
    );
}

#[test]
fn linear_extrude_negative_scale_clamps_to_zero() {
    assert_eq!(
        linear("linear_extrude(height = 5, scale = [-1, 4]) square(2);")
            .0
            .scale,
        [0.0, 4.0]
    );
}

#[test]
fn linear_extrude_convexity() {
    assert_eq!(
        linear("linear_extrude(height = 5, convexity = 4) square(2);")
            .0
            .convexity,
        4
    );
    assert_eq!(
        linear("linear_extrude(height = 5) square(2);").0.convexity,
        1
    );
}

#[test]
fn linear_extrude_default_fragments_are_seeds() {
    assert_eq!(
        linear("linear_extrude(height = 5) circle(2);").0.fragments,
        Fragments {
            fn_: 0.0,
            fs: 2.0,
            fa: 12.0
        }
    );
}

#[test]
fn linear_extrude_fn_propagates_from_dynamic_scope() {
    // A $fn set in the enclosing scope reaches the extrude's fragment trio.
    assert_eq!(
        linear("$fn = 64; linear_extrude(height = 5) circle(2);")
            .0
            .fragments
            .fn_,
        64.0
    );
}

#[test]
fn linear_extrude_fn_from_named_argument() {
    // A call-site $fn overrides the dynamic value (upstream CurveDiscretizer).
    assert_eq!(
        linear("$fn = 8; linear_extrude(height = 5, $fn = 48) circle(2);")
            .0
            .fragments
            .fn_,
        48.0
    );
}

#[test]
fn linear_extrude_h_is_height_alias() {
    // Upstream LinearExtrudeNode.cc (master): the OPTIONAL parameter set is
    // {"convexity", "h"} (line 50) and height is read via
    // parameters[{"height", "h"}] (line 64), so `h` is a genuine short alias
    // for `height` — a real (coincidental) namesake of cylinder's separate
    // `h`, NOT a conflation of it. `linear_extrude(h = 5)` therefore binds
    // height = 5 with no "not specified as parameter" warning.
    let (root, ev) = eval("linear_extrude(h = 5) square(2);");
    let CsgNode::LinearExtrude { params, .. } = first(&root) else {
        panic!("expected a linear_extrude node");
    };
    assert_eq!(params.height, 5.0);
    assert!(
        !ev.warnings()
            .iter()
            .any(|w| w.contains("not specified as parameter")),
        "`h` is an upstream parameter alias; it must not warn: {:?}",
        ev.warnings()
    );
}

#[test]
fn linear_extrude_child_is_group_of_2d_profile() {
    let (_, child) = linear("linear_extrude(height = 5) square(2);");
    let profile = group_children(&child);
    assert_eq!(profile.len(), 1);
    let Primitive::Square { .. } = as_primitive(&profile[0]) else {
        panic!("the profile child is a 2D square");
    };
}

// ---- rotate_extrude ----

/// The rotate_extrude node's params + child directly under the root.
fn rotate(src: &str) -> (RotateExtrudeParams, CsgNode) {
    let (root, _) = eval(src);
    match first(&root) {
        CsgNode::RotateExtrude { params, child, .. } => (*params, (**child).clone()),
        other => panic!("expected a rotate_extrude node, got {other:?}"),
    }
}

#[test]
fn rotate_extrude_default_angle_and_start() {
    let params = rotate("rotate_extrude() translate([4, 0, 0]) circle(1);").0;
    assert_eq!(params.angle, 360.0);
    assert_eq!(params.start, 180.0);
}

#[test]
fn rotate_extrude_angle_supplied_starts_at_zero() {
    let params = rotate("rotate_extrude(angle = 270) translate([4, 0, 0]) circle(1);").0;
    assert_eq!(params.angle, 270.0);
    assert_eq!(params.start, 0.0);
}

#[test]
fn rotate_extrude_angle_out_of_range_snaps_to_360() {
    assert_eq!(
        rotate("rotate_extrude(angle = 720) translate([4, 0, 0]) circle(1);")
            .0
            .angle,
        360.0
    );
}

#[test]
fn rotate_extrude_explicit_start_overrides() {
    let params = rotate("rotate_extrude(angle = 90, start = 45) translate([4, 0, 0]) circle(1);").0;
    assert_eq!(params.angle, 90.0);
    assert_eq!(params.start, 45.0);
}

#[test]
fn rotate_extrude_default_convexity_is_two() {
    assert_eq!(
        rotate("rotate_extrude() translate([4, 0, 0]) circle(1);")
            .0
            .convexity,
        2
    );
}

#[test]
fn rotate_extrude_convexity_floors_at_two() {
    assert_eq!(
        rotate("rotate_extrude(convexity = 1) translate([4, 0, 0]) circle(1);")
            .0
            .convexity,
        2
    );
    assert_eq!(
        rotate("rotate_extrude(convexity = 6) translate([4, 0, 0]) circle(1);")
            .0
            .convexity,
        6
    );
}

#[test]
fn rotate_extrude_fragments_and_fn_propagation() {
    let params = rotate("$fn = 96; rotate_extrude() translate([4, 0, 0]) circle(1);").0;
    assert_eq!(params.fragments.fn_, 96.0);
    assert_eq!(params.fragments.fs, 2.0);
    assert_eq!(params.fragments.fa, 12.0);
}

#[test]
fn rotate_extrude_a_is_angle_alias() {
    // Upstream RotateExtrudeNode.cc (master): the OPTIONAL parameter set is
    // {"convexity", "a"} (line 51) and the angle is read via
    // parameters[{"angle", "a"}] (line 60), so `a` is a genuine short alias
    // for `angle` — distinct from the rotate() TRANSFORM's `a`, NOT a
    // conflation of it. `rotate_extrude(a = 270)` binds angle = 270 (which,
    // being a supplied angle, sets start = 0) with no parameter warning.
    let (root, ev) = eval("rotate_extrude(a = 270) translate([4, 0, 0]) circle(1);");
    let CsgNode::RotateExtrude { params, .. } = first(&root) else {
        panic!("expected a rotate_extrude node");
    };
    assert_eq!(params.angle, 270.0);
    assert_eq!(params.start, 0.0);
    assert!(
        !ev.warnings()
            .iter()
            .any(|w| w.contains("not specified as parameter")),
        "`a` is an upstream parameter alias; it must not warn: {:?}",
        ev.warnings()
    );
}

#[test]
fn rotate_extrude_child_is_group_of_2d_profile() {
    let (_, child) = rotate("rotate_extrude() circle(1);");
    let profile = group_children(&child);
    assert_eq!(profile.len(), 1);
    assert!(matches!(
        as_primitive(&profile[0]),
        Primitive::Circle { .. }
    ));
}

// ---- a full multi-construct program is fully typed ----
//
// `morphe parse --eval corpus/core/*.scad` must produce a complete CsgNode
// tree with zero Unresolved nodes. This verifies the underlying invariant
// directly at the evaluator level: a program exercising every
// core construct (primitives + transforms + booleans + hull + extrudes + module +
// children + for + if) evaluates to a tree containing no `Unresolved` and no
// unresolved builtin `Call` node.

/// Assert a subtree contains no `Unresolved` and no `Call` node (every builtin
/// a core program uses is mapped to a typed node).
fn assert_fully_typed(node: &CsgNode) {
    match node {
        CsgNode::Unresolved { kind, .. } => panic!("Unresolved node remained: {kind}"),
        CsgNode::Call { name, .. } => panic!("unresolved builtin Call remained: {name}"),
        CsgNode::Primitive { .. } => {}
        CsgNode::Group { children, .. }
        | CsgNode::Union { children, .. }
        | CsgNode::Difference { children, .. }
        | CsgNode::Intersection { children, .. }
        | CsgNode::Hull { children, .. }
        | CsgNode::Minkowski { children, .. } => {
            children.iter().for_each(assert_fully_typed);
        }
        CsgNode::Transform { child, .. }
        | CsgNode::LinearExtrude { child, .. }
        | CsgNode::RotateExtrude { child, .. }
        | CsgNode::Offset { child, .. }
        | CsgNode::Projection { child, .. } => assert_fully_typed(child),
        // import(), surface() and text()
        // are fully-typed geometry leaves (like Primitive), they can appear
        // in fully-typed trees too.
        CsgNode::Import { .. } | CsgNode::Surface { .. } | CsgNode::Text { .. } => {}
    }
}

#[test]
fn full_multi_construct_program_is_fully_typed() {
    let src = "\
        $fn = 32;\
        module ring(r) {\
            difference() {\
                circle(r);\
                circle(r - 1);\
            }\
        }\
        difference() {\
            cube(10, center = true);\
            translate([0, 0, 2]) cylinder(h = 6, r = 3);\
            for (i = [0 : 2]) translate([i, 0, 0]) sphere(0.5);\
        }\
        intersection() { cube(4, center = true); sphere(2.5); }\
        linear_extrude(height = 4, twist = 90) ring(5);\
        union() {\
            square(3);\
            hull() { circle(1); translate([5, 0, 0]) circle(1); }\
        }\
        if (true) { rotate_extrude(angle = 270) translate([4, 0, 0]) circle(1); }\
        polyhedron(points = [[0,0,0],[1,0,0],[0,1,0],[0,0,1]], faces = [[0,1,2],[0,1,3],[1,2,3],[0,2,3]]);";
    let (root, ev) = eval(src);
    assert!(
        ev.errors().is_empty(),
        "the core corpus program must evaluate cleanly: {:?}",
        ev.errors()
    );
    assert_fully_typed(&root);
    // Sanity: the root actually holds the expected top-level constructs.
    let children = group_children(&root);
    assert!(children
        .iter()
        .any(|c| matches!(c, CsgNode::Difference { .. })));
    assert!(children
        .iter()
        .any(|c| matches!(c, CsgNode::Intersection { .. })));
    assert!(children
        .iter()
        .any(|c| matches!(c, CsgNode::LinearExtrude { .. })));
    assert!(children.iter().any(|c| matches!(c, CsgNode::Union { .. })));
    assert!(children.iter().any(|c| matches!(c, CsgNode::Group { .. }))); // the if-branch group
}
