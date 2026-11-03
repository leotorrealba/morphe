//! Tessellation of resolved primitives into geometry.
//!
//! [`tessellate_primitive`] turns the evaluator's resolved
//! [`morphe_eval::csg::Primitive`] leaves into a [`Geometry`] — a watertight
//! [`Solid`] for the four 3D primitives (`cube`/`sphere`/`cylinder`/
//! `polyhedron`) or a 2D [`Profile`] for the three cross-section primitives
//! (`square`/`circle`/`polygon`) —
//! porting upstream OpenSCAD's vertex/contour generation verbatim from
//! `src/core/primitives.cc` (master, fetched 2026-07-03).
//!
//! # 2D cross-sections
//!
//! `square`/`circle`/`polygon` are OpenSCAD's 2D primitives; upstream builds an
//! `Outline2d` for each (`primitives.cc` `SquareNode`/`CircleNode`/
//! `PolygonNode::createGeometry`). This module reproduces those outlines as
//! contours and hands them to [`Profile::from_contours`]:
//!
//! - `square([x,y], center)` is a single rectangular contour; `center` shifts
//!   its minimum corner from the origin to `[-x/2, -y/2]` (upstream
//!   `SquareNode`, which offsets the four corners by `-size/2` when centred).
//! - `circle(r, fragments)` is **not** a true circle but a regular
//!   `fragments`-gon: upstream `CircleNode::createGeometry` emits exactly
//!   `fragments` vertices at angles `2π·i/fragments` on radius `r`, so
//!   `circle(r, $fn=6)` is a hexagon of area `(3√3/2)r²`, not `πr²`.
//! - `polygon(points, paths, convexity)` is one contour per path (or the whole
//!   point list when `paths` is omitted). Upstream `PolygonNode` adds one
//!   `Outline2d` per path to a `Polygon2d` filled with the **even-odd** rule,
//!   so a nested path becomes a hole regardless of its winding — reproduced
//!   here with [`FillRule::EvenOdd`].
//!
//! # Winding convention
//!
//! Manifold — like OpenSCAD's `PolySet` — orders each face **counter-clockwise
//! when viewed from outside** (outward normals by the right-hand rule). The
//! cube face `{4, 5, 7, 6}` (`primitives.cc` L126) and this crate's own
//! `unit_cube_mesh` fixture both use that convention, so `cube`/`sphere`/
//! `cylinder` reuse upstream's face order as-is. `polyhedron` is the exception:
//! its *input* faces are clockwise-when-viewed-from-outside (per the OpenSCAD
//! manual), which upstream reverses (`primitives.cc` L407 `std::reverse`) into
//! the outward convention — so this module reverses each polyhedron face too.
//!
//! # Degenerate parameters
//!
//! Upstream returns empty geometry (and sometimes warns) for degenerate params
//! — non-positive `cube` size, `r <= 0` sphere, `h <= 0` / both-radii-zero
//! cylinder (`primitives.cc` L102/L179/L253). This kernel instead surfaces
//! those as [`KernelError::Degenerate`] so the caller decides; a collinear or
//! otherwise non-manifold `polyhedron` is rejected by Manifold and surfaces as
//! [`KernelError::Solid`].

use manifold3d::{CrossSection, JoinType, Manifold, OpType};
use morphe_eval::csg::{
    Affine, Fragments, LinearExtrudeParams, OffsetJoin, OffsetParams, Primitive,
    RotateExtrudeParams, TransformKind,
};
use morphe_eval::eval::resolve_fragments;

use crate::profile::{FillRule, Profile};
use crate::solid::{Solid, SolidError};

/// Hard ceiling on the circular fragment count of a `circle`/`cylinder`
/// primitive, an `offset` round-join arc, or a `text` glyph run's resolved
/// count.
///
/// A morphe hardening with no upstream analogue (the twin of
/// [`MAX_EXTRUDE_SLICES`]/[`MAX_REVOLVE_SEGMENTS`]): `$fn=1e7`, or `$fa`/`$fs`
/// at their `F_MINIMUM` floor over a huge radius, would otherwise make the
/// tessellator allocate a runaway buffer and OOM/abort. A count above the cap
/// is a loud [`KernelError::Resolve`] naming the construct — never a silently
/// coarser shape. Real models use a few hundred fragments at most (the corpus
/// tops out at `$fn=256`), so `100_000` never trips on legal input.
pub const MAX_PRIMITIVE_FRAGMENTS: u32 = 100_000;

/// Hard ceiling on a `sphere`'s vertex count (`ceil(fragments/2) ·
/// fragments`), checked before any buffer is allocated.
///
/// A sphere's mesh grows **quadratically** in its fragment count, so the
/// linear [`MAX_PRIMITIVE_FRAGMENTS`] cap alone still admits ~5·10⁹ vertices.
/// `4_000_000` vertices (≈ `$fn=2828`) is far finer than any printable model
/// needs while keeping the worst-case mesh to a few hundred MB.
pub const MAX_SPHERE_VERTICES: usize = 4_000_000;

/// Fail loudly when a resolved fragment count exceeds
/// [`MAX_PRIMITIVE_FRAGMENTS`]. `op` names the construct in the message.
pub(crate) fn check_fragment_cap(op: &str, fragments: u32) -> Result<(), KernelError> {
    if fragments > MAX_PRIMITIVE_FRAGMENTS {
        return Err(KernelError::Resolve(format!(
            "`{op}` resolves to {fragments} fragments (cap {MAX_PRIMITIVE_FRAGMENTS}); \
             lower $fn or raise $fa/$fs"
        )));
    }
    Ok(())
}

/// Geometry produced by [`tessellate_primitive`]: a 3D [`Solid`] or a 2D
/// [`Profile`].
///
/// The extrude operations (`linear_extrude` / `rotate_extrude`) key
/// off this to tell a solid apart from a cross-section they can lift into a
/// solid.
pub enum Geometry {
    /// A 3D body from `cube`/`sphere`/`cylinder`/`polyhedron`.
    Solid(Solid),
    /// A 2D cross-section from `square`/`circle`/`polygon`.
    Profile(Profile),
}

impl Geometry {
    /// The wrapped [`Solid`] if this is 3D geometry, else `None`.
    #[must_use]
    pub fn as_solid(&self) -> Option<&Solid> {
        match self {
            Geometry::Solid(s) => Some(s),
            Geometry::Profile(_) => None,
        }
    }

    /// The wrapped [`Profile`] if this is 2D geometry, else `None`.
    #[must_use]
    pub fn as_profile(&self) -> Option<&Profile> {
        match self {
            Geometry::Profile(p) => Some(p),
            Geometry::Solid(_) => None,
        }
    }
}

