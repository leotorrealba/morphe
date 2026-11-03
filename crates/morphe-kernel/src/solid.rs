//! The [`Solid`] newtype — Morphe's handle onto a watertight 3D body.
//!
//! `Solid` is a thin wrapper around a [`manifold3d::Manifold`]: the fast CSG
//! kernel (elalish/manifold, the same engine modern OpenSCAD uses). Wrapping it
//! behind our own newtype keeps the third-party type out of the rest of the
//! codebase, so the boolean, transform, and export code and the evaluator
//! speak `Solid` and never touch `manifold3d` directly.
//!
//! This module provides the binding and the mesh round-trip: construct a
//! `Solid` from an indexed triangle mesh, read one back out, and a trivial
//! [`Solid::cube`] constructor so there is real geometry to assert on.
//! Tessellation of primitives, booleans, and STL export live in other modules
//! and build on this same handle.

use manifold3d::{Manifold, MeshGL64};

/// A watertight 3D body: a manifold triangle mesh with a well-defined volume.
///
/// Constructed from an indexed triangle mesh ([`Solid::from_mesh`]) or a
/// primitive constructor ([`Solid::cube`]), and read back out as
/// `(vertices, triangles)` with [`Solid::to_mesh`]. The wrapped manifold owns
/// its native handle and frees it on drop.
pub struct Solid(Manifold);

/// An error constructing a [`Solid`] from mesh data.
///
/// Carries the manifold kernel's own message (e.g. the input triangles do not
/// form a closed, consistently-oriented surface). Kept as a string so the
/// third-party error type does not leak into our public API — the eval/CLI
/// seam matches on `Result<Solid, SolidError>`, not on `manifold3d` types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolidError(String);

impl std::fmt::Display for SolidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid solid: {}", self.0)
    }
}

impl std::error::Error for SolidError {}

impl Solid {
    /// Build a `Solid` from an indexed triangle mesh: `verts` are `[x, y, z]`
    /// positions and each triangle in `tris` indexes three of them.
    ///
    /// The mesh must be a closed, consistently-oriented (outward-facing)
    /// manifold; otherwise the kernel rejects it and this returns
    /// [`SolidError`]. The kernel merges exactly-coincident vertices, so the
    /// vertex count read back may be lower than `verts.len()` for a mesh that
    /// repeats positions.
    pub fn from_mesh(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> Result<Self, SolidError> {
        let mut props = Vec::with_capacity(verts.len() * 3);
        for v in verts {
            props.extend_from_slice(v);
        }
        let mut indices = Vec::with_capacity(tris.len() * 3);
        for t in tris {
            indices.push(u64::from(t[0]));
            indices.push(u64::from(t[1]));
            indices.push(u64::from(t[2]));
        }

        // n_props = 3: each vertex carries only its position (x, y, z).
        let mesh = MeshGL64::new(&props, 3, &indices).map_err(|e| SolidError(e.to_string()))?;
        let manifold = Manifold::from_meshgl64(&mesh).map_err(|e| SolidError(e.to_string()))?;
        // `from_meshgl64` accepts the buffers eagerly but defers the
        // manifoldness check; force it so a non-manifold input fails here
        // rather than silently downstream.
        manifold.status().map_err(|e| SolidError(e.to_string()))?;
        Ok(Self(manifold))
    }

    /// Wrap a raw [`manifold3d::Manifold`] produced inside the kernel — e.g.
    /// by [`Profile::extrude`](crate::profile::Profile::extrude) lifting a
    /// cross-section into a body. Crate-internal so the third-party handle
    /// never leaks past the newtype boundary.
    pub(crate) fn from_manifold(manifold: Manifold) -> Self {
        Self(manifold)
    }

    /// Borrow the wrapped [`manifold3d::Manifold`] — the raw kernel side used by
    /// the boolean/hull functions in [`crate::tessellate`] to feed Manifold's
    /// batch operators. Crate-internal so the third-party handle never leaks
    /// past the newtype boundary.
    pub(crate) fn manifold(&self) -> &Manifold {
        &self.0
    }

    /// Whether the body encloses no volume — a *valid* empty result (e.g. a
    /// `difference` that removes everything, or the `intersection` of disjoint
    /// bodies). Distinct from a construction failure: an empty `Solid` is still
    /// a manifold and exports as an empty STL, matching upstream OpenSCAD.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A valid **empty** body — no volume, no triangles. This is a genuine
    /// manifold (not a construction failure): the tree resolver
    /// hands it to the STL exporter when a `.scad` file produces no geometry,
    /// which serializes to an empty STL (an 84-byte binary file, or a bare
    /// `solid morphe` / `endsolid morphe` ASCII pair), matching upstream
    /// OpenSCAD's output for an empty top-level object.
    #[must_use]
    pub fn empty() -> Self {
        Self(Manifold::empty())
    }

    /// An axis-aligned box of the given `size` (`[x, y, z]` edge lengths).
    ///
    /// When `center` is false the box's minimum corner sits at the origin
    /// (matching OpenSCAD's `cube` default); when true it is centred on the
    /// origin. Produces 8 vertices and 12 triangles.
    #[must_use]
    pub fn cube(size: [f64; 3], center: bool) -> Self {
        Self(Manifold::cube(size[0], size[1], size[2], center))
    }

    /// Extract the body as an indexed triangle mesh: `(vertices, triangles)`,
    /// where `vertices` are `[x, y, z]` positions and each triangle indexes
    /// three of them. This is the inverse of [`Solid::from_mesh`].
    #[must_use]
    pub fn to_mesh(&self) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (vert_props, n_props, tri_indices) = self.0.to_mesh_f64();
        let verts = vert_props
            .chunks_exact(n_props)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        let tris = tri_indices
            .chunks_exact(3)
            .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
            .collect();
        (verts, tris)
    }

