//! Recursive resolution of an evaluated CsgNode tree into kernel geometry
//! (the pipeline integration point).
//!
//! [`resolve`] walks one [`morphe_eval::csg::CsgNode`] and turns it into a
//! kernel [`Geometry`] (a 3D [`Solid`] or a 2D [`Profile`]), driving the
//! primitive tessellators, the extrude ops, the boolean/hull combinators, and
//! the transforms. It is the consumer that ties the whole kernel together —
//! the evaluator produces the tree, this maps it onto geometry, and STL export
//! serializes the result.
//!
//! # 2D vs 3D grouping and mixing
//!
//! An n-ary combinator (`union`/`difference`/`intersection`/`hull`, and the
//! implicit union of a [`CsgNode::Group`]) can be either 2D or 3D depending on
//! its children. This resolver keys the operation's dimension off the **first
//! resolved child** and combines only same-dimension children with the matching
//! combinator (the 3D `Solid` functions or their `_profiles` twins). This ports
//! upstream OpenSCAD's `GeometryEvaluator::isValidDim` + `collectChildren2D/3D`
//! (`src/geometry/GeometryEvaluator.cc`, master, fetched 2026-07-03): upstream
//! seeds the operation dimension from the first non-background child, and any
//! later child of the *other* dimension emits `Warning: Mixing 2D and 3D
//! objects is not supported` and is dropped (`collectChildren{2D,3D}` skip the
//! off-dimension geometry). We emit that same warning once into the sink and
//! keep only the governing-dimension children.
//!
//! # Extrude of a 3D child
//!
//! `linear_extrude`/`rotate_extrude` need a 2D profile. Upstream warns
//! (`Ignoring 3D child object for 2D operation`) and treats such a child as
//! absent, yielding empty geometry. morphe instead surfaces it as a
//! [`KernelError::Resolve`] so the CLI reports the exact construct and exits
//! nonzero — a deliberate divergence toward a louder error, pinned by the
//! "extrude of a 3D child errors" test.
//!
//! # Empty geometry
//!
//! [`resolve`] returns `Ok(None)` when a node contributes no geometry: a
//! childless `union()`/`group`/… , a group all of whose children are empty, or
//! a transform/extrude over an empty child. `Ok(None)` means "no node"; an
//! `Ok(Some(g))` where `g` is itself empty (a `difference` that removed
//! everything) is a valid empty *body* the exporter still writes. The CLI turns
//! a top-level `None` into an empty STL.

use morphe_eval::csg::CsgNode;

use crate::profile::Profile;
use crate::tessellate::{
    apply_transform, difference, difference_profiles, hull, hull_profiles, intersection,
    intersection_profiles, linear_extrude, minkowski, minkowski_profiles, offset_profile,
    project_solid, rotate_extrude, tessellate_primitive, union, union_profiles, Geometry,
    KernelError,
};

/// The n-ary combinator that groups a node's children — the five CSG operators
/// plus the implicit union a [`CsgNode::Group`] and the top level apply.
#[derive(Clone, Copy)]
enum Combinator {
    Union,
    Difference,
    Intersection,
    Hull,
    Minkowski,
}

/// Maximum [`CsgNode`] nesting depth the resolver recurses through before it
/// errors instead of overflowing the native stack. The eval-side depth guards
/// (`MAX_EXPR_DEPTH`/`MAX_INST_DEPTH`) already bound any eval-produced tree, but
/// a **hand-built** `CsgNode` tree (this crate's tests and future direct
/// callers) bypasses those, so the resolver caps its own recursion. Sized to
/// match the eval-side guards.
const MAX_RESOLVE_DEPTH: usize = 2048;

/// Resolve a single [`CsgNode`] into kernel [`Geometry`].
///
/// Returns `Ok(None)` when the node contributes no geometry (see the module
/// docs on empty geometry), `Ok(Some(_))` for a resolved solid or profile, and
/// [`KernelError`] for a degenerate/non-manifold leaf, an unresolved
/// call/placeholder node, an extrude over a 3D child, or a tree nested deeper
/// than [`MAX_RESOLVE_DEPTH`].
///
/// Any resolver warning — 2D/3D mixing — is appended to `warnings` in tree
/// order (the CLI relays them to stderr).
///
/// Runs on the dedicated large-stack pipeline thread
/// ([`morphe_eval::stack::with_large_stack`]), so [`MAX_RESOLVE_DEPTH`] — not
/// the caller's native stack — bounds the recursion.
pub fn resolve(
    node: &CsgNode,
    warnings: &mut Vec<String>,
) -> Result<Option<Geometry>, KernelError> {
    morphe_eval::stack::with_large_stack(|| resolve_at(node, 0, warnings))
}

