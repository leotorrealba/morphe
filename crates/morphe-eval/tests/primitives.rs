//! Primitive module calls mapping to typed CSG leaf nodes.
//!
//! Every rule here is transcribed from upstream openscad
//! `src/core/primitives.cc` (`builtin_cube`/`sphere`/`cylinder`/`square`/
//! `circle`/`polygon`/`polyhedron`, `lookup_radius`) and the fragment
//! resolution of `Calc::get_fragments_from_r` /
//! `CurveDiscretizer::getCircularSegmentCount` (master + tag
//! openscad-2021.01, fetched 2026-07-03):
//!
//! - `cube`/`square`: `size` is a scalar (all dimensions) or a vec3/vec2;
//!   an undef/absent size keeps the default (1 in every dimension) and any
//!   other value warns and keeps the default. `center` applies only when
//!   the argument is a bool. A degenerate (<=0 / non-finite) size still
//!   produces a node — emptiness is decided later in the kernel.
//! - `sphere`/`circle`: `r`/`d` via `lookup_radius` (diameter wins, warning
//!   if radius is also defined); default r = 1. `fragments` =
//!   `get_fragments_from_r(r)`.
//! - `cylinder`: `h` default 1; `r`/`d` set both radii, `r1`/`d1`/`r2`/`d2`
//!   override each end (default 1); a scalar `r`/`d` alongside any
//!   `r1`/`r2`/`d1`/`d2` warns "Cylinder parameters ambiguous". `fragments`
//!   from `max(r1, r2)`.
//! - `polygon`: required `points` (vec2 each); optional `paths` (None when
//!   omitted/undef). `polyhedron`: required `points` (vec3, vec2 padded with
//!   z=0), `faces` index lists (3+ in-bounds indices kept), legacy
//!   `triangles` alias with a deprecation warning. `convexity` clamped >= 1.

use morphe_eval::csg::{CsgNode, Primitive};
use morphe_eval::eval::Evaluator;
use morphe_lang::parser::parse_program;

/// Parse and evaluate a whole program, returning the root node and the
/// evaluator (for its warning/error sinks).
fn eval(src: &str) -> (CsgNode, Evaluator) {
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    let program = parse_program(src).expect("program should parse");
    let root = evaluator.eval_program(&program);
    (root, evaluator)
}

/// The first primitive leaf directly under the root group.
fn first_primitive(root: &CsgNode) -> &Primitive {
    let CsgNode::Group { children, .. } = root else {
        panic!("expected a group root, got {root:?}");
    };
    match &children[0] {
        CsgNode::Primitive { primitive, .. } => primitive,
        other => panic!("expected a primitive node, got {other:?}"),
    }
}

/// Parse a single primitive statement and return its leaf.
fn prim(src: &str) -> Primitive {
    let (root, evaluator) = eval(src);
    assert!(
        evaluator.errors().is_empty(),
        "unexpected errors: {:?}",
        evaluator.errors()
    );
    first_primitive(&root).clone()
}

fn has_warning(evaluator: &Evaluator, needle: &str) -> bool {
    evaluator.warnings().iter().any(|w| w.contains(needle))
}

// ------------------------------------------------------------------- cube

#[test]
fn cube_scalar_size_fills_all_three_dimensions() {
    assert_eq!(
        prim("cube(3);"),
        Primitive::Cube {
            size: [3.0, 3.0, 3.0],
            center: false
        }
    );
}

#[test]
fn cube_vector_size_maps_component_wise() {
    assert_eq!(
        prim("cube([1, 2, 3]);"),
        Primitive::Cube {
            size: [1.0, 2.0, 3.0],
            center: false
        }
    );
}

#[test]
fn cube_center_true_is_applied() {
    assert_eq!(
        prim("cube(2, center = true);"),
        Primitive::Cube {
            size: [2.0, 2.0, 2.0],
            center: true
        }
    );
}

#[test]
fn cube_center_defaults_to_false_and_ignores_non_bool() {
    // Upstream applies center only when type() == BOOL, so a number leaves
    // it at the default false.
    assert_eq!(
        prim("cube(2, center = 1);"),
        Primitive::Cube {
            size: [2.0, 2.0, 2.0],
            center: false
        }
    );
}

#[test]
fn cube_no_args_uses_default_unit_size() {
    assert_eq!(
        prim("cube();"),
        Primitive::Cube {
            size: [1.0, 1.0, 1.0],
            center: false
        }
    );
}

