//! The [`Profile`] newtype — Morphe's handle onto a 2D cross-section.
//!
//! `Profile` is the 2D analogue of [`crate::Solid`]: a thin wrapper around a
//! [`manifold3d::CrossSection`] (the Clipper2-backed 2D region type the same
//! kernel exposes alongside `Manifold`). Wrapping it behind our own newtype
//! keeps the third-party type out of the rest of the codebase, exactly as
//! `Solid` does for `Manifold`, so the extrude operations (`linear_extrude`,
//! `rotate_extrude`) and the evaluator speak `Profile` and never touch
//! `manifold3d` directly.
//!
//! This module provides the binding plus construction from a list of
//! contours under a chosen [`FillRule`]; the extrude operations consume the
//! wrapped cross-section to lift a profile into a [`crate::Solid`].

use manifold3d::{CrossSection, FillRule as ManifoldFillRule, Manifold};

use crate::solid::Solid;

/// How overlapping or nested contours combine into filled area when a
/// [`Profile`] is built from polygons.
///
/// Mirrors `manifold3d::FillRule` (Clipper2's fill rules) but is redeclared
/// here so the third-party enum stays out of Morphe's public API — the eval /
/// CLI seam matches on this type, not on `manifold3d`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// A point is inside when the number of contours crossing a ray from it is
    /// odd. Nested contours therefore alternate solid/hole regardless of their
    /// winding direction — the rule OpenSCAD's `polygon` uses so an inner path
    /// becomes a hole no matter which way it is wound.
    EvenOdd,
    /// Inside when the signed crossing (winding) count is non-zero.
    NonZero,
    /// Inside when the signed crossing count is positive.
    Positive,
    /// Inside when the signed crossing count is negative.
    Negative,
}

impl FillRule {
    fn to_manifold(self) -> ManifoldFillRule {
        match self {
            FillRule::EvenOdd => ManifoldFillRule::EvenOdd,
            FillRule::NonZero => ManifoldFillRule::NonZero,
            FillRule::Positive => ManifoldFillRule::Positive,
            FillRule::Negative => ManifoldFillRule::Negative,
        }
    }
}

/// A 2D region: one or more closed contours enclosing a well-defined area.
///
/// Constructed from a list of contours ([`Profile::from_contours`]) and read
/// back out with [`Profile::to_polygons`]. The wrapped cross-section owns its
/// native handle and frees it on drop. A `Profile` may be empty (zero area,
/// zero contours) — e.g. a set of collinear points encloses nothing.
pub struct Profile(CrossSection);

impl Profile {
    /// Build a `Profile` from a list of contours, each a closed ring of
    /// `[x, y]` points (the closing edge back to the first point is implicit).
    ///
    /// `fill_rule` decides how the contours combine: with [`FillRule::EvenOdd`]
    /// a contour nested inside another becomes a hole irrespective of winding
    /// (OpenSCAD's `polygon` semantics). The kernel normalises the result, so
    /// the reported [`area`](Self::area) is always non-negative regardless of
    /// the input winding direction.
    #[must_use]
    pub fn from_contours(contours: &[Vec<[f64; 2]>], fill_rule: FillRule) -> Self {
        Self(CrossSection::from_polygons_with_fill_rule(
            contours,
            fill_rule.to_manifold(),
        ))
    }

    /// Wrap a raw [`manifold3d::CrossSection`] produced inside the kernel — e.g.
    /// by the 2D boolean/hull functions in [`crate::tessellate`] combining
    /// several profiles. Crate-internal so the third-party handle never leaks
    /// past the newtype boundary.
    pub(crate) fn from_cross_section(cross_section: CrossSection) -> Self {
        Self(cross_section)
    }

    /// Borrow the wrapped [`manifold3d::CrossSection`] — the raw kernel side
    /// used by the 2D boolean/hull functions to feed Clipper2's batch operators.
    pub(crate) fn cross_section(&self) -> &CrossSection {
        &self.0
    }

    /// Total enclosed area (holes subtracted). Always non-negative.
    #[must_use]
    pub fn area(&self) -> f64 {
        self.0.area()
    }

    /// Number of distinct contours (outlines and holes) in the region.
    #[must_use]
    pub fn num_contour(&self) -> usize {
        self.0.num_contour()
    }

    /// Total number of vertices across all contours.
    #[must_use]
    pub fn num_vert(&self) -> usize {
        self.0.num_vert()
    }

    /// Extract the region as a list of contours, each a ring of `[x, y]`
    /// points. The inverse of [`Profile::from_contours`] (up to kernel
    /// normalisation of winding and vertex order).
    #[must_use]
    pub fn to_polygons(&self) -> Vec<Vec<[f64; 2]>> {
        self.0.to_polygons()
    }