/// Depth-tracked recursion behind [`resolve`]. `depth` counts nesting levels so
/// a pathologically deep hand-built tree errors rather than overflowing the
/// stack (see [`MAX_RESOLVE_DEPTH`]).
fn resolve_at(
    node: &CsgNode,
    depth: usize,
    warnings: &mut Vec<String>,
) -> Result<Option<Geometry>, KernelError> {
    if depth > MAX_RESOLVE_DEPTH {
        return Err(KernelError::Resolve("tree nesting too deep".to_string()));
    }
    match node {
        CsgNode::Primitive { primitive, .. } => Ok(Some(tessellate_primitive(primitive)?)),
        CsgNode::Transform { kind, child, .. } => match resolve_at(child, depth + 1, warnings)? {
            Some(geom) => Ok(Some(apply_transform(kind, geom)?)),
            None => Ok(None),
        },
        CsgNode::Group { children, .. } => combine(children, Combinator::Union, depth, warnings),
        CsgNode::Union { children, .. } => combine(children, Combinator::Union, depth, warnings),
        CsgNode::Difference { children, .. } => {
            combine(children, Combinator::Difference, depth, warnings)
        }
        CsgNode::Intersection { children, .. } => {
            combine(children, Combinator::Intersection, depth, warnings)
        }
        CsgNode::Hull { children, .. } => combine(children, Combinator::Hull, depth, warnings),
        // Minkowski composes like the booleans — the
        // resolver's first-child dimensionality rule governs (off-dimension
        // children warn + drop), then the per-dimension sum.
        CsgNode::Minkowski { children, .. } => {
            combine(children, Combinator::Minkowski, depth, warnings)
        }
        CsgNode::LinearExtrude { params, child, .. } => {
            match resolve_profile_child(child, "linear_extrude", depth, warnings)? {
                Some(profile) => Ok(Some(Geometry::Solid(linear_extrude(&profile, params)?))),
                None => Ok(None),
            }
        }
        CsgNode::RotateExtrude { params, child, .. } => {
            match resolve_profile_child(child, "rotate_extrude", depth, warnings)? {
                Some(profile) => Ok(Some(Geometry::Solid(rotate_extrude(&profile, params)?))),
                None => Ok(None),
            }
        }
        // Offset maps a 2D profile to a grown/shrunk 2D profile
        // (round/miter/bevel joins resolved at eval); hand-built node tests
        // also exercise the arm directly.
        CsgNode::Offset { params, child, .. } => {
            match resolve_profile_child(child, "offset", depth, warnings)? {
                Some(profile) => Ok(Some(Geometry::Profile(offset_profile(&profile, params)?))),
                None => Ok(None),
            }
        }
        // Projection maps 3D solids to a 2D profile — the cut
        // (z=0 section) or the shadow (union of all sections). Children
        // resolve as 3D ONLY: 2D children warn + drop (upstream
        // `collectChildren3D` "Ignoring 2D child object"), the solids union,
        // then project. No resolved children (or an empty union) is NO
        // geometry (Ok(None)); a projection that misses (cut above the
        // solid) is a valid EMPTY profile — downstream distinguishes.
        CsgNode::Projection { cut, child, .. } => {
            let mut solids = Vec::new();
            let group_children: &[CsgNode] = match child.as_ref() {
                CsgNode::Group { children, .. } => children,
                other => std::slice::from_ref(other),
            };
            for kid in group_children {
                match resolve_at(kid, depth + 1, warnings)? {
                    None => {}
                    Some(Geometry::Solid(s)) => solids.push(s),
                    Some(Geometry::Profile(_)) => {
                        warnings.push("Ignoring 2D child object for projection".to_string())
                    }
                }
            }
            match union(&solids)? {
                None => Ok(None),
                Some(solid) => Ok(Some(Geometry::Profile(project_solid(&solid, *cut)?))),
            }
        }
        // An unresolved builtin call or a `let`-placeholder should have become an
        // EvalError before reaching the kernel; defend against a tree that still
        // carries one.
        // import() opens the resolved absolute path the eval
        // constructor pinned; a missing or rejected
        // file errors loudly naming that path.
        CsgNode::Import { path, .. } => Ok(Some(Geometry::Solid(crate::import::load_stl(path)?))),
        // surface() reads the resolved absolute path, parses the
        // .dat heightfield, and builds the solid (upstream SurfaceNode
        // geometry ported in surface.rs). Missing/rejected file errors
        // loudly naming that path.
        CsgNode::Surface {
            path,
            center,
            invert,
            ..
        } => {
            // The size cap, enforced by ONE open +
            // regular-file check + bounded read (`read_capped`; the old
            // stat-then-reopen let `/dev/zero` through and hung on FIFOs).
            // Pinned by surface's read_capped tests.
            let bytes = crate::read_capped(path, "surface", crate::surface::MAX_DAT_BYTES)?;
            let text = String::from_utf8(bytes).map_err(|e| {
                KernelError::Resolve(format!("`surface` cannot read `{}`: {e}", path.display()))
            })?;
            let grid = crate::surface::parse_dat(&text)?;
            Ok(Some(Geometry::Solid(crate::surface::surface_solid(
                &grid, *center, *invert,
            )?)))
        }
        // text() lays the glyph run out as a 2D Profile — the
        // 2D geometry leaf `linear_extrude` lifts (children contribute
        // nothing, like any primitive).
        CsgNode::Text { params, .. } => Ok(Some(Geometry::Profile(crate::text::text_profile(
            &params.text,
            params,
        )?))),
        CsgNode::Call { name, .. } => Err(KernelError::Resolve(format!(
            "unresolved module call `{name}` reached the kernel (should have errored at eval time)"
        ))),
        CsgNode::Unresolved { kind, .. } => Err(KernelError::Resolve(format!(
            "unresolved `{kind}` node reached the kernel (should have errored at eval time)"
        ))),
    }
}

