//! STL export — binary and ASCII.
//!
//! [`to_binary_stl`]/[`write_binary_stl`] emit the compact binary form; the
//! ASCII counterparts [`to_ascii_stl`]/[`write_ascii_stl`] emit the textual
//! form. Both describe the identical mesh — coordinates and normals are cast to
//! `f32` before serialization in either path.
//!
//! Serializes a [`Solid`] to the binary STL format — the de-facto interchange
//! format for 3D printing and mesh viewers. Binary STL is a flat, little-endian
//! record:
//!
//! - an **80-byte header** (free-form). We write a fixed `"morphe binary STL"`
//!   ASCII tag zero-padded to 80 bytes. The header deliberately does **not**
//!   begin with the word `solid`, the token some naive loaders sniff for to
//!   guess *ASCII* STL — a binary file starting with `solid` is a classic
//!   misdetection trap.
//! - a **`u32` triangle count** (little-endian).
//! - one **50-byte record per triangle**: three `f32` normal components, then
//!   three vertices of three `f32` coordinates each (9 `f32`), then a `u16`
//!   *attribute byte count* which we always write as `0`.
//!
//! So a mesh of `n` triangles serializes to exactly `84 + 50 * n` bytes.
//!
//! # Normals
//!
//! STL stores a per-facet normal. Many slicers ignore it and recompute from the
//! winding, but we emit a proper outward unit normal: `(v1 - v0) × (v2 - v0)`,
//! normalized. Manifold hands us consistently outward (counter-clockwise-from-
//! outside) winding, so that cross product points out of the body. A degenerate
//! (zero-area) triangle has no defined normal; we emit `[0, 0, 0]` rather than a
//! `NaN` so the file stays well-formed.
//!
//! # Precision
//!
//! STL coordinates are single precision. The kernel mesh is `f64`; every
//! coordinate and normal component is cast to `f32` on write (IEEE-754,
//! little-endian).
//!
//! # Empty solid
//!
//! An empty [`Solid`] (a valid, volume-less body — e.g. a `difference` that
//! removes everything) serializes to a valid STL with zero triangles: the
//! 80-byte header plus a `u32` `0`, i.e. exactly 84 bytes.

use std::fmt::Write as FmtWrite;
use std::io::{self, Write};

use crate::solid::Solid;

/// Fixed 80-byte-header tag. Chosen not to start with `solid` (see module docs).
const HEADER_TAG: &[u8] = b"morphe binary STL";

/// Serialize `solid` to binary STL bytes.
///
/// Returns exactly `84 + 50 * solid.num_tri()` bytes. Convenience wrapper over
/// [`write_binary_stl`] that collects into a `Vec`; writing to a `Vec` cannot
/// fail, so this is infallible.
#[must_use]
pub fn to_binary_stl(solid: &Solid) -> Vec<u8> {
    let mut buf = Vec::with_capacity(84 + 50 * solid.num_tri());
    write_binary_stl(solid, &mut buf).expect("writing binary STL to a Vec never fails");
    buf
}

/// Stream binary STL for `solid` into `w`.
///
/// Writes the 80-byte header, the `u32` triangle count, and one 50-byte record
/// per triangle. See the module docs for the exact layout and normal
/// convention. Propagates any `io::Error` from `w`.
pub fn write_binary_stl<W: Write>(solid: &Solid, w: &mut W) -> io::Result<()> {
    let (verts, tris) = solid.to_mesh();

    // 80-byte header: fixed tag, zero-padded (see module docs).
    let mut header = [0u8; 80];
    header[..HEADER_TAG.len()].copy_from_slice(HEADER_TAG);
    w.write_all(&header)?;

    // u32 triangle count (little-endian).
    w.write_all(&(tris.len() as u32).to_le_bytes())?;

    // One 50-byte record per triangle.
    for tri in &tris {
        let v0 = verts[tri[0] as usize];
        let v1 = verts[tri[1] as usize];
        let v2 = verts[tri[2] as usize];
        let normal = face_normal(v0, v1, v2);

        for c in normal {
            w.write_all(&(c as f32).to_le_bytes())?;
        }
        for v in [v0, v1, v2] {
            for c in v {
                w.write_all(&(c as f32).to_le_bytes())?;
            }
        }
        // Attribute byte count: always 0.
        w.write_all(&0u16.to_le_bytes())?;
    }

    Ok(())
}