#[test]
fn cube_negative_size_still_produces_a_node() {
    // createGeometry returns an empty PolySet for size <= 0, but the node
    // itself is always produced — emptiness is the kernel's job.
    assert_eq!(
        prim("cube(-2);"),
        Primitive::Cube {
            size: [-2.0, -2.0, -2.0],
            center: false
        }
    );
}

#[test]
fn cube_undef_size_keeps_default_without_warning() {
    let (root, evaluator) = eval("cube(undef);");
    assert_eq!(
        first_primitive(&root),
        &Primitive::Cube {
            size: [1.0, 1.0, 1.0],
            center: false
        }
    );
    assert!(evaluator.warnings().is_empty());
}

#[test]
fn cube_invalid_size_warns_and_keeps_default() {
    let (root, evaluator) = eval("cube(\"x\");");
    assert_eq!(
        first_primitive(&root),
        &Primitive::Cube {
            size: [1.0, 1.0, 1.0],
            center: false
        }
    );
    assert!(has_warning(&evaluator, "Unable to convert cube(size="));
}

// ----------------------------------------------------------------- sphere

#[test]
fn sphere_radius() {
    let Primitive::Sphere { r, .. } = prim("sphere(4);") else {
        panic!("expected a sphere");
    };
    assert_eq!(r, 4.0);
}

#[test]
fn sphere_diameter_halves_to_radius() {
    let Primitive::Sphere { r, .. } = prim("sphere(d = 10);") else {
        panic!("expected a sphere");
    };
    assert_eq!(r, 5.0);
}

#[test]
fn sphere_diameter_wins_over_radius_with_warning() {
    // lookup_radius: diameter wins; "Ignoring radius variable 'r' as
    // diameter 'd' is defined too."
    let (root, evaluator) = eval("sphere(r = 3, d = 10);");
    let Primitive::Sphere { r, .. } = first_primitive(&root) else {
        panic!("expected a sphere");
    };
    assert_eq!(*r, 5.0);
    assert!(has_warning(
        &evaluator,
        "Ignoring radius variable 'r' as diameter 'd' is defined too."
    ));
}

#[test]
fn sphere_no_args_defaults_radius_to_one() {
    let Primitive::Sphere { r, .. } = prim("sphere();") else {
        panic!("expected a sphere");
    };
    assert_eq!(r, 1.0);
}

#[test]
fn sphere_fn_forces_fragment_count() {
    // $fn > 0 forces the segment count (clamped up to 3); a hexagon here.
    let Primitive::Sphere { fragments, .. } = prim("sphere(5, $fn = 6);") else {
        panic!("expected a sphere");
    };
    assert_eq!(fragments, 6);
}

#[test]
fn sphere_default_fragments_derive_from_radius() {
    // $fn = 0 (default), $fs = 2, $fa = 12: ceil(min(360/12, r*2*PI/2), 5).
    // r = 1 -> min(30, PI) -> max(5) -> 5. r = 10 -> min(30, 10*PI) -> 30.
    let Primitive::Sphere { fragments, .. } = prim("sphere(1);") else {
        panic!("expected a sphere");
    };
    assert_eq!(fragments, 5);
    let Primitive::Sphere { fragments, .. } = prim("sphere(10);") else {
        panic!("expected a sphere");
    };
    assert_eq!(fragments, 30);
}

#[test]
fn sphere_scope_fn_propagates_into_fragment_count() {
    // A dynamically-scoped $fn reaches the primitive even without a named arg.
    let Primitive::Sphere { fragments, .. } = prim("$fn = 12; sphere(5);") else {
        panic!("expected a sphere");
    };
    assert_eq!(fragments, 12);
}

// --------------------------------------------------------------- cylinder

#[test]
fn cylinder_single_radius_sets_both_ends() {
    assert_eq!(
        prim("cylinder(h = 10, r = 4);"),
        Primitive::Cylinder {
            h: 10.0,
            r1: 4.0,
            r2: 4.0,
            center: false,
            fragments: 13
        }
    );
}

#[test]
fn cylinder_r1_r2_makes_a_cone() {
    let Primitive::Cylinder { h, r1, r2, .. } = prim("cylinder(h = 10, r1 = 5, r2 = 0);") else {
        panic!("expected a cylinder");
    };
    assert_eq!((h, r1, r2), (10.0, 5.0, 0.0));
}

#[test]
fn cylinder_d1_d2_halve_to_radii() {
    let Primitive::Cylinder { r1, r2, .. } = prim("cylinder(h = 2, d1 = 10, d2 = 4);") else {
        panic!("expected a cylinder");
    };
    assert_eq!((r1, r2), (5.0, 2.0));
}