/// Resolve a slice of top-level [`CsgNode`]s into one [`Geometry`] — the
/// implicit union OpenSCAD applies over a file's top-level statements. Equal to
/// [`resolve`] of a [`CsgNode::Group`] wrapping the same nodes.
///
/// `Ok(None)` when nothing resolves (an empty file); otherwise the union of all
/// resolved top-level geometries (2D or 3D per the first resolved node — see the
/// module docs). Runs on the large-stack pipeline thread like [`resolve`].
pub fn resolve_root(
    nodes: &[CsgNode],
    warnings: &mut Vec<String>,
) -> Result<Option<Geometry>, KernelError> {
    morphe_eval::stack::with_large_stack(|| combine(nodes, Combinator::Union, 0, warnings))
}

/// Resolve the child of a `linear_extrude`/`rotate_extrude` to a 2D [`Profile`].
///
/// `None` when the child contributes no geometry (nothing to sweep); a
/// [`KernelError::Resolve`] naming `op` when the child resolves to a 3D solid
/// (see the module docs on extrude-of-3D).
fn resolve_profile_child(
    child: &CsgNode,
    op: &str,
    depth: usize,
    warnings: &mut Vec<String>,
) -> Result<Option<Profile>, KernelError> {
    match resolve_at(child, depth + 1, warnings)? {
        None => Ok(None),
        Some(Geometry::Profile(profile)) => Ok(Some(profile)),
        Some(Geometry::Solid(_)) => Err(KernelError::Resolve(format!(
            "{op} expects a 2D profile child but its child resolved to a 3D solid"
        ))),
    }
}