/// An error tessellating a [`Primitive`] into a [`Solid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// Degenerate primitive parameters that upstream OpenSCAD skips as empty
    /// geometry: non-positive cube/square size, `r <= 0` sphere/circle, `h <= 0`
    /// or both-radii-zero cylinder, fewer than 3 `circle` fragments, a
    /// polyhedron with too few points/faces, a polygon with fewer than 3 points
    /// or an empty `paths` list, or any non-finite coordinate.
    Degenerate(String),
    /// A `polyhedron` face or a `polygon` path references a point index outside
    /// the point list. Caught before any buffer reaches the native kernel,
    /// which would otherwise index out of bounds on the vertex buffer.
    BadIndex(String),
    /// The manifold kernel rejected the tessellated mesh (e.g. a polyhedron
    /// whose faces are not a closed, consistently-oriented surface).
    Solid(SolidError),
    /// A tree-resolution error: a node the resolver cannot
    /// turn into geometry. Two cases carry this — an unresolved
    /// [`CsgNode::Call`](morphe_eval::csg::CsgNode::Call) /
    /// [`CsgNode::Unresolved`](morphe_eval::csg::CsgNode::Unresolved) node that
    /// reached the kernel (it should have been an `EvalError` at eval time), and
    /// a `linear_extrude`/`rotate_extrude` whose child resolves to a 3D solid
    /// instead of a 2D profile. The message names the offending construct.
    Resolve(String),
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::Degenerate(msg) => write!(f, "degenerate primitive: {msg}"),
            KernelError::BadIndex(msg) => write!(f, "polyhedron index out of range: {msg}"),
            KernelError::Solid(e) => write!(f, "{e}"),
            KernelError::Resolve(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for KernelError {}

impl From<SolidError> for KernelError {
    fn from(e: SolidError) -> Self {
        KernelError::Solid(e)
    }
}

/// Build [`Geometry`] for a resolved primitive.
///
/// Returns `Geometry::Solid` for the four 3D primitives (`cube`, `sphere`,
/// `cylinder`, `polyhedron`) and `Geometry::Profile` for the three 2D
/// cross-sections (`square`, `circle`, `polygon`), or a [`KernelError`] for
/// degenerate, out-of-range, or non-manifold parameters.
///
/// Every well-formed primitive yields geometry — a degenerate one is an
/// `Err`, never an empty `Ok` — so there is no `Option` here.
pub fn tessellate_primitive(prim: &Primitive) -> Result<Geometry, KernelError> {
    match prim {
        Primitive::Cube { size, center } => tessellate_cube(*size, *center).map(Geometry::Solid),
        Primitive::Sphere { r, fragments } => {
            tessellate_sphere(*r, *fragments).map(Geometry::Solid)
        }
        Primitive::Cylinder {
            h,
            r1,
            r2,
            center,
            fragments,
        } => tessellate_cylinder(*h, *r1, *r2, *center, *fragments).map(Geometry::Solid),
        Primitive::Polyhedron { points, faces, .. } => {
            tessellate_polyhedron(points, faces).map(Geometry::Solid)
        }
        Primitive::Square { size, center } => {
            tessellate_square(*size, *center).map(Geometry::Profile)
        }
        Primitive::Circle { r, fragments } => {
            tessellate_circle(*r, *fragments).map(Geometry::Profile)
        }
        Primitive::Polygon { points, paths, .. } => {
            tessellate_polygon(points, paths.as_deref()).map(Geometry::Profile)
        }
    }
}

/// `cube([x,y,z], center)` — reuses [`Solid::cube`] (upstream `CubeNode`,
/// `primitives.cc` L100). A non-positive or non-finite edge is empty upstream
/// (L102) and a [`KernelError::Degenerate`] here.
fn tessellate_cube(size: [f64; 3], center: bool) -> Result<Solid, KernelError> {
    if size.iter().any(|&s| s <= 0.0 || !s.is_finite()) {
        return Err(KernelError::Degenerate(format!(
            "cube size must be positive and finite, got {size:?}"
        )));
    }
    Ok(Solid::cube(size, center))
}

/// `sphere(r, fragments)` — ported from upstream `SphereNode::createGeometry`
/// (`primitives.cc` L177-223).
///
/// Rings `num_rings = (fragments + 1) / 2`; ring `i`'s latitude is
/// `phi = 180*(i + 0.5)/num_rings` degrees, its radius `r*sin(phi)` at height
/// `r*cos(phi)`; each ring is `fragments` longitudes `360*k/fragments`. The
/// north pole is a fan over ring 0, the south pole a reversed fan over the last
/// ring, and adjacent rings are joined by quads. Total vertices are
/// `num_rings * fragments` (no explicit pole vertex — upstream closes with flat
/// polygonal caps).
fn tessellate_sphere(r: f64, fragments: u32) -> Result<Solid, KernelError> {
    if r <= 0.0 || !r.is_finite() {
        return Err(KernelError::Degenerate(format!(
            "sphere radius must be positive and finite, got {r}"
        )));
    }
    if fragments < 3 {
        return Err(KernelError::Degenerate(format!(
            "sphere needs at least 3 fragments, got {fragments}"
        )));
    }
    let nf = fragments as usize;
    let num_rings = fragments.div_ceil(2) as usize;
    // Quadratic-growth guard BEFORE the allocation (`$fn=1e7` would request
    // ~5·10¹³ vertices and abort with `capacity overflow`).
    let vertex_count = num_rings.saturating_mul(nf);
    if vertex_count > MAX_SPHERE_VERTICES {
        return Err(KernelError::Resolve(format!(
            "`sphere` with {fragments} fragments needs {vertex_count} vertices \
             (cap {MAX_SPHERE_VERTICES}); lower $fn or raise $fa/$fs"
        )));
    }

    let mut verts: Vec<[f64; 3]> = Vec::with_capacity(num_rings * nf);
    for i in 0..num_rings {
        let phi = 180.0 * (i as f64 + 0.5) / num_rings as f64;
        let ring_r = r * deg_sin(phi);
        let z = r * deg_cos(phi);
        push_circle(&mut verts, ring_r, z, nf);
    }

    let mut tris: Vec<[u32; 3]> = Vec::new();
    // North pole cap: fan over ring 0 (indices 0..nf).
    let top: Vec<u32> = (0..nf as u32).collect();
    fan(&mut tris, &top);
    // Quads between adjacent rings (primitives.cc L206-215).
    for i in 0..num_rings.saturating_sub(1) {
        for k in 0..nf {
            let a = (i * nf + (k + 1) % nf) as u32;
            let b = (i * nf + k) as u32;
            let c = ((i + 1) * nf + k) as u32;
            let d = ((i + 1) * nf + (k + 1) % nf) as u32;
            push_quad(&mut tris, a, b, c, d);
        }
    }
    // South pole cap: reversed fan over the last ring (primitives.cc L217-220).
    let n = num_rings * nf;
    let bottom: Vec<u32> = (0..nf).map(|i| (n - i - 1) as u32).collect();
    fan(&mut tris, &bottom);

    Solid::from_mesh(&verts, &tris).map_err(KernelError::from)
}

/// `cylinder(h, r1, r2, center, fragments)` — ported from upstream
/// `CylinderNode::createGeometry` (`primitives.cc` L251-307).
///
/// `fragments` around; `r1` is the bottom radius, `r2` the top. `z1`/`z2` are
/// `[0, h]`, or `[-h/2, h/2]` when centred. A zero top radius (`cone`) or zero
/// bottom radius (`inverted_cone`) collapses that end to a single apex vertex
/// with a triangle fan; otherwise the two circles are joined by quads. Each
/// non-apex end is closed by a cap fan (bottom reversed for its outward `-z`
/// normal).
fn tessellate_cylinder(
    h: f64,
    r1: f64,
    r2: f64,
    center: bool,
    fragments: u32,
) -> Result<Solid, KernelError> {
    if h <= 0.0
        || !h.is_finite()
        || r1 < 0.0
        || !r1.is_finite()
        || r2 < 0.0
        || !r2.is_finite()
        || (r1 <= 0.0 && r2 <= 0.0)
    {
        return Err(KernelError::Degenerate(format!(
            "cylinder needs h > 0 and at least one positive radius, got h={h}, r1={r1}, r2={r2}"
        )));
    }
    if fragments < 3 {
        return Err(KernelError::Degenerate(format!(
            "cylinder needs at least 3 fragments, got {fragments}"
        )));
    }
    check_fragment_cap("cylinder", fragments)?;
    let nf = fragments as usize;
    let (z1, z2) = if center {
        (-h / 2.0, h / 2.0)
    } else {
        (0.0, h)
    };
    let cone = r2 == 0.0;
    let inverted_cone = r1 == 0.0;

    let mut verts: Vec<[f64; 3]> = Vec::with_capacity(if cone || inverted_cone {
        nf + 1
    } else {
        2 * nf
    });
    // Bottom: apex vertex or a full circle (primitives.cc L275-279).
    if inverted_cone {
        verts.push([0.0, 0.0, z1]);
    } else {
        push_circle(&mut verts, r1, z1, nf);
    }
    // Top: apex vertex or a full circle (primitives.cc L280-284).
    if cone {
        verts.push([0.0, 0.0, z2]);
    } else {
        push_circle(&mut verts, r2, z2, nf);
    }

    let mut tris: Vec<[u32; 3]> = Vec::new();
    // Side faces (primitives.cc L286-291).
    let nf_u = nf as u32;
    for i in 0..nf_u {
        let j = (i + 1) % nf_u;
        if cone {
            tris.push([i, j, nf_u]);
        } else if inverted_cone {
            tris.push([0, j + 1, i + 1]);
        } else {
            push_quad(&mut tris, i, j, j + nf_u, i + nf_u);
        }
    }
    // Bottom cap: reversed fan (primitives.cc L293-298).
    if !inverted_cone {
        let cap: Vec<u32> = (0..nf_u).map(|i| nf_u - i - 1).collect();
        fan(&mut tris, &cap);
    }
    // Top cap: forward fan over the top circle (primitives.cc L299-305).
    if !cone {
        let offset = if inverted_cone { 1 } else { nf_u };
        let cap: Vec<u32> = (0..nf_u).map(|i| offset + i).collect();
        fan(&mut tris, &cap);
    }

    Solid::from_mesh(&verts, &tris).map_err(KernelError::from)
}

/// `polyhedron(points, faces)` — ported from upstream
/// `PolyhedronNode::createGeometry` (`primitives.cc` L399-414).
///
/// Faces are consumed as given but **reversed** (upstream `std::reverse`,
/// L407) to turn the manual's clockwise-from-outside input into Manifold's
/// counter-clockwise-from-outside (outward-normal) convention, then
/// fan-triangulated (upstream keeps convex n-gons and defers triangulation;
/// Manifold needs triangles). A degenerate hull (collinear / not closed) is
/// rejected by Manifold as [`KernelError::Solid`].
///
/// A face index outside the point list is rejected as [`KernelError::BadIndex`]
/// *before* any buffer reaches native Manifold — an out-of-range index would
/// otherwise index the native vertex buffer out of bounds. Non-finite point
/// coordinates are rejected as [`KernelError::Degenerate`], matching the
/// finite-only contract of the other primitives.
fn tessellate_polyhedron(points: &[[f64; 3]], faces: &[Vec<usize>]) -> Result<Solid, KernelError> {
    if points.len() < 4 || faces.len() < 4 {
        return Err(KernelError::Degenerate(format!(
            "polyhedron needs at least 4 points and 4 faces, got {} points and {} faces",
            points.len(),
            faces.len()
        )));
    }
    if let Some(bad) = points.iter().position(|p| p.iter().any(|c| !c.is_finite())) {
        return Err(KernelError::Degenerate(format!(
            "polyhedron point {bad} has a non-finite coordinate: {:?}",
            points[bad]
        )));
    }
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for (f, face) in faces.iter().enumerate() {
        if let Some(&i) = face.iter().find(|&&i| i >= points.len()) {
            return Err(KernelError::BadIndex(format!(
                "face {f} references point {i} but there are only {} points",
                points.len()
            )));
        }
        let mut idx: Vec<u32> = face.iter().map(|&i| i as u32).collect();
        idx.reverse();
        fan(&mut tris, &idx);
    }
    Solid::from_mesh(points, &tris).map_err(KernelError::from)
}

/// `square([x,y], center)` — a single rectangular contour (upstream
/// `SquareNode::createGeometry`). Non-positive or non-finite sides are empty
/// upstream and a [`KernelError::Degenerate`] here. The four corners wind
/// counter-clockwise; `center` shifts the minimum corner from the origin to
/// `[-x/2, -y/2]`.
fn tessellate_square(size: [f64; 2], center: bool) -> Result<Profile, KernelError> {
    if size.iter().any(|&s| s <= 0.0 || !s.is_finite()) {
        return Err(KernelError::Degenerate(format!(
            "square size must be positive and finite, got {size:?}"
        )));
    }
    let [x, y] = size;
    let (ox, oy) = if center {
        (-x / 2.0, -y / 2.0)
    } else {
        (0.0, 0.0)
    };
    let contour = vec![[ox, oy], [ox + x, oy], [ox + x, oy + y], [ox, oy + y]];
    Ok(Profile::from_contours(&[contour], FillRule::EvenOdd))
}

/// `circle(r, fragments)` — a regular `fragments`-gon inscribed in radius `r`,
/// **not** a true circle. Upstream `CircleNode::createGeometry` builds its own
/// `Outline2d` in a loop of `i in 0..fragments` placing vertex `i` at
/// `(r*cos(2π·i/fragments), r*sin(2π·i/fragments))` — this ports that loop in
/// degrees via [`deg_cos`]/[`deg_sin`]. (The separate free function
/// `generate_circle`, `primitives.cc` L59-65, is the *3D* helper used by
/// sphere/cylinder — [`push_circle`] here — not this 2D primitive.)
///
/// `r <= 0` / non-finite is empty upstream and a [`KernelError::Degenerate`].
/// The `fragments < 3` guard is defensive: eval's `resolve_fragments` already
/// clamps the resolved count to `>= 3`, so this only trips on a hand-built
/// [`Primitive`], and mirrors the sphere/cylinder fragment guards.
fn tessellate_circle(r: f64, fragments: u32) -> Result<Profile, KernelError> {
    if r <= 0.0 || !r.is_finite() {
        return Err(KernelError::Degenerate(format!(
            "circle radius must be positive and finite, got {r}"
        )));
    }
    if fragments < 3 {
        return Err(KernelError::Degenerate(format!(
            "circle needs at least 3 fragments, got {fragments}"
        )));
    }
    check_fragment_cap("circle", fragments)?;
    let nf = fragments as usize;
    let mut contour: Vec<[f64; 2]> = Vec::with_capacity(nf);
    for k in 0..nf {
        let phi = 360.0 * k as f64 / nf as f64;
        contour.push([r * deg_cos(phi), r * deg_sin(phi)]);
    }
    Ok(Profile::from_contours(&[contour], FillRule::EvenOdd))
}

/// `polygon(points, paths, convexity)` — one contour per path, or the whole
/// point list as one contour when `paths` is `None` (upstream
/// `PolygonNode::createGeometry`). Contours are filled with the even-odd rule
/// so a nested path becomes a hole regardless of its winding.
///
/// # Degenerate and out-of-range inputs
///
/// - With `paths = None` the whole point list is one contour, so it needs at
///   least 3 points and every point must be finite.
/// - With `paths = Some(..)` **only path-referenced points contribute
///   geometry** — matching upstream `PolygonNode`, which reads only the indices
///   each path names — so finiteness is checked per *referenced* point; a NaN
///   in an unreferenced point is ignored. Each path must name at least 3
///   vertices, and an empty `paths` list has no contour at all.
/// - After building, a set of contours that encloses no area (empty / zero
///   area — e.g. all-collinear points, or degenerate paths) is surfaced as
///   [`KernelError::Degenerate`] rather than a silent empty [`Profile`].
/// - A path index outside the point list is a [`KernelError::BadIndex`], caught
///   before any buffer reaches the native kernel (matching the `polyhedron`
///   contract).
fn tessellate_polygon(
    points: &[[f64; 2]],
    paths: Option<&[Vec<usize>]>,
) -> Result<Profile, KernelError> {
    let finite = |i: usize| -> Result<(), KernelError> {
        if points[i].iter().any(|c| !c.is_finite()) {
            Err(KernelError::Degenerate(format!(
                "polygon point {i} has a non-finite coordinate: {:?}",
                points[i]
            )))
        } else {
            Ok(())
        }
    };

    let contours: Vec<Vec<[f64; 2]>> = match paths {
        None => {
            if points.len() < 3 {
                return Err(KernelError::Degenerate(format!(
                    "polygon needs at least 3 points, got {}",
                    points.len()
                )));
            }
            for i in 0..points.len() {
                finite(i)?;
            }
            vec![points.to_vec()]
        }
        Some(paths) => {
            if paths.is_empty() {
                return Err(KernelError::Degenerate(
                    "polygon paths is empty: no contour to build".to_string(),
                ));
            }
            let mut contours = Vec::with_capacity(paths.len());
            for (p, path) in paths.iter().enumerate() {
                if path.len() < 3 {
                    return Err(KernelError::Degenerate(format!(
                        "polygon path {p} needs at least 3 vertices, got {}",
                        path.len()
                    )));
                }
                for &i in path {
                    if i >= points.len() {
                        return Err(KernelError::BadIndex(format!(
                            "path {p} references point {i} but there are only {} points",
                            points.len()
                        )));
                    }
                    finite(i)?;
                }
                contours.push(path.iter().map(|&i| points[i]).collect());
            }
            contours
        }
    };

    let profile = Profile::from_contours(&contours, FillRule::EvenOdd);
    if profile.is_empty() || profile.area() <= 0.0 {
        return Err(KernelError::Degenerate(
            "polygon contours enclose no area".to_string(),
        ));
    }
    Ok(profile)
}

// ─── linear_extrude ──────────────────────────────────────────────────────────
//
// Lifts a 2D [`Profile`] into a 3D [`Solid`] per upstream `linear_extrude`
// (OpenSCAD master, fetched 2026-07-03: `src/core/LinearExtrudeNode.cc`,
// `src/geometry/linear_extrude.cc`, `src/core/CurveDiscretizer.cc`). The mesh
// itself is generated by Manifold's `Extrude`
// (`Manifold::extrude_with_options(cs, height, nDivisions, twistDegrees,
// scaleTopX, scaleTopY)`, which spans `z ∈ [0, height]`); this function only
// resolves the OpenSCAD parameters and maps them onto that call.
//
// # Twist sign
//
// OpenSCAD's positive `twist` rotates the top **clockwise looking down +Z**:
// `linear_extrude.cc` builds every slice transform as `rotate_degrees(-rot)`
// with `rot = twist * slice/num_slices`, so the top is rotated `-twist` in the
// standard CCW sense. Verified against the installed OpenSCAD 2021.01 oracle: a
// bar along +X twisted 90° lands along -Y at the top. Manifold's `Extrude`
// twists in the opposite (CCW-positive) sense, so we pass `-twist`; the
// `twist_90_matches_openscad_sign` test pins this down.
//
// # Slices from twist (upstream `CurveDiscretizer`)
//
// When `slices` is `None`, the count is derived exactly as
// `linear_extrude.cc` does, from the resolved `$fn/$fa/$fs` ([`Fragments`]) and
// the profile's largest squared radius `max_r_sqr`:
//   * twist, uniform scale == 1  → `getHelixSlices` (helical arc length / $fs,
//     twist / $fa, floored at `ceil(twist/120)`);
//   * twist, non-uniform scale   → `max(getDiagonalSlices, getHelixSlices)`;
//   * twist, uniform scale ≠ 1    → `getConicalHelixSlices` (Archimedes-spiral
//     arc length);
//   * no twist, non-uniform scale → `getDiagonalSlices`;
//   * no twist, uniform scale     → 1.
// Each estimator falls back to `max(ceil(twist/120), 1)` (or 1) when the
// profile is degenerate-thin (`max_r < GRID_FINE`).
//
// # Fidelity gap
//
// Manifold interpolates the whole polygon **linearly** between the transformed
// bottom/top copies per division; it does not re-segment straight outline edges
// the way upstream `splitOutline` does. Slice count (vertical subdivision) and
// scale/twist interpolation match upstream; extra *outline* vertices for curved
// twisted edges are not inserted. This affects only the smoothness of long
// straight edges under twist, not volume, manifoldness, or slice count.

/// OpenSCAD `GRID_FINE` (`src/geometry/Grid.h`) — the near-zero radius below
/// which the slice estimators decline to compute (returning the twist fallback).
const GRID_FINE: f64 = 0.000_000_953_674_316_406_25;

/// Degrees→radians, matching upstream `M_DEG2RAD` (`src/utils/degree_trig.h`).
const DEG2RAD: f64 = std::f64::consts::PI / 180.0;

/// Hard ceiling on the resolved vertical slice count (Manifold `nDivisions`).
///
/// A morphe hardening with no upstream analogue — OpenSCAD caps slices only by
/// native stack/time. The `$fa`/`$fs`/`$fn`-derived counts for any legal input
/// stay far below this (e.g. a 360° twist resolves to a few hundred slices); it
/// only guards pathological direct-kernel input — an extreme scale delta over a
/// near-zero `$fs`, or a huge explicit `slices` — from resolving to a count that
/// would make Manifold allocate a runaway mesh and OOM. `100_000` slices is
/// already far finer than any real model needs and fits comfortably in `i32`.
const MAX_EXTRUDE_SLICES: i32 = 100_000;

/// Extrude a 2D [`Profile`] into a 3D [`Solid`] per upstream `linear_extrude`.
///
/// `height` must be `> 0` and finite (upstream clamps a non-positive height to
/// `0`, which yields empty geometry — surfaced here as
/// [`KernelError::Degenerate`] so the caller decides, matching the primitive
/// tessellators). An empty profile likewise has nothing to sweep and is
/// `Degenerate`. Negative `scale` components clamp to `0` (upstream
/// `LinearExtrudeNode.cc`), so a `[0, 0]` scale sweeps to a point (a
/// cone/pyramid tip). See the module section above for twist sign, the
/// slices-from-twist formula, and the Manifold fidelity gap.
pub fn linear_extrude(
    profile: &Profile,
    params: &LinearExtrudeParams,
) -> Result<Solid, KernelError> {
    if !params.height.is_finite() || params.height <= 0.0 {
        return Err(KernelError::Degenerate(format!(
            "linear_extrude needs a positive finite height, got {}",
            params.height
        )));
    }
    // Twist / scale must be finite before they reach `resolve_slices`: a
    // non-finite twist drives the `ceil(twist/…)` slice estimators to
    // `i32::MAX` divisions (Manifold would then allocate a giant mesh and
    // OOM/abort), and a non-finite scale poisons the delta-based estimators the
    // same way. Reject up front, matching the finite-only contract of the
    // primitive tessellators.
    if !params.twist.is_finite() {
        return Err(KernelError::Degenerate(format!(
            "linear_extrude twist must be finite, got {}",
            params.twist
        )));
    }
    if params.scale.iter().any(|s| !s.is_finite()) {
        return Err(KernelError::Degenerate(format!(
            "linear_extrude scale must be finite, got {:?}",
            params.scale
        )));
    }
    if profile.is_empty() {
        return Err(KernelError::Degenerate(
            "linear_extrude of an empty profile encloses no area".to_string(),
        ));
    }
    // Negative scale clamps to 0 (upstream `LinearExtrudeNode.cc`:
    // `if (node->scale_x < 0) node->scale_x = 0; ...`).
    let scale = [params.scale[0].max(0.0), params.scale[1].max(0.0)];
    let slices = resolve_slices(profile, params, scale);
    // See the module `# Twist sign` note: OpenSCAD twist is CW-looking-down,
    // Manifold's is the opposite, so negate.
    let twist_degrees = -params.twist;
    Ok(profile.extrude(params.height, slices, twist_degrees, scale, params.center))
}

/// Resolve the vertical slice count (Manifold `nDivisions`) for an extrude.
///
/// An explicit `slices` (upstream `has_slices`) wins; otherwise the count is
/// derived from twist, the clamped scale, and [`Fragments`] exactly as upstream
/// `linear_extrude.cc` dispatches over the twist/uniform-scale cases.
fn resolve_slices(profile: &Profile, params: &LinearExtrudeParams, scale: [f64; 2]) -> i32 {
    if let Some(s) = params.slices {
        // Clamp into `[1, MAX_EXTRUDE_SLICES]` before the u32→i32 cast: this both
        // guards the OOM cliff and avoids truncating a value above `i32::MAX`.
        return s.clamp(1, MAX_EXTRUDE_SLICES as u32) as i32;
    }
    let frag = params.fragments.clamped();
    let h = params.height;
    let twist = params.twist;
    let [sx, sy] = scale;
    // Largest squared distance of any profile vertex from the Z axis — upstream
    // `max_r1_sqr` (r before scaling).
    let max_r_sqr = profile
        .to_polygons()
        .iter()
        .flatten()
        .map(|p| p[0] * p[0] + p[1] * p[1])
        .fold(0.0_f64, f64::max);
    // Fallback when an estimator declines (thin profile): `max(ceil(twist/120),1)`.
    let twist_fallback = ((twist.abs() / 120.0).ceil() as i32).max(1);

    let n = if twist != 0.0 {
        if sx == 1.0 && sy == 1.0 {
            get_helix_slices(&frag, max_r_sqr, h, twist).unwrap_or(twist_fallback)
        } else if sx != sy {
            let non_uni = get_diagonal_slices(&frag, max_delta_sqr(profile, scale), h).unwrap_or(1);
            let tw = get_helix_slices(&frag, max_r_sqr, h, twist).unwrap_or(twist_fallback);
            non_uni.max(tw)
        } else {
            get_conical_helix_slices(&frag, max_r_sqr, h, twist, sx).unwrap_or(twist_fallback)
        }
    } else if sx != sy {
        get_diagonal_slices(&frag, max_delta_sqr(profile, scale), h).unwrap_or(1)
    } else {
        1
    };
    // Floor at 1 and cap at MAX_EXTRUDE_SLICES: an extreme scale-delta over a
    // tiny $fs can drive getDiagonalSlices arbitrarily high (fidelity-correct
    // but an OOM cliff), so the cap is a morphe hardening — see the constant.
    n.clamp(1, MAX_EXTRUDE_SLICES)
}

/// Grow/shrink a 2D profile by `delta` (upstream `OffsetNode`
/// over Clipper2's offset). The join style comes RESOLVED from eval
/// ([`OffsetJoin`], since upstream's stored `chamfer` bool alone cannot
/// distinguish r-mode-round from delta-miter):
///
/// - [`OffsetJoin::Round`] (r-mode) → [`JoinType::Round`] with the join-arc
///   facet count from [`resolve_fragments`] evaluated at |delta| (upstream
///   discretizes the join arcs exactly like circle segments, `CurveDiscretizer`).
/// - [`OffsetJoin::Miter`] (delta-mode) → [`JoinType::Miter`] with Clipper2's
///   default miter limit (`2.0`; upstream exposes no miter parameter — the
///   value is the Clipper2 library default).
/// - [`OffsetJoin::Bevel`] (delta + chamfer) → [`JoinType::Square`].
///
/// A negative delta shrinks. The result is a Profile — `linear_extrude` lifts
/// it exactly as a primitive 2D shape.
pub fn offset_profile(profile: &Profile, params: &OffsetParams) -> Result<Profile, KernelError> {
    // A non-finite distance is a loud error, not a silent monster/empty
    // (probed: NaN → empty, inf → 1.7e19 square). Eval filters these
    // already; the guard hardens the pub re-exported entry point.
    if !params.delta.is_finite() {
        return Err(KernelError::Resolve(format!(
            "offset delta must be finite, got {}",
            params.delta
        )));
    }

    // Resolve the join-arc count the way every other curve does: the standard
    // fragment path over |delta| (the join arc radius), clamped upstream-style
    // inside `resolve_fragments`. Only Round consumes it, but resolving
    // unconditionally keeps the fragment semantics observable for tests of
    // every mode; the hard cap applies only where the arcs are actually built.
    let fragments = params.fragments;
    let segments = resolve_fragments(
        params.delta.abs(),
        fragments.fn_,
        fragments.fs,
        fragments.fa,
    );
    if params.join == OffsetJoin::Round {
        check_fragment_cap("offset", segments)?;
    }

    let joined = profile.cross_section().offset(
        params.delta,
        match params.join {
            OffsetJoin::Round => JoinType::Round,
            OffsetJoin::Miter => JoinType::Miter,
            OffsetJoin::Bevel => JoinType::Square,
        },
        2.0,
        i32::try_from(segments).unwrap_or(64),
    );
    Ok(Profile::from_cross_section(joined))
}

/// Minkowski sum of solids (upstream `cgaladv.cc builtin_minkowski`): a left
/// fold over the kernel's native [`Manifold::minkowski_sum`]. `n=0` → `None`
/// (empty); `n=1` → passthrough (upstream identity cases). PERF NOTE:
/// non-convex inputs
/// scale with the face-count PRODUCT (the kernel's own caveat — the same one
/// OpenSCAD carries); convex inputs are fast. No hard guard: corpus fixtures
/// are bounded and the cliff is documented here for authors.
pub fn minkowski(solids: &[Solid]) -> Result<Option<Solid>, KernelError> {
    let Some(first) = solids.first() else {
        return Ok(None);
    };
    let mut acc = Solid::from_manifold(first.manifold().clone());
    for next in &solids[1..] {
        acc = Solid::from_manifold(acc.manifold().minkowski_sum(next.manifold()));
    }
    Ok(Some(acc))
}

/// Minkowski sum of 2D profiles via the PRISM IDENTITY:
/// `CrossSection` has no minkowski op (verified against manifold-csg 0.3.1),
/// but extruding both operands to height 1 gives
/// `minkowski(A×[0,1], B×[0,1]) = (A⊕B)×[0,2]`, so the planar sum is the
/// z=1 cross-section of the 3D sum: extrude → `minkowski_sum` →
/// `slice_to_cross_section(1.0)`. `n=0` → `None`; `n=1` → passthrough.
pub fn minkowski_profiles(profiles: &[Profile]) -> Result<Option<Profile>, KernelError> {
    let Some(first) = profiles.first() else {
        return Ok(None);
    };
    // n=1 is a TRUE passthrough (the identity case) — skipping the
    // extrude→sum→slice round trip keeps the profile identical instead of
    // re-triangulating through the prism identity.
    if profiles.len() == 1 {
        return Ok(Some(Profile::from_cross_section(
            first.cross_section().clone(),
        )));
    }
    let mut acc = first.cross_section().extrude(1.0);
    for next in &profiles[1..] {
        acc = acc.minkowski_sum(&next.cross_section().extrude(1.0));
    }
    Ok(Some(Profile::from_cross_section(
        acc.slice_to_cross_section(1.0),
    )))
}

/// Project a solid to the XY plane (upstream
/// `ProjectionNode::createGeometry`). `cut=true` → the z=0 cross-section
/// region ([`Manifold::slice_to_cross_section`] at height 0); `cut=false`
/// → the shadow, the union of all cross-sections ([`Manifold::project`]
/// polygons re-filled with the even-odd rule so holes in the solid survive
/// as holes in the profile). An empty/void result is a VALID empty Profile,
/// not an error (a cut above the solid legitimately projects nothing).
///
/// Boundary semantics (probe-derived, deterministic): the cut captures
/// material strictly ABOVE z=0 — a top face coincident with the plane cuts
/// EMPTY; a bottom face coincident or a straddling solid cuts the FULL
/// section. Never an error, never sliver geometry.
pub fn project_solid(solid: &Solid, cut: bool) -> Result<Profile, KernelError> {
    if cut {
        let cross = solid.manifold().slice_to_cross_section(0.0);
        Ok(Profile::from_cross_section(cross))
    } else {
        let polygons = solid.manifold().project();
        Ok(Profile::from_contours(&polygons, FillRule::EvenOdd))
    }
}

/// Largest squared per-vertex displacement under scaling — upstream
/// `calc_max_delta_sqr` (`linear_extrude.cc`): `max |(scale-1)·v|²`.
fn max_delta_sqr(profile: &Profile, scale: [f64; 2]) -> f64 {
    let [sx, sy] = scale;
    profile
        .to_polygons()
        .iter()
        .flatten()
        .map(|p| {
            let dx = (sx - 1.0) * p[0];
            let dy = (sy - 1.0) * p[1];
            dx * dx + dy * dy
        })
        .fold(0.0_f64, f64::max)
}

/// Helical arc length a corner vertex travels under pure twist — upstream
/// `helix_arc_length` (`CurveDiscretizer.cc`): with `T = twist·deg2rad` and
/// `c = height/T`, the length is `T·sqrt(r² + c²)`.
fn helix_arc_length(r_sqr: f64, height: f64, twist_degrees: f64) -> f64 {
    let t = twist_degrees * DEG2RAD;
    let c = height / t;
    t * (r_sqr + c * c).sqrt()
}

/// Slices for a twisted extrude with uniform scale `== 1` — upstream
/// `CurveDiscretizer::getHelixSlices`. `$fn` (when set) gives
/// `ceil(twist/360·$fn)`, else `min(ceil(twist/$fa), ceil(arc/$fs))`, all
/// floored at `ceil(twist/120)`. `None` when the profile is thinner than
/// `GRID_FINE` (the caller falls back). The `$fe` branch is skipped — that
/// experimental feature is off by default upstream.
fn get_helix_slices(frag: &Fragments, r_sqr: f64, height: f64, twist: f64) -> Option<i32> {
    let twist = twist.abs();
    let min_slices = ((twist / 120.0).ceil() as i32).max(1);
    if r_sqr.sqrt() < GRID_FINE || !frag.fn_.is_finite() || height.is_nan() || twist.is_nan() {
        return None;
    }
    if frag.fn_ > 0.0 {
        let fn_slices = (twist / 360.0 * frag.fn_).ceil() as i32;
        return Some(fn_slices.max(min_slices));
    }
    let fa_slices = (twist / frag.fa).ceil() as i32;
    let fs_slices = (helix_arc_length(r_sqr, height, twist) / frag.fs).ceil() as i32;
    Some(fa_slices.min(fs_slices).max(min_slices))
}

/// Arc length of an Archimedes spiral section — upstream `archimedes_length`
/// (`CurveDiscretizer.cc`): `0.5·a·(θ·sqrt(1+θ²) + asinh(θ))`.
fn archimedes_length(a: f64, theta: f64) -> f64 {
    0.5 * a * (theta * (1.0 + theta * theta).sqrt() + theta.asinh())
}

/// Slices for a twisted extrude with **uniform** scale `!= 1` — upstream
/// `CurveDiscretizer::getConicalHelixSlices` (a vertex traces a section of an
/// Archimedes spiral; its 3D length over the height sets the `$fs` limit).
fn get_conical_helix_slices(
    frag: &Fragments,
    r_sqr: f64,
    height: f64,
    twist: f64,
    scale: f64,
) -> Option<i32> {
    let twist = twist.abs();
    let r = r_sqr.sqrt();
    let min_slices = ((twist / 120.0).ceil() as i32).max(1);
    if r < GRID_FINE || !frag.fn_.is_finite() {
        return None;
    }
    if frag.fn_ > 0.0 {
        let fn_slices = (twist * frag.fn_ / 360.0).ceil() as i32;
        return Some(fn_slices.max(min_slices));
    }
    let rads = twist * DEG2RAD;
    // scale == 1 is dispatched to the helix path, never here; guard anyway.
    let angle_end = if scale > 1.0 {
        rads * scale / (scale - 1.0)
    } else if scale < 1.0 {
        rads / (1.0 - scale)
    } else {
        return Some(min_slices);
    };
    let angle_start = angle_end - rads;
    let a = r / angle_end;
    let spiral_length = archimedes_length(a, angle_end) - archimedes_length(a, angle_start);
    let total_length = (spiral_length * spiral_length + height * height).sqrt();
    let fs_slices = (total_length / frag.fs).ceil() as i32;
    let fa_slices = (twist / frag.fa).ceil() as i32;
    Some(fa_slices.min(fs_slices).max(min_slices))
}

/// Slices for a **non-uniform** scale extrude (twist ignored here) — upstream
/// `CurveDiscretizer::getDiagonalSlices`: `$fn` used directly when set, else the
/// diagonal vertex path `sqrt(delta² + height²)` divided by `$fs`, floored at 1.
fn get_diagonal_slices(frag: &Fragments, delta_sqr: f64, height: f64) -> Option<i32> {
    let min_slices = 1;
    if delta_sqr.sqrt() < GRID_FINE || !frag.fn_.is_finite() {
        return None;
    }
    if frag.fn_ > 0.0 {
        return Some((frag.fn_ as i32).max(min_slices));
    }
    let fs_slices = ((delta_sqr + height * height).sqrt() / frag.fs).ceil() as i32;
    Some(fs_slices.max(min_slices))
}

// ─── rotate_extrude ──────────────────────────────────────────────────────────
//
// Revolves a 2D [`Profile`] about the Z axis into a 3D [`Solid`] per upstream
// `rotate_extrude` (OpenSCAD master, fetched 2026-07-03:
// `src/core/RotateExtrudeNode.cc`, `src/geometry/rotate_extrude.cc`,
// `src/core/CurveDiscretizer.cc`). The mesh is generated by Manifold's `Revolve`
// (`Manifold::revolve(cs, circularSegments, revolveDegrees)`); this function
// only resolves the OpenSCAD parameters and maps them onto that call.
//
// # Axis and profile mapping
//
// A profile vertex `(x, y)` maps to `(radius = x, axial height = y)`: `x` is the
// distance from the rotation axis and `y` the height along it. Upstream
// `rotate_extrude.cc` places each revolved vertex at
// `(v[0]·cos(angle), v[0]·sin(angle), v[1])` — `x` is the radius, `y` the Z
// height — and Manifold `Revolve` matches it (`constructors.cpp` L390:
// `{currPolyVertex.x·cos φ, currPolyVertex.x·sin φ, currPolyVertex.y}`),
// revolving the section about *its* Y axis, which becomes the result's Z axis.
//
// # Segment count (upstream `getCircularSegmentCount`)
//
// `circular_segments` is the FULL-circle facet count from the profile's maximum
// radius (its largest `x`), scaled to the sweep angle:
// `ceil(get_fragments_from_r(max_x, $fn, $fs, $fa) · |angle| / 360)`, floored at
// 1. This ports upstream `CurveDiscretizer::getCircularSegmentCount(r, angle)`
// (`CurveDiscretizer.cc`), which returns `max(1, (int)ceil(result))` with
// `result *= |angle|/360` applied first — the `$fn>0` branch is
// `ceil(ceil(fn) · |angle|/360)` and the `$fn=0` branch is
// `ceil(ceil(max(min(360/$fa, 2πr/$fs), 5)) · |angle|/360)` (the experimental
// `$fe` branch is off by default upstream and skipped). The inner `ceil` is the
// full-circle count [`resolve_fragments`] already returns; this scales it by the
// angle fraction and applies the OUTER `ceil` — so `$fn=10, angle=90` resolves to
// `ceil(10·¼) = ceil(2.5) = 3` facets, not `2`. For a full `360°` revolve the
// scale is `1`, so `$fn=6` gives a 6-faceted (hexagonal cross-section) ring.
// Manifold honours an explicit `nDivisions` only when it is
// `> 2` (`constructors.cpp` L345); a resolved count of `1`/`2` (a very coarse
// partial sweep) instead falls back to Manifold's own quality-based density —
// the one case this does not reproduce upstream's exact wedge count.
//
// The `max_x` radius is the profile's largest positive `x` (its outer radius),
// as the task specifies and as Manifold's own auto path uses; upstream master
// instead feeds `getCircularSegmentCount` the radial *width* `max_x - min_x`.
// The two agree when the profile touches the axis (`min_x = 0`) and differ for
// an offset ring, where the outer radius is the more faithful smoothness driver.
//
// # Start offset
//
// Manifold always begins the sweep at angle `0`; upstream offsets every vertex
// by `start` (`rotate_extrude.cc`: `angle = node.start + j·node.angle/…`), so a
// non-zero `start` is applied by rotating the finished solid `start` degrees
// about Z. For a full revolution this is a no-op on the shape (a solid of
// revolution is invariant under rotation about its own axis); it matters only
// for a partial sweep, where it repositions the wedge.
//
// # Negative-x profiles
//
// Upstream `rotate_extrude.cc` ERRORS when a profile straddles the Y axis
// (`max_x > 0 && min_x < 0`). Manifold instead CLIPS the profile to the `x >= 0`
// half-plane before revolving (`constructors.cpp` L310-334, interpolating a new
// vertex where an edge crosses `x = 0`), so a straddling profile silently
// revolves only its positive part. A profile with no positive-`x` part at all
// (`max_x <= 0`) has nothing to revolve and is rejected here as
// [`KernelError::Degenerate`] rather than handed to Manifold as an invalid body.

/// Hard ceiling on the resolved circular segment count (Manifold `nDivisions`).
///
/// The rotate-extrude twin of [`MAX_EXTRUDE_SLICES`]: a morphe hardening with no
/// upstream analogue. A pathological `$fs`/`$fa` near zero over a large radius
/// can drive [`resolve_fragments`] arbitrarily high, so the cap guards Manifold
/// from allocating a runaway revolved mesh. Any legal input stays far below it.
const MAX_REVOLVE_SEGMENTS: i32 = 100_000;

/// Revolve a 2D [`Profile`] about the Z axis into a 3D [`Solid`] per upstream
/// `rotate_extrude`.
///
/// `angle` must be finite and non-zero — a zero sweep encloses no volume
/// (upstream produces empty geometry; surfaced here as [`KernelError::Degenerate`]
/// so the caller decides, matching the primitive tessellators and
/// [`linear_extrude`]). An empty profile likewise has nothing to sweep, and a
/// profile with no positive-`x` part (`max_x <= 0`) cannot be revolved. See the
/// module section above for the axis/mapping, the max-radius segment formula,
/// `start` handling, and Manifold's negative-`x` clipping.
pub fn rotate_extrude(
    profile: &Profile,
    params: &RotateExtrudeParams,
) -> Result<Solid, KernelError> {
    if profile.is_empty() {
        return Err(KernelError::Degenerate(
            "rotate_extrude of an empty profile encloses no area".to_string(),
        ));
    }
    if !params.angle.is_finite() || params.angle == 0.0 {
        return Err(KernelError::Degenerate(format!(
            "rotate_extrude needs a finite non-zero angle, got {}",
            params.angle
        )));
    }
    if !params.start.is_finite() {
        return Err(KernelError::Degenerate(format!(
            "rotate_extrude start must be finite, got {}",
            params.start
        )));
    }
    // Pre-clamp the sweep to (-360, 360]: eval already snaps an out-of-range
    // angle to 360 (csg.rs `RotateExtrudeParams::angle`, mirroring upstream), but
    // a direct kernel caller may hand us any magnitude. Match that snap here so
    // both the segment count and the sweep agree — Manifold itself only clamps a
    // revolve above 360 (`constructors.cpp` L340) and not a large negative one.
    let angle = clamp_revolve_angle(params.angle);
    // Outer radius = the profile's largest positive x. Manifold revolves only
    // the x >= 0 half; a profile wholly on the -x side has nothing to sweep.
    let max_x = profile
        .to_polygons()
        .iter()
        .flatten()
        .map(|p| p[0])
        .fold(0.0_f64, f64::max);
    if max_x <= 0.0 {
        return Err(KernelError::Degenerate(
            "rotate_extrude profile has no part with x > 0 to revolve".to_string(),
        ));
    }
    let segments = resolve_revolve_segments(max_x, params.fragments, angle);
    // Manifold revolves counter-clockwise; a negative sweep would flip triangle
    // orientation (an inside-out body). Revolve the positive magnitude and place
    // the arc with the start rotation: a sweep of `angle` from `start` covers
    // `[start, start+angle]` — for a negative `angle` that is a positive-magnitude
    // arc rotated to begin at `start + angle` (upstream's `start + j·angle/N`).
    let start_rotation = if angle >= 0.0 {
        params.start
    } else {
        params.start + angle
    };
    Ok(profile.revolve(segments, angle.abs(), start_rotation))
}

/// Snap an out-of-range sweep to a full `360°`, mirroring eval's
/// `RotateExtrudeParams::angle` resolution (`csg.rs`): a value `<= -360` or
/// `> 360` becomes `360`, anything in `(-360, 360]` passes through. Keeps the
/// pub kernel entrypoint's pre-clamp contract consistent for direct callers.
fn clamp_revolve_angle(angle: f64) -> f64 {
    if angle <= -360.0 || angle > 360.0 {
        360.0
    } else {
        angle
    }
}

/// Resolve Manifold's `nDivisions` (angular facet count) for a revolve — the
/// full-circle count from `max_radius` scaled to the sweep `angle` (upstream
/// `CurveDiscretizer::getCircularSegmentCount(r, angle)`, `CurveDiscretizer.cc`).
/// Upstream returns `max(1, (int)ceil(full · |angle|/360))`, so the angle-scaled
/// product is **ceil**'d (not floored): `$fn=10, angle=90` → `ceil(2.5) = 3`
/// facets. [`resolve_fragments`] floors `$fs`/`$fa` at `F_MINIMUM` (the shared
/// `clamp_fragment_vars`) and supplies the inner `ceil`; the result is
/// clamped into `[1, MAX_REVOLVE_SEGMENTS]`.
fn resolve_revolve_segments(max_radius: f64, fragments: Fragments, angle: f64) -> i32 {
    let full = resolve_fragments(max_radius, fragments.fn_, fragments.fs, fragments.fa);
    let scaled = (f64::from(full) * angle.abs() / 360.0).ceil();
    (scaled as i64).clamp(1, i64::from(MAX_REVOLVE_SEGMENTS)) as i32
}

// ─── boolean evaluation ──────────────────────────────────────────────────────
//
// The five CSG combinators over already-tessellated geometry — the raw kernel
// side of upstream `union`/`difference`/`intersection`/`hull`
// (`src/core/csgops.cc`, `src/core/cgaladv.cc`). The tree resolver
// (`resolve.rs`) walks a [`morphe_eval::csg::CsgNode`], tessellates the leaves, groups
// the results by dimensionality, and calls these on the resulting slices. They
// take pre-resolved `&[Solid]` (or `&[Profile]`); deciding 2D-vs-3D grouping,
// and erroring on a mix of the two (upstream's "Warning: Mixing 2D and 3D
// objects"), is the resolver's job, not these functions'.
//
// # `Option` and empty-result semantics
//
// The return is `Result<Option<_>, KernelError>`, but Manifold/Clipper2
// guarantee a valid (possibly empty) result from booleans on valid inputs, so
// these never actually `Err` — the `Result` keeps the signature uniform with
// the rest of the kernel and lets the resolver use one error channel. The rule is
// simple and identical across all five combinators (3D and 2D):
//
//   * `None` — **and only** — for an empty input slice (no operands: a childless
//     `union()`/`difference()`/…). Nothing ran; the op contributes no node.
//   * `Some(x)` for ANY non-empty slice — the op RAN. `x` may itself be empty
//     (`x.is_empty()`): a `difference` that removes everything, an
//     `intersection` of disjoint bodies, or even a slice whose operands are all
//     empty. An empty *result* is a valid empty body that exports as an empty
//     STL, matching upstream OpenSCAD — it is NOT collapsed to `None`.
//
// So `[]` → `None`, but `[empty]` → `Some(empty)`, and all three booleans agree
// (no minuend-is-special `None` case for `difference`): upstream `difference`
// collects its children unconditionally (`csgops.cc builtin_difference`), and an
// empty first child yields empty geometry rather than dropping the node — see
// `CsgNode::Difference`'s doc in `morphe_eval::csg`.
//
// A single operand is the identity for union/difference/intersection (returned
// as a clone — of an empty operand, still `Some(empty)`), but NOT for `hull` —
// the hull of one body is its convex hull, which differs from the body itself
// whenever the body is concave.
//
// # Batch vs. fold, and the 3D/2D API asymmetry
//
// Manifold exposes `Manifold::batch_union` / `batch_difference` / `batch_hull`
// (single-pass, more robust than a pairwise fold) but no public batch
// *intersection*, so 3D intersection folds with the pairwise
// `Manifold::intersection`. Clipper2's `CrossSection` does expose a public
// `batch_boolean(_, OpType)`, so all three 2D booleans go through a single
// batch call. `batch_difference` / `OpType::Subtract` both mean "the first
// minus the union of the rest", matching `difference`'s minuend-is-special rule.

/// Boolean union of solids — upstream `union()` (`csgops.cc builtin_union`).
///
/// `None` for an empty slice; a clone of the sole solid for one; otherwise
/// `Manifold::batch_union`. See the module section above for the empty-result
/// semantics.
pub fn union(solids: &[Solid]) -> Result<Option<Solid>, KernelError> {
    Ok(match solids {
        [] => None,
        [only] => Some(Solid::from_manifold(only.manifold().clone())),
        many => {
            let manifolds: Vec<Manifold> = many.iter().map(|s| s.manifold().clone()).collect();
            Some(Solid::from_manifold(Manifold::batch_union(&manifolds)))
        }
    })
}

/// Boolean difference of solids: `solids[0]` minus the union of `solids[1..]` —
/// upstream `difference()` (`csgops.cc builtin_difference`).
///
/// `None` only for an empty slice; a clone of the sole solid for one operand;
/// otherwise `Manifold::batch_difference`. A minuend fully removed by the
/// subtrahends — or an empty minuend — yields `Some(empty Solid)`, not `None`
/// (the op ran); see the module section above.
pub fn difference(solids: &[Solid]) -> Result<Option<Solid>, KernelError> {
    Ok(match solids {
        [] => None,
        [only] => Some(Solid::from_manifold(only.manifold().clone())),
        many => {
            let manifolds: Vec<Manifold> = many.iter().map(|s| s.manifold().clone()).collect();
            Some(Solid::from_manifold(Manifold::batch_difference(&manifolds)))
        }
    })
}

/// Boolean intersection of solids — upstream `intersection()`
/// (`csgops.cc builtin_intersection`).
///
/// `None` for an empty slice; a clone of the sole solid for one; otherwise the
/// pairwise `Manifold::intersection` folded left-to-right (Manifold exposes no
/// public batch intersection). Disjoint bodies yield `Some(empty Solid)` — see
/// the module section above.
pub fn intersection(solids: &[Solid]) -> Result<Option<Solid>, KernelError> {
    Ok(match solids {
        [] => None,
        [only] => Some(Solid::from_manifold(only.manifold().clone())),
        [first, rest @ ..] => {
            let acc = rest.iter().fold(first.manifold().clone(), |acc, s| {
                acc.intersection(s.manifold())
            });
            Some(Solid::from_manifold(acc))
        }
    })
}

/// Convex hull of solids — upstream 3D `hull()` (`cgaladv.cc builtin_hull`):
/// the convex hull of the combined vertices of every input body.
///
/// `None` only for an empty slice. Unlike the booleans a single operand is NOT
/// the identity — `hull([s])` is the convex hull of `s` (equal to `s` only when
/// `s` is already convex). Empty inputs contribute no vertices; a hull of only
/// empty bodies is `Some(empty Solid)`.
///
/// 2D hull of [`Profile`]s is [`hull_profiles`]; the tree resolver picks the
/// dimension from the children (upstream decides 2D-vs-3D the same way).
pub fn hull(solids: &[Solid]) -> Result<Option<Solid>, KernelError> {
    Ok(match solids {
        [] => None,
        many => {
            let manifolds: Vec<Manifold> = many.iter().map(|s| s.manifold().clone()).collect();
            Some(Solid::from_manifold(Manifold::batch_hull(&manifolds)))
        }
    })
}

// ─── 2D boolean evaluation ───────────────────────────────────────────────────
//
// The `Profile`/`CrossSection` twins of the 3D combinators above, needed so a
// `union`/`difference`/`intersection`/`hull` whose children are all 2D
// cross-sections composes into one `Profile` before a `linear_extrude` /
// `rotate_extrude` lifts it into a solid (upstream applies the 2D CSG op, then
// extrudes the combined `Polygon2d`). Semantics mirror the 3D functions exactly
// — same `Option`/empty rules, same minuend-is-special difference, same
// single-operand identity (except `hull`). Clipper2's `CrossSection` exposes a
// public `batch_boolean`, so all three booleans use a single batch call.

/// Boolean union of profiles — the 2D twin of [`union`], via
/// `CrossSection::batch_union`.
pub fn union_profiles(profiles: &[Profile]) -> Result<Option<Profile>, KernelError> {
    Ok(match profiles {
        [] => None,
        [only] => Some(Profile::from_cross_section(only.cross_section().clone())),
        many => {
            let sections: Vec<CrossSection> =
                many.iter().map(|p| p.cross_section().clone()).collect();
            Some(Profile::from_cross_section(CrossSection::batch_union(
                &sections,
            )))
        }
    })
}

/// Boolean difference of profiles: first minus the union of the rest — the 2D
/// twin of [`difference`], via `CrossSection::batch_boolean(_, Subtract)`.
pub fn difference_profiles(profiles: &[Profile]) -> Result<Option<Profile>, KernelError> {
    Ok(match profiles {
        [] => None,
        [only] => Some(Profile::from_cross_section(only.cross_section().clone())),
        many => {
            let sections: Vec<CrossSection> =
                many.iter().map(|p| p.cross_section().clone()).collect();
            Some(Profile::from_cross_section(CrossSection::batch_boolean(
                &sections,
                OpType::Subtract,
            )))
        }
    })
}

/// Boolean intersection of profiles — the 2D twin of [`intersection`], via
/// `CrossSection::batch_boolean(_, Intersect)`.
pub fn intersection_profiles(profiles: &[Profile]) -> Result<Option<Profile>, KernelError> {
    Ok(match profiles {
        [] => None,
        [only] => Some(Profile::from_cross_section(only.cross_section().clone())),
        many => {
            let sections: Vec<CrossSection> =
                many.iter().map(|p| p.cross_section().clone()).collect();
            Some(Profile::from_cross_section(CrossSection::batch_boolean(
                &sections,
                OpType::Intersect,
            )))
        }
    })
}

/// Convex hull of profiles — the 2D twin of [`hull`], via
/// `CrossSection::batch_hull`. As in 3D a single operand is NOT the identity.
///
/// One 2D/3D difference: a hull of only *empty* inputs is a zero-area section,
/// but Clipper2 hands it back as a single empty contour rather than a truly
/// empty region, so `Profile::is_empty()` reports `false` on it (Manifold's 3D
/// `batch_hull` instead yields a genuinely empty body). Either way the op ran,
/// so the return is `Some`, never `None`.
pub fn hull_profiles(profiles: &[Profile]) -> Result<Option<Profile>, KernelError> {
    Ok(match profiles {
        [] => None,
        many => {
            let sections: Vec<CrossSection> =
                many.iter().map(|p| p.cross_section().clone()).collect();
            Some(Profile::from_cross_section(CrossSection::batch_hull(
                &sections,
            )))
        }
    })
}

// ─── Affine / resize / color transforms ──────────────────────────────────────
//
// A [`CsgNode::Transform`](morphe_eval::csg::CsgNode::Transform) applies a
// [`TransformKind`] to the geometry of its (already-evaluated) child. The eval
// seam collapses `translate`/`rotate`/`scale`/`mirror`/`multmatrix` into a
// single row-major 4×4 [`Affine`] matrix (see the builders in
// `morphe_eval::eval`); `resize` and `color` keep distinct kinds because a
// resize needs the child bounding box only the kernel can measure and a color
// is non-geometric. [`apply_transform`] dispatches all three.
//
// # Matrix layout (transpose)
//
// The eval [`Affine`] is **row-major** — `a[row][col]`, translation in column 3
// — so a point `p` maps to `out[r] = Σ_c a[r][c]·p[c] + a[r][3]`. Manifold's
// `Manifold::transform` instead takes a **column-major** 4×3 `[f64; 12]` whose
// columns are the X/Y/Z basis vectors then the translation
// (`col k, row r → m[k*3 + r]`), i.e. the transpose of our upper 3×4 block. So
// [`solid_matrix`] copies `a[r][c] → m[c*3 + r]` for `r ∈ 0..3, c ∈ 0..4`; the
// bottom row `a[3]` (always `[0,0,0,1]` for the affines eval builds, since
// `multmatrix` already divided through by `m[3][3]`) is not needed. Worked
// example: `rotate([0,0,90])` builds the row-major rotation
// `[[0,-1,0],[1,0,0],[0,0,1]]`, whose column-major image sets the X-basis
// column to `(0,1,0)` — so the point `(1,0,0)` maps to `(0,1,0)`, i.e. X→Y,
// exactly as `rotate_z_90_maps_x_axis_to_y` asserts.
//
// # 2D projection
//
// A 2D [`Profile`] takes the same 4×4 affine but only its X/Y sub-block is
// applied; the Z row/column is dropped ([`profile_matrix`] keeps
// `a[0][0], a[1][0], a[0][1], a[1][1]` as the 2D basis and `a[0][3], a[1][3]` as
// the translation, feeding Manifold's column-major 3×2 `[f64; 6]`). This is the
// XY projection OpenSCAD applies when a 3D transform wraps a 2D child. For the
// in-plane transforms (`translate`/`scale`/`rotate` about Z / `mirror` across an
// in-plane axis) the Z row/column is genuinely irrelevant. For a Z-involving
// transform it is NOT a no-op: e.g. `rotate([90,0,0])` on a 2D child does not
// vanish — its X/Y sub-block `[[1,0],[0,0]]` collapses the Y basis, projecting
// the section onto the X axis, exactly as upstream projects such transforms
// onto XY.

/// Apply a resolved [`TransformKind`] to a child's [`Geometry`] — the kernel
/// side of a [`CsgNode::Transform`](morphe_eval::csg::CsgNode::Transform).
///
/// - [`TransformKind::Affine`] multiplies every vertex by the 4×4 matrix (2D
///   profiles use its X/Y sub-block; see the module section above). A non-finite
///   matrix entry is a [`KernelError::Degenerate`].
/// - [`TransformKind::Resize`] scales each axis so the child's bounding-box
///   extent becomes `newsize` (upstream `GeometryUtils::getResizeTransform`);
///   see [`resize_scale`]. A zero-extent axis is left unscaled (upstream warns
///   and skips it); a non-finite `newsize` component is a
///   [`KernelError::Degenerate`]; an empty child passes through unchanged.
/// - [`TransformKind::Color`] is non-geometric (STL ignores it), so it returns
///   the geometry unchanged — the color lives on the CSG node for a future
///   preview/exporter, not in the mesh.
pub fn apply_transform(kind: &TransformKind, geom: Geometry) -> Result<Geometry, KernelError> {
    match kind {
        TransformKind::Affine(m) => match geom {
            Geometry::Solid(s) => Ok(Geometry::Solid(Solid::from_manifold(
                s.manifold().transform(&solid_matrix(m)?),
            ))),
            Geometry::Profile(p) => Ok(Geometry::Profile(Profile::from_cross_section(
                p.cross_section().transform(&profile_matrix(m)?),
            ))),
        },
        TransformKind::Resize { newsize, auto } => match geom {
            Geometry::Solid(s) => resize_solid(*newsize, *auto, s).map(Geometry::Solid),
            Geometry::Profile(p) => resize_profile(*newsize, *auto, p).map(Geometry::Profile),
        },
        // Non-geometric: geometry is returned untouched (see the doc comment).
        TransformKind::Color(_) => Ok(geom),
    }
}

/// Row-major 4×4 [`Affine`] → Manifold's column-major 4×3 `[f64; 12]`
/// (`a[r][c] → m[c*3 + r]`, the transpose of the upper 3×4 block; see the
/// module section). A non-finite used entry is rejected before it reaches the
/// native kernel.
fn solid_matrix(a: &Affine) -> Result<[f64; 12], KernelError> {
    let mut m = [0.0f64; 12];
    for c in 0..4 {
        for r in 0..3 {
            let v = a[r][c];
            if !v.is_finite() {
                return Err(KernelError::Degenerate(format!(
                    "non-finite affine matrix entry a[{r}][{c}] = {v}"
                )));
            }
            m[c * 3 + r] = v;
        }
    }
    Ok(m)
}

/// Row-major 4×4 [`Affine`] → Manifold's column-major 3×2 `[f64; 6]` for a 2D
/// [`Profile`]: the X/Y basis (`a[0][0], a[1][0], a[0][1], a[1][1]`) and the
/// X/Y translation (`a[0][3], a[1][3]`); the Z row/column is dropped (see the
/// module section).
fn profile_matrix(a: &Affine) -> Result<[f64; 6], KernelError> {
    // (row, col) of each of the six values, in Manifold column-major order:
    // X basis, Y basis, translation.
    let picks = [(0, 0), (1, 0), (0, 1), (1, 1), (0, 3), (1, 3)];
    let mut m = [0.0f64; 6];
    for (slot, &(r, c)) in m.iter_mut().zip(picks.iter()) {
        let v = a[r][c];
        if !v.is_finite() {
            return Err(KernelError::Degenerate(format!(
                "non-finite affine matrix entry a[{r}][{c}] = {v}"
            )));
        }
        *slot = v;
    }
    Ok(m)
}

/// Per-axis resize scale factors — a port of upstream
/// `GeometryUtils::getResizeTransform` (`src/geometry/GeometryUtils.cc`,
/// master, fetched 2026-07-03):
///
/// ```text
/// int maxdim = argmax_i(newsize[i]);                  // governing axis
/// scale[i] = newsize[i] > 0 ? newsize[i] / dims[i] : 1;
/// autoscale = scale[maxdim];
/// out[i] = (!auto[i] || newsize[i] > 0) ? scale[i] : autoscale;
/// ```
///
/// So a `newsize[i]` of `0` keeps axis `i` at its natural size unless `auto[i]`
/// is set, in which case it copies the scale of the largest explicitly-sized
/// axis (preserving that axis's ratio). `N` is the dimensionality (3 for a
/// solid, 2 for a profile).
///
/// Two guards over the bare upstream expression, both keeping the model
/// renderable rather than erroring mid-scale:
/// - A positive `newsize[i]` on a **zero-extent** axis cannot be scaled
///   (`newsize / 0`). Upstream OpenSCAD warns and leaves that axis unscaled
///   (factor `1`), continuing the render; we match it with the `dims[i] > 0`
///   condition, so a flat axis simply keeps its size instead of blowing up to
///   an infinite scale.
/// - A **non-finite** `newsize` component (NaN/inf) is rejected up front as a
///   [`KernelError::Degenerate`], for parity with [`solid_matrix`] — an inf
///   newsize would otherwise drive an inf scale into Manifold.
fn resize_scale<const N: usize>(
    newsize: [f64; N],
    auto: [bool; N],
    dims: [f64; N],
) -> Result<[f64; N], KernelError> {
    for (i, &n) in newsize.iter().enumerate() {
        if !n.is_finite() {
            return Err(KernelError::Degenerate(format!(
                "non-finite resize newsize[{i}] = {n}"
            )));
        }
    }
    let mut maxdim = 0;
    for i in 1..N {
        if newsize[i] > newsize[maxdim] {
            maxdim = i;
        }
    }
    let mut scale = [1.0f64; N];
    for i in 0..N {
        // Skip a zero-extent axis: upstream warns and leaves it unscaled.
        if newsize[i] > 0.0 && dims[i] > 0.0 {
            scale[i] = newsize[i] / dims[i];
        }
    }
    let autoscale = scale[maxdim];
    let mut out = [1.0f64; N];
    for i in 0..N {
        out[i] = if !auto[i] || newsize[i] > 0.0 {
            scale[i]
        } else {
            autoscale
        };
    }
    Ok(out)
}

/// `resize(newsize, auto)` on a [`Solid`]: scale each axis so its bounding-box
/// extent becomes `newsize` (upstream `ManifoldGeometry::resize`). An empty
/// solid has no bounding box, so it passes through unchanged. The scale is a
/// pure diagonal about the origin (upstream builds an origin-scale matrix), so
/// [`Solid`]'s minimum corner moves proportionally — matching OpenSCAD.
fn resize_solid(newsize: [f64; 3], auto: [bool; 3], s: Solid) -> Result<Solid, KernelError> {
    let dims = match s.manifold().bounding_box() {
        Some(b) => b.dimensions(),
        None => return Ok(s), // empty solid: nothing to resize
    };
    let sc = resize_scale(newsize, auto, dims)?;
    Ok(Solid::from_manifold(
        s.manifold().scale(sc[0], sc[1], sc[2]),
    ))
}

/// `resize(newsize, auto)` on a [`Profile`] — the 2D twin of [`resize_solid`],
/// using the 2D bounding rectangle. Only the X/Y components of `newsize`/`auto`
/// apply (a 2D section has no Z extent). An empty profile passes through.
fn resize_profile(newsize: [f64; 3], auto: [bool; 3], p: Profile) -> Result<Profile, KernelError> {
    if p.is_empty() {
        return Ok(p); // empty profile: nothing to resize
    }
    let dims = p.cross_section().bounds().dimensions();
    let sc = resize_scale([newsize[0], newsize[1]], [auto[0], auto[1]], dims)?;
    Ok(Profile::from_cross_section(
        p.cross_section().scale(sc[0], sc[1]),
    ))
}

/// Push `nf` vertices of a circle of radius `r` at height `z`, at longitudes
/// `360*k/nf` degrees (upstream `generate_circle`, `primitives.cc` L59-65).
fn push_circle(verts: &mut Vec<[f64; 3]>, r: f64, z: f64, nf: usize) {
    for k in 0..nf {
        let phi = 360.0 * k as f64 / nf as f64;
        verts.push([r * deg_cos(phi), r * deg_sin(phi), z]);
    }
}

/// Fan-triangulate a convex polygon `[v0, v1, .., vm]` into
/// `(v0, v1, v2), (v0, v2, v3), ..`, preserving its winding.
fn fan(tris: &mut Vec<[u32; 3]>, poly: &[u32]) {
    for k in 1..poly.len().saturating_sub(1) {
        tris.push([poly[0], poly[k], poly[k + 1]]);
    }
}

/// Split quad `[a, b, c, d]` into triangles `(a, b, c), (a, c, d)`, preserving
/// its winding.
fn push_quad(tris: &mut Vec<[u32; 3]>, a: u32, b: u32, c: u32, d: u32) {
    tris.push([a, b, c]);
    tris.push([a, c, d]);
}

/// Cosine of an angle in degrees, snapping the four cardinal angles to exact
/// values so cardinal longitudes/latitudes land on the axes.
fn deg_cos(deg: f64) -> f64 {
    let d = deg.rem_euclid(360.0);
    if d == 0.0 {
        1.0
    } else if d == 90.0 || d == 270.0 {
        0.0
    } else if d == 180.0 {
        -1.0
    } else {
        d.to_radians().cos()
    }
}

/// Sine of an angle in degrees, snapping the four cardinal angles to exact
/// values.
fn deg_sin(deg: f64) -> f64 {
    let d = deg.rem_euclid(360.0);
    if d == 0.0 || d == 180.0 {
        0.0
    } else if d == 90.0 {
        1.0
    } else if d == 270.0 {
        -1.0
    } else {
        d.to_radians().sin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Signed volume of an indexed triangle mesh via the divergence theorem —
    /// `sum over tris of (v0 · (v1 × v2)) / 6`. Independent of Manifold, so a
    /// match against [`Solid::volume`] proves the kernel preserved our mesh.
    fn mesh_volume(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> f64 {
        let mut vol = 0.0;
        for t in tris {
            let a = verts[t[0] as usize];
            let b = verts[t[1] as usize];
            let c = verts[t[2] as usize];
            // a · (b × c)
            let cross = [
                b[1] * c[2] - b[2] * c[1],
                b[2] * c[0] - b[0] * c[2],
                b[0] * c[1] - b[1] * c[0],
            ];
            vol += a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2];
        }
        vol / 6.0
    }

    fn solid(prim: &Primitive) -> Solid {
        match tessellate_primitive(prim).expect("well-formed primitive tessellates") {
            Geometry::Solid(s) => s,
            Geometry::Profile(_) => panic!("3D primitive should yield a Solid, not a Profile"),
        }
    }

    fn profile(prim: &Primitive) -> Profile {
        match tessellate_primitive(prim).expect("well-formed primitive tessellates") {
            Geometry::Profile(p) => p,
            Geometry::Solid(_) => panic!("2D primitive should yield a Profile, not a Solid"),
        }
    }

    // ----- cube -----

    #[test]
    fn cube_uncentered_volume_and_counts() {
        let s = solid(&Primitive::Cube {
            size: [2.0, 3.0, 4.0],
            center: false,
        });
        assert_eq!(s.num_vert(), 8);
        assert_eq!(s.num_tri(), 12);
        assert!(s.is_manifold());
        assert!((s.volume() - 24.0).abs() < 1e-9, "got {}", s.volume());
    }

    #[test]
    fn cube_centered_is_origin_symmetric() {
        let s = solid(&Primitive::Cube {
            size: [2.0, 2.0, 2.0],
            center: true,
        });
        assert!((s.volume() - 8.0).abs() < 1e-9);
        let (verts, _) = s.to_mesh();
        // Centred: every corner is (+-1, +-1, +-1); the min corner is negative.
        let min_x = verts.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min);
        assert!((min_x - -1.0).abs() < 1e-9, "centred min x should be -1");
    }

    #[test]
    fn cube_uncentered_sits_in_positive_octant() {
        let s = solid(&Primitive::Cube {
            size: [1.0, 1.0, 1.0],
            center: false,
        });
        let (verts, _) = s.to_mesh();
        let min = verts
            .iter()
            .flat_map(|v| v.iter().copied())
            .fold(f64::INFINITY, f64::min);
        assert!((min - 0.0).abs() < 1e-9, "min corner should sit at origin");
    }

    #[test]
    fn cube_zero_size_is_degenerate() {
        let e = tessellate_primitive(&Primitive::Cube {
            size: [0.0, 1.0, 1.0],
            center: false,
        });
        assert!(matches!(e, Err(KernelError::Degenerate(_))));
    }

    // ----- sphere -----

    #[test]
    fn sphere_fn8_matches_upstream_counts() {
        // fragments = 8 -> num_rings = (8+1)/2 = 4, vertices = 4*8 = 32.
        // tris = top fan (8-2) + bottom fan (8-2) + 3 ring-gaps * 8 quads * 2.
        let s = solid(&Primitive::Sphere {
            r: 1.0,
            fragments: 8,
        });
        assert_eq!(s.num_vert(), 32, "num_rings*fragments = 4*8");
        assert_eq!(s.num_tri(), 6 + 6 + 3 * 8 * 2);
        assert!(s.is_manifold());
    }

    #[test]
    fn sphere_volume_below_analytic_and_positive() {
        let r = 2.0;
        let s = solid(&Primitive::Sphere { r, fragments: 16 });
        let analytic = 4.0 / 3.0 * PI * r * r * r;
        assert!(s.volume() > 0.0);
        // A ring-inscribed polytope under-fills the ball.
        assert!(
            s.volume() < analytic,
            "discrete {} should be below analytic {}",
            s.volume(),
            analytic
        );
    }

    #[test]
    fn sphere_converges_to_analytic_as_fragments_grow() {
        let r = 1.0;
        let analytic = 4.0 / 3.0 * PI * r * r * r;
        let coarse = solid(&Primitive::Sphere { r, fragments: 8 }).volume();
        let fine = solid(&Primitive::Sphere { r, fragments: 64 }).volume();
        assert!(
            (fine - analytic).abs() < (coarse - analytic).abs(),
            "finer sphere ({fine}) should be closer to {analytic} than coarse ({coarse})"
        );
        assert!(
            (fine - analytic).abs() / analytic < 0.01,
            "fn=64 sphere should be within 1% of analytic, got {fine}"
        );
    }

    #[test]
    fn sphere_volume_matches_recomputed_discrete_volume() {
        // Solid::volume must equal the volume of the exact mesh we generated.
        let r = 1.5;
        let fragments = 12;
        let s = solid(&Primitive::Sphere { r, fragments });
        // Rebuild the same mesh to compute its discrete volume directly.
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let nf = fragments as usize;
        let num_rings = fragments.div_ceil(2) as usize;
        for i in 0..num_rings {
            let phi = 180.0 * (i as f64 + 0.5) / num_rings as f64;
            push_circle(&mut verts, r * deg_sin(phi), r * deg_cos(phi), nf);
        }
        let mut tris: Vec<[u32; 3]> = Vec::new();
        let top: Vec<u32> = (0..nf as u32).collect();
        fan(&mut tris, &top);
        for i in 0..num_rings - 1 {
            for k in 0..nf {
                push_quad(
                    &mut tris,
                    (i * nf + (k + 1) % nf) as u32,
                    (i * nf + k) as u32,
                    ((i + 1) * nf + k) as u32,
                    ((i + 1) * nf + (k + 1) % nf) as u32,
                );
            }
        }
        let n = num_rings * nf;
        let bottom: Vec<u32> = (0..nf).map(|i| (n - i - 1) as u32).collect();
        fan(&mut tris, &bottom);

        let discrete = mesh_volume(&verts, &tris);
        assert!(
            (s.volume() - discrete).abs() < 1e-9,
            "solid volume {} vs discrete {}",
            s.volume(),
            discrete
        );
    }

    #[test]
    fn sphere_zero_radius_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Sphere {
                r: 0.0,
                fragments: 8
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn sphere_too_few_fragments_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Sphere {
                r: 1.0,
                fragments: 2
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn huge_fragment_counts_error_instead_of_panicking() {
        // `$fa=0; $fs=0` used to resolve to u32::MAX (capacity-overflow
        // panic) and `$fn=1e7` asks for ~5·10¹³ sphere vertices: both must
        // be a loud Resolve error before any allocation.
        for fragments in [u32::MAX, 10_000_000, 3_000] {
            let r = tessellate_primitive(&Primitive::Sphere { r: 1.0, fragments });
            assert!(
                matches!(&r, Err(KernelError::Resolve(m)) if m.contains("cap")),
                "sphere fragments={fragments}"
            );
        }
        let over = MAX_PRIMITIVE_FRAGMENTS + 1;
        assert!(matches!(
            tessellate_primitive(&Primitive::Circle {
                r: 1.0,
                fragments: over
            }),
            Err(KernelError::Resolve(_))
        ));
        assert!(matches!(
            tessellate_primitive(&Primitive::Cylinder {
                h: 1.0,
                r1: 1.0,
                r2: 1.0,
                center: false,
                fragments: u32::MAX
            }),
            Err(KernelError::Resolve(_))
        ));
        // At the cap a (linear-cost) circle still tessellates.
        assert!(tessellate_primitive(&Primitive::Circle {
            r: 1.0,
            fragments: MAX_PRIMITIVE_FRAGMENTS
        })
        .is_ok());
    }

    #[test]
    fn zero_fa_fs_sphere_resolves_to_upstream_count() {
        // End to end through the shared clamp: $fa=0, $fs=0, r=1 → 629
        // fragments (F_MINIMUM floor), a finite tessellable sphere.
        let fragments = resolve_fragments(1.0, 0.0, 0.0, 0.0);
        assert_eq!(fragments, 629);
        let s = solid(&Primitive::Sphere { r: 1.0, fragments });
        assert!(s.is_manifold());
    }

    // ----- cylinder -----

    #[test]
    fn cylinder_straight_counts_and_manifold() {
        // Straight cylinder: 2*nf verts; sides nf quads *2 + two caps of nf-2.
        let s = solid(&Primitive::Cylinder {
            h: 2.0,
            r1: 1.0,
            r2: 1.0,
            center: false,
            fragments: 6,
        });
        assert_eq!(s.num_vert(), 12);
        assert_eq!(s.num_tri(), 6 * 2 + 2 * (6 - 2));
        assert!(s.is_manifold());
    }

    #[test]
    fn cylinder_straight_volume_approaches_analytic() {
        let (r, h, fragments) = (1.0, 3.0, 64u32);
        let s = solid(&Primitive::Cylinder {
            h,
            r1: r,
            r2: r,
            center: false,
            fragments,
        });
        let analytic = PI * r * r * h;
        assert!(s.volume() < analytic, "inscribed prism under-fills");
        assert!(
            (s.volume() - analytic).abs() / analytic < 0.01,
            "fn=64 cylinder within 1% of {analytic}, got {}",
            s.volume()
        );
    }

    #[test]
    fn cylinder_cone_apex_at_top() {
        // r2 = 0 -> cone: nf + 1 verts (bottom circle + apex), sides nf tris,
        // bottom cap nf-2, no top cap.
        let fragments = 8;
        let s = solid(&Primitive::Cylinder {
            h: 2.0,
            r1: 1.0,
            r2: 0.0,
            center: false,
            fragments,
        });
        assert_eq!(s.num_vert(), 8 + 1);
        assert_eq!(s.num_tri(), 8 + (8 - 2));
        assert!(s.is_manifold());
        // Cone volume = (1/3) pi r^2 h, under-filled by the inscribed polygon.
        let analytic = PI * 1.0 * 1.0 * 2.0 / 3.0;
        assert!(s.volume() > 0.0 && s.volume() < analytic);
    }

    #[test]
    fn cylinder_inverted_cone_apex_at_bottom() {
        // r1 = 0 -> inverted cone: apex + top circle.
        let fragments = 8;
        let s = solid(&Primitive::Cylinder {
            h: 2.0,
            r1: 0.0,
            r2: 1.0,
            center: false,
            fragments,
        });
        assert_eq!(s.num_vert(), 1 + 8);
        assert_eq!(s.num_tri(), 8 + (8 - 2));
        assert!(s.is_manifold());
        assert!(s.volume() > 0.0);
    }

    #[test]
    fn cylinder_frustum_volume() {
        // Truncated cone (r1 != r2, both > 0): analytic frustum volume.
        let (r1, r2, h, fragments) = (2.0, 1.0, 3.0, 128u32);
        let s = solid(&Primitive::Cylinder {
            h,
            r1,
            r2,
            center: false,
            fragments,
        });
        let analytic = PI * h / 3.0 * (r1 * r1 + r1 * r2 + r2 * r2);
        assert!(
            (s.volume() - analytic).abs() / analytic < 0.01,
            "frustum within 1% of {analytic}, got {}",
            s.volume()
        );
        assert!(s.is_manifold());
    }

    #[test]
    fn cylinder_center_shifts_z_symmetric() {
        let h = 4.0;
        let s = solid(&Primitive::Cylinder {
            h,
            r1: 1.0,
            r2: 1.0,
            center: true,
            fragments: 6,
        });
        let (verts, _) = s.to_mesh();
        let min_z = verts.iter().map(|v| v[2]).fold(f64::INFINITY, f64::min);
        let max_z = verts.iter().map(|v| v[2]).fold(f64::NEG_INFINITY, f64::max);
        assert!((min_z - -h / 2.0).abs() < 1e-9);
        assert!((max_z - h / 2.0).abs() < 1e-9);
    }

    #[test]
    fn cylinder_matches_recomputed_discrete_volume() {
        // Solid::volume must equal the volume of the exact frustum mesh we
        // generate (a quad-sided drum with two cap fans) — computed here from
        // the PRE-Manifold generator geometry, independent of Manifold's own
        // output, so it validates our tessellation rather than a round-trip.
        let (h, r1, r2, fragments) = (2.0, 1.5, 0.7, 9u32);
        let center = true;
        let s = solid(&Primitive::Cylinder {
            h,
            r1,
            r2,
            center,
            fragments,
        });
        let nf = fragments as usize;
        let (z1, z2) = (-h / 2.0, h / 2.0);
        let mut verts: Vec<[f64; 3]> = Vec::new();
        push_circle(&mut verts, r1, z1, nf); // bottom 0..nf
        push_circle(&mut verts, r2, z2, nf); // top nf..2nf
        let mut tris: Vec<[u32; 3]> = Vec::new();
        let nf_u = nf as u32;
        for i in 0..nf_u {
            let j = (i + 1) % nf_u;
            push_quad(&mut tris, i, j, j + nf_u, i + nf_u);
        }
        let bottom: Vec<u32> = (0..nf_u).map(|i| nf_u - i - 1).collect();
        fan(&mut tris, &bottom);
        let top: Vec<u32> = (0..nf_u).map(|i| nf_u + i).collect();
        fan(&mut tris, &top);

        let discrete = mesh_volume(&verts, &tris);
        assert!(
            (s.volume() - discrete).abs() < 1e-9,
            "solid volume {} vs discrete {}",
            s.volume(),
            discrete
        );
    }

    #[test]
    fn cylinder_zero_height_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Cylinder {
                h: 0.0,
                r1: 1.0,
                r2: 1.0,
                center: false,
                fragments: 6
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn cylinder_both_radii_zero_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Cylinder {
                h: 2.0,
                r1: 0.0,
                r2: 0.0,
                center: false,
                fragments: 6
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    // ----- polyhedron -----

    /// A unit cube as an OpenSCAD `polyhedron`, faces ordered
    /// clockwise-when-viewed-from-outside (the manual's convention, as in its
    /// cube example). This module reverses each face to feed Manifold.
    fn cube_polyhedron() -> Primitive {
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let faces = vec![
            vec![0, 1, 2, 3], // bottom
            vec![4, 5, 1, 0], // front
            vec![7, 6, 5, 4], // top
            vec![5, 6, 2, 1], // right
            vec![6, 7, 3, 2], // back
            vec![7, 4, 0, 3], // left
        ];
        Primitive::Polyhedron {
            points,
            faces,
            convexity: 1,
        }
    }

    #[test]
    fn polyhedron_cube_positive_volume_and_manifold() {
        let s = solid(&cube_polyhedron());
        assert!(s.is_manifold());
        assert!(
            (s.volume() - 1.0).abs() < 1e-9,
            "reversed-winding cube volume should be +1, got {}",
            s.volume()
        );
        assert_eq!(s.num_vert(), 8);
    }

    #[test]
    fn polyhedron_ngon_faces_fan_triangulate() {
        // The cube uses 4-gon (quad) faces; a correct fan triangulation gives
        // 12 triangles (2 per face). Proves n-gon faces are handled.
        let s = solid(&cube_polyhedron());
        assert_eq!(s.num_tri(), 12);
    }

    #[test]
    fn polyhedron_tetrahedron_from_triangular_faces() {
        // Tetra, faces clockwise-from-outside so the reverse yields outward
        // normals (positive volume).
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let faces = vec![
            vec![0, 1, 2], // z = 0 base
            vec![0, 2, 3], // x = 0
            vec![0, 3, 1], // y = 0
            vec![1, 3, 2], // slanted
        ];
        let s = solid(&Primitive::Polyhedron {
            points,
            faces,
            convexity: 1,
        });
        assert!(s.is_manifold());
        assert_eq!(s.num_vert(), 4);
        assert_eq!(s.num_tri(), 4);
        assert!(
            (s.volume() - 1.0 / 6.0).abs() < 1e-9,
            "tetra volume should be 1/6, got {}",
            s.volume()
        );
    }

    #[test]
    fn polyhedron_too_few_faces_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Polyhedron {
                points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                faces: vec![vec![0, 1, 2]],
                convexity: 1,
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn polyhedron_non_manifold_is_rejected() {
        // Faces [0,3,1] and [0,1,3] are the same triangle wound in opposite
        // directions: after this module reverses each, edge 0-3 is shared by
        // three faces (a doubled/duplicated triangle), so the surface is not a
        // manifold and Manifold rejects it as a Solid error.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let faces = vec![vec![0, 2, 3], vec![0, 3, 1], vec![1, 3, 2], vec![0, 1, 3]];
        let r = tessellate_primitive(&Primitive::Polyhedron {
            points,
            faces,
            convexity: 1,
        });
        assert!(matches!(r, Err(KernelError::Solid(_))));
    }

    #[test]
    fn polyhedron_out_of_range_index_is_error() {
        // Face 3 references point index 9 with only 4 points: caught as a
        // BadIndex before any buffer reaches native Manifold (no OOB/crash).
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let faces = vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 1], vec![1, 3, 9]];
        let r = tessellate_primitive(&Primitive::Polyhedron {
            points,
            faces,
            convexity: 1,
        });
        assert!(matches!(r, Err(KernelError::BadIndex(_))));
    }

    #[test]
    fn polyhedron_non_finite_point_is_error() {
        // A NaN coordinate is rejected as Degenerate, matching the finite-only
        // contract of cube/sphere/cylinder.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, f64::NAN],
        ];
        let faces = vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 1], vec![1, 3, 2]];
        let r = tessellate_primitive(&Primitive::Polyhedron {
            points,
            faces,
            convexity: 1,
        });
        assert!(matches!(r, Err(KernelError::Degenerate(_))));
    }

    // ----- square (2D) -----

    #[test]
    fn square_uncentered_area_and_counts() {
        let p = profile(&Primitive::Square {
            size: [2.0, 3.0],
            center: false,
        });
        assert_eq!(p.num_contour(), 1);
        assert_eq!(p.num_vert(), 4, "a rectangle has 4 corners");
        assert!(!p.is_empty());
        assert!((p.area() - 6.0).abs() < 1e-9, "2x3 area, got {}", p.area());
    }

    #[test]
    fn square_uncentered_sits_in_positive_quadrant() {
        let p = profile(&Primitive::Square {
            size: [1.0, 1.0],
            center: false,
        });
        let min = p
            .to_polygons()
            .iter()
            .flatten()
            .flat_map(|v| v.iter().copied())
            .fold(f64::INFINITY, f64::min);
        assert!((min - 0.0).abs() < 1e-9, "min corner should sit at origin");
    }

    #[test]
    fn square_centered_is_origin_symmetric() {
        let p = profile(&Primitive::Square {
            size: [2.0, 2.0],
            center: true,
        });
        assert!((p.area() - 4.0).abs() < 1e-9);
        let verts = p.to_polygons();
        let min_x = verts
            .iter()
            .flatten()
            .map(|v| v[0])
            .fold(f64::INFINITY, f64::min);
        let max_x = verts
            .iter()
            .flatten()
            .map(|v| v[0])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!((min_x - -1.0).abs() < 1e-9, "centred min x should be -1");
        assert!((max_x - 1.0).abs() < 1e-9, "centred max x should be +1");
    }

    #[test]
    fn square_zero_size_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Square {
                size: [0.0, 1.0],
                center: false
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn square_negative_size_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Square {
                size: [1.0, -2.0],
                center: false
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    // ----- circle (2D) -----

    #[test]
    fn circle_fn6_is_a_hexagon() {
        // fragments = 6 -> a regular hexagon: 6 vertices, area (3*sqrt(3)/2)*r^2.
        let r = 2.0;
        let p = profile(&Primitive::Circle { r, fragments: 6 });
        assert_eq!(p.num_contour(), 1);
        assert_eq!(p.num_vert(), 6, "fn=6 is a hexagon");
        let expected = 3.0 * 3.0_f64.sqrt() / 2.0 * r * r;
        // Manifold's Clipper2 backend snaps vertices to a fine grid, so an area
        // from irrational (non-axis-aligned) vertices is inexact. Observed error
        // here is ~8e-9 absolute (~7e-10 relative); a 1e-8 relative tolerance keeps
        // ~13x margin over that while still catching any real regression.
        assert!(
            (p.area() - expected).abs() / expected < 1e-8,
            "hexagon area {} vs analytic {}",
            p.area(),
            expected
        );
    }

    #[test]
    fn circle_fn4_is_a_square_diamond() {
        // fragments = 4 -> vertices at 0/90/180/270 deg: a diamond of area 2*r^2.
        let r = 1.5;
        let p = profile(&Primitive::Circle { r, fragments: 4 });
        assert_eq!(p.num_vert(), 4);
        assert!(
            (p.area() - 2.0 * r * r).abs() < 1e-9,
            "fn=4 diamond area {} vs 2r^2 {}",
            p.area(),
            2.0 * r * r
        );
    }

    #[test]
    fn circle_area_matches_regular_ngon_formula() {
        // Regular n-gon inscribed in radius r: area = (n/2) * r^2 * sin(2pi/n).
        let (r, fragments) = (3.0, 12u32);
        let p = profile(&Primitive::Circle { r, fragments });
        let n = fragments as f64;
        let analytic = 0.5 * n * r * r * (2.0 * PI / n).sin();
        // Same Clipper2 grid-snap as the hexagon: observed error ~2e-8 absolute
        // (~7e-10 relative), gated at 1e-8 relative (~13x margin).
        assert!(
            (p.area() - analytic).abs() / analytic < 1e-8,
            "n-gon area {} vs formula {}",
            p.area(),
            analytic
        );
    }

    #[test]
    fn circle_converges_to_pi_r_squared_as_fragments_grow() {
        let r = 1.0;
        let analytic = PI * r * r;
        let coarse = profile(&Primitive::Circle { r, fragments: 8 }).area();
        let fine = profile(&Primitive::Circle { r, fragments: 256 }).area();
        // The inscribed polygon under-fills the disc and tightens as fn grows.
        assert!(
            coarse < analytic && fine < analytic,
            "inscribed under-fills"
        );
        assert!(
            (fine - analytic).abs() < (coarse - analytic).abs(),
            "finer circle ({fine}) should approach {analytic} closer than coarse ({coarse})"
        );
        assert!(
            (fine - analytic).abs() / analytic < 0.01,
            "fn=256 circle area should be within 1% of pi*r^2, got {fine}"
        );
    }

    #[test]
    fn circle_zero_radius_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Circle {
                r: 0.0,
                fragments: 16
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn circle_too_few_fragments_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Circle {
                r: 1.0,
                fragments: 2
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    // ----- polygon (2D) -----

    #[test]
    fn polygon_single_outline_area_shoelace() {
        // A right triangle (0,0)-(4,0)-(0,3): shoelace area = 0.5*4*3 = 6.
        let p = profile(&Primitive::Polygon {
            points: vec![[0.0, 0.0], [4.0, 0.0], [0.0, 3.0]],
            paths: None,
            convexity: 1,
        });
        assert_eq!(p.num_contour(), 1);
        assert!(
            (p.area() - 6.0).abs() < 1e-9,
            "triangle area, got {}",
            p.area()
        );
    }

    #[test]
    fn polygon_concave_outline_area() {
        // An L-shape (unit square notched out of a 2x2 square): area = 3.
        let p = profile(&Primitive::Polygon {
            points: vec![
                [0.0, 0.0],
                [2.0, 0.0],
                [2.0, 1.0],
                [1.0, 1.0],
                [1.0, 2.0],
                [0.0, 2.0],
            ],
            paths: None,
            convexity: 2,
        });
        assert!(
            (p.area() - 3.0).abs() < 1e-9,
            "L-shape area, got {}",
            p.area()
        );
    }

    #[test]
    fn polygon_with_hole_via_paths_subtracts_area() {
        // 6x6 outer with a 2x2 inner hole (both CCW): area = 36 - 4 = 32.
        let p = profile(&Primitive::Polygon {
            points: vec![
                [0.0, 0.0],
                [6.0, 0.0],
                [6.0, 6.0],
                [0.0, 6.0],
                [2.0, 2.0],
                [4.0, 2.0],
                [4.0, 4.0],
                [2.0, 4.0],
            ],
            paths: Some(vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]]),
            convexity: 2,
        });
        assert_eq!(p.num_contour(), 2, "outer + hole");
        assert!(
            (p.area() - 32.0).abs() < 1e-9,
            "outer(36) - hole(4) = 32, got {}",
            p.area()
        );
    }

    #[test]
    fn polygon_hole_is_winding_independent() {
        // Same 6x6 outer + 2x2 inner, but the inner ring wound CLOCKWISE.
        // Even-odd fill still carves it as a hole -> area = 32 regardless.
        let p = profile(&Primitive::Polygon {
            points: vec![
                [0.0, 0.0],
                [6.0, 0.0],
                [6.0, 6.0],
                [0.0, 6.0],
                [2.0, 2.0],
                [2.0, 4.0],
                [4.0, 4.0],
                [4.0, 2.0],
            ],
            paths: Some(vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]]),
            convexity: 2,
        });
        assert!(
            (p.area() - 32.0).abs() < 1e-9,
            "cw hole still subtracts -> 32, got {}",
            p.area()
        );
    }

    #[test]
    fn polygon_outline_area_is_winding_independent() {
        // The same square wound CW vs CCW encloses the same positive area.
        let ccw = profile(&Primitive::Polygon {
            points: vec![[0.0, 0.0], [5.0, 0.0], [5.0, 5.0], [0.0, 5.0]],
            paths: None,
            convexity: 1,
        });
        let cw = profile(&Primitive::Polygon {
            points: vec![[0.0, 0.0], [0.0, 5.0], [5.0, 5.0], [5.0, 0.0]],
            paths: None,
            convexity: 1,
        });
        assert!((ccw.area() - 25.0).abs() < 1e-9);
        assert!((cw.area() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn polygon_too_few_points_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0]],
                paths: None,
                convexity: 1,
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn polygon_non_finite_point_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, f64::NAN]],
                paths: None,
                convexity: 1,
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn polygon_empty_paths_is_degenerate() {
        assert!(matches!(
            tessellate_primitive(&Primitive::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                paths: Some(vec![]),
                convexity: 1,
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn polygon_out_of_range_path_index_is_error() {
        // Path references point index 5 with only 3 points: a BadIndex, caught
        // before any buffer reaches the native kernel.
        assert!(matches!(
            tessellate_primitive(&Primitive::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                paths: Some(vec![vec![0, 1, 5]]),
                convexity: 1,
            }),
            Err(KernelError::BadIndex(_))
        ));
    }

    #[test]
    fn polygon_path_with_too_few_indices_is_degenerate() {
        // Four points but a path naming only two of them: the contour cannot
        // enclose area, so it is a Degenerate error rather than a silent empty
        // Profile.
        assert!(matches!(
            tessellate_primitive(&Primitive::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                paths: Some(vec![vec![0, 1]]),
                convexity: 1,
            }),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn polygon_unreferenced_nan_point_is_ignored() {
        // A NaN in point 3, which no path references: upstream reads only the
        // referenced indices, so this renders the (0,0)-(4,0)-(0,3) triangle
        // fine, area = 6.
        let p = profile(&Primitive::Polygon {
            points: vec![[0.0, 0.0], [4.0, 0.0], [0.0, 3.0], [f64::NAN, f64::NAN]],
            paths: Some(vec![vec![0, 1, 2]]),
            convexity: 1,
        });
        assert!(
            (p.area() - 6.0).abs() < 1e-9,
            "triangle area ignoring unreferenced NaN, got {}",
            p.area()
        );
    }

    // ----- linear_extrude -----

    /// A `side x side` square centred on the origin — max radius `side/√2` at
    /// the corners, so it exercises the radius-dependent slice estimators.
    fn centred_square(side: f64) -> Profile {
        let h = side / 2.0;
        Profile::from_contours(
            &[vec![[-h, -h], [h, -h], [h, h], [-h, h]]],
            FillRule::EvenOdd,
        )
    }

    /// [`LinearExtrudeParams`] with the upstream default `$fn=0, $fs=2, $fa=12`,
    /// `convexity=1`, uncentred — the knobs under test are passed explicitly.
    fn extrude_params(
        height: f64,
        center: bool,
        twist: f64,
        slices: Option<u32>,
        scale: [f64; 2],
    ) -> LinearExtrudeParams {
        LinearExtrudeParams {
            height,
            center,
            twist,
            slices,
            scale,
            convexity: 1,
            fragments: Fragments {
                fn_: 0.0,
                fs: 2.0,
                fa: 12.0,
            },
        }
    }

    #[test]
    fn straight_extrude_volume_is_area_times_height() {
        // side-2 square (area 4) swept 5 tall: an exact 4*5 = 20 prism.
        let p = centred_square(2.0);
        let s = linear_extrude(&p, &extrude_params(5.0, false, 0.0, None, [1.0, 1.0]))
            .expect("straight extrude of a square");
        assert!(s.is_manifold());
        assert!(
            (s.volume() - 20.0).abs() < 1e-9,
            "prism volume should be area*height = 20, got {}",
            s.volume()
        );
    }

    #[test]
    fn straight_extrude_uncentred_z_range_is_zero_to_height() {
        let p = centred_square(2.0);
        let s = linear_extrude(&p, &extrude_params(5.0, false, 0.0, None, [1.0, 1.0])).unwrap();
        let (verts, _) = s.to_mesh();
        let min_z = verts.iter().map(|v| v[2]).fold(f64::INFINITY, f64::min);
        let max_z = verts.iter().map(|v| v[2]).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (min_z - 0.0).abs() < 1e-9,
            "uncentred base at z=0, got {min_z}"
        );
        assert!((max_z - 5.0).abs() < 1e-9, "top at z=height, got {max_z}");
    }

    #[test]
    fn centred_extrude_z_range_is_symmetric() {
        // center=true shifts the [0,h] extrusion to [-h/2, h/2].
        let p = centred_square(2.0);
        let h = 6.0;
        let s = linear_extrude(&p, &extrude_params(h, true, 0.0, None, [1.0, 1.0])).unwrap();
        let (verts, _) = s.to_mesh();
        let min_z = verts.iter().map(|v| v[2]).fold(f64::INFINITY, f64::min);
        let max_z = verts.iter().map(|v| v[2]).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (min_z + h / 2.0).abs() < 1e-9,
            "centred base at -h/2, got {min_z}"
        );
        assert!(
            (max_z - h / 2.0).abs() < 1e-9,
            "centred top at +h/2, got {max_z}"
        );
        assert!(
            (s.volume() - 24.0).abs() < 1e-9,
            "volume unchanged by centring"
        );
    }

    #[test]
    fn twist_90_matches_openscad_sign() {
        // Oracle (OpenSCAD 2021.01): a bar along +X, twisted +90, lands along
        // -Y at the top (positive twist is clockwise looking down +Z). A wrong
        // Manifold sign would put the far arm at +Y instead.
        let bar = Profile::from_contours(
            &[vec![[0.0, -0.5], [10.0, -0.5], [10.0, 0.5], [0.0, 0.5]]],
            FillRule::EvenOdd,
        );
        let s = linear_extrude(
            &bar,
            &extrude_params(10.0, false, 90.0, Some(40), [1.0, 1.0]),
        )
        .unwrap();
        assert!(s.is_manifold());
        let (verts, _) = s.to_mesh();
        // Among top-layer vertices, the far arm is the one furthest from the Z axis.
        let far = verts
            .iter()
            .filter(|v| v[2] > 9.9)
            .max_by(|a, b| {
                (a[0] * a[0] + a[1] * a[1])
                    .partial_cmp(&(b[0] * b[0] + b[1] * b[1]))
                    .unwrap()
            })
            .copied()
            .expect("top layer has vertices");
        assert!(
            far[1] < -5.0 && far[0].abs() < 1.0,
            "top far arm should sit at -Y (clockwise-down twist), got {far:?}"
        );
    }

    #[test]
    fn twist_360_preserves_volume_and_is_manifold() {
        // Twist rotates each cross-section but preserves its area, so a finely
        // sliced twisted prism keeps ~area*height. The straight-line lerp
        // between slices under-fills, shrinking as O(1/slices) (same as
        // upstream, which also lerps between slices) — at 360 slices (1°/slice)
        // the deficit is ~0.6%, so it stays just below expected.
        let p = centred_square(2.0); // area 4
        let expected = 4.0 * 10.0;
        let s = linear_extrude(
            &p,
            &extrude_params(10.0, false, 360.0, Some(360), [1.0, 1.0]),
        )
        .unwrap();
        assert!(s.is_manifold());
        assert!(s.volume() > 0.0);
        assert!(
            s.volume() <= expected * 1.000_001,
            "twist under-fills, so volume {} should not exceed {expected}",
            s.volume()
        );
        assert!(
            (s.volume() - expected).abs() / expected < 0.01,
            "finely-sliced twist volume {} should be within 1% of {expected}",
            s.volume()
        );
    }

    #[test]
    fn twist_90_is_manifold_with_default_slices() {
        // No explicit slices: the kernel derives them from twist + $fa/$fs.
        let p = centred_square(2.0);
        let s = linear_extrude(&p, &extrude_params(8.0, false, 90.0, None, [1.0, 1.0])).unwrap();
        assert!(s.is_manifold());
        assert!(s.volume() > 0.0);
    }

    #[test]
    fn finer_slices_refine_twist_toward_analytic_volume() {
        // More slices → the lerp under-fill shrinks → volume closer to area*h.
        let p = centred_square(2.0);
        let expected = 4.0 * 10.0;
        let coarse = linear_extrude(&p, &extrude_params(10.0, false, 360.0, Some(6), [1.0, 1.0]))
            .unwrap()
            .volume();
        let fine = linear_extrude(
            &p,
            &extrude_params(10.0, false, 360.0, Some(90), [1.0, 1.0]),
        )
        .unwrap()
        .volume();
        assert!(
            (fine - expected).abs() < (coarse - expected).abs(),
            "finer twist ({fine}) should approach {expected} closer than coarse ({coarse})"
        );
    }

    #[test]
    fn scale_2_2_frustum_volume_is_exact() {
        // Uniform scale, no twist → one slice → an exact square frustum.
        // Prismatoid volume = h/3 * (A0 + A1 + sqrt(A0*A1)):
        // base 2x2 (A0=4), top 4x4 (A1=16) → 3/3 * (4+16+8) = 28.
        let p = centred_square(2.0);
        let s = linear_extrude(&p, &extrude_params(3.0, false, 0.0, None, [2.0, 2.0])).unwrap();
        assert!(s.is_manifold());
        assert!(
            (s.volume() - 28.0).abs() < 1e-6,
            "square frustum volume should be 28, got {}",
            s.volume()
        );
    }

    #[test]
    fn scale_0_0_tip_is_a_pyramid() {
        // Scale to a point → a pyramid: volume = base_area * height / 3.
        // base 2x2 (area 4), height 3 → 4.
        let p = centred_square(2.0);
        let s = linear_extrude(&p, &extrude_params(3.0, false, 0.0, None, [0.0, 0.0])).unwrap();
        assert!(s.is_manifold());
        assert!(
            (s.volume() - 4.0).abs() < 1e-6,
            "pyramid volume should be area*height/3 = 4, got {}",
            s.volume()
        );
    }

    #[test]
    fn negative_scale_clamps_to_zero() {
        // A negative scale clamps to 0 (upstream), so [-1,-1] == [0,0]: same
        // pyramid, not an inverted/degenerate solid.
        let p = centred_square(2.0);
        let tip = linear_extrude(&p, &extrude_params(3.0, false, 0.0, None, [0.0, 0.0]))
            .unwrap()
            .volume();
        let clamped =
            linear_extrude(&p, &extrude_params(3.0, false, 0.0, None, [-1.0, -1.0])).unwrap();
        assert!(clamped.is_manifold());
        assert!(
            (clamped.volume() - tip).abs() < 1e-9,
            "negative scale should clamp to a point like [0,0] ({tip}), got {}",
            clamped.volume()
        );
    }

    #[test]
    fn more_slices_add_triangles_under_twist() {
        // Slice count controls vertical subdivision: more slices, more faces,
        // same (area-preserving) twist, both watertight.
        let p = centred_square(2.0);
        let coarse =
            linear_extrude(&p, &extrude_params(10.0, false, 180.0, Some(4), [1.0, 1.0])).unwrap();
        let fine = linear_extrude(
            &p,
            &extrude_params(10.0, false, 180.0, Some(20), [1.0, 1.0]),
        )
        .unwrap();
        assert!(coarse.is_manifold() && fine.is_manifold());
        assert!(
            fine.num_tri() > coarse.num_tri(),
            "20 slices ({}) should have more triangles than 4 ({})",
            fine.num_tri(),
            coarse.num_tri()
        );
    }

    #[test]
    fn explicit_slices_win_over_derivation() {
        // has_slices short-circuits the estimators regardless of twist.
        let p = centred_square(2.0);
        let params = extrude_params(10.0, false, 360.0, Some(11), [1.0, 1.0]);
        assert_eq!(resolve_slices(&p, &params, [1.0, 1.0]), 11);
    }

    #[test]
    fn slices_from_twist_uniform_matches_helix_formula() {
        // side-2 centred square: corner radius √2 (max_r_sqr = 2), twist 360,
        // height 10, defaults $fs=2/$fa=12. Upstream getHelixSlices:
        //   min_slices = ceil(360/120) = 3
        //   fa_slices  = ceil(360/12)  = 30
        //   arc = T*sqrt(r²+c²), T=2π, c=10/2π=1.5915 → sqrt(2+2.533)=2.129
        //         → 6.283*2.129 = 13.38; fs_slices = ceil(13.38/2) = 7
        //   result = max(min(30,7), 3) = 7
        let p = centred_square(2.0);
        let params = extrude_params(10.0, false, 360.0, None, [1.0, 1.0]);
        assert_eq!(resolve_slices(&p, &params, [1.0, 1.0]), 7);
    }

    #[test]
    fn slices_from_twist_zero_is_one() {
        // No twist, uniform scale → a single slice (a plain prism).
        let p = centred_square(2.0);
        let params = extrude_params(10.0, false, 0.0, None, [1.0, 1.0]);
        assert_eq!(resolve_slices(&p, &params, [1.0, 1.0]), 1);
    }

    #[test]
    fn slices_from_twist_fn_override() {
        // $fn set: getHelixSlices uses ceil(twist/360 * fn) = ceil(360/360*24)=24,
        // floored at min_slices=3 → 24.
        let p = centred_square(2.0);
        let mut params = extrude_params(10.0, false, 360.0, None, [1.0, 1.0]);
        params.fragments.fn_ = 24.0;
        assert_eq!(resolve_slices(&p, &params, [1.0, 1.0]), 24);
    }

    #[test]
    fn slices_from_diagonal_nonuniform_scale_no_twist() {
        // Non-uniform scale, no twist → getDiagonalSlices. side-2 centred square
        // scaled [3,1]: max |(scale-1)·v|² at a corner is ((3-1)*1)² = 4.
        // fs_slices = ceil(sqrt(4 + 10²)/2) = ceil(sqrt(104)/2) = ceil(5.0990) = 6.
        let p = centred_square(2.0);
        let params = extrude_params(10.0, false, 0.0, None, [3.0, 1.0]);
        assert_eq!(resolve_slices(&p, &params, [3.0, 1.0]), 6);
    }

    #[test]
    fn height_zero_is_degenerate() {
        let p = centred_square(2.0);
        assert!(matches!(
            linear_extrude(&p, &extrude_params(0.0, false, 0.0, None, [1.0, 1.0])),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn height_negative_is_degenerate() {
        let p = centred_square(2.0);
        assert!(matches!(
            linear_extrude(&p, &extrude_params(-3.0, false, 0.0, None, [1.0, 1.0])),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn empty_profile_is_degenerate() {
        let empty = Profile::from_contours(&[], FillRule::EvenOdd);
        assert!(empty.is_empty());
        assert!(matches!(
            linear_extrude(&empty, &extrude_params(5.0, false, 0.0, None, [1.0, 1.0])),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn non_finite_twist_is_degenerate() {
        // An infinite/NaN twist would drive the ceil() slice estimators to
        // i32::MAX divisions and OOM Manifold; reject before resolve_slices.
        let p = centred_square(2.0);
        for twist in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert!(
                matches!(
                    linear_extrude(&p, &extrude_params(5.0, false, twist, None, [1.0, 1.0])),
                    Err(KernelError::Degenerate(_))
                ),
                "twist {twist} should be Degenerate"
            );
        }
    }

    #[test]
    fn non_finite_scale_is_degenerate() {
        // A non-finite scale poisons the delta-based estimators the same way.
        let p = centred_square(2.0);
        for scale in [[f64::INFINITY, 1.0], [1.0, f64::NAN]] {
            assert!(
                matches!(
                    linear_extrude(&p, &extrude_params(5.0, false, 0.0, None, scale)),
                    Err(KernelError::Degenerate(_))
                ),
                "scale {scale:?} should be Degenerate"
            );
        }
    }

    #[test]
    fn huge_explicit_slices_clamp_to_cap() {
        // A slices value above i32::MAX must clamp (not truncate) to the cap.
        let p = centred_square(2.0);
        let params = extrude_params(10.0, false, 0.0, Some(u32::MAX), [1.0, 1.0]);
        assert_eq!(resolve_slices(&p, &params, [1.0, 1.0]), MAX_EXTRUDE_SLICES);
    }

    #[test]
    fn huge_scale_delta_slices_clamp_to_cap() {
        // An extreme scale delta over the default $fs drives getDiagonalSlices
        // arbitrarily high; the morphe cap holds it to MAX_EXTRUDE_SLICES.
        let p = centred_square(2.0);
        let scale = [1.0e6, 1.0];
        let params = extrude_params(10.0, false, 0.0, None, scale);
        assert_eq!(resolve_slices(&p, &params, scale), MAX_EXTRUDE_SLICES);
    }

    #[test]
    fn capped_extrude_still_builds_a_manifold() {
        // The cap must yield a real watertight solid, not an OOM/abort. A plain
        // (no twist/scale) prism keeps the mesh cheap while still exercising the
        // clamped nDivisions path end to end through Manifold.
        let p = centred_square(2.0);
        let s = linear_extrude(
            &p,
            &extrude_params(10.0, false, 0.0, Some(u32::MAX), [1.0, 1.0]),
        )
        .expect("capped extrude builds");
        assert!(s.is_manifold());
        assert!(
            (s.volume() - 40.0).abs() < 1e-6,
            "capped straight prism volume should still be 40, got {}",
            s.volume()
        );
    }

    // ----- rotate_extrude -----

    /// An axis-parallel rectangle profile `x ∈ [x0, x1]`, `y ∈ [y0, y1]` wound
    /// counter-clockwise. When `x0 > 0` it is a ring cross-section offset from
    /// the revolve (Z) axis; `x0 = 0` makes it touch the axis (no inner hole).
    fn rect_profile(x0: f64, x1: f64, y0: f64, y1: f64) -> Profile {
        Profile::from_contours(
            &[vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]],
            FillRule::EvenOdd,
        )
    }

    /// A regular `m`-gon of radius `r` centred at `(cx, 0)`, vertex 0 at angle 0
    /// (so its max x is `cx + r`) — the polygonal `circle` a `rotate_extrude`
    /// actually revolves into a (faceted) torus when `cx > r`.
    fn ngon_profile(m: usize, r: f64, cx: f64) -> Profile {
        let contour: Vec<[f64; 2]> = (0..m)
            .map(|k| {
                let a = 2.0 * PI * k as f64 / m as f64;
                [cx + r * a.cos(), r * a.sin()]
            })
            .collect();
        Profile::from_contours(&[contour], FillRule::EvenOdd)
    }

    /// [`RotateExtrudeParams`] with an explicit `$fn` (so the segment count is
    /// deterministic) and the upstream `$fs=2, $fa=12` defaults for the others.
    fn revolve_params(angle: f64, start: f64, fn_: f64) -> RotateExtrudeParams {
        RotateExtrudeParams {
            angle,
            start,
            convexity: 2,
            fragments: Fragments {
                fn_,
                fs: 2.0,
                fa: 12.0,
            },
        }
    }

    /// First radial moment `Q = ∫∫_D x dA` of the rectangle — its area times the
    /// centroid radius, `(x1²-x0²)(y1-y0)/2`. The exact discrete volume of an
    /// `n`-segment revolve of any convex profile is `n·sin(2π/n)·Q` (a corner
    /// vertex traces flat facets; integrating the linear morph of the section
    /// between adjacent slices gives `sin(Δφ)·Q` per segment).
    fn rect_q(x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
        (x1 * x1 - x0 * x0) * (y1 - y0) / 2.0
    }

    /// Exact faceted-revolve volume `n·sin(2π/n)·Q` for a full `n`-segment sweep.
    fn faceted_full_volume(n: i32, q: f64) -> f64 {
        let n = f64::from(n);
        n * (2.0 * PI / n).sin() * q
    }

    #[test]
    fn full_revolve_square_ring_matches_exact_discrete_volume() {
        // Ring cross-section x∈[2,3], y∈[0,1] revolved 360° with $fn=8 → exactly
        // 8 angular facets, so the volume is the closed-form n·sin(2π/n)·Q.
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let s = rotate_extrude(&p, &revolve_params(360.0, 0.0, 8.0)).expect("ring revolves");
        assert!(s.is_manifold());
        let expected = faceted_full_volume(8, rect_q(2.0, 3.0, 0.0, 1.0));
        assert!(
            (s.volume() - expected).abs() < 1e-9,
            "faceted ring volume {} vs exact {}",
            s.volume(),
            expected
        );
    }

    #[test]
    fn full_revolve_square_ring_converges_to_pappus() {
        // Pappus: V = 2π·R_c·A for a smooth full revolution. A finer facet count
        // must land closer to it than a coarse one, and within 0.1% by $fn=256.
        let (x0, x1, y0, y1) = (2.0, 3.0, 0.0, 1.0);
        let area = (x1 - x0) * (y1 - y0);
        let r_c = (x0 + x1) / 2.0;
        let pappus = 2.0 * PI * r_c * area;
        let p = rect_profile(x0, x1, y0, y1);
        let coarse = rotate_extrude(&p, &revolve_params(360.0, 0.0, 8.0))
            .unwrap()
            .volume();
        let fine = rotate_extrude(&p, &revolve_params(360.0, 0.0, 256.0))
            .unwrap()
            .volume();
        assert!(
            (fine - pappus).abs() < (coarse - pappus).abs(),
            "finer revolve ({fine}) should beat coarse ({coarse}) vs Pappus {pappus}"
        );
        assert!(
            (fine - pappus).abs() / pappus < 0.001,
            "fn=256 ring within 0.1% of Pappus {pappus}, got {fine}"
        );
    }

    #[test]
    fn full_revolve_fn6_is_hexagonal_cross_section_ring() {
        // $fn=6, full sweep → 6 angular facets (a hexagonal path). Exact volume
        // and a manifold, and materially coarser (smaller) than a fine ring.
        let (x0, x1, y0, y1) = (1.0, 2.0, 0.0, 1.0);
        let p = rect_profile(x0, x1, y0, y1);
        let s = rotate_extrude(&p, &revolve_params(360.0, 0.0, 6.0)).expect("hex ring revolves");
        assert!(s.is_manifold());
        let expected = faceted_full_volume(6, rect_q(x0, x1, y0, y1));
        assert!(
            (s.volume() - expected).abs() < 1e-9,
            "hex ring volume {} vs exact {}",
            s.volume(),
            expected
        );
        // sin(60°)·6/2π ≈ 0.827, so a hexagonal ring holds ~83% of the smooth
        // Pappus volume — a visibly faceted, not round, ring.
        let pappus = 2.0 * PI * 1.5 * 1.0;
        assert!(s.volume() < 0.85 * pappus);
    }

    #[test]
    fn torus_from_circle_profile_approaches_analytic() {
        // A true torus (tube radius r about centre radius R) has V = 2π²·R·r².
        // Revolving a 128-gon circle with a 128-facet sweep lands within 1%.
        let (r, big_r) = (1.0, 3.0);
        let analytic = 2.0 * PI * PI * big_r * r * r;
        let p = ngon_profile(128, r, big_r);
        let s = rotate_extrude(&p, &revolve_params(360.0, 0.0, 128.0)).expect("torus revolves");
        assert!(s.is_manifold());
        assert!(s.volume() > 0.0);
        assert!(
            (s.volume() - analytic).abs() / analytic < 0.01,
            "128-facet torus within 1% of {analytic}, got {}",
            s.volume()
        );
    }

    #[test]
    fn torus_converges_as_fragments_grow() {
        let (r, big_r) = (1.0, 3.0);
        let analytic = 2.0 * PI * PI * big_r * r * r;
        let coarse = rotate_extrude(
            &ngon_profile(12, r, big_r),
            &revolve_params(360.0, 0.0, 12.0),
        )
        .unwrap()
        .volume();
        let fine = rotate_extrude(
            &ngon_profile(96, r, big_r),
            &revolve_params(360.0, 0.0, 96.0),
        )
        .unwrap()
        .volume();
        assert!(
            (fine - analytic).abs() < (coarse - analytic).abs(),
            "finer torus ({fine}) should be closer to {analytic} than coarse ({coarse})"
        );
    }

    #[test]
    fn partial_revolve_180_is_half_of_full() {
        // $fn=24 full → 24 facets of 15°; 180° → floor(24·½)=12 facets of 15°.
        // Matched facet angle makes the half-sweep volume exactly half.
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let full = rotate_extrude(&p, &revolve_params(360.0, 0.0, 24.0))
            .unwrap()
            .volume();
        let half = rotate_extrude(&p, &revolve_params(180.0, 0.0, 24.0)).expect("half revolves");
        assert!(half.is_manifold());
        assert!(
            (half.volume() - full / 2.0).abs() < 1e-9,
            "180° volume {} should be half of full {}",
            half.volume(),
            full
        );
    }

    #[test]
    fn partial_revolve_90_is_quarter_of_full() {
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let full = rotate_extrude(&p, &revolve_params(360.0, 0.0, 24.0))
            .unwrap()
            .volume();
        let quarter =
            rotate_extrude(&p, &revolve_params(90.0, 0.0, 24.0)).expect("quarter revolves");
        assert!(quarter.is_manifold());
        assert!(
            (quarter.volume() - full / 4.0).abs() < 1e-9,
            "90° volume {} should be a quarter of full {}",
            quarter.volume(),
            full
        );
    }

    #[test]
    fn partial_revolve_90_matches_exact_discrete_volume() {
        // 90° with $fn=24 → 6 facets of Δφ=15°; V = 6·sin(15°)·Q.
        let (x0, x1, y0, y1) = (2.0, 3.0, 0.0, 1.0);
        let p = rect_profile(x0, x1, y0, y1);
        let s = rotate_extrude(&p, &revolve_params(90.0, 0.0, 24.0)).unwrap();
        let expected = 6.0 * (2.0 * PI / 24.0).sin() * rect_q(x0, x1, y0, y1);
        assert!(
            (s.volume() - expected).abs() < 1e-9,
            "90° facet volume {} vs exact {}",
            s.volume(),
            expected
        );
    }

    #[test]
    fn start_offset_preserves_volume_and_manifold() {
        // start only repositions a partial wedge; it must not change its volume
        // or manifoldness. It does move geometry — the bounding box shifts.
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let at_zero = rotate_extrude(&p, &revolve_params(90.0, 0.0, 24.0)).unwrap();
        let at_45 = rotate_extrude(&p, &revolve_params(90.0, 45.0, 24.0)).unwrap();
        assert!(at_45.is_manifold());
        assert!(
            (at_45.volume() - at_zero.volume()).abs() < 1e-9,
            "start offset must not change volume"
        );
        // The start=0 wedge lies in the +x/+y quadrant band [0°,90°]; the +45°
        // wedge reaches to 135°, so its minimum x goes negative.
        let (v0, _) = at_zero.to_mesh();
        let (v45, _) = at_45.to_mesh();
        let min_x0 = v0.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min);
        let min_x45 = v45.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min);
        assert!(min_x0 >= -1e-9, "start=0 wedge stays in x>=0, got {min_x0}");
        assert!(min_x45 < -1e-9, "start=45 wedge reaches x<0, got {min_x45}");
    }

    #[test]
    fn full_revolve_start_offset_is_shape_invariant() {
        // A full solid of revolution is invariant under rotation about its axis,
        // so start changes nothing measurable about the body.
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let a = rotate_extrude(&p, &revolve_params(360.0, 0.0, 16.0)).unwrap();
        let b = rotate_extrude(&p, &revolve_params(360.0, 90.0, 16.0)).unwrap();
        assert!((a.volume() - b.volume()).abs() < 1e-9);
        assert_eq!(a.num_tri(), b.num_tri());
    }

    #[test]
    fn negative_angle_revolves_the_other_way_same_volume() {
        // OpenSCAD allows a negative sweep (revolves clockwise); it is a valid
        // positive-volume solid congruent to the +angle body.
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let pos = rotate_extrude(&p, &revolve_params(90.0, 0.0, 24.0)).unwrap();
        let neg =
            rotate_extrude(&p, &revolve_params(-90.0, 0.0, 24.0)).expect("neg sweep revolves");
        assert!(neg.is_manifold());
        assert!(
            neg.volume() > 0.0,
            "negative sweep must be a positive solid"
        );
        assert!(
            (neg.volume() - pos.volume()).abs() < 1e-9,
            "|−90°| volume {} should equal +90° volume {}",
            neg.volume(),
            pos.volume()
        );
    }

    #[test]
    fn profile_touching_axis_revolves_into_a_solid_disc() {
        // x0 = 0 touches the Z axis, so the revolve has no inner hole — a solid
        // faceted cylinder. Its volume is still the exact n·sin(2π/n)·Q.
        let (x0, x1, y0, y1) = (0.0, 2.0, 0.0, 3.0);
        let p = rect_profile(x0, x1, y0, y1);
        let s = rotate_extrude(&p, &revolve_params(360.0, 0.0, 64.0)).expect("disc revolves");
        assert!(s.is_manifold());
        let expected = faceted_full_volume(64, rect_q(x0, x1, y0, y1));
        assert!(
            (s.volume() - expected).abs() < 1e-9,
            "solid disc volume {} vs exact {}",
            s.volume(),
            expected
        );
        // Sanity: approaches the smooth cylinder πr²h = π·4·3.
        assert!((s.volume() - PI * 4.0 * 3.0).abs() / (PI * 4.0 * 3.0) < 0.01);
    }

    #[test]
    fn revolve_empty_profile_is_degenerate() {
        let empty = Profile::from_contours(&[], FillRule::EvenOdd);
        assert!(matches!(
            rotate_extrude(&empty, &revolve_params(360.0, 0.0, 16.0)),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn zero_angle_is_degenerate() {
        let p = rect_profile(1.0, 2.0, 0.0, 1.0);
        assert!(matches!(
            rotate_extrude(&p, &revolve_params(0.0, 0.0, 16.0)),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn non_finite_angle_is_degenerate() {
        let p = rect_profile(1.0, 2.0, 0.0, 1.0);
        assert!(matches!(
            rotate_extrude(&p, &revolve_params(f64::NAN, 0.0, 16.0)),
            Err(KernelError::Degenerate(_))
        ));
        assert!(matches!(
            rotate_extrude(&p, &revolve_params(f64::INFINITY, 0.0, 16.0)),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn non_finite_start_is_degenerate() {
        let p = rect_profile(1.0, 2.0, 0.0, 1.0);
        assert!(matches!(
            rotate_extrude(&p, &revolve_params(360.0, f64::NAN, 16.0)),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn profile_wholly_left_of_axis_is_degenerate() {
        // Every vertex has x < 0: Manifold would clip it to nothing, so there is
        // no positive part to revolve — rejected up front.
        let p = rect_profile(-3.0, -1.0, 0.0, 1.0);
        assert!(matches!(
            rotate_extrude(&p, &revolve_params(360.0, 0.0, 16.0)),
            Err(KernelError::Degenerate(_))
        ));
    }

    #[test]
    fn segment_count_scales_full_circle_count_by_angle() {
        // Unit test on the resolver: $fn=36 full → 36; 120° → ceil(36·⅓)=12;
        // 90° → ceil(36·¼)=9. Max radius only sets the $fn=0 path, unused here.
        let frags = revolve_params(360.0, 0.0, 36.0).fragments;
        assert_eq!(resolve_revolve_segments(3.0, frags, 360.0), 36);
        assert_eq!(resolve_revolve_segments(3.0, frags, 120.0), 12);
        assert_eq!(resolve_revolve_segments(3.0, frags, 90.0), 9);
    }

    #[test]
    fn segment_count_ceils_the_angle_scaled_product() {
        // Upstream getCircularSegmentCount applies the OUTER ceil AFTER scaling by
        // |angle|/360: $fn=10, angle=90 → ceil(10·¼) = ceil(2.5) = 3 (not floor→2).
        let frags = revolve_params(90.0, 0.0, 10.0).fragments;
        assert_eq!(resolve_revolve_segments(3.0, frags, 90.0), 3);
        // And the end-to-end body carries those 3 facets: V = 3·sin(2π·¼/3)·Q with
        // Δφ = 90°/3 = 30°, i.e. 3·sin(30°)·Q.
        let (x0, x1, y0, y1) = (2.0, 3.0, 0.0, 1.0);
        let s = rotate_extrude(
            &rect_profile(x0, x1, y0, y1),
            &revolve_params(90.0, 0.0, 10.0),
        )
        .unwrap();
        let expected = 3.0 * (30.0_f64.to_radians()).sin() * rect_q(x0, x1, y0, y1);
        assert!(
            (s.volume() - expected).abs() < 1e-9,
            "ceil'd 3-facet 90° volume {} vs exact {}",
            s.volume(),
            expected
        );
    }

    #[test]
    fn angle_over_360_is_clamped_to_full_revolution() {
        // A direct caller may bypass eval's snap; the kernel pre-clamps
        // |angle|>360 to a full 360° sweep (matching eval), so a 720° request is
        // the same body as 360°, not a double-wound mesh.
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let full = rotate_extrude(&p, &revolve_params(360.0, 0.0, 16.0)).unwrap();
        let over = rotate_extrude(&p, &revolve_params(720.0, 0.0, 16.0)).expect("clamped revolve");
        assert!(over.is_manifold());
        assert!(
            (over.volume() - full.volume()).abs() < 1e-9,
            "720° should clamp to the 360° volume {}, got {}",
            full.volume(),
            over.volume()
        );
        // The resolver clamps too: 720° resolves to the same count as 360°.
        let frags = revolve_params(720.0, 0.0, 16.0).fragments;
        assert_eq!(
            resolve_revolve_segments(3.0, frags, clamp_revolve_angle(720.0)),
            resolve_revolve_segments(3.0, frags, 360.0)
        );
    }

    #[test]
    fn coarse_partial_sweep_falls_back_to_manifold_density() {
        // DIVERGENCE (documented): $fn=3, angle=90 → ceil(3·¼)=1 segment. Manifold
        // honours an explicit nDivisions only when >2 (constructors.cpp L345), so
        // a resolved 1 falls back to Manifold's own quality-based density instead
        // of upstream's exact single flat wedge. We pin that it still yields a
        // watertight, positive-volume solid — the facet smoothness differs, the
        // manifoldness does not.
        assert_eq!(
            resolve_revolve_segments(3.0, revolve_params(90.0, 0.0, 3.0).fragments, 90.0),
            1
        );
        let p = rect_profile(2.0, 3.0, 0.0, 1.0);
        let s = rotate_extrude(&p, &revolve_params(90.0, 0.0, 3.0)).expect("coarse sweep revolves");
        assert!(s.is_manifold(), "coarse partial sweep is still a manifold");
        assert!(s.volume() > 0.0);
    }

    #[test]
    fn straddling_profile_is_clipped_to_positive_x() {
        // DIVERGENCE (documented): a profile crossing the Y axis (min_x<0, max_x>0)
        // is an ERROR upstream (rotate_extrude.cc: "may not lie across the Y
        // axis"), but Manifold silently CLIPS it to the x>=0 half-plane before
        // revolving (constructors.cpp L310-334). We pin the clip: x∈[-1,2] revolves
        // as if x∈[0,2] — a solid disc, not an error — so the behaviour is visible
        // if a future guard changes it.
        let straddle = rotate_extrude(
            &rect_profile(-1.0, 2.0, 0.0, 1.0),
            &revolve_params(360.0, 0.0, 64.0),
        )
        .expect("straddling profile is clipped, not rejected");
        assert!(straddle.is_manifold());
        let clipped = rotate_extrude(
            &rect_profile(0.0, 2.0, 0.0, 1.0),
            &revolve_params(360.0, 0.0, 64.0),
        )
        .unwrap();
        assert!(
            (straddle.volume() - clipped.volume()).abs() < 1e-9,
            "clipped straddle volume {} should equal the x>=0 disc {}",
            straddle.volume(),
            clipped.volume()
        );
    }

    #[test]
    fn segment_count_is_capped() {
        // A pathological explicit $fn would ask for a runaway facet count; the
        // resolver clamps it to MAX_REVOLVE_SEGMENTS before it reaches Manifold.
        let frags = revolve_params(360.0, 0.0, 1e9).fragments;
        assert_eq!(
            resolve_revolve_segments(3.0, frags, 360.0),
            MAX_REVOLVE_SEGMENTS
        );
    }

    // ─── booleans: 3D ────────────────────────────────────────────────────────

    /// An axis-aligned box with arbitrary min/max corners — the boolean tests
    /// need boxes at chosen offsets (independent of `Solid` transforms),
    /// so this builds the 12-triangle mesh directly with the crate's outward
    /// winding, giving exact hand-computable overlap volumes.
    fn box_solid(min: [f64; 3], max: [f64; 3]) -> Solid {
        let [x0, y0, z0] = min;
        let [x1, y1, z1] = max;
        let verts = vec![
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ];
        Solid::from_mesh(&verts, &tris).expect("axis-aligned box is a manifold")
    }

    /// Translate a solid by rebuilding its mesh — again, no `Solid::translate`
    /// yet, so the sphere-hull test moves a sphere this way.
    fn translated(s: &Solid, d: [f64; 3]) -> Solid {
        let (mut verts, tris) = s.to_mesh();
        for v in &mut verts {
            v[0] += d[0];
            v[1] += d[1];
            v[2] += d[2];
        }
        Solid::from_mesh(&verts, &tris).expect("a translated manifold is still a manifold")
    }

    fn vol(s: &Solid) -> f64 {
        s.volume()
    }

    #[test]
    fn union_empty_list_is_none() {
        assert!(union(&[]).unwrap().is_none());
    }

    #[test]
    fn union_single_is_identity() {
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let u = union(std::slice::from_ref(&a)).unwrap().unwrap();
        assert!(u.is_manifold());
        assert!(
            (vol(&u) - 8.0).abs() < 1e-9,
            "single-union volume {}",
            vol(&u)
        );
        assert_eq!(u.num_tri(), a.num_tri());
    }

    #[test]
    fn union_two_overlapping_cubes_inclusion_exclusion() {
        // A=[0,2]^3 (8), B=[1,3]^3 (8), overlap [1,2]^3 (1) → union = 8+8-1 = 15.
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let b = box_solid([1.0, 1.0, 1.0], [3.0, 3.0, 3.0]);
        let u = union(&[a, b]).unwrap().unwrap();
        assert!(u.is_manifold());
        assert!((vol(&u) - 15.0).abs() < 1e-9, "union volume {}", vol(&u));
    }

    #[test]
    fn union_disjoint_cubes_sum_volumes() {
        let a = box_solid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = box_solid([2.0, 0.0, 0.0], [3.0, 1.0, 1.0]);
        let u = union(&[a, b]).unwrap().unwrap();
        assert!(u.is_manifold());
        assert!(
            (vol(&u) - 2.0).abs() < 1e-9,
            "disjoint union volume {}",
            vol(&u)
        );
    }

    #[test]
    fn union_batch_of_three() {
        // Three unit cubes in a row, pairwise touching only on faces → vol 3.
        let a = box_solid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = box_solid([1.0, 0.0, 0.0], [2.0, 1.0, 1.0]);
        let c = box_solid([2.0, 0.0, 0.0], [3.0, 1.0, 1.0]);
        let u = union(&[a, b, c]).unwrap().unwrap();
        assert!(u.is_manifold());
        assert!(
            (vol(&u) - 3.0).abs() < 1e-9,
            "three-box union volume {}",
            vol(&u)
        );
    }

    #[test]
    fn difference_empty_list_is_none() {
        assert!(difference(&[]).unwrap().is_none());
    }

    #[test]
    fn difference_single_is_identity() {
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let d = difference(std::slice::from_ref(&a)).unwrap().unwrap();
        assert!(d.is_manifold());
        assert!((vol(&d) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn difference_cube_minus_cube_volume() {
        // A=[0,2]^3 (8) minus B=[1,3]^3 removes the [1,2]^3 overlap (1) → 7.
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let b = box_solid([1.0, 1.0, 1.0], [3.0, 3.0, 3.0]);
        let d = difference(&[a, b]).unwrap().unwrap();
        assert!(d.is_manifold());
        assert!(
            (vol(&d) - 7.0).abs() < 1e-9,
            "difference volume {}",
            vol(&d)
        );
    }

    #[test]
    fn difference_removing_everything_yields_empty_solid() {
        // A fully enclosed by B → A - B is a valid EMPTY solid (Some, is_empty),
        // not None: an empty result exports as an empty STL, matching OpenSCAD.
        let a = box_solid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = box_solid([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]);
        let d = difference(&[a, b]).unwrap().unwrap();
        assert!(d.is_empty(), "fully-subtracted difference is empty");
        assert!(d.is_manifold(), "an empty solid is still a valid manifold");
        assert!(vol(&d).abs() < 1e-12, "empty solid volume {}", vol(&d));
    }

    /// A valid EMPTY solid, sourced from a disjoint intersection — there is no
    /// empty-`Solid` constructor, and this exercises the "op ran → Some(empty)"
    /// path that produces one in the first place.
    fn empty_solid() -> Solid {
        let x = box_solid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let y = box_solid([5.0, 5.0, 5.0], [6.0, 6.0, 6.0]);
        let e = intersection(&[x, y]).unwrap().unwrap();
        assert!(e.is_empty(), "disjoint intersection must be an empty solid");
        e
    }

    #[test]
    fn difference_with_empty_minuend_is_some_empty() {
        // An empty first operand still means the op RAN: first-minus-rest with an
        // empty minuend is empty → Some(empty), NOT None (upstream `difference`
        // collects children unconditionally; an empty first child yields empty
        // geometry). Symmetric with union/intersection on empty operands.
        let c = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let d = difference(&[empty_solid(), c]).unwrap().unwrap();
        assert!(d.is_empty(), "empty minuend minus anything is empty");
        assert!(d.is_manifold());
    }

    #[test]
    fn union_single_empty_operand_is_some_empty() {
        let u = union(&[empty_solid()]).unwrap().unwrap();
        assert!(u.is_empty());
        assert!(u.is_manifold());
    }

    #[test]
    fn difference_single_empty_operand_is_some_empty() {
        let d = difference(&[empty_solid()]).unwrap().unwrap();
        assert!(d.is_empty());
        assert!(d.is_manifold());
    }

    #[test]
    fn intersection_single_empty_operand_is_some_empty() {
        let i = intersection(&[empty_solid()]).unwrap().unwrap();
        assert!(i.is_empty());
        assert!(i.is_manifold());
    }

    #[test]
    fn difference_order_is_significant() {
        // A=[0,2]^3 (8), B=[1,4]^3 (27), overlap [1,2]^3 (1). A-B = 7, B-A = 26.
        let a = || box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let b = || box_solid([1.0, 1.0, 1.0], [4.0, 4.0, 4.0]);
        let ab = difference(&[a(), b()]).unwrap().unwrap();
        let ba = difference(&[b(), a()]).unwrap().unwrap();
        assert!((vol(&ab) - 7.0).abs() < 1e-9, "A-B volume {}", vol(&ab));
        assert!((vol(&ba) - 26.0).abs() < 1e-9, "B-A volume {}", vol(&ba));
    }

    #[test]
    fn intersection_empty_list_is_none() {
        assert!(intersection(&[]).unwrap().is_none());
    }

    #[test]
    fn intersection_single_is_identity() {
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let i = intersection(std::slice::from_ref(&a)).unwrap().unwrap();
        assert!(i.is_manifold());
        assert!((vol(&i) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn intersection_overlap_volume() {
        // A=[0,2]^3, B=[1,3]^3 → intersection is the [1,2]^3 overlap = 1.
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let b = box_solid([1.0, 1.0, 1.0], [3.0, 3.0, 3.0]);
        let i = intersection(&[a, b]).unwrap().unwrap();
        assert!(i.is_manifold());
        assert!(
            (vol(&i) - 1.0).abs() < 1e-9,
            "intersection volume {}",
            vol(&i)
        );
    }

    #[test]
    fn intersection_disjoint_is_empty_solid() {
        let a = box_solid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = box_solid([2.0, 0.0, 0.0], [3.0, 1.0, 1.0]);
        let i = intersection(&[a, b]).unwrap().unwrap();
        assert!(i.is_empty(), "disjoint intersection is empty");
        assert!(i.is_manifold());
        assert!(vol(&i).abs() < 1e-12);
    }

    #[test]
    fn intersection_of_three_folds() {
        // A=[0,3]x[0,3]x[0,3], B shifted +1 in x, C shifted +2 in x. The common
        // region is x∈[2,3], y,z∈[0,3] → 1*3*3 = 9.
        let a = box_solid([0.0, 0.0, 0.0], [3.0, 3.0, 3.0]);
        let b = box_solid([1.0, 0.0, 0.0], [4.0, 3.0, 3.0]);
        let c = box_solid([2.0, 0.0, 0.0], [5.0, 3.0, 3.0]);
        let i = intersection(&[a, b, c]).unwrap().unwrap();
        assert!(i.is_manifold());
        assert!(
            (vol(&i) - 9.0).abs() < 1e-9,
            "triple intersection volume {}",
            vol(&i)
        );
    }

    #[test]
    fn hull_empty_list_is_none() {
        assert!(hull(&[]).unwrap().is_none());
    }

    #[test]
    fn hull_of_single_convex_box_preserves_volume() {
        // A box is convex, so its hull equals itself.
        let a = box_solid([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]);
        let h = hull(std::slice::from_ref(&a)).unwrap().unwrap();
        assert!(h.is_manifold());
        assert!(
            (vol(&h) - 24.0).abs() < 1e-9,
            "hull-of-box volume {}",
            vol(&h)
        );
    }

    #[test]
    fn hull_of_two_separated_boxes_is_convex_and_bigger() {
        // Two unit cubes at opposite corners of [0,4]^3: the convex hull spans
        // both, so its volume exceeds the 2 units of geometry and stays within
        // the [0,4]^3 (=64) bounding box.
        let a = box_solid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = box_solid([3.0, 3.0, 3.0], [4.0, 4.0, 4.0]);
        let h = hull(&[a, b]).unwrap().unwrap();
        assert!(h.is_manifold());
        assert!(
            vol(&h) > 2.0,
            "hull {} should exceed the 2 units of input",
            vol(&h)
        );
        assert!(
            vol(&h) < 64.0,
            "hull {} must fit the [0,4]^3 bound",
            vol(&h)
        );
    }

    #[test]
    fn hull_of_two_spheres_exceeds_either() {
        let sphere = solid(&Primitive::Sphere {
            r: 1.0,
            fragments: 32,
        });
        let one = vol(&sphere);
        let moved = translated(&sphere, [5.0, 0.0, 0.0]);
        let h = hull(&[sphere, moved]).unwrap().unwrap();
        assert!(h.is_manifold());
        assert!(
            vol(&h) > 2.0 * one,
            "hull of two separated spheres {} should exceed twice one sphere {}",
            vol(&h),
            one
        );
    }

    #[test]
    fn hull_of_only_empty_bodies_is_some_empty() {
        // A non-empty slice → the op ran; empties contribute no vertices, so the
        // hull is a valid empty solid (Some(empty)), not None.
        let h = hull(&[empty_solid(), empty_solid()]).unwrap().unwrap();
        assert!(h.is_empty(), "hull of only empty bodies is empty");
        assert!(h.is_manifold());
    }

    // ─── booleans: 2D profiles ───────────────────────────────────────────────

    #[test]
    fn union_profiles_two_overlapping_squares() {
        // A=[0,2]^2 (4), B=[1,3]^2 (4), overlap [1,2]^2 (1) → 4+4-1 = 7.
        let a = rect_profile(0.0, 2.0, 0.0, 2.0);
        let b = rect_profile(1.0, 3.0, 1.0, 3.0);
        let u = union_profiles(&[a, b]).unwrap().unwrap();
        assert!(!u.is_empty());
        assert!((u.area() - 7.0).abs() < 1e-9, "union area {}", u.area());
    }

    #[test]
    fn union_profiles_empty_is_none() {
        assert!(union_profiles(&[]).unwrap().is_none());
    }

    #[test]
    fn union_profiles_single_is_identity() {
        let a = rect_profile(0.0, 2.0, 0.0, 3.0);
        let u = union_profiles(std::slice::from_ref(&a)).unwrap().unwrap();
        assert!((u.area() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn difference_profiles_square_minus_square() {
        // A=[0,2]^2 (4) minus B=[1,3]^2 removes [1,2]^2 (1) → 3.
        let a = rect_profile(0.0, 2.0, 0.0, 2.0);
        let b = rect_profile(1.0, 3.0, 1.0, 3.0);
        let d = difference_profiles(&[a, b]).unwrap().unwrap();
        assert!(
            (d.area() - 3.0).abs() < 1e-9,
            "difference area {}",
            d.area()
        );
    }

    #[test]
    fn difference_profiles_removing_everything_is_empty() {
        let a = rect_profile(0.0, 1.0, 0.0, 1.0);
        let b = rect_profile(-1.0, 2.0, -1.0, 2.0);
        let d = difference_profiles(&[a, b]).unwrap().unwrap();
        assert!(d.is_empty(), "fully-subtracted 2D difference is empty");
        assert!(d.area().abs() < 1e-12);
    }

    #[test]
    fn intersection_profiles_overlap_area() {
        let a = rect_profile(0.0, 2.0, 0.0, 2.0);
        let b = rect_profile(1.0, 3.0, 1.0, 3.0);
        let i = intersection_profiles(&[a, b]).unwrap().unwrap();
        assert!(
            (i.area() - 1.0).abs() < 1e-9,
            "intersection area {}",
            i.area()
        );
    }

    #[test]
    fn hull_profiles_two_separated_squares_is_bigger() {
        let a = rect_profile(0.0, 1.0, 0.0, 1.0);
        let b = rect_profile(3.0, 4.0, 3.0, 4.0);
        let h = hull_profiles(&[a, b]).unwrap().unwrap();
        assert!(!h.is_empty());
        assert!(
            h.area() > 2.0,
            "2D hull area {} should exceed the 2 units of input",
            h.area()
        );
    }

    /// A valid EMPTY profile, sourced from a disjoint 2D intersection — the 2D
    /// twin of [`empty_solid`].
    fn empty_profile() -> Profile {
        let a = rect_profile(0.0, 1.0, 0.0, 1.0);
        let b = rect_profile(5.0, 6.0, 5.0, 6.0);
        let e = intersection_profiles(&[a, b]).unwrap().unwrap();
        assert!(
            e.is_empty(),
            "disjoint 2D intersection must be an empty profile"
        );
        e
    }

    #[test]
    fn union_profiles_single_empty_operand_is_some_empty() {
        let u = union_profiles(&[empty_profile()]).unwrap().unwrap();
        assert!(u.is_empty());
    }

    #[test]
    fn difference_profiles_single_empty_operand_is_some_empty() {
        let d = difference_profiles(&[empty_profile()]).unwrap().unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn difference_profiles_with_empty_minuend_is_some_empty() {
        // 2D twin: empty first operand → Some(empty), NOT None.
        let c = rect_profile(0.0, 2.0, 0.0, 2.0);
        let d = difference_profiles(&[empty_profile(), c]).unwrap().unwrap();
        assert!(d.is_empty(), "empty 2D minuend minus anything is empty");
    }

    #[test]
    fn intersection_profiles_single_empty_operand_is_some_empty() {
        let i = intersection_profiles(&[empty_profile()]).unwrap().unwrap();
        assert!(i.is_empty());
    }

    #[test]
    fn hull_profiles_of_only_empty_is_some_zero_area() {
        // A non-empty slice → the op RAN → Some (never None), the invariant that
        // matters. The *result* of a 2D hull of only-empty inputs is a zero-area
        // section — but Clipper2 reports it as a single empty contour, so
        // `is_empty()` is FALSE here (unlike Manifold's 3D `batch_hull`, which
        // yields a truly empty body). We pin the actual behaviour: Some, area 0.
        let h = hull_profiles(&[empty_profile(), empty_profile()])
            .unwrap()
            .unwrap();
        assert!(h.area().abs() < 1e-12, "2D hull of empties has zero area");
    }

    // ─── Transform application ───────────────────────────────────────────────

    const TOL: f64 = 1e-9;

    /// Axis-aligned bounding box of a solid, computed from its extracted mesh
    /// so the assertion is independent of the kernel's own bbox query.
    fn bbox_solid(s: &Solid) -> ([f64; 3], [f64; 3]) {
        let (verts, _) = s.to_mesh();
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for v in &verts {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        (lo, hi)
    }

    /// Axis-aligned bounding box of a profile, from its extracted contours.
    fn bbox_profile(p: &Profile) -> ([f64; 2], [f64; 2]) {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for ring in p.to_polygons() {
            for v in ring {
                for k in 0..2 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
        }
        (lo, hi)
    }

    fn cube_solid(size: [f64; 3], center: bool) -> Solid {
        solid(&Primitive::Cube { size, center })
    }

    fn square_profile(size: [f64; 2], center: bool) -> Profile {
        profile(&Primitive::Square { size, center })
    }

    fn xform_solid(kind: &TransformKind, s: Solid) -> Solid {
        match apply_transform(kind, Geometry::Solid(s)).expect("transform succeeds") {
            Geometry::Solid(s) => s,
            Geometry::Profile(_) => panic!("solid in, solid out"),
        }
    }

    fn xform_profile(kind: &TransformKind, p: Profile) -> Profile {
        match apply_transform(kind, Geometry::Profile(p)).expect("transform succeeds") {
            Geometry::Profile(p) => p,
            Geometry::Solid(_) => panic!("profile in, profile out"),
        }
    }

    fn translate_affine(v: [f64; 3]) -> Affine {
        [
            [1.0, 0.0, 0.0, v[0]],
            [0.0, 1.0, 0.0, v[1]],
            [0.0, 0.0, 1.0, v[2]],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn scale_affine(v: [f64; 3]) -> Affine {
        [
            [v[0], 0.0, 0.0, 0.0],
            [0.0, v[1], 0.0, 0.0],
            [0.0, 0.0, v[2], 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    /// Row-major rotation of +90° about Z: maps (x,y,z) → (-y, x, z).
    fn rotate_z_90_affine() -> Affine {
        [
            [0.0, -1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    /// Reflection across the YZ plane (mirror normal [1,0,0]): x → -x.
    fn mirror_x_affine() -> Affine {
        [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    /// Assert a transform failed with [`KernelError::Degenerate`]. `Geometry`
    /// has no `Debug`, so `unwrap_err` is unavailable — match instead.
    fn expect_degenerate(r: Result<Geometry, KernelError>) {
        match r {
            Ok(_) => panic!("expected a Degenerate error, got Ok"),
            Err(e) => assert!(matches!(e, KernelError::Degenerate(_)), "got {e:?}"),
        }
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-6)
    }
    fn close2(a: [f64; 2], b: [f64; 2]) -> bool {
        (0..2).all(|k| (a[k] - b[k]).abs() < 1e-6)
    }

    // ── Affine on solids ──

    #[test]
    fn affine_translate_moves_bbox_and_preserves_volume() {
        let s = cube_solid([1.0, 1.0, 1.0], false); // [0,1]^3
        let t = xform_solid(&TransformKind::Affine(translate_affine([1.0, 2.0, 3.0])), s);
        let (lo, hi) = bbox_solid(&t);
        assert!(close(lo, [1.0, 2.0, 3.0]), "min corner shifts, got {lo:?}");
        assert!(close(hi, [2.0, 3.0, 4.0]), "max corner shifts, got {hi:?}");
        assert!(
            (t.volume() - 1.0).abs() < TOL,
            "translation preserves volume"
        );
    }

    #[test]
    fn affine_rotate_z_90_maps_x_axis_to_y() {
        // A 2×1×1 box on [0,2]×[0,1]×[0,1]; +90° about Z sends its x-extent to y.
        let s = cube_solid([2.0, 1.0, 1.0], false);
        let t = xform_solid(&TransformKind::Affine(rotate_z_90_affine()), s);
        let (lo, hi) = bbox_solid(&t);
        // (2,0,0) → (0,2,0): the box now spans x∈[-1,0], y∈[0,2], z∈[0,1].
        assert!(close(lo, [-1.0, 0.0, 0.0]), "rotated min, got {lo:?}");
        assert!(close(hi, [0.0, 2.0, 1.0]), "rotated max, got {hi:?}");
        assert!((t.volume() - 2.0).abs() < TOL, "rotation preserves volume");
    }

    #[test]
    fn affine_scale_doubles_x_extent_and_volume() {
        let s = cube_solid([1.0, 1.0, 1.0], false);
        let t = xform_solid(&TransformKind::Affine(scale_affine([2.0, 1.0, 1.0])), s);
        let (lo, hi) = bbox_solid(&t);
        assert!((hi[0] - lo[0] - 2.0).abs() < TOL, "x-extent doubles");
        assert!((hi[1] - lo[1] - 1.0).abs() < TOL, "y-extent unchanged");
        assert!(
            (t.volume() - 2.0).abs() < TOL,
            "scale [2,1,1] doubles volume"
        );
    }

    #[test]
    fn affine_mirror_x_negates_x_and_stays_positive_volume() {
        let s = cube_solid([1.0, 1.0, 1.0], false); // x∈[0,1]
        let t = xform_solid(&TransformKind::Affine(mirror_x_affine()), s);
        let (lo, hi) = bbox_solid(&t);
        assert!(
            close(lo, [-1.0, 0.0, 0.0]),
            "x reflects to [-1,0], got {lo:?}"
        );
        assert!(close(hi, [0.0, 1.0, 1.0]), "mirrored max, got {hi:?}");
        // Pin the sign, not just the magnitude: a reflection has negative
        // determinant, but Manifold auto-flips triangle winding so the result
        // stays outward-oriented with POSITIVE volume. Asserting `> 0` (not
        // `.abs()`) makes an inside-out regression fail here.
        assert!(
            t.volume() > 0.0 && (t.volume() - 1.0).abs() < TOL,
            "mirror keeps positive, unit volume, got {}",
            t.volume()
        );
    }

    #[test]
    fn affine_multmatrix_general_scale_and_translate() {
        // A general affine: scale x by 2 then translate x by +1.
        let m = [
            [2.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let s = cube_solid([1.0, 1.0, 1.0], false); // x∈[0,1]
        let t = xform_solid(&TransformKind::Affine(m), s);
        let (lo, hi) = bbox_solid(&t);
        // x: [0,1] → [1,3]; y,z unchanged.
        assert!(close(lo, [1.0, 0.0, 0.0]), "got {lo:?}");
        assert!(close(hi, [3.0, 1.0, 1.0]), "got {hi:?}");
        assert!((t.volume() - 2.0).abs() < TOL);
    }

    #[test]
    fn affine_identity_is_noop() {
        let s = cube_solid([2.0, 3.0, 4.0], true);
        let (lo0, hi0) = bbox_solid(&s);
        let v0 = s.volume();
        let t = xform_solid(&TransformKind::Affine(translate_affine([0.0, 0.0, 0.0])), s);
        let (lo1, hi1) = bbox_solid(&t);
        assert!(close(lo0, lo1) && close(hi0, hi1), "identity keeps bbox");
        assert!((t.volume() - v0).abs() < TOL, "identity keeps volume");
    }

    #[test]
    fn affine_general_rotation_preserves_volume() {
        // rotate([30,40,50]) row-major Euler Rz*Ry*Rx, built here directly.
        let (ax, ay, az): (f64, f64, f64) = (30.0, 40.0, 50.0);
        let (sx, cx) = (ax.to_radians().sin(), ax.to_radians().cos());
        let (sy, cy) = (ay.to_radians().sin(), ay.to_radians().cos());
        let (sz, cz) = (az.to_radians().sin(), az.to_radians().cos());
        let m = [
            [cy * cz, cz * sx * sy - cx * sz, cx * cz * sy + sx * sz, 0.0],
            [
                cy * sz,
                cx * cz + sx * sy * sz,
                -cz * sx + cx * sy * sz,
                0.0,
            ],
            [-sy, cy * sx, cx * cy, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let s = cube_solid([2.0, 3.0, 4.0], true);
        let v0 = s.volume();
        let t = xform_solid(&TransformKind::Affine(m), s);
        assert!(
            (t.volume() - v0).abs() < 1e-6,
            "rotation preserves volume: {} vs {v0}",
            t.volume()
        );
    }

    #[test]
    fn affine_compose_translate_then_rotate() {
        // translate([1,0,0]) then rotate +90° about Z (maps (x,y)→(-y,x)).
        // After translate the [0,1]^3 cube spans x∈[1,2], y∈[0,1]; the rotation
        // sends x∈[1,2]→new-y∈[1,2] and y∈[0,1]→new-x∈[-1,0].
        let s = cube_solid([1.0, 1.0, 1.0], false);
        let s = xform_solid(&TransformKind::Affine(translate_affine([1.0, 0.0, 0.0])), s);
        let s = xform_solid(&TransformKind::Affine(rotate_z_90_affine()), s);
        let (lo, hi) = bbox_solid(&s);
        assert!(close(lo, [-1.0, 1.0, 0.0]), "composed min, got {lo:?}");
        assert!(close(hi, [0.0, 2.0, 1.0]), "composed max, got {hi:?}");
        assert!((s.volume() - 1.0).abs() < TOL);
    }

    #[test]
    fn affine_non_finite_matrix_is_degenerate() {
        let mut m = translate_affine([1.0, 0.0, 0.0]);
        m[0][0] = f64::NAN;
        let s = cube_solid([1.0, 1.0, 1.0], false);
        expect_degenerate(apply_transform(
            &TransformKind::Affine(m),
            Geometry::Solid(s),
        ));
    }

    #[test]
    fn affine_infinite_matrix_is_degenerate() {
        let mut m = scale_affine([1.0, 1.0, 1.0]);
        m[1][3] = f64::INFINITY;
        let s = cube_solid([1.0, 1.0, 1.0], false);
        expect_degenerate(apply_transform(
            &TransformKind::Affine(m),
            Geometry::Solid(s),
        ));
    }

    #[test]
    fn affine_on_empty_solid_stays_empty() {
        // Subtracting a cube from an identical cube is a valid empty solid.
        let a1 = cube_solid([1.0, 1.0, 1.0], false);
        let a2 = cube_solid([1.0, 1.0, 1.0], false);
        let empty = difference(&[a1, a2]).unwrap().unwrap();
        assert!(empty.is_empty(), "self-difference is empty");
        let t = xform_solid(
            &TransformKind::Affine(translate_affine([5.0, 5.0, 5.0])),
            empty,
        );
        assert!(t.is_empty(), "transforming empty keeps it empty");
    }

    // ── Affine on profiles (2D projection of the 4×4) ──

    #[test]
    fn affine_profile_translate_ignores_z() {
        let p = square_profile([1.0, 1.0], false); // [0,1]^2
        let t = xform_profile(
            &TransformKind::Affine(translate_affine([3.0, 4.0, 99.0])),
            p,
        );
        let (lo, hi) = bbox_profile(&t);
        assert!(
            close2(lo, [3.0, 4.0]),
            "2D min shifts, z ignored, got {lo:?}"
        );
        assert!(close2(hi, [4.0, 5.0]), "got {hi:?}");
    }

    #[test]
    fn affine_profile_rotate_z_90_swaps_extents() {
        let p = square_profile([2.0, 1.0], false); // x∈[0,2], y∈[0,1]
        let t = xform_profile(&TransformKind::Affine(rotate_z_90_affine()), p);
        let (lo, hi) = bbox_profile(&t);
        // (2,0)→(0,2): x∈[-1,0], y∈[0,2].
        assert!(close2(lo, [-1.0, 0.0]), "got {lo:?}");
        assert!(close2(hi, [0.0, 2.0]), "got {hi:?}");
    }

    #[test]
    fn affine_profile_scale_multiplies_area() {
        let p = square_profile([1.0, 1.0], false);
        let a0 = p.area();
        let t = xform_profile(&TransformKind::Affine(scale_affine([2.0, 3.0, 1.0])), p);
        assert!((t.area() - a0 * 6.0).abs() < TOL, "area scales by 2·3");
    }

    #[test]
    fn affine_profile_mirror_x_preserves_area() {
        let p = square_profile([1.0, 1.0], false); // x∈[0,1]
        let a0 = p.area();
        let t = xform_profile(&TransformKind::Affine(mirror_x_affine()), p);
        let (lo, hi) = bbox_profile(&t);
        assert!(
            close2(lo, [-1.0, 0.0]) && close2(hi, [0.0, 1.0]),
            "x reflects"
        );
        assert!((t.area() - a0).abs() < TOL, "mirror preserves area");
    }

    #[test]
    fn affine_profile_non_finite_is_degenerate() {
        let mut m = scale_affine([1.0, 1.0, 1.0]);
        m[0][1] = f64::NAN;
        let p = square_profile([1.0, 1.0], false);
        expect_degenerate(apply_transform(
            &TransformKind::Affine(m),
            Geometry::Profile(p),
        ));
    }

    // ── Resize on solids ──

    #[test]
    fn resize_explicit_all_axes() {
        let s = cube_solid([2.0, 2.0, 2.0], false); // extents 2,2,2
        let t = xform_solid(
            &TransformKind::Resize {
                newsize: [4.0, 6.0, 8.0],
                auto: [false, false, false],
            },
            s,
        );
        let (lo, hi) = bbox_solid(&t);
        let dims = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        assert!(close(dims, [4.0, 6.0, 8.0]), "resized dims, got {dims:?}");
    }

    #[test]
    fn resize_zero_axis_is_kept() {
        let s = cube_solid([2.0, 2.0, 2.0], false);
        let t = xform_solid(
            &TransformKind::Resize {
                newsize: [4.0, 0.0, 0.0],
                auto: [false, false, false],
            },
            s,
        );
        let (lo, hi) = bbox_solid(&t);
        let dims = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        // x resized to 4; y,z keep their natural 2 (0 newsize, no auto).
        assert!(close(dims, [4.0, 2.0, 2.0]), "got {dims:?}");
    }

    #[test]
    fn resize_auto_scales_proportionally() {
        let s = cube_solid([2.0, 2.0, 2.0], false);
        let t = xform_solid(
            &TransformKind::Resize {
                newsize: [4.0, 0.0, 0.0],
                auto: [false, true, true],
            },
            s,
        );
        let (lo, hi) = bbox_solid(&t);
        let dims = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        // x scales ×2; auto y,z copy that factor → all become 4.
        assert!(
            close(dims, [4.0, 4.0, 4.0]),
            "auto proportional, got {dims:?}"
        );
    }

    #[test]
    fn resize_preserves_volume_ratio() {
        let s = cube_solid([2.0, 2.0, 2.0], false); // volume 8
        let t = xform_solid(
            &TransformKind::Resize {
                newsize: [4.0, 6.0, 8.0],
                auto: [false, false, false],
            },
            s,
        );
        // scale factors 2,3,4 → volume 8·24 = 192.
        assert!((t.volume() - 192.0).abs() < 1e-6, "volume {}", t.volume());
    }

    #[test]
    fn resize_all_zero_is_identity() {
        let s = cube_solid([2.0, 3.0, 4.0], false);
        let v0 = s.volume();
        let t = xform_solid(
            &TransformKind::Resize {
                newsize: [0.0, 0.0, 0.0],
                auto: [false, false, false],
            },
            s,
        );
        assert!((t.volume() - v0).abs() < TOL, "all-zero resize is a no-op");
    }

    #[test]
    fn resize_empty_solid_passes_through() {
        let a1 = cube_solid([1.0, 1.0, 1.0], false);
        let a2 = cube_solid([1.0, 1.0, 1.0], false);
        let empty = difference(&[a1, a2]).unwrap().unwrap();
        assert!(empty.is_empty());
        let t = xform_solid(
            &TransformKind::Resize {
                newsize: [4.0, 4.0, 4.0],
                auto: [false, false, false],
            },
            empty,
        );
        assert!(t.is_empty(), "resizing empty keeps it empty");
    }

    #[test]
    fn resize_non_finite_newsize_is_degenerate() {
        // An inf/NaN newsize would drive an inf scale into Manifold; reject it
        // up front, for parity with the affine non-finite guard.
        let s = cube_solid([2.0, 2.0, 2.0], false);
        expect_degenerate(apply_transform(
            &TransformKind::Resize {
                newsize: [f64::INFINITY, 0.0, 0.0],
                auto: [false, false, false],
            },
            Geometry::Solid(s),
        ));
        let p = square_profile([2.0, 2.0], false);
        expect_degenerate(apply_transform(
            &TransformKind::Resize {
                newsize: [f64::NAN, 0.0, 0.0],
                auto: [false, false, false],
            },
            Geometry::Profile(p),
        ));
    }

    // ── Resize on profiles (2D bbox) ──

    #[test]
    fn resize_profile_explicit() {
        let p = square_profile([2.0, 2.0], false);
        let t = xform_profile(
            &TransformKind::Resize {
                newsize: [4.0, 6.0, 0.0],
                auto: [false, false, false],
            },
            p,
        );
        let (lo, hi) = bbox_profile(&t);
        let dims = [hi[0] - lo[0], hi[1] - lo[1]];
        assert!(close2(dims, [4.0, 6.0]), "2D resize dims, got {dims:?}");
    }

    #[test]
    fn resize_profile_auto_proportional() {
        let p = square_profile([2.0, 2.0], false);
        let t = xform_profile(
            &TransformKind::Resize {
                newsize: [4.0, 0.0, 0.0],
                auto: [false, true, false],
            },
            p,
        );
        let (lo, hi) = bbox_profile(&t);
        let dims = [hi[0] - lo[0], hi[1] - lo[1]];
        assert!(
            close2(dims, [4.0, 4.0]),
            "auto y copies x factor, got {dims:?}"
        );
    }

    #[test]
    fn resize_profile_zero_axis_kept() {
        let p = square_profile([2.0, 2.0], false);
        let t = xform_profile(
            &TransformKind::Resize {
                newsize: [4.0, 0.0, 0.0],
                auto: [false, false, false],
            },
            p,
        );
        let (lo, hi) = bbox_profile(&t);
        let dims = [hi[0] - lo[0], hi[1] - lo[1]];
        assert!(close2(dims, [4.0, 2.0]), "y kept, got {dims:?}");
    }

    // ── Color is a geometric identity ──

    #[test]
    fn color_is_geometric_identity_on_solid() {
        let s = cube_solid([2.0, 3.0, 4.0], true);
        let (lo0, hi0) = bbox_solid(&s);
        let v0 = s.volume();
        let t = xform_solid(&TransformKind::Color([1.0, 0.0, 0.0, 1.0]), s);
        let (lo1, hi1) = bbox_solid(&t);
        assert!(close(lo0, lo1) && close(hi0, hi1), "color keeps bbox");
        assert!((t.volume() - v0).abs() < TOL, "color keeps volume");
    }

    #[test]
    fn color_is_geometric_identity_on_profile() {
        let p = square_profile([3.0, 5.0], false);
        let a0 = p.area();
        let t = xform_profile(&TransformKind::Color([0.2, 0.4, 0.6, 0.8]), p);
        assert!((t.area() - a0).abs() < TOL, "color keeps area");
    }

    /// Cross-check the row-major → column-major layout end to end: a +90° Z
    /// rotation must send the +X unit direction to +Y. Uses a thin bar so the
    /// mapped axis is unambiguous in the bbox.
    #[test]
    fn matrix_layout_rotation_maps_unit_x_to_unit_y() {
        // A bar 4 long in x, thin in y/z, sitting at x∈[0,4].
        let s = cube_solid([4.0, 0.5, 0.5], false);
        let t = xform_solid(&TransformKind::Affine(rotate_z_90_affine()), s);
        let (lo, hi) = bbox_solid(&t);
        // The long axis is now y: y-extent ≈ 4, x-extent ≈ 0.5.
        assert!((hi[1] - lo[1] - 4.0).abs() < 1e-6, "long axis is now +Y");
        assert!(
            (hi[0] - lo[0] - 0.5).abs() < 1e-6,
            "x is now the short axis"
        );
        // Specifically the far tip (4,·,·) went to y≈4 (positive), not y≈-4.
        assert!(
            hi[1] > 3.9 && lo[1] > -0.6,
            "X mapped to +Y, got y∈[{},{}]",
            lo[1],
            hi[1]
        );
    }

    // -----------------------------------------------------------------------
    // offset_profile
    // -----------------------------------------------------------------------

    /// Default fragment trio ($fn=0, $fs=2, $fa=12) for the analytic tests.
    fn default_fragments() -> Fragments {
        Fragments {
            fn_: 0.0,
            fs: 2.0,
            fa: 12.0,
        }
    }

    #[test]
    fn offset_round_r2_on_square10_area() {
        // The Steiner identity — square(10) offset r=2 →
        // 100 + perimeter·r + πr² = 100 + 40·2 + 4π ≈ 192.566 (1% arc
        // tolerance; $fn=32 keeps the arcs tight).
        let square = square_profile([10.0, 10.0], false);
        let grown = offset_profile(
            &square,
            &OffsetParams {
                delta: 2.0,
                join: OffsetJoin::Round,
                chamfer: false,
                fragments: Fragments {
                    fn_: 32.0,
                    fs: 2.0,
                    fa: 12.0,
                },
            },
        )
        .expect("offset succeeds");
        let expected = 100.0 + 80.0 + std::f64::consts::PI * 4.0;
        let got = grown.area();
        assert!(
            (got - expected).abs() / expected < 0.01,
            "round-join area {got} vs Steiner {expected}"
        );
    }

    #[test]
    fn offset_negative_r_shrinks() {
        // Square(10) offset r=-2 → 6×6 = 36 exactly (no reflex corners
        // on a convex square — the arcs have nothing to fill).
        let square = square_profile([10.0, 10.0], false);
        let shrunk = offset_profile(
            &square,
            &OffsetParams {
                delta: -2.0,
                join: OffsetJoin::Round,
                chamfer: false,
                fragments: default_fragments(),
            },
        )
        .expect("offset succeeds");
        let got = shrunk.area();
        assert!(
            (got - 36.0).abs() / 36.0 < 0.001,
            "shrunk area {got} vs exact 36"
        );
    }

    #[test]
    fn offset_miter_and_bevel_corner_geometry() {
        // Miter keeps 4 vertices on a square; bevel adds one per corner
        // (4 → 8). Polygon vertex counts via to_polygons outer rings.
        let square = square_profile([10.0, 10.0], false);
        let count_outer_vertices = |p: &Profile| -> usize {
            p.to_polygons()
                .iter()
                .map(|ring| ring.len())
                .max()
                .expect("at least one ring")
        };

        let mitered = offset_profile(
            &square,
            &OffsetParams {
                delta: 2.0,
                join: OffsetJoin::Miter,
                chamfer: false,
                fragments: default_fragments(),
            },
        )
        .expect("miter succeeds");
        assert_eq!(count_outer_vertices(&mitered), 4, "miter preserves corners");
        let miter_area = mitered.area();
        assert!(
            (miter_area - 196.0).abs() / 196.0 < 0.001,
            "miter area {miter_area} vs exact 14x14=196"
        );

        let beveled = offset_profile(
            &square,
            &OffsetParams {
                delta: 2.0,
                join: OffsetJoin::Bevel,
                chamfer: true,
                fragments: default_fragments(),
            },
        )
        .expect("bevel succeeds");
        assert_eq!(
            count_outer_vertices(&beveled),
            8,
            "bevel adds a vertex per corner"
        );
        // Clipper2's JoinType::Square squares the miter tip at 1×delta along
        // the corner bisector: per-corner fill 2δ²(√2−1) — vs the miter's δ²
        // or a classic 45° chamfer's δ²/2 (which would total 100+80+2δ²).
        // Total = 100 + 80 + 4·2δ²(√2−1); probing verified the form is
        // scale-invariant (δ=0.5/1/2 all exact). Modern OpenSCAD also maps
        // chamfer→jtSquare, so this is likely NOT a divergence at all; an
        // oracle comparison would confirm it.
        let d = 2.0_f64;
        let expected = 100.0 + 80.0 + 4.0 * 2.0 * d * d * (std::f64::consts::SQRT_2 - 1.0);
        let bevel_area = beveled.area();
        assert!(
            (bevel_area - expected).abs() / expected < 0.001,
            "bevel area {bevel_area} vs Clipper2 square-join closed form {expected}"
        );
        // And it sits strictly between the round (π) and miter fills.
        assert!(bevel_area > 192.566 && bevel_area < 196.0);
    }

    // -----------------------------------------------------------------------
    // project_solid
    // -----------------------------------------------------------------------

    #[test]
    fn projection_cut_cube_below_origin_is_full_square() {
        // translate([0,0,-1]) cube(10), cut at z=0 → the full 10x10
        // cross-section: area 100.
        let cube = cube_solid([10.0, 10.0, 10.0], false);
        let moved = xform_solid(
            &TransformKind::Affine([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, -1.0],
                [0.0, 0.0, 0.0, 1.0],
            ]),
            cube,
        );
        let cut = project_solid(&moved, true).expect("cut projects");
        assert!(
            (cut.area() - 100.0).abs() / 100.0 < 1e-9,
            "cut area {}",
            cut.area()
        );
    }

    #[test]
    fn projection_shadow_of_hollow_solid_has_hole() {
        // A hollow solid (difference of cylinders) shadows with a hole:
        // outer area minus inner ≈ π(R² − r²). R=4, r=2 → 12π ≈ 37.699.
        let outer = solid(&Primitive::Cylinder {
            h: 10.0,
            r1: 4.0,
            r2: 4.0,
            center: false,
            fragments: 64,
        });
        let inner = solid(&Primitive::Cylinder {
            h: 12.0,
            r1: 2.0,
            r2: 2.0,
            center: false,
            fragments: 64,
        });
        let hollow = match difference(&[outer, inner]).expect("difference") {
            Some(s) => s,
            None => panic!("hollow solid"),
        };
        let shadow = project_solid(&hollow, false).expect("shadow projects");
        let expected = std::f64::consts::PI * (16.0 - 4.0);
        assert!(
            (shadow.area() - expected).abs() / expected < 0.02,
            "shadow area {} vs pi(R^2 - r^2) {expected}",
            shadow.area()
        );
        // Two contours: the outer ring + the hole.
        assert_eq!(
            shadow.to_polygons().len(),
            2,
            "hole survives as a second contour"
        );
    }

    #[test]
    fn projection_cut_above_solid_is_empty_profile() {
        // A cut plane at z=0 above the solid (cube at z=5..15): the section
        // is empty — a VALID empty Profile, not an error.
        let cube = cube_solid([10.0, 10.0, 10.0], false);
        let moved = xform_solid(
            &TransformKind::Affine([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 5.0],
                [0.0, 0.0, 0.0, 1.0],
            ]),
            cube,
        );
        let cut = project_solid(&moved, true).expect("empty section is Ok");
        assert_eq!(cut.area(), 0.0, "empty profile, zero area");
        assert!(cut.to_polygons().is_empty());
    }

    // -----------------------------------------------------------------------
    // minkowski (a correctness hot spot)
    // -----------------------------------------------------------------------

    #[test]
    fn minkowski_cube_cube_volume_is_8000() {
        // Cube(10) ⊕ cube(10) is the exact 20-cube → 8000.
        let a = cube_solid([10.0, 10.0, 10.0], false);
        let b = cube_solid([10.0, 10.0, 10.0], false);
        let sum = minkowski(&[a, b]).expect("sum").expect("non-empty");
        assert!(
            (sum.volume() - 8000.0).abs() / 8000.0 < 1e-9,
            "cube⊕cube volume {}",
            sum.volume()
        );
    }

    #[test]
    fn minkowski_cube_sphere_rounded_box_volume() {
        // The rounded-box analytic — V = a³ + 6a²r + 3πar² + (4/3)πr³
        // (six face slabs + twelve quarter-cylinder edges + eight corner
        // octants) = 1000 + 1200 + 120π + (32/3)π ≈ 2610.52 at a=10, r=2.
        // ±3% for the faceted sphere ($fn=64 keeps it tight).
        let cube = cube_solid([10.0, 10.0, 10.0], false);
        let sphere = solid(&Primitive::Sphere {
            r: 2.0,
            fragments: 64,
        });
        let sum = minkowski(&[cube, sphere]).expect("sum").expect("non-empty");
        let expected = 1000.0
            + 6.0 * 100.0 * 2.0
            + 3.0 * std::f64::consts::PI * 10.0 * 4.0
            + (4.0 / 3.0) * std::f64::consts::PI * 8.0;
        assert!(
            (sum.volume() - expected).abs() / expected < 0.03,
            "rounded box {} vs analytic {expected}",
            sum.volume()
        );
    }

    #[test]
    fn minkowski_square_circle_area_is_steiner() {
        // The 2D prism-identity path — square(10) ⊕ circle(2) has area
        // 100 + 80 + 4π ≈ 192.566 (the planar Steiner form; $fn=64).
        let sq = square_profile([10.0, 10.0], false);
        let ci = profile(&Primitive::Circle {
            r: 2.0,
            fragments: 64,
        });
        let sum = minkowski_profiles(&[sq, ci])
            .expect("sum")
            .expect("non-empty");
        let expected = 100.0 + 80.0 + std::f64::consts::PI * 4.0;
        assert!(
            (sum.area() - expected).abs() / expected < 0.02,
            "2D sum area {} vs Steiner {expected}",
            sum.area()
        );
    }

    #[test]
    fn minkowski_identity_and_empty_cases() {
        // N=0 → None; n=1 → passthrough (volume preserved).
        assert!(minkowski(&[]).expect("ok").is_none());
        let cube = cube_solid([7.0, 8.0, 9.0], false);
        let one = minkowski(&[cube]).expect("ok").expect("passthrough");
        assert!((one.volume() - 504.0).abs() / 504.0 < 1e-9);
        // The 2D twins.
        assert!(minkowski_profiles(&[]).expect("ok").is_none());
        let sq = square_profile([5.0, 5.0], false);
        let one_p = minkowski_profiles(&[sq]).expect("ok").expect("passthrough");
        assert!((one_p.area() - 25.0).abs() / 25.0 < 1e-9);
    }
}
