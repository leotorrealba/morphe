//! Serializable geometry statistics.
//!
//! [`geometry_stats`] measures a [`Solid`] into a [`GeometryStats`] — bounding
//! box, volume, surface area, manifoldness, and mesh counts — as a plain
//! `serde`-serializable value. The AI loop's `get_geometry_stats` tool and
//! the CLI consume this as JSON, so every field name is snake_case and
//! the struct round-trips through `serde_json` unchanged.
//!
//! The 2D analogue [`profile_stats`] measures a [`Profile`] into a
//! [`ProfileStats`] (area + 2D bounding box). Both derive `Deserialize` and
//! `PartialEq` so a JSON round-trip can be asserted by value.
//!
//! # Empty geometry
//!
//! An empty [`Solid`]/[`Profile`] (a valid, volume/area-less body — e.g. a
//! `difference` that removes everything) has no bounding box: Manifold's
//! `bounding_box()` returns `None` for it, so [`GeometryStats::bbox`] and
//! [`GeometryStats::bbox_size`] are `None` (serialized as JSON `null`) rather
//! than a degenerate zeroed box. Volume, surface area, and counts are `0`, and
//! `is_manifold` stays `true` (an empty body is a valid manifold).

use serde::{Deserialize, Serialize};

use crate::profile::Profile;
use crate::solid::Solid;

/// Serializable geometry statistics for a [`Solid`].
///
/// Produced by [`geometry_stats`]. Field names are snake_case and the struct
/// round-trips through `serde_json` (derives `Serialize` + `Deserialize`), so
/// the AI-loop tool and CLI can emit or re-read it as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryStats {
    /// Axis-aligned bounding box as `[min_corner, max_corner]`, each `[x, y, z]`.
    /// `None` for an empty solid (no enclosed volume, no box).
    pub bbox: Option<[[f64; 3]; 2]>,
    /// Bounding-box extent `[x, y, z]` (`max - min`). `None` for an empty solid.
    /// Included as a convenience for the AI loop (which reasons about size more
    /// than raw corners).
    pub bbox_size: Option<[f64; 3]>,
    /// Signed enclosed volume (`0.0` for an empty solid).
    pub volume: f64,
    /// Total surface area (`0.0` for an empty solid).
    pub surface_area: f64,
    /// Whether the body is a valid manifold (true for anything the kernel
    /// constructs, including an empty body).
    pub is_manifold: bool,
    /// Number of triangles in the mesh.
    pub triangle_count: usize,
    /// Number of distinct vertices in the mesh.
    pub vertex_count: usize,
}

/// Measure a [`Solid`] into a [`GeometryStats`].
///
/// The bounding box comes from Manifold's own `bounding_box()` (which returns
/// `None` for an empty body); [`GeometryStats::bbox_size`] is its `max - min`
/// extent. See the module docs for empty-solid semantics.
#[must_use]
pub fn geometry_stats(solid: &Solid) -> GeometryStats {
    let bbox = solid.manifold().bounding_box().map(|b| [b.min(), b.max()]);
    let bbox_size = bbox.map(|[min, max]| [max[0] - min[0], max[1] - min[1], max[2] - min[2]]);
    GeometryStats {
        bbox,
        bbox_size,
        volume: solid.volume(),
        surface_area: solid.manifold().surface_area(),
        is_manifold: solid.is_manifold(),
        triangle_count: solid.num_tri(),
        vertex_count: solid.num_vert(),
    }
}

/// Serializable geometry statistics for a 2D [`Profile`].
///
/// The 2D analogue of [`GeometryStats`]: area plus a 2D bounding box. The AI
/// tool mostly reasons about 3D solids, but a 2D result still reports something
/// meaningful. (No centroid — Manifold's `CrossSection` exposes none cheaply.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileStats {
    /// Axis-aligned 2D bounding box as `[min_corner, max_corner]`, each
    /// `[x, y]`. `None` for an empty profile (no enclosed area, no box).
    pub bbox: Option<[[f64; 2]; 2]>,
    /// Bounding-box extent `[x, y]` (`max - min`). `None` for an empty profile.
    pub bbox_size: Option<[f64; 2]>,
    /// Total enclosed area (holes subtracted; `0.0` for an empty profile).
    pub area: f64,
    /// Number of distinct contours (outlines and holes).
    pub contour_count: usize,
    /// Total number of vertices across all contours.
    pub vertex_count: usize,
}