#[test]
fn cylinder_d_sets_both_radii() {
    let Primitive::Cylinder { r1, r2, .. } = prim("cylinder(h = 2, d = 8);") else {
        panic!("expected a cylinder");
    };
    assert_eq!((r1, r2), (4.0, 4.0));
}

#[test]
fn cylinder_r_with_r1_warns_ambiguous_and_r1_overrides() {
    // "Cylinder parameters ambiguous"; assignment order lets r1 override r.
    let (root, evaluator) = eval("cylinder(h = 1, r = 2, r1 = 9);");
    let Primitive::Cylinder { r1, r2, .. } = first_primitive(&root) else {
        panic!("expected a cylinder");
    };
    assert_eq!((*r1, *r2), (9.0, 2.0));
    assert!(has_warning(&evaluator, "Cylinder parameters ambiguous"));
}

#[test]
fn cylinder_defaults_h_and_radii_to_one_center_false() {
    assert_eq!(
        prim("cylinder();"),
        Primitive::Cylinder {
            h: 1.0,
            r1: 1.0,
            r2: 1.0,
            center: false,
            fragments: 5
        }
    );
}

#[test]
fn cylinder_center_true() {
    let Primitive::Cylinder { center, .. } = prim("cylinder(h = 1, r = 1, center = true);") else {
        panic!("expected a cylinder");
    };
    assert!(center);
}

#[test]
fn cylinder_fragments_use_the_larger_radius() {
    // max(r1, r2) = 10 -> 30 fragments (as for a radius-10 circle).
    let Primitive::Cylinder { fragments, .. } = prim("cylinder(h = 1, r1 = 10, r2 = 1);") else {
        panic!("expected a cylinder");
    };
    assert_eq!(fragments, 30);
}

#[test]
fn cylinder_fn_gives_hexagonal_facets() {
    // $fn = 6 => 6 facets regardless of radius (the hexagon check).
    let Primitive::Cylinder { fragments, .. } = prim("cylinder(h = 10, r = 5, $fn = 6);") else {
        panic!("expected a cylinder");
    };
    assert_eq!(fragments, 6);
}

#[test]
fn cylinder_positional_h_r1_r2_center() {
    // Positional order is h, r1, r2, center.
    assert_eq!(
        prim("cylinder(10, 4, 2, true);"),
        Primitive::Cylinder {
            h: 10.0,
            r1: 4.0,
            r2: 2.0,
            center: true,
            fragments: 13
        }
    );
}

// ----------------------------------------------------------------- square

#[test]
fn square_scalar_and_vector_and_center() {
    assert_eq!(
        prim("square(5);"),
        Primitive::Square {
            size: [5.0, 5.0],
            center: false
        }
    );
    assert_eq!(
        prim("square([2, 3]);"),
        Primitive::Square {
            size: [2.0, 3.0],
            center: false
        }
    );
    assert_eq!(
        prim("square(4, center = true);"),
        Primitive::Square {
            size: [4.0, 4.0],
            center: true
        }
    );
}

#[test]
fn square_default_unit_size() {
    assert_eq!(
        prim("square();"),
        Primitive::Square {
            size: [1.0, 1.0],
            center: false
        }
    );
}

// ----------------------------------------------------------------- circle

#[test]
fn circle_radius_diameter_and_fragments() {
    let Primitive::Circle { r, fragments } = prim("circle(10);") else {
        panic!("expected a circle");
    };
    assert_eq!((r, fragments), (10.0, 30));

    let Primitive::Circle { r, .. } = prim("circle(d = 10);") else {
        panic!("expected a circle");
    };
    assert_eq!(r, 5.0);

    let Primitive::Circle { fragments, .. } = prim("circle(5, $fn = 6);") else {
        panic!("expected a circle");
    };
    assert_eq!(fragments, 6);
}

// ---------------------------------------------------------------- polygon

#[test]
fn polygon_points_only_has_no_paths() {
    assert_eq!(
        prim("polygon([[0, 0], [1, 0], [1, 1]]);"),
        Primitive::Polygon {
            points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
            paths: None,
            convexity: 1,
        }
    );
}

#[test]
fn polygon_with_paths_stores_index_lists() {
    assert_eq!(
        prim("polygon(points = [[0, 0], [1, 0], [1, 1]], paths = [[0, 1, 2]]);"),
        Primitive::Polygon {
            points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
            paths: Some(vec![vec![0, 1, 2]]),
            convexity: 1,
        }
    );
}

#[test]
fn polygon_convexity_is_stored_and_clamped() {
    let Primitive::Polygon { convexity, .. } =
        prim("polygon([[0, 0], [1, 0], [1, 1]], convexity = 5);")
    else {
        panic!("expected a polygon");
    };
    assert_eq!(convexity, 5);
}