    /// Number of distinct vertices in the body.
    #[must_use]
    pub fn num_vert(&self) -> usize {
        self.0.num_vert()
    }

    /// Number of triangles in the body.
    #[must_use]
    pub fn num_tri(&self) -> usize {
        self.0.num_tri()
    }

    /// Signed volume of the body. A unit box reports `1.0` (within floating
    /// point tolerance).
    #[must_use]
    pub fn volume(&self) -> f64 {
        self.0.volume()
    }

    /// Whether the body is a valid manifold. True for anything successfully
    /// constructed here (the kernel guarantees manifoldness by construction);
    /// exposed so callers and tests can assert it explicitly.
    #[must_use]
    pub fn is_manifold(&self) -> bool {
        self.0.status().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit cube `[0,1]^3` as an explicit indexed mesh: 8 corners and 12
    /// outward-facing triangles (two per face). Used to exercise
    /// [`Solid::from_mesh`] with geometry we control end to end.
    fn unit_cube_mesh() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0], // 0
            [1.0, 0.0, 0.0], // 1
            [1.0, 1.0, 0.0], // 2
            [0.0, 1.0, 0.0], // 3
            [0.0, 0.0, 1.0], // 4
            [1.0, 0.0, 1.0], // 5
            [1.0, 1.0, 1.0], // 6
            [0.0, 1.0, 1.0], // 7
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2], // bottom (-z)
            [4, 5, 6],
            [4, 6, 7], // top (+z)
            [0, 1, 5],
            [0, 5, 4], // front (-y)
            [3, 7, 6],
            [3, 6, 2], // back (+y)
            [0, 4, 7],
            [0, 7, 3], // left (-x)
            [1, 2, 6],
            [1, 6, 5], // right (+x)
        ];
        (verts, tris)
    }

    #[test]
    fn from_mesh_builds_manifold_box() {
        let (verts, tris) = unit_cube_mesh();
        let solid = Solid::from_mesh(&verts, &tris).expect("closed box is a valid manifold");

        assert_eq!(solid.num_vert(), 8, "a box has 8 corners");
        assert_eq!(solid.num_tri(), 12, "a box has 12 triangles (2 per face)");
        assert!(solid.is_manifold());
        assert!(
            (solid.volume() - 1.0).abs() < 1e-9,
            "unit box volume should be 1.0, got {}",
            solid.volume()
        );
    }

    #[test]
    fn round_trip_preserves_box_geometry() {
        let (verts, tris) = unit_cube_mesh();
        let solid = Solid::from_mesh(&verts, &tris).unwrap();

        // Extract, then rebuild from the extracted mesh: geometry survives the
        // construct -> extract -> construct cycle.
        let (out_verts, out_tris) = solid.to_mesh();
        assert_eq!(out_verts.len(), 8);
        assert_eq!(out_tris.len(), 12);

        let rebuilt = Solid::from_mesh(&out_verts, &out_tris).expect("extracted mesh rebuilds");
        assert_eq!(rebuilt.num_vert(), 8);
        assert_eq!(rebuilt.num_tri(), 12);
        assert!(
            (rebuilt.volume() - solid.volume()).abs() < 1e-9,
            "round-trip must preserve volume"
        );
    }

    #[test]
    fn round_trip_preserves_tetrahedron() {
        // A corner tetrahedron: 4 verts, 4 outward faces, volume 1/6.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let tris = vec![[0, 2, 1], [0, 3, 2], [0, 1, 3], [1, 2, 3]];
        let solid = Solid::from_mesh(&verts, &tris).expect("closed tetra is a valid manifold");

        assert_eq!(solid.num_vert(), 4);
        assert_eq!(solid.num_tri(), 4);
        assert!(
            (solid.volume() - 1.0 / 6.0).abs() < 1e-9,
            "tetra volume should be 1/6, got {}",
            solid.volume()
        );

        let (out_verts, out_tris) = solid.to_mesh();
        let rebuilt = Solid::from_mesh(&out_verts, &out_tris).unwrap();
        assert!((rebuilt.volume() - 1.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn cube_constructor_matches_analytic() {
        let solid = Solid::cube([1.0, 1.0, 1.0], false);
        assert_eq!(solid.num_vert(), 8);
        assert_eq!(solid.num_tri(), 12);
        assert!(solid.is_manifold());
        assert!((solid.volume() - 1.0).abs() < 1e-9);

        let big = Solid::cube([2.0, 3.0, 4.0], true);
        assert!(
            (big.volume() - 24.0).abs() < 1e-9,
            "2x3x4 box volume should be 24, got {}",
            big.volume()
        );
    }

    #[test]
    fn open_mesh_is_rejected() {
        // A single triangle is not a closed surface, so it is not a manifold.
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0, 1, 2]];
        assert!(
            Solid::from_mesh(&verts, &tris).is_err(),
            "an open (non-closed) mesh must not construct a Solid"
        );
    }
}