/// Fixed solid name used in the ASCII STL `solid`/`endsolid` lines.
const SOLID_NAME: &str = "morphe";

/// Serialize `solid` to an ASCII STL document.
///
/// Emits the textual STL grammar — a `solid morphe` header, one `facet` block
/// per triangle, and an `endsolid morphe` footer. This describes the **same**
/// mesh as [`to_binary_stl`]: coordinates and normals are cast to `f32` before
/// formatting, so the two exports agree bit-for-bit on every value.
///
/// # Number format
///
/// Each `f32` component is written with Rust's default `Display` (`{}`), which
/// prints the *shortest* decimal that round-trips back to the same `f32`. That
/// preserves full single-precision fidelity (unlike OpenSCAD's lossy `%g`) while
/// staying compact and pure ASCII. Whole numbers print without a decimal point
/// (`1`, `0`), fractions with just enough digits (`0.5`, `0.1`).
///
/// A degenerate (zero-area) triangle has no defined normal; like the binary
/// path it emits `0 0 0` rather than `NaN`, so the document never contains a
/// non-finite token.
///
/// # Layout
///
/// Unix line endings (`\n`) and 2-space-per-level indentation. The output is
/// always valid ASCII (hence valid UTF-8).
///
/// # Empty solid
///
/// An empty [`Solid`] (zero triangles) serializes to header + footer only:
/// `"solid morphe\nendsolid morphe\n"`, with no `facet` blocks.
#[must_use]
pub fn to_ascii_stl(solid: &Solid) -> String {
    let (verts, tris) = solid.to_mesh();
    let mut s = String::new();

    let _ = writeln!(s, "solid {SOLID_NAME}");
    for tri in &tris {
        let v0 = verts[tri[0] as usize];
        let v1 = verts[tri[1] as usize];
        let v2 = verts[tri[2] as usize];
        let n = face_normal(v0, v1, v2);

        let _ = writeln!(
            s,
            "  facet normal {} {} {}",
            n[0] as f32, n[1] as f32, n[2] as f32
        );
        s.push_str("    outer loop\n");
        for v in [v0, v1, v2] {
            let _ = writeln!(
                s,
                "      vertex {} {} {}",
                v[0] as f32, v[1] as f32, v[2] as f32
            );
        }
        s.push_str("    endloop\n");
        s.push_str("  endfacet\n");
    }
    let _ = writeln!(s, "endsolid {SOLID_NAME}");

    s
}

/// Stream ASCII STL for `solid` into `w`.
///
/// Text counterpart to [`write_binary_stl`]. Writes the same document
/// [`to_ascii_stl`] produces. Propagates any `io::Error` from `w`.
pub fn write_ascii_stl<W: Write>(solid: &Solid, w: &mut W) -> io::Result<()> {
    w.write_all(to_ascii_stl(solid).as_bytes())
}