#[test]
fn polygon_out_of_bounds_path_index_warns_and_is_skipped() {
    let (root, evaluator) = eval("polygon([[0, 0], [1, 0]], [[0, 1, 5]]);");
    let Primitive::Polygon { paths, .. } = first_primitive(&root) else {
        panic!("expected a polygon");
    };
    assert_eq!(paths, &Some(vec![vec![0, 1]]));
    assert!(has_warning(&evaluator, "Point index 5 is out of bounds"));
}

#[test]
fn polygon_missing_points_warns_and_is_empty() {
    let (root, evaluator) = eval("polygon();");
    assert_eq!(
        first_primitive(&root),
        &Primitive::Polygon {
            points: Vec::new(),
            paths: None,
            convexity: 1
        }
    );
    assert!(has_warning(
        &evaluator,
        "Unable to convert points = undef to a vector of coordinates"
    ));
}

// ------------------------------------------------------------- polyhedron

#[test]
fn polyhedron_points_and_faces() {
    assert_eq!(
        prim(
            "polyhedron(points = [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], \
             faces = [[0, 1, 2], [0, 1, 3], [1, 2, 3], [0, 2, 3]]);"
        ),
        Primitive::Polyhedron {
            points: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            faces: vec![vec![0, 1, 2], vec![0, 1, 3], vec![1, 2, 3], vec![0, 2, 3]],
            convexity: 1,
        }
    );
}

#[test]
fn polyhedron_legacy_triangles_alias_warns_and_is_used() {
    // triangles was removed on OpenSCAD master; kept here for backward compatibility
    // with a deprecation warning.
    let (root, evaluator) = eval(
        "polyhedron(points = [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], \
         triangles = [[0, 1, 2], [0, 1, 3], [1, 2, 3], [0, 2, 3]]);",
    );
    let Primitive::Polyhedron { faces, .. } = first_primitive(&root) else {
        panic!("expected a polyhedron");
    };
    assert_eq!(faces.len(), 4);
    assert!(has_warning(
        &evaluator,
        "polyhedron(triangles=[]) will be removed in future releases. Use polyhedron(faces=[]) instead."
    ));
}

#[test]
fn polyhedron_drops_faces_with_fewer_than_three_indices() {
    let (root, evaluator) = eval("polyhedron([[0, 0, 0], [1, 0, 0], [0, 1, 0]], [[0, 1]]);");
    let Primitive::Polyhedron { faces, .. } = first_primitive(&root) else {
        panic!("expected a polyhedron");
    };
    assert!(faces.is_empty());
    assert!(evaluator.errors().is_empty());
}

#[test]
fn polyhedron_pads_vec2_points_with_zero_z() {
    // getVec3(x, y, z, 0.0) accepts a 2D point, padding z = 0.
    let Primitive::Polyhedron { points, .. } =
        prim("polyhedron([[1, 2], [3, 4, 5]], [[0, 1, 0]]);")
    else {
        panic!("expected a polyhedron");
    };
    assert_eq!(points, vec![[1.0, 2.0, 0.0], [3.0, 4.0, 5.0]]);
}

// -------------------------------------------------------------- 2D vs 3D

#[test]
fn primitives_are_tagged_2d_or_3d() {
    assert!(prim("square(1);").is_2d());
    assert!(prim("circle(1);").is_2d());
    assert!(prim("polygon([[0, 0], [1, 0], [1, 1]]);").is_2d());
    assert!(!prim("cube(1);").is_2d());
    assert!(!prim("sphere(1);").is_2d());
    assert!(!prim("cylinder(h = 1, r = 1);").is_2d());
    assert!(!prim("polyhedron([[0, 0, 0], [1, 0, 0], [0, 1, 0]], [[0, 1, 2]]);").is_2d());
}

// ----------------------------------------------- positional argument filling

// Upstream `Parameters::parse(args, required, optional)` fills positional
// arguments across BOTH lists: `parse_without_defaults`
// (`src/core/Parameters.cc`, master, fetched 2026-07-03) loops
// `parameter_position < required.size() + optional.size()`, drawing the
// candidate from `required` first, then `optional` (lines 207-211). So an
// optional parameter (`d`, `d1`, `r1`...) IS positionally fillable — it is
// not name-only.