/// Resolve every child, group the results by dimensionality, and apply `comb`.
///
/// The operation's dimension is the first resolved child's; a child of the other
/// dimension warns (`Mixing 2D and 3D objects is not supported`) and is dropped
/// (upstream `isValidDim` + `collectChildren2D/3D`). All-2D children compose
/// into a [`Profile`] via the `_profiles` combinators; all-3D into a [`Solid`].
fn combine(
    children: &[CsgNode],
    comb: Combinator,
    depth: usize,
    warnings: &mut Vec<String>,
) -> Result<Option<Geometry>, KernelError> {
    let mut geoms = Vec::with_capacity(children.len());
    for child in children {
        if let Some(geom) = resolve_at(child, depth + 1, warnings)? {
            geoms.push(geom);
        }
    }
    if geoms.is_empty() {
        return Ok(None);
    }

    // The first resolved child seeds the operation's dimension (upstream
    // `isValidDim`); off-dimension children are dropped with one mixing warning.
    let governing_3d = matches!(geoms[0], Geometry::Solid(_));
    let mut solids = Vec::new();
    let mut profiles = Vec::new();
    let mut mixed = false;
    for geom in geoms {
        match geom {
            Geometry::Solid(s) => {
                if governing_3d {
                    solids.push(s);
                } else {
                    mixed = true;
                }
            }
            Geometry::Profile(p) => {
                if governing_3d {
                    mixed = true;
                } else {
                    profiles.push(p);
                }
            }
        }
    }
    if mixed {
        warnings.push("Mixing 2D and 3D objects is not supported".to_string());
    }

    // The governing-dimension bucket holds at least the first child, so the
    // combinator always sees a non-empty slice and returns `Some`.
    if governing_3d {
        let result = match comb {
            Combinator::Union => union(&solids)?,
            Combinator::Difference => difference(&solids)?,
            Combinator::Intersection => intersection(&solids)?,
            Combinator::Hull => hull(&solids)?,
            Combinator::Minkowski => minkowski(&solids)?,
        };
        Ok(result.map(Geometry::Solid))
    } else {
        let result = match comb {
            Combinator::Union => union_profiles(&profiles)?,
            Combinator::Difference => difference_profiles(&profiles)?,
            Combinator::Intersection => intersection_profiles(&profiles)?,
            Combinator::Hull => hull_profiles(&profiles)?,
            Combinator::Minkowski => minkowski_profiles(&profiles)?,
        };
        Ok(result.map(Geometry::Profile))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use morphe_eval::csg::{
        Fragments, LinearExtrudeParams, OffsetJoin, OffsetParams, Primitive, TransformKind,
    };
    use morphe_lang::ast::Modifiers;

    // ── tree constructors (hand-built, no parse/eval needed) ────────────────

    fn prim(primitive: Primitive) -> CsgNode {
        CsgNode::Primitive {
            primitive,
            modifiers: Modifiers::default(),
        }
    }

    fn cube(size: [f64; 3], center: bool) -> CsgNode {
        prim(Primitive::Cube { size, center })
    }

    fn circle(r: f64, fragments: u32) -> CsgNode {
        prim(Primitive::Circle { r, fragments })
    }

    fn group(children: Vec<CsgNode>) -> CsgNode {
        CsgNode::Group {
            children,
            modifiers: Modifiers::default(),
        }
    }

    fn translate(t: [f64; 3], child: CsgNode) -> CsgNode {
        let m = [
            [1.0, 0.0, 0.0, t[0]],
            [0.0, 1.0, 0.0, t[1]],
            [0.0, 0.0, 1.0, t[2]],
            [0.0, 0.0, 0.0, 1.0],
        ];
        CsgNode::Transform {
            kind: TransformKind::Affine(m),
            child: Box::new(group(vec![child])),
            modifiers: Modifiers::default(),
        }
    }

    fn frags(fn_: f64) -> Fragments {
        Fragments {
            fn_,
            fs: 2.0,
            fa: 12.0,
        }
    }

    /// Resolve a node, panicking on error / empty — the common test path.
    fn resolve_some(node: &CsgNode) -> Geometry {
        let mut w = Vec::new();
        resolve(node, &mut w)
            .expect("node resolves without error")
            .expect("node produces geometry")
    }

    fn solid_of(node: &CsgNode) -> Solid {
        match resolve_some(node) {
            Geometry::Solid(s) => s,
            Geometry::Profile(_) => panic!("expected a 3D solid, got a 2D profile"),
        }
    }

    use crate::Solid;

    // ── primitive ───────────────────────────────────────────────────────────

    #[test]
    fn primitive_resolves_to_solid() {
        let s = solid_of(&cube([2.0, 3.0, 4.0], false));
        assert!(s.is_manifold());
        assert!((s.volume() - 24.0).abs() < 1e-9);
    }

    #[test]
    fn primitive_2d_resolves_to_profile() {
        let g = resolve_some(&circle(5.0, 32));
        assert!(matches!(g, Geometry::Profile(_)));
    }

    // ── transform of primitive ────────────────────────────────────────────

    #[test]
    fn transform_of_primitive_preserves_volume_and_moves() {
        let s = solid_of(&translate([10.0, 0.0, 0.0], cube([1.0, 1.0, 1.0], false)));
        assert!(
            (s.volume() - 1.0).abs() < 1e-9,
            "translate preserves volume"
        );
        let (verts, _) = s.to_mesh();
        let min_x = verts.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min);
        assert!(
            (min_x - 10.0).abs() < 1e-9,
            "cube moved to x>=10, got {min_x}"
        );
    }

    // ── booleans ──────────────────────────────────────────────────────────

    #[test]
    fn difference_of_two_cubes_has_correct_volume() {
        // 3^3 box centred on origin minus a 1^3 box fully inside it → 27 - 1.
        let node = CsgNode::Difference {
            children: vec![cube([3.0, 3.0, 3.0], true), cube([1.0, 1.0, 1.0], true)],
            modifiers: Modifiers::default(),
        };
        let s = solid_of(&node);
        assert!(s.is_manifold());
        assert!(
            (s.volume() - 26.0).abs() < 1e-6,
            "27 - 1, got {}",
            s.volume()
        );
    }

    #[test]
    fn union_of_two_disjoint_cubes_sums_volume() {
        let node = CsgNode::Union {
            children: vec![
                cube([1.0, 1.0, 1.0], false),
                translate([5.0, 0.0, 0.0], cube([2.0, 2.0, 2.0], false)),
            ],
            modifiers: Modifiers::default(),
        };
        let s = solid_of(&node);
        assert!(
            (s.volume() - (1.0 + 8.0)).abs() < 1e-6,
            "1 + 8, got {}",
            s.volume()
        );
    }

    #[test]
    fn intersection_of_overlapping_cubes() {
        // [0,2]^3 ∩ [1,3]^3 → [1,2]^3, volume 1.
        let node = CsgNode::Intersection {
            children: vec![
                cube([2.0, 2.0, 2.0], false),
                translate([1.0, 1.0, 1.0], cube([2.0, 2.0, 2.0], false)),
            ],
            modifiers: Modifiers::default(),
        };
        let s = solid_of(&node);
        assert!(
            (s.volume() - 1.0).abs() < 1e-6,
            "overlap is 1, got {}",
            s.volume()
        );
    }

    // ── linear_extrude ────────────────────────────────────────────────────

    #[test]
    fn linear_extrude_of_circle_matches_cylinder_volume() {
        let r = 5.0;
        let h = 10.0;
        let fn_ = 32;
        let extruded = solid_of(&CsgNode::LinearExtrude {
            params: LinearExtrudeParams {
                height: h,
                center: false,
                twist: 0.0,
                slices: None,
                scale: [1.0, 1.0],
                convexity: 1,
                fragments: frags(fn_ as f64),
            },
            child: Box::new(group(vec![circle(r, fn_)])),
            modifiers: Modifiers::default(),
        });
        // A cylinder primitive with the same radius/height/facets is the same
        // prism, so their volumes must agree closely.
        let cyl = solid_of(&prim(Primitive::Cylinder {
            h,
            r1: r,
            r2: r,
            center: false,
            fragments: fn_,
        }));
        assert!(extruded.is_manifold());
        assert!(extruded.volume() > 0.0);
        assert!(
            (extruded.volume() - cyl.volume()).abs() < 1e-6,
            "extruded 32-gon {} vs cylinder {}",
            extruded.volume(),
            cyl.volume()
        );
    }

    #[test]
    fn linear_extrude_of_3d_child_errors() {
        let mut w = Vec::new();
        let node = CsgNode::LinearExtrude {
            params: LinearExtrudeParams {
                height: 10.0,
                center: false,
                twist: 0.0,
                slices: None,
                scale: [1.0, 1.0],
                convexity: 1,
                fragments: frags(0.0),
            },
            // A cube is 3D — extrude cannot lift it.
            child: Box::new(group(vec![cube([1.0, 1.0, 1.0], false)])),
            modifiers: Modifiers::default(),
        };
        assert!(
            matches!(resolve(&node, &mut w), Err(KernelError::Resolve(_))),
            "extrude of a 3D child must be a Resolve error"
        );
    }

    // ── nested tree ───────────────────────────────────────────────────────

    #[test]
    fn nested_difference_of_union_and_translate() {
        // (cube[0,4]^3 ∪ nothing-new) − (translated 2^3 cube inside) — a real
        // multi-level tree: difference[ union[big, big2], translate(small) ].
        let big = cube([4.0, 4.0, 4.0], false); // volume 64
        let big2 = translate([3.0, 0.0, 0.0], cube([2.0, 4.0, 4.0], false)); // adds [4,5]x… = 16
        let hole = translate([1.0, 1.0, 1.0], cube([2.0, 2.0, 2.0], false)); // 8, fully inside big
        let node = CsgNode::Difference {
            children: vec![
                CsgNode::Union {
                    children: vec![big, big2],
                    modifiers: Modifiers::default(),
                },
                hole,
            ],
            modifiers: Modifiers::default(),
        };
        let s = solid_of(&node);
        assert!(s.is_manifold());
        // union volume 64 + 16 = 80 (disjoint x-extension), minus 8 hole = 72.
        assert!((s.volume() - 72.0).abs() < 1e-6, "got {}", s.volume());
    }

    // ── depth guard ───────────────────────────────────────────────────────

    #[test]
    fn resolve_deep_tree_errors_not_overflows() {
        // A ~3000-deep hand-built Transform/Union chain — deeper than
        // MAX_RESOLVE_DEPTH — must error cleanly rather than overflow the stack.
        // Run on a generous stack so a *missing* guard would still overflow
        // (proving the guard, not the stack size, is what stops the recursion).
        let handle = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let mut node = cube([1.0, 1.0, 1.0], false);
                for i in 0..3000 {
                    node = if i % 2 == 0 {
                        translate([0.0, 0.0, 1.0], node)
                    } else {
                        CsgNode::Union {
                            children: vec![node],
                            modifiers: Modifiers::default(),
                        }
                    };
                }
                let mut w = Vec::new();
                matches!(resolve(&node, &mut w), Err(KernelError::Resolve(_)))
            })
            .expect("spawn resolver thread");
        assert!(
            handle
                .join()
                .expect("resolver thread must not overflow the stack"),
            "a too-deep tree must return a Resolve error"
        );
    }

    #[test]
    fn resolve_legal_deep_tree_on_small_caller_stack() {
        // A tree just under MAX_RESOLVE_DEPTH, resolved from a plain 2 MiB
        // thread (the desktop's spawn_blocking size): resolve_root runs on
        // its own large-stack thread, so this must succeed, not overflow.
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let mut node = cube([1.0, 1.0, 1.0], false);
                for _ in 0..2000 {
                    node = CsgNode::Union {
                        children: vec![node],
                        modifiers: Modifiers::default(),
                    };
                }
                let mut w = Vec::new();
                let ok = matches!(
                    resolve_root(std::slice::from_ref(&node), &mut w),
                    Ok(Some(Geometry::Solid(_)))
                );
                // Drop the deep tree on the big stack too.
                morphe_eval::stack::with_large_stack(move || drop(node));
                ok
            })
            .expect("spawn resolver thread");
        assert!(
            handle
                .join()
                .expect("must not overflow a 2 MiB caller stack"),
            "a legal-depth tree must resolve"
        );
    }

    // ── multiple top-level nodes ──────────────────────────────────────────

    #[test]
    fn resolve_root_unions_top_level_nodes() {
        let mut w = Vec::new();
        let nodes = vec![
            cube([1.0, 1.0, 1.0], false),
            translate([5.0, 0.0, 0.0], cube([2.0, 2.0, 2.0], false)),
        ];
        let g = resolve_root(&nodes, &mut w).unwrap().unwrap();
        match g {
            Geometry::Solid(s) => {
                assert!((s.volume() - 9.0).abs() < 1e-6, "1 + 8, got {}", s.volume())
            }
            Geometry::Profile(_) => panic!("expected a solid"),
        }
    }

    #[test]
    fn resolve_root_empty_is_none() {
        let mut w = Vec::new();
        assert!(resolve_root(&[], &mut w).unwrap().is_none());
    }

    // ── 2D-only top level ─────────────────────────────────────────────────

    #[test]
    fn resolve_root_2d_only_is_profile() {
        let mut w = Vec::new();
        let g = resolve_root(&[circle(5.0, 32)], &mut w).unwrap().unwrap();
        assert!(matches!(g, Geometry::Profile(_)), "2D top level stays 2D");
    }

    // ── mixing 2D and 3D ──────────────────────────────────────────────────

    #[test]
    fn union_mixing_2d_and_3d_warns_and_keeps_first_dimension() {
        let mut w = Vec::new();
        // First child is 3D → the op is 3D, the 2D circle is dropped with a warn.
        let node = CsgNode::Union {
            children: vec![cube([2.0, 2.0, 2.0], false), circle(5.0, 32)],
            modifiers: Modifiers::default(),
        };
        let g = resolve(&node, &mut w).unwrap().unwrap();
        assert!(matches!(g, Geometry::Solid(_)), "governing dimension is 3D");
        assert!(
            w.iter().any(|m| m.contains("Mixing 2D and 3D")),
            "expected a mixing warning, got {w:?}"
        );
    }

    #[test]
    fn call_node_is_a_resolve_error() {
        let mut w = Vec::new();
        let node = CsgNode::Call {
            name: "minkowski".to_string(),
            args: vec![],
            children: vec![],
            modifiers: Modifiers::default(),
            span: morphe_lang::lexer::Span { start: 0, end: 0 },
        };
        assert!(matches!(
            resolve(&node, &mut w),
            Err(KernelError::Resolve(_))
        ));
    }

    // -----------------------------------------------------------------------
    // The Offset resolve arm
    // -----------------------------------------------------------------------

    #[test]
    fn offset_resolve_arm_grows_a_profile() {
        // A hand-built Offset node resolves through the real dispatch
        // to a grown Profile.
        let node = CsgNode::Offset {
            params: OffsetParams {
                delta: 1.0,
                join: OffsetJoin::Miter,
                chamfer: false,
                fragments: Fragments {
                    fn_: 0.0,
                    fs: 2.0,
                    fa: 12.0,
                },
            },
            child: Box::new(CsgNode::Primitive {
                primitive: Primitive::Square {
                    size: [10.0, 10.0],
                    center: false,
                },
                modifiers: Default::default(),
            }),
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        match resolve_root(&[node], &mut warnings) {
            Ok(Some(Geometry::Profile(p))) => {
                let area = p.area();
                assert!(
                    (area - 144.0).abs() / 144.0 < 0.001,
                    "miter grow 10→12 per side: {area} vs 144"
                );
            }
            Ok(None) => panic!("offset resolved to None (expected a profile)"),
            Ok(Some(Geometry::Solid(_))) => panic!("offset must stay 2D (resolved to a Solid)"),
            Err(e) => panic!("offset resolve errored: {e}"),
        }
    }

    #[test]
    fn offset_of_3d_child_errors_loudly() {
        // A 3D child is a loud KernelError::Resolve — never a silent
        // drop or empty pass.
        let node = CsgNode::Offset {
            params: OffsetParams {
                delta: 1.0,
                join: OffsetJoin::Round,
                chamfer: false,
                fragments: Fragments {
                    fn_: 0.0,
                    fs: 2.0,
                    fa: 12.0,
                },
            },
            child: Box::new(CsgNode::Primitive {
                primitive: Primitive::Cube {
                    size: [5.0, 5.0, 5.0],
                    center: false,
                },
                modifiers: Default::default(),
            }),
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        let result = resolve_root(&[node], &mut warnings);
        assert!(matches!(
            &result,
            Err(KernelError::Resolve(msg)) if msg.contains("offset expects a 2D profile child")
        ));
    }

    // -----------------------------------------------------------------------
    // The Projection resolve arm
    // -----------------------------------------------------------------------

    #[test]
    fn projection_resolve_arm_produces_profile() {
        // Through the real dispatch: projection(cut=true) over a translated
        // cube → a 10x10 Profile (area 100).
        let node = CsgNode::Projection {
            cut: true,
            child: Box::new(CsgNode::Group {
                children: vec![CsgNode::Transform {
                    kind: TransformKind::Affine([
                        [1.0, 0.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, -1.0],
                        [0.0, 0.0, 0.0, 1.0],
                    ]),
                    child: Box::new(CsgNode::Primitive {
                        primitive: Primitive::Cube {
                            size: [10.0, 10.0, 10.0],
                            center: false,
                        },
                        modifiers: Default::default(),
                    }),
                    modifiers: Default::default(),
                }],
                modifiers: Default::default(),
            }),
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        match resolve_root(&[node], &mut warnings) {
            Ok(Some(Geometry::Profile(p))) => {
                assert!((p.area() - 100.0).abs() / 100.0 < 1e-9);
            }
            Ok(None) => panic!("projection resolved to None"),
            Ok(Some(Geometry::Solid(_))) => panic!("projection must be 2D"),
            Err(e) => panic!("projection resolve errored: {e}"),
        }
    }

    #[test]
    fn projection_of_2d_children_warns_and_drops() {
        // A 2D-only child set warns + drops → no geometry.
        let node = CsgNode::Projection {
            cut: false,
            child: Box::new(CsgNode::Group {
                children: vec![CsgNode::Primitive {
                    primitive: Primitive::Square {
                        size: [10.0, 10.0],
                        center: false,
                    },
                    modifiers: Default::default(),
                }],
                modifiers: Default::default(),
            }),
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        let result = resolve_root(&[node], &mut warnings);
        assert!(
            matches!(result, Ok(None)),
            "2D child dropped (Ok(None) expected)"
        );
        assert!(
            warnings.iter().any(|w| w.contains("Ignoring 2D child")),
            "warned: {warnings:?}"
        );
    }

    // -----------------------------------------------------------------------
    // The Minkowski resolve arm
    // -----------------------------------------------------------------------

    #[test]
    fn minkowski_resolve_arm_sums_cubes() {
        // Through the real dispatch: cube(10) ⊕ cube(10) → the exact 20-cube.
        let node = CsgNode::Minkowski {
            children: vec![
                cube([10.0, 10.0, 10.0], false),
                cube([10.0, 10.0, 10.0], false),
            ],
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        match resolve_root(&[node], &mut warnings) {
            Ok(Some(Geometry::Solid(s))) => {
                assert!((s.volume() - 8000.0).abs() / 8000.0 < 1e-9);
            }
            Ok(None) => panic!("minkowski resolved to None"),
            Ok(Some(Geometry::Profile(_))) => panic!("3D sum must be a Solid"),
            Err(e) => panic!("minkowski resolve errored: {e}"),
        }
    }

    #[test]
    fn minkowski_2d_via_prism_identity_resolves() {
        // The 2D path through the arm: square ⊕ circle → Profile at the
        // Steiner area (2% at $fn=64).
        let node = CsgNode::Minkowski {
            children: vec![
                prim(Primitive::Square {
                    size: [10.0, 10.0],
                    center: false,
                }),
                prim(Primitive::Circle {
                    r: 2.0,
                    fragments: 64,
                }),
            ],
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        match resolve_root(&[node], &mut warnings) {
            Ok(Some(Geometry::Profile(p))) => {
                let expected = 100.0 + 80.0 + std::f64::consts::PI * 4.0;
                assert!((p.area() - expected).abs() / expected < 0.02);
            }
            Ok(None) => panic!("minkowski 2D resolved to None"),
            Ok(Some(Geometry::Solid(_))) => panic!("2D sum must be a Profile"),
            Err(e) => panic!("minkowski 2D errored: {e}"),
        }
    }

    #[test]
    fn minkowski_mixing_2d_3d_warns_and_drops() {
        // The first-child dimensionality rule — a 3D first child keeps
        // 3D and warns + drops the 2D later child.
        let node = CsgNode::Minkowski {
            children: vec![
                cube([5.0, 5.0, 5.0], false),
                prim(Primitive::Square {
                    size: [10.0, 10.0],
                    center: false,
                }),
            ],
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        match resolve_root(&[node], &mut warnings) {
            Ok(Some(Geometry::Solid(s))) => {
                // cube(5) alone (the square dropped) — passthrough identity.
                assert!((s.volume() - 125.0).abs() / 125.0 < 1e-9);
            }
            Ok(None) => panic!("minkowski resolved to None"),
            Ok(Some(Geometry::Profile(_))) => panic!("3D first child must govern"),
            Err(e) => panic!("minkowski errored: {e}"),
        }
        assert!(
            warnings.iter().any(|w| w.contains("Mixing 2D and 3D")),
            "warned: {warnings:?}"
        );
    }

    // ── surface() resolve arm ───────────────────────────────────────────────

    #[test]
    fn surface_resolve_builds_heightfield_solid() {
        // A 2x2 grid of 2.0 → a 1x1 footprint box, z from 0 to 2 → volume 2.
        let dir = std::env::temp_dir().join(format!("morphe-s25-2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("h.dat");
        std::fs::write(&path, "2.0 2.0\n2.0 2.0\n").expect("fixture write");

        let node = CsgNode::Surface {
            path: path.clone(),
            center: false,
            invert: false,
            convexity: 1,
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        match resolve_root(&[node], &mut warnings) {
            Ok(Some(Geometry::Solid(s))) => {
                assert!(
                    (s.volume() - 2.0).abs() < 1e-9,
                    "volume 2, got {}",
                    s.volume()
                );
            }
            Ok(Some(Geometry::Profile(_))) => panic!("expected a solid, got a profile"),
            Ok(None) => panic!("expected a solid, got empty"),
            Err(e) => panic!("surface resolve errored: {e}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn surface_missing_file_errors_naming_resolved_path() {
        let node = CsgNode::Surface {
            path: PathBuf::from("/definitely/not/here/heightmap.dat"),
            center: false,
            invert: false,
            convexity: 1,
            modifiers: Default::default(),
        };
        let mut warnings = Vec::new();
        let err = match resolve_root(&[node], &mut warnings) {
            Ok(Some(_)) => panic!("missing file must error, got a solid"),
            Ok(None) => panic!("missing file must error, got empty"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/definitely/not/here/heightmap.dat"),
            "error must name the resolved path: {msg}"
        );
    }
}