    /// Whether the region encloses no area.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sweep this cross-section into a [`Solid`] along +Z — the raw kernel side
    /// of `linear_extrude` (upstream `LinearExtrudeNode`). The parameter
    /// resolution (height validation, twist-sign mapping, slice count, negative
    /// scale clamp) lives in [`crate::tessellate::linear_extrude`]; this method
    /// only calls the kernel with already-resolved values.
    ///
    /// `slices` is Manifold's `nDivisions` (vertical subdivision count for twist
    /// smoothness), `twist_degrees` the total top-vs-bottom twist already mapped
    /// to Manifold's sign, and `scale` the per-axis top scale `[x, y]`. The raw
    /// extrusion spans `z ∈ [0, height]`; `center` shifts it to `[-h/2, h/2]`.
    #[must_use]
    pub(crate) fn extrude(
        &self,
        height: f64,
        slices: i32,
        twist_degrees: f64,
        scale: [f64; 2],
        center: bool,
    ) -> Solid {
        let manifold = Manifold::extrude_with_options(
            &self.0,
            height,
            slices,
            twist_degrees,
            scale[0],
            scale[1],
        );
        let manifold = if center {
            manifold.translate(0.0, 0.0, -height / 2.0)
        } else {
            manifold
        };
        Solid::from_manifold(manifold)
    }

    /// Revolve this cross-section into a [`Solid`] — the raw kernel side of
    /// `rotate_extrude` (upstream `RotateExtrudeNode`). Parameter resolution
    /// (angle/start validation, the max-radius segment count) lives in
    /// [`crate::tessellate::rotate_extrude`]; this method only calls the kernel
    /// with already-resolved values.
    ///
    /// The cross-section's local `(x, y)` maps to `(radius, axial height)`: the
    /// profile's `x` becomes the distance from the rotation (Z) axis and its `y`
    /// becomes the axial Z coordinate, so the region sweeps around Z (Manifold
    /// `Revolve` revolves the section about *its* Y axis, which becomes the
    /// result's Z axis). `circular_segments` is Manifold's `nDivisions` (the
    /// number of angular facets already scaled for a partial `angle_degrees`),
    /// and `revolve_degrees` the sweep. Manifold clips any part of the profile
    /// with `x < 0` against the axis before revolving. A non-zero `start_degrees`
    /// rotates the finished body about Z, since Manifold always begins the sweep
    /// at angle `0` (upstream applies `start` as a per-vertex angle offset).
    #[must_use]
    pub(crate) fn revolve(
        &self,
        circular_segments: i32,
        revolve_degrees: f64,
        start_degrees: f64,
    ) -> Solid {
        let manifold = Manifold::revolve(&self.0, circular_segments, revolve_degrees);
        let manifold = if start_degrees != 0.0 {
            manifold.rotate(0.0, 0.0, start_degrees)
        } else {
            manifold
        };
        Solid::from_manifold(manifold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit square `[0,1]^2` as a single counter-clockwise contour.
    fn unit_square() -> Vec<[f64; 2]> {
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
    }

    #[test]
    fn from_contours_single_square_area_and_counts() {
        let p = Profile::from_contours(&[unit_square()], FillRule::EvenOdd);
        assert!(!p.is_empty());
        assert_eq!(p.num_contour(), 1);
        assert_eq!(p.num_vert(), 4);
        assert!(
            (p.area() - 1.0).abs() < 1e-9,
            "unit square area, got {}",
            p.area()
        );
    }

    #[test]
    fn area_is_winding_independent() {
        // Same square wound clockwise: EvenOdd fills its interior all the same,
        // and the reported area is the positive geometric area either way.
        let cw = vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        let p = Profile::from_contours(&[cw], FillRule::EvenOdd);
        assert!(
            (p.area() - 1.0).abs() < 1e-9,
            "cw square area, got {}",
            p.area()
        );
    }

    #[test]
    fn even_odd_nested_contour_is_a_hole() {
        // 10x10 outer with a 4x4 inner ring (both CCW): EvenOdd carves the
        // inner as a hole -> area = 100 - 16 = 84.
        let outer = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let inner = vec![[3.0, 3.0], [7.0, 3.0], [7.0, 7.0], [3.0, 7.0]];
        let p = Profile::from_contours(&[outer, inner], FillRule::EvenOdd);
        assert_eq!(p.num_contour(), 2);
        assert!(
            (p.area() - 84.0).abs() < 1e-9,
            "square-with-hole area, got {}",
            p.area()
        );
    }

    #[test]
    fn to_polygons_round_trips_area() {
        let p = Profile::from_contours(&[unit_square()], FillRule::EvenOdd);
        let rings = p.to_polygons();
        assert_eq!(rings.len(), 1);
        let rebuilt = Profile::from_contours(&rings, FillRule::EvenOdd);
        assert!((rebuilt.area() - p.area()).abs() < 1e-9);
    }

    #[test]
    fn empty_contour_list_is_empty() {
        let p = Profile::from_contours(&[], FillRule::EvenOdd);
        assert!(p.is_empty());
        assert_eq!(p.num_contour(), 0);
        assert!((p.area() - 0.0).abs() < 1e-12);
    }
}