#[test]
fn sphere_second_positional_fills_diameter_and_d_wins_over_r() {
    // sphere(5, 3): required {"r"} takes 5, optional {"d"} takes 3
    // positionally; lookup_radius then lets the diameter win -> r = 1.5,
    // with the ignoring-radius warning. NOT a "too many unnamed arguments".
    let (root, evaluator) = eval("sphere(5, 3);");
    let Primitive::Sphere { r, .. } = first_primitive(&root) else {
        panic!("expected a sphere");
    };
    assert_eq!(*r, 1.5);
    assert!(has_warning(
        &evaluator,
        "Ignoring radius variable 'r' as diameter 'd' is defined too."
    ));
    assert!(!has_warning(&evaluator, "Too many unnamed arguments"));
}

#[test]
fn circle_second_positional_fills_diameter() {
    let (root, _) = eval("circle(5, 3);");
    let Primitive::Circle { r, .. } = first_primitive(&root) else {
        panic!("expected a circle");
    };
    assert_eq!(*r, 1.5);
}

#[test]
fn cylinder_fifth_positional_fills_r_and_warns_ambiguous() {
    // cylinder(1, 2, 3, true, 9): required {h,r1,r2,center} take the first
    // four, optional {r,d,d1,d2} take the fifth as r = 9. r-with-r1/r2 warns
    // "Cylinder parameters ambiguous", then r1/r2 override r -> r1=2, r2=3.
    let (root, evaluator) = eval("cylinder(1, 2, 3, true, 9);");
    let Primitive::Cylinder { r1, r2, .. } = first_primitive(&root) else {
        panic!("expected a cylinder");
    };
    assert_eq!((*r1, *r2), (2.0, 3.0));
    assert!(has_warning(&evaluator, "Cylinder parameters ambiguous"));
    assert!(!has_warning(&evaluator, "Too many unnamed arguments"));
}

#[test]
fn sphere_overflowing_positional_warns_too_many() {
    // Genuinely more positionals than required+optional slots (2 for sphere)
    // hits the "Too many unnamed arguments supplied" branch.
    let (_, evaluator) = eval("sphere(1, 2, 3);");
    assert!(has_warning(
        &evaluator,
        "Too many unnamed arguments supplied"
    ));
}

// --------------------------------------------- non-finite coordinate guard

// Upstream `builtin_polygon`/`builtin_polyhedron` guard each parsed point
// with `!std::isfinite(...)` (`primitives.cc` lines 671-672 and 433-434,
// master, fetched 2026-07-03), which is false for BOTH infinity and NaN —
// so a non-finite coordinate warns and becomes the origin, not retained.

#[test]
fn polygon_non_finite_point_becomes_origin() {
    let (root, evaluator) = eval("polygon([[1, 1/0], [2, 3]]);");
    let Primitive::Polygon { points, .. } = first_primitive(&root) else {
        panic!("expected a polygon");
    };
    assert_eq!(points, &vec![[0.0, 0.0], [2.0, 3.0]]);
    assert!(has_warning(&evaluator, "Unable to convert points[0]"));

    // NaN is treated the same as infinity (isfinite rejects both).
    let (root, _) = eval("polygon([[1, 0/0], [2, 3]]);");
    let Primitive::Polygon { points, .. } = first_primitive(&root) else {
        panic!("expected a polygon");
    };
    assert_eq!(points, &vec![[0.0, 0.0], [2.0, 3.0]]);
}

#[test]
fn polyhedron_non_finite_point_becomes_origin() {
    let (root, evaluator) = eval("polyhedron([[0, 0, 0/0], [1, 0, 0], [0, 1, 0]], [[0, 1, 2]]);");
    let Primitive::Polyhedron { points, .. } = first_primitive(&root) else {
        panic!("expected a polyhedron");
    };
    assert_eq!(points[0], [0.0, 0.0, 0.0]);
    assert!(has_warning(&evaluator, "Unable to convert points[0]"));
}

#[test]
fn face_non_number_index_warns_unable_to_convert() {
    // Upstream `builtin_polyhedron` inner loop (primitives.cc lines 459-461):
    // a non-NUMBER face entry logs "Unable to convert faces[i][j] = .. to a
    // number" and is skipped — the warning IS emitted upstream.
    let (_, evaluator) = eval("polyhedron([[0,0,0],[1,0,0],[0,1,0]], [[0, 1, \"x\"]]);");
    assert!(has_warning(
        &evaluator,
        "Unable to convert faces[0][2] = \"x\" to a number"
    ));
}

// -------------------------------------------------------------- modifiers

#[test]
fn primitive_carries_modifier_prefix() {
    let (root, _) = eval("#cube(1);");
    let CsgNode::Group { children, .. } = &root else {
        panic!("expected a group");
    };
    let CsgNode::Primitive { modifiers, .. } = &children[0] else {
        panic!("expected a primitive node");
    };
    assert!(modifiers.debug);
}