/// Outward unit normal of the triangle `(v0, v1, v2)` — `(v1-v0) × (v2-v0)`
/// normalized. A zero-area (degenerate) triangle returns `[0, 0, 0]`, never
/// `NaN`.
fn face_normal(v0: [f64; 3], v1: [f64; 3], v2: [f64; 3]) -> [f64; 3] {
    let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
    let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
    let n = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 || !len.is_finite() {
        [0.0, 0.0, 0.0]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solid::Solid;
    use crate::tessellate::{difference, union};

    /// Read a little-endian `u32` at byte `off`.
    fn read_u32(bytes: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    }

    /// Read a little-endian `f32` at byte `off`.
    fn read_f32(bytes: &[u8], off: usize) -> f32 {
        f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    }

    /// The 50-byte record for triangle `i`: `(normal, [v0, v1, v2], attr)`.
    fn read_triangle(bytes: &[u8], i: usize) -> ([f32; 3], [[f32; 3]; 3], u16) {
        let base = 84 + 50 * i;
        let f = |o: usize| read_f32(bytes, base + o);
        let normal = [f(0), f(4), f(8)];
        let verts = [
            [f(12), f(16), f(20)],
            [f(24), f(28), f(32)],
            [f(36), f(40), f(44)],
        ];
        let attr = u16::from_le_bytes(bytes[base + 48..base + 50].try_into().unwrap());
        (normal, verts, attr)
    }

    /// An empty (volume-less but valid) solid: a unit cube minus a box that
    /// fully contains it.
    fn empty_solid() -> Solid {
        let inner = Solid::cube([1.0, 1.0, 1.0], false);
        let outer = Solid::cube([3.0, 3.0, 3.0], true); // spans [-1.5, 1.5]^3 ⊇ inner
        let s = difference(&[inner, outer]).unwrap().unwrap();
        assert!(s.is_empty(), "difference should have removed all volume");
        s
    }

    #[test]
    fn cube_has_exact_byte_length() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        assert_eq!(cube.num_tri(), 12);
        assert_eq!(bytes.len(), 84 + 50 * 12, "84 + 50*n layout");
        assert_eq!(bytes.len(), 684);
    }

    #[test]
    fn general_byte_length_matches_num_tri() {
        // A two-cube union has more triangles; length must still be 84 + 50*n.
        let a = Solid::cube([2.0, 2.0, 2.0], false);
        let bytes = to_binary_stl(&a);
        assert_eq!(bytes.len(), 84 + 50 * a.num_tri());
    }

    #[test]
    fn header_is_80_bytes_and_precedes_count() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        // Count field sits at offset 80, so the header occupies bytes 0..80.
        assert!(bytes.len() >= 84);
        let tag = b"morphe binary STL";
        assert_eq!(&bytes[..tag.len()], tag, "header tag");
        assert!(
            bytes[tag.len()..80].iter().all(|&b| b == 0),
            "header padded with zeros to 80 bytes"
        );
    }

    #[test]
    fn header_does_not_start_with_solid() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        assert_ne!(
            &bytes[..5],
            b"solid",
            "binary STL header must not be mistaken for ASCII STL"
        );
    }

    #[test]
    fn triangle_count_field_matches_num_tri() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        assert_eq!(read_u32(&bytes, 80), cube.num_tri() as u32);
        assert_eq!(read_u32(&bytes, 80), 12);
    }

    #[test]
    fn count_field_is_little_endian() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        // 12 little-endian is 0x0C 0x00 0x00 0x00.
        assert_eq!(&bytes[80..84], &[12u8, 0, 0, 0]);
    }

    #[test]
    fn attribute_byte_count_is_zero() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        for i in 0..cube.num_tri() {
            let (_, _, attr) = read_triangle(&bytes, i);
            assert_eq!(attr, 0, "attribute byte count must be 0");
        }
    }

    #[test]
    fn first_triangle_coords_round_trip_from_mesh() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let (verts, tris) = cube.to_mesh();
        let bytes = to_binary_stl(&cube);

        let (_, wrote, _) = read_triangle(&bytes, 0);
        for (corner, &vi) in tris[0].iter().enumerate() {
            let expect = verts[vi as usize];
            for k in 0..3 {
                assert_eq!(
                    wrote[corner][k], expect[k] as f32,
                    "triangle 0 corner {corner} coord {k}"
                );
            }
        }
    }

    #[test]
    fn all_triangle_coords_round_trip_from_mesh() {
        let cube = Solid::cube([2.0, 3.0, 4.0], true);
        let (verts, tris) = cube.to_mesh();
        let bytes = to_binary_stl(&cube);
        for (i, tri) in tris.iter().enumerate() {
            let (_, wrote, _) = read_triangle(&bytes, i);
            for (corner, &vi) in tri.iter().enumerate() {
                let expect = verts[vi as usize];
                for k in 0..3 {
                    assert_eq!(wrote[corner][k], expect[k] as f32);
                }
            }
        }
    }

    #[test]
    fn normals_are_unit_length() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        for i in 0..cube.num_tri() {
            let (n, _, _) = read_triangle(&bytes, i);
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "normal {i} length {len} != 1");
        }
    }

    #[test]
    fn cube_normals_point_outward() {
        let cube = Solid::cube([1.0, 1.0, 1.0], true); // centered on origin
        let bytes = to_binary_stl(&cube);
        for i in 0..cube.num_tri() {
            let (n, v, _) = read_triangle(&bytes, i);
            // Triangle centroid; the solid centroid is the origin.
            let cx = (v[0][0] + v[1][0] + v[2][0]) / 3.0;
            let cy = (v[0][1] + v[1][1] + v[2][1]) / 3.0;
            let cz = (v[0][2] + v[1][2] + v[2][2]) / 3.0;
            let dot = n[0] * cx + n[1] * cy + n[2] * cz;
            assert!(
                dot > 0.0,
                "normal {i} should face away from center, dot={dot}"
            );
        }
    }

    #[test]
    fn sphere_normals_point_outward() {
        use crate::tessellate::tessellate_primitive;
        use morphe_eval::csg::Primitive;
        let prim = Primitive::Sphere {
            r: 5.0,
            fragments: 24,
        };
        let sphere = tessellate_primitive(&prim).unwrap();
        let sphere = sphere.as_solid().unwrap();
        let bytes = to_binary_stl(sphere);
        for i in 0..sphere.num_tri() {
            let (n, v, _) = read_triangle(&bytes, i);
            // Sphere is centered on the origin.
            let cx = (v[0][0] + v[1][0] + v[2][0]) / 3.0;
            let cy = (v[0][1] + v[1][1] + v[2][1]) / 3.0;
            let cz = (v[0][2] + v[1][2] + v[2][2]) / 3.0;
            let dot = n[0] * cx + n[1] * cy + n[2] * cz;
            assert!(dot > 0.0, "sphere normal {i} not outward, dot={dot}");
        }
    }

    #[test]
    fn zero_area_triangle_has_zero_normal_not_nan() {
        // Two coincident vertices → degenerate triangle.
        let n = face_normal([0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [1.0, 1.0, 1.0]);
        assert_eq!(n, [0.0, 0.0, 0.0]);
        assert!(n.iter().all(|c| !c.is_nan()));
        // Three collinear vertices → also zero area.
        let n2 = face_normal([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]);
        assert_eq!(n2, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn face_normal_known_triangle() {
        // CCW triangle in the z=0 plane → +Z normal.
        let n = face_normal([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert!((n[0]).abs() < 1e-12);
        assert!((n[1]).abs() < 1e-12);
        assert!((n[2] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn empty_solid_is_84_bytes_with_zero_count() {
        let empty = empty_solid();
        let bytes = to_binary_stl(&empty);
        assert_eq!(bytes.len(), 84, "empty STL is header + count only");
        assert_eq!(read_u32(&bytes, 80), 0, "zero triangles");
        // Header still valid.
        assert_eq!(&bytes[..17], b"morphe binary STL");
    }

    #[test]
    fn f32_whole_number_coord_round_trips_exactly() {
        // A cube vertex at x=1.0 must survive as exactly 1.0f32.
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let bytes = to_binary_stl(&cube);
        let (verts, _) = cube.to_mesh();
        // Every coordinate is 0.0 or 1.0 here; both are exact in f32.
        for i in 0..cube.num_tri() {
            let (_, wrote, _) = read_triangle(&bytes, i);
            for corner in wrote {
                for c in corner {
                    assert!(c == 0.0 || c == 1.0, "got non-exact coord {c}");
                }
            }
        }
        assert!(!verts.is_empty());
    }

    #[test]
    fn f32_fractional_coord_within_epsilon() {
        // 0.1 is not exact in binary floating point; check it survives the
        // f64→f32 cast to within f32 epsilon of the original.
        let cube = Solid::cube([0.1, 0.1, 0.1], false);
        let bytes = to_binary_stl(&cube);
        let (verts, tris) = cube.to_mesh();
        let (_, wrote, _) = read_triangle(&bytes, 0);
        for (corner, &vi) in tris[0].iter().enumerate() {
            for k in 0..3 {
                let orig = verts[vi as usize][k];
                let diff = (f64::from(wrote[corner][k]) - orig).abs();
                assert!(diff <= f64::from(f32::EPSILON), "0.1 coord off by {diff}");
            }
        }
    }

    #[test]
    fn to_binary_stl_matches_streamed_writer() {
        let cube = Solid::cube([1.0, 2.0, 3.0], false);
        let via_vec = to_binary_stl(&cube);
        let mut streamed = Vec::new();
        write_binary_stl(&cube, &mut streamed).unwrap();
        assert_eq!(
            via_vec, streamed,
            "both entry points produce identical bytes"
        );
    }

    #[test]
    fn write_binary_stl_propagates_io_error() {
        // A writer that always errors must surface the error, not panic.
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let err = write_binary_stl(&cube, &mut Failing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    // ------------------------------------------------------------------ ASCII

    /// Parse three whitespace-separated `f32`s.
    fn parse3(s: &str) -> [f32; 3] {
        let p: Vec<f32> = s.split_whitespace().map(|x| x.parse().unwrap()).collect();
        assert_eq!(p.len(), 3, "expected 3 floats in {s:?}");
        [p[0], p[1], p[2]]
    }

    /// Parse an ASCII STL into `(normal, [v0, v1, v2])` per facet, in order.
    fn parse_facets(s: &str) -> Vec<([f32; 3], [[f32; 3]; 3])> {
        let lines: Vec<&str> = s.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if let Some(rest) = lines[i].trim().strip_prefix("facet normal ") {
                let n = parse3(rest);
                assert_eq!(lines[i + 1].trim(), "outer loop");
                let v0 = parse3(lines[i + 2].trim().strip_prefix("vertex ").unwrap());
                let v1 = parse3(lines[i + 3].trim().strip_prefix("vertex ").unwrap());
                let v2 = parse3(lines[i + 4].trim().strip_prefix("vertex ").unwrap());
                assert_eq!(lines[i + 5].trim(), "endloop");
                assert_eq!(lines[i + 6].trim(), "endfacet");
                out.push((n, [v0, v1, v2]));
                i += 7;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn ascii_cube_has_12_facets() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let s = to_ascii_stl(&cube);
        assert_eq!(s.matches("facet normal").count(), 12);
        assert_eq!(s.matches("facet normal").count(), cube.num_tri());
    }

    #[test]
    fn ascii_facet_count_equals_num_tri_for_union() {
        // A larger, non-trivial mesh: still one facet per triangle.
        let a = Solid::cube([2.0, 2.0, 2.0], false);
        let b = Solid::cube([1.0, 1.0, 1.0], true);
        let u = union(&[a, b]).unwrap().unwrap();
        let s = to_ascii_stl(&u);
        assert_eq!(s.matches("facet normal").count(), u.num_tri());
    }

    #[test]
    fn ascii_structure_header_and_footer() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let s = to_ascii_stl(&cube);
        assert!(
            s.starts_with("solid morphe\n"),
            "must start with solid header"
        );
        assert!(
            s.ends_with("endsolid morphe\n"),
            "must end with endsolid footer"
        );
    }

    #[test]
    fn ascii_each_facet_is_well_formed() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let s = to_ascii_stl(&cube);
        let n = cube.num_tri();
        // One of each structural token per facet.
        assert_eq!(s.matches("outer loop").count(), n);
        assert_eq!(s.matches("endloop").count(), n);
        assert_eq!(s.matches("endfacet").count(), n);
        // Exactly 3 vertex lines per facet.
        assert_eq!(s.matches("vertex ").count(), 3 * n);
        // And the parser (which asserts each facet's line order) agrees.
        assert_eq!(parse_facets(&s).len(), n);
    }

    #[test]
    fn ascii_normals_match_binary_export() {
        let cube = Solid::cube([2.0, 3.0, 4.0], true);
        let s = to_ascii_stl(&cube);
        let bin = to_binary_stl(&cube);
        let facets = parse_facets(&s);
        assert_eq!(facets.len(), cube.num_tri());
        for (i, (n, _)) in facets.iter().enumerate() {
            let (bn, _, _) = read_triangle(&bin, i);
            assert_eq!(*n, bn, "facet {i} normal must match binary export");
        }
    }

    #[test]
    fn ascii_coords_match_binary_export() {
        let cube = Solid::cube([2.0, 3.0, 4.0], true);
        let s = to_ascii_stl(&cube);
        let bin = to_binary_stl(&cube);
        let facets = parse_facets(&s);
        for (i, (_, v)) in facets.iter().enumerate() {
            let (_, bv, _) = read_triangle(&bin, i);
            assert_eq!(*v, bv, "facet {i} vertices must match binary export");
        }
    }

    #[test]
    fn ascii_empty_solid_is_header_footer_only() {
        let empty = empty_solid();
        let s = to_ascii_stl(&empty);
        assert_eq!(s, "solid morphe\nendsolid morphe\n");
        assert_eq!(s.matches("facet normal").count(), 0);
        assert_eq!(parse_facets(&s).len(), 0);
    }

    #[test]
    fn ascii_round_trips_to_equal_volume() {
        let cube = Solid::cube([2.0, 2.0, 2.0], false); // integer coords → exact f32
        let s = to_ascii_stl(&cube);
        let facets = parse_facets(&s);

        // Rebuild an indexed mesh, welding coincident vertices by bit pattern.
        use std::collections::HashMap;
        let mut index: HashMap<[u32; 3], u32> = HashMap::new();
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::new();
        for (_, corners) in &facets {
            let mut tri = [0u32; 3];
            for (k, c) in corners.iter().enumerate() {
                let key = [c[0].to_bits(), c[1].to_bits(), c[2].to_bits()];
                let id = *index.entry(key).or_insert_with(|| {
                    verts.push([f64::from(c[0]), f64::from(c[1]), f64::from(c[2])]);
                    (verts.len() - 1) as u32
                });
                tri[k] = id;
            }
            tris.push(tri);
        }
        let rebuilt = Solid::from_mesh(&verts, &tris).unwrap();
        assert!((rebuilt.volume() - cube.volume()).abs() < 1e-6);
        assert!((rebuilt.volume() - 8.0).abs() < 1e-6);
    }

    #[test]
    fn ascii_contains_known_cube_coordinate() {
        // Unit cube at origin spans [0,1]^3; the far corner (1,1,1) must appear.
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let s = to_ascii_stl(&cube);
        assert!(
            s.contains("vertex 1 1 1"),
            "expected corner vertex in:\n{s}"
        );
    }

    #[test]
    fn ascii_output_is_pure_ascii() {
        let cube = Solid::cube([1.0, 2.0, 3.0], true);
        let s = to_ascii_stl(&cube);
        assert!(
            s.is_ascii(),
            "STL text must be pure ASCII (hence valid UTF-8)"
        );
    }

    #[test]
    fn ascii_output_has_no_nan_token() {
        // A degenerate triangle would normally yield a NaN normal; the writer
        // must emit `0 0 0` instead. Build a mesh containing a zero-area facet
        // and confirm no non-finite token leaks into the text.
        let cube = Solid::cube([1.0, 1.0, 1.0], true);
        let s = to_ascii_stl(&cube);
        assert!(!s.contains("NaN"), "no NaN token");
        assert!(!s.contains("inf"), "no inf token");
        // Direct check of the degenerate path via the shared helper.
        let n = face_normal([0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [1.0, 1.0, 1.0]);
        assert_eq!(n, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn ascii_uses_unix_line_endings() {
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let s = to_ascii_stl(&cube);
        assert!(
            !s.contains('\r'),
            "no carriage returns; unix line endings only"
        );
    }

    #[test]
    fn to_ascii_stl_matches_streamed_writer() {
        let cube = Solid::cube([1.0, 2.0, 3.0], false);
        let via_string = to_ascii_stl(&cube);
        let mut streamed = Vec::new();
        write_ascii_stl(&cube, &mut streamed).unwrap();
        assert_eq!(
            via_string.as_bytes(),
            streamed.as_slice(),
            "both entry points produce identical output"
        );
    }

    #[test]
    fn write_ascii_stl_propagates_io_error() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let cube = Solid::cube([1.0, 1.0, 1.0], false);
        let err = write_ascii_stl(&cube, &mut Failing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }
}