/// Measure a [`Profile`] into a [`ProfileStats`].
///
/// The 2D bounding box comes from the cross-section's `bounds()` rectangle; an
/// empty profile has no box, so [`ProfileStats::bbox`]/`bbox_size` are `None`.
#[must_use]
pub fn profile_stats(profile: &Profile) -> ProfileStats {
    let bbox = if profile.is_empty() {
        None
    } else {
        let r = profile.cross_section().bounds();
        Some([r.min(), r.max()])
    };
    let bbox_size = bbox.map(|[min, max]| [max[0] - min[0], max[1] - min[1]]);
    ProfileStats {
        bbox,
        bbox_size,
        area: profile.area(),
        contour_count: profile.num_contour(),
        vertex_count: profile.num_vert(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::FillRule;
    use crate::tessellate::tessellate_primitive;
    use morphe_eval::csg::Primitive;
    use std::f64::consts::PI;

    /// An empty (volume-less) solid: a small centred cube fully removed by a
    /// larger one. The op ran and produced a valid, empty manifold.
    fn empty_solid() -> Solid {
        let small = Solid::cube([1.0, 1.0, 1.0], true);
        let big = Solid::cube([4.0, 4.0, 4.0], true);
        let out = crate::tessellate::difference(&[small, big])
            .expect("difference of two solids")
            .expect("two operands is Some");
        assert!(out.is_empty(), "big cube should fully remove the small one");
        out
    }

    /// A tessellated sphere of the given radius via the primitive path.
    fn sphere(r: f64, fragments: u32) -> Solid {
        let g =
            tessellate_primitive(&Primitive::Sphere { r, fragments }).expect("sphere tessellates");
        match g {
            crate::tessellate::Geometry::Solid(s) => s,
            crate::tessellate::Geometry::Profile(_) => panic!("sphere is a solid"),
        }
    }

    // ── 3D solid: cube ───────────────────────────────────────────────────────

    #[test]
    fn cube_bbox_min_max() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        assert_eq!(stats.bbox, Some([[0.0, 0.0, 0.0], [2.0, 2.0, 2.0]]));
    }

    #[test]
    fn cube_bbox_size() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        assert_eq!(stats.bbox_size, Some([2.0, 2.0, 2.0]));
    }

    #[test]
    fn cube_volume() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        assert!(
            (stats.volume - 8.0).abs() < 1e-9,
            "cube(2) volume should be 8, got {}",
            stats.volume
        );
    }

    #[test]
    fn cube_surface_area() {
        // 6 faces * 2*2 = 24.
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        assert!(
            (stats.surface_area - 24.0).abs() < 1e-9,
            "cube(2) surface area should be 24, got {}",
            stats.surface_area
        );
    }

    #[test]
    fn cube_is_manifold_true() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        assert!(stats.is_manifold);
    }

    #[test]
    fn cube_triangle_and_vertex_counts() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        assert_eq!(stats.triangle_count, 12, "a box has 12 triangles");
        assert_eq!(stats.vertex_count, 8, "a box has 8 vertices");
    }

    #[test]
    fn centered_cube_bbox_symmetric() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], true));
        assert_eq!(stats.bbox, Some([[-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]]));
        assert_eq!(stats.bbox_size, Some([2.0, 2.0, 2.0]));
    }

    // ── 3D solid: sphere (tessellation-tolerant) ─────────────────────────────

    #[test]
    fn sphere_volume_approx() {
        let r = 10.0;
        let stats = geometry_stats(&sphere(r, 128));
        let analytic = 4.0 / 3.0 * PI * r * r * r;
        // Inscribed tessellation under-reports; a 128-gon sphere is within ~1%.
        assert!(
            (stats.volume - analytic).abs() / analytic < 0.02,
            "sphere volume {} should be near {}",
            stats.volume,
            analytic
        );
    }

    #[test]
    fn sphere_surface_area_approx() {
        let r = 10.0;
        let stats = geometry_stats(&sphere(r, 128));
        let analytic = 4.0 * PI * r * r;
        assert!(
            (stats.surface_area - analytic).abs() / analytic < 0.02,
            "sphere area {} should be near {}",
            stats.surface_area,
            analytic
        );
    }

    #[test]
    fn sphere_bbox_approx() {
        let r = 10.0;
        let stats = geometry_stats(&sphere(r, 128));
        let [min, max] = stats.bbox.expect("sphere has a bbox");
        for axis in 0..3 {
            assert!(
                (max[axis] - r).abs() < 0.1 * r,
                "sphere bbox max[{axis}] {} should be near {r}",
                max[axis]
            );
            assert!(
                (min[axis] + r).abs() < 0.1 * r,
                "sphere bbox min[{axis}] {} should be near {}",
                min[axis],
                -r
            );
        }
    }

    // ── 3D solid: empty ──────────────────────────────────────────────────────

    #[test]
    fn empty_solid_bbox_none() {
        let stats = geometry_stats(&empty_solid());
        assert_eq!(stats.bbox, None);
        assert_eq!(stats.bbox_size, None);
    }

    #[test]
    fn empty_solid_volume_and_area_zero() {
        let stats = geometry_stats(&empty_solid());
        assert!((stats.volume).abs() < 1e-12, "empty volume is 0");
        assert!((stats.surface_area).abs() < 1e-12, "empty area is 0");
    }

    #[test]
    fn empty_solid_counts_zero_and_manifold() {
        let stats = geometry_stats(&empty_solid());
        assert_eq!(stats.triangle_count, 0);
        assert_eq!(stats.vertex_count, 0);
        assert!(stats.is_manifold, "an empty body is a valid manifold");
    }

    // ── 3D solid: transforms shift/scale the bbox ────────────────────────────

    #[test]
    fn translated_cube_bbox_shifts() {
        let cube = Solid::cube([2.0, 2.0, 2.0], false);
        let moved = Solid::from_manifold(cube.manifold().translate(1.0, 2.0, 3.0));
        let stats = geometry_stats(&moved);
        assert_eq!(stats.bbox, Some([[1.0, 2.0, 3.0], [3.0, 4.0, 5.0]]));
        assert_eq!(stats.bbox_size, Some([2.0, 2.0, 2.0]));
    }

    #[test]
    fn scaled_cube_bbox_and_size() {
        let cube = Solid::cube([2.0, 2.0, 2.0], false);
        let scaled = Solid::from_manifold(cube.manifold().scale(2.0, 3.0, 4.0));
        let stats = geometry_stats(&scaled);
        assert_eq!(stats.bbox, Some([[0.0, 0.0, 0.0], [4.0, 6.0, 8.0]]));
        assert_eq!(stats.bbox_size, Some([4.0, 6.0, 8.0]));
        // Volume scales by 2*3*4 = 24: 8 -> 192.
        assert!(
            (stats.volume - 192.0).abs() < 1e-9,
            "scaled volume should be 192, got {}",
            stats.volume
        );
    }

    // ── JSON serialization ───────────────────────────────────────────────────

    #[test]
    fn json_round_trip_equals_original() {
        let stats = geometry_stats(&Solid::cube([2.0, 3.0, 4.0], true));
        let json = serde_json::to_string(&stats).expect("serialize");
        let back: GeometryStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stats, back, "stats must survive a JSON round-trip");
    }

    #[test]
    fn json_field_names_are_snake_case() {
        let stats = geometry_stats(&Solid::cube([2.0, 2.0, 2.0], false));
        let json = serde_json::to_string(&stats).expect("serialize");
        for key in [
            "\"bbox\"",
            "\"bbox_size\"",
            "\"surface_area\"",
            "\"is_manifold\"",
            "\"triangle_count\"",
            "\"vertex_count\"",
        ] {
            assert!(json.contains(key), "JSON {json} should contain {key}");
        }
    }

    #[test]
    fn json_empty_solid_bbox_is_null() {
        let stats = geometry_stats(&empty_solid());
        let json = serde_json::to_string(&stats).expect("serialize");
        assert!(
            json.contains("\"bbox\":null"),
            "empty solid bbox should serialize as null, got {json}"
        );
    }

    // ── 2D profile ───────────────────────────────────────────────────────────

    #[test]
    fn profile_stats_square_area_and_bbox() {
        let square = vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        let p = Profile::from_contours(&[square], FillRule::EvenOdd);
        let stats = profile_stats(&p);
        assert!((stats.area - 4.0).abs() < 1e-9, "2x2 square area is 4");
        assert_eq!(stats.bbox, Some([[0.0, 0.0], [2.0, 2.0]]));
        assert_eq!(stats.bbox_size, Some([2.0, 2.0]));
        assert_eq!(stats.contour_count, 1);
        assert_eq!(stats.vertex_count, 4);
    }

    #[test]
    fn profile_stats_empty_bbox_none() {
        let p = Profile::from_contours(&[], FillRule::EvenOdd);
        let stats = profile_stats(&p);
        assert!(p.is_empty());
        assert_eq!(stats.bbox, None);
        assert_eq!(stats.bbox_size, None);
        assert!((stats.area).abs() < 1e-12);
    }

    #[test]
    fn profile_stats_json_round_trip() {
        let square = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let p = Profile::from_contours(&[square], FillRule::EvenOdd);
        let stats = profile_stats(&p);
        let json = serde_json::to_string(&stats).expect("serialize");
        let back: ProfileStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stats, back);
    }
}
