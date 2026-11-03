//! STL importer — binary or ASCII `*.stl` bytes into a manifold-checked
//! [`Solid`].
//!
//! Binary-vs-ASCII sniffing is deliberately arithmetic-only: a foreign binary
//! file may legally begin with the ASCII `solid`
//! marker (our own exporter writes a text header, too), so the header string
//! is never trusted. Binary iff `len >= 84 && (len - 84) % 50 == 0` AND the
//! little-endian triangle-count at offset 80 equals `(len - 84) / 50`.
//! Anything else is parsed as ASCII facet records.
//!
//! Both paths funnel through [`Solid::from_mesh`], so an open/degenerate
//! mesh fails loudly as `invalid solid: ...` instead of importing.
//!
//! Errors always carry the file path; a bad record also reports its byte
//! offset, so a data problem greps back to a file position.

use std::path::Path;
use std::str::FromStr;

use crate::solid::Solid;
use crate::tessellate::KernelError;

/// An indexed triangle mesh: vertex positions + triangle index triples.
type IndexMesh = (Vec<[f64; 3]>, Vec<[u32; 3]>);

/// Hard cap on imported STL size (an uncapped `fs::read` of `/dev/zero` or a
/// 10 GB "stl" is an OOM). STLs are geometry dumps; anything
/// past 512 MB is not a usable part.
pub const MAX_STL_BYTES: u64 = 512 * 1024 * 1024;

/// Load `path` as a binary or ASCII STL and validate it manifold-closed.
pub fn load_stl(path: &Path) -> Result<Solid, KernelError> {
    // The shared, unit-tested size cap (same helper as the .dat side — one
    // enforcement pattern, one test).
    // One open, regular-file-only, bounded read (no stat-then-reopen TOCTOU).
    let bytes = crate::read_capped(path, "import", MAX_STL_BYTES)?;
    let (verts, tris) = parse_stl(path, &bytes)?;
    Solid::from_mesh(&verts, &tris).map_err(|e| {
        KernelError::Resolve(format!(
            "`import` mesh in `{}` rejected as non-manifold: {e}",
            path.display()
        ))
    })
}

/// Route `bytes` to the binary or ASCII decoder (module doc: never trust
/// the header string).
fn parse_stl(path: &Path, bytes: &[u8]) -> Result<IndexMesh, KernelError> {
    let parsed = if is_binary_stl(bytes) {
        parse_binary(path, bytes)?
    } else {
        parse_ascii(
            path,
            std::str::from_utf8(bytes).map_err(|e| {
                KernelError::Resolve(format!(
                    "`import` `{}`: not utf-8 ascii: {e}",
                    path.display()
                ))
            })?,
        )?
    };
    Ok(dedupe(parsed.0, parsed.1))
}

/// Re-index triangles over a vertex set deduped on exact coordinate bits
/// (+0/−0 normalized). STL text and foreign exporters repeat positions per
/// triangle; [`Solid::from_mesh`]'s validation needs shared indices (a probe
/// caught the 'merges internally' doc failing on duplicated vertices).
pub(crate) fn dedupe(verts: Vec<[f64; 3]>, tris: Vec<[u32; 3]>) -> IndexMesh {
    use std::collections::HashMap;
    let mut out = Vec::<[f64; 3]>::new();
    let mut map = HashMap::<[u64; 3], u32>::new();
    let mut remap = |v: [f64; 3]| -> u32 {
        let norm = [norm0(v[0]), norm0(v[1]), norm0(v[2])];
        let key = [norm[0].to_bits(), norm[1].to_bits(), norm[2].to_bits()];
        if let Some(&i) = map.get(&key) {
            i
        } else {
            let i = out.len() as u32;
            out.push(norm);
            map.insert(key, i);
            i
        }
    };
    let tris = tris
        .into_iter()
        .map(|t| {
            [
                remap(verts[t[0] as usize]),
                remap(verts[t[1] as usize]),
                remap(verts[t[2] as usize]),
            ]
        })
        .collect::<Vec<[u32; 3]>>();
    (out, tris)
}

/// Normalize −0.0 to +0.0 so the hash sees them coincident.
fn norm0(x: f64) -> f64 {
    if x == 0.0 {
        0.0
    } else {
        x
    }
}

/// Binary iff the byte-layout arithmetic is self-consistent (module doc).
fn is_binary_stl(bytes: &[u8]) -> bool {
    bytes.len() >= 84
        && (bytes.len() - 84).is_multiple_of(50)
        && u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize
            == (bytes.len() - 84) / 50
}

/// Decode a binary STL: 84-byte header then packed 50-byte records of
/// `[normal, a, b, c, attr]` little-endian f32/u16.
fn parse_binary(path: &Path, bytes: &[u8]) -> Result<IndexMesh, KernelError> {
    let n = (bytes.len() - 84) / 50;
    let mut verts = Vec::with_capacity(n * 3);
    let mut tris = Vec::with_capacity(n);
    for i in 0..n {
        let base = 84 + i * 50 + 12; // skip the normal
        let base_idx = verts.len() as u32;
        for k in 0..3_u32 {
            let at = base + 12 * (k as usize);
            let x = f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            let y =
                f32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
            let z =
                f32::from_le_bytes([bytes[at + 8], bytes[at + 9], bytes[at + 10], bytes[at + 11]]);
            if !(x.is_finite() && y.is_finite() && z.is_finite()) {
                return Err(err_at(path, at + 4 * (k as usize), "non-finite vertex"));
            }
            verts.push([x as f64, y as f64, z as f64]);
        }
        tris.push([base_idx, base_idx + 1, base_idx + 2]);
    }
    Ok((verts, tris))
}

/// Parse ASCII STL: whitespace tokenizer (CRLF-tolerant), keywords matched
/// case-insensitively; the file must open with `solid` and every facet must
/// carry `normal` + 3-number / `outer loop` / three `vertex` lines /
/// `endloop` / `endfacet`; ends at `endsolid`. Errors name the path and the
/// byte offset of the offending token.
fn parse_ascii(path: &Path, text: &str) -> Result<IndexMesh, KernelError> {
    let mut toks = tokenize(text);
    match toks.next() {
        Some((t, _)) if t.eq_ignore_ascii_case("solid") => {}
        Some((t, off)) => {
            let _ = &t;
            return Err(err_at(path, off, "not an STL — expected a `solid` header"));
        }
        None => {
            return Err(err_at(path, 0, "not an STL — empty file"));
        }
    }
    let (mut verts, mut tris) = (Vec::new(), Vec::new());
    // Tokens between the `solid` keyword and the first `facet` are the
    // caller's name (skipped, upstream-compatible); inside facet land we
    // stay strict (junk errors at its offset).
    let mut in_facets = false;
    while let Some((t, t_off)) = toks.next() {
        if t.eq_ignore_ascii_case("endsolid") {
            return Ok((verts, tris));
        }
        if !t.eq_ignore_ascii_case("facet") {
            if in_facets {
                return Err(err_at(path, t_off, "expected `facet` or `endsolid`"));
            }
            continue; // name-zone noise (upstream tolerates it)
        }
        in_facets = true;
        // `normal` + 3 numbers — the recorded hint is ignored (as upstream).
        let (nt, nt_off) = toks
            .next()
            .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
        if !nt.eq_ignore_ascii_case("normal") {
            return Err(err_at(path, nt_off, "expected `normal`"));
        }
        for _ in 0..3 {
            let (n, n_off) = toks
                .next()
                .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
            if f64::from_str(&n).is_err() {
                return Err(err_at(path, n_off, "not a number"));
            }
        }
        let (outer, o_off) = toks
            .next()
            .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
        if !outer.eq_ignore_ascii_case("outer") {
            return Err(err_at(path, o_off, "expected `outer`"));
        }
        let (lp, l_off) = toks
            .next()
            .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
        if !lp.eq_ignore_ascii_case("loop") {
            return Err(err_at(path, l_off, "expected `loop`"));
        }
        let base = verts.len() as u32;
        for _ in 0..3_u32 {
            let (v_t, v_off) = toks
                .next()
                .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
            if !v_t.eq_ignore_ascii_case("vertex") {
                return Err(err_at(path, v_off, "expected `vertex`"));
            }
            let mut v = [0.0_f64; 3];
            for slot in &mut v {
                let (n, n_off) = toks
                    .next()
                    .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
                *slot = f64::from_str(&n).map_err(|_| err_at(path, n_off, "not a number"))?;
                if !slot.is_finite() {
                    return Err(err_at(path, n_off, "non-finite vertex"));
                }
            }
            verts.push(v);
        }
        let (el, el_off) = toks
            .next()
            .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
        if !el.eq_ignore_ascii_case("endloop") {
            return Err(err_at(path, el_off, "expected `endloop`"));
        }
        let (ef, ef_off) = toks
            .next()
            .ok_or_else(|| err(path, "`endsolid` missing (truncated)"))?;
        if !ef.eq_ignore_ascii_case("endfacet") {
            return Err(err_at(path, ef_off, "expected `endfacet`"));
        }
        tris.push([base, base + 1, base + 2]);
    }
    Err(err(path, "`endsolid` missing (file truncated)"))
}

/// A `KernelError::Resolve` that names only the path (no offset).
fn err(path: &Path, why: &str) -> KernelError {
    KernelError::Resolve(format!("`import` `{}` — {why}", path.display()))
}

/// A `KernelError::Resolve` naming the path plus the byte offset of the
/// offending record/token.
fn err_at(path: &Path, off: usize, why: &str) -> KernelError {
    KernelError::Resolve(format!(
        "`import` `{}` at byte offset {off}: {why}",
        path.display()
    ))
}

/// Zero-allocation tokenizer yielding each whitespace-delimited token with
/// its byte offset in the file (STL files are small; bail early per bad token).
fn tokenize(text: &str) -> TokIter<'_> {
    TokIter { text, at: 0 }
}

struct TokIter<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Iterator for TokIter<'a> {
    type Item = (String, usize);
    fn next(&mut self) -> Option<(String, usize)> {
        let bytes = self.text.as_bytes();
        let mut start = self.at;
        // Whitespace is ASCII-only. The Latin-1 cast
        // made UTF-8 continuation bytes (NEL 0x85 / NBSP 0xA0) look like
        // whitespace, putting a token boundary mid-character → PANIC on
        // `text[start..end]`. `is_ascii_whitespace` means a boundary can only
        // ever land on an ASCII byte (always a char boundary); non-ASCII
        // bytes are absorbed into tokens.
        // Skip leading ASCII whitespace.
        while start < bytes.len() && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        if start >= bytes.len() {
            self.at = bytes.len();
            return None;
        }
        let mut end = start + 1;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        self.at = end;
        let token = self.text[start..end].to_string();
        Some((token, start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let mut at = std::env::temp_dir();
        at.push(format!("morphe-import-test-{}-{name}", std::process::id()));
        std::fs::write(&at, bytes).expect("fixture write");
        at
    }

    fn binary_fixture(vertices: &[[f32; 3]], tris: &[[u32; 3]], header: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = header.to_vec();
        out.resize(80, b' ');
        let n = tris.len() as u32;
        out.extend_from_slice(&n.to_le_bytes());
        for tri in tris {
            for _ in 0..3 {
                out.extend_from_slice(&0f32.to_le_bytes());
            }
            for idx in tri {
                let v = &vertices[*idx as usize];
                for c in v {
                    out.extend_from_slice(&(*c).to_le_bytes());
                }
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out
    }

    #[test]
    fn dbg_duplicated_vertex_mesh_checks() {
        // Straight f64 to_mesh data re-imported — should be manifold.
        let solid = Solid::cube([2.0, 3.0, 4.0], false);
        let (verts, tris) = solid.to_mesh();
        let back = Solid::from_mesh(&verts, &tris).unwrap();
        assert!((back.volume() - solid.volume()).abs() < 1e-9);

        // Probe finding that motivated `dedupe`: duplicated vertex copies
        // (per-triangle) are NOT merged internally despite the `from_mesh`
        // doc claim — the loader dedupes before validation.
        let mut dv: Vec<[f64; 3]> = Vec::new();
        let mut dt: Vec<[u32; 3]> = Vec::new();
        for tri in &tris {
            let base = dv.len() as u32;
            for idx in tri {
                dv.push(verts[*idx as usize]);
            }
            dt.push([base, base + 1, base + 2]);
        }
        assert!(
            Solid::from_mesh(&dv, &dt).is_err(),
            "probe: duplicated-vertex meshes are rejected unless deduped"
        );
    }

    #[test]
    fn import_binary_round_trips_own_export() {
        let solid = Solid::cube([2.0, 3.0, 4.0], false);
        let (verts, tris) = solid.to_mesh();
        let verts32: Vec<[f32; 3]> = verts
            .iter()
            .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32])
            .collect();
        let bytes = binary_fixture(&verts32, &tris, b"round trip via our own exporter");
        let path = write_tmp("roundtrip.stl", &bytes);
        let loaded = load_stl(&path).expect("round trip parses");
        let _ = std::fs::remove_file(&path);
        assert!(
            (loaded.volume() - solid.volume()).abs() < 1e-9,
            "volume survives the round trip: got {} vs {}",
            loaded.volume(),
            solid.volume()
        );
    }

    #[test]
    fn import_ascii_parses_handwritten_facets() {
        // A closed 1-tetra ASCII solid must import with volume 1/6.
        let ascii = "solid tetr\n\
            facet normal 0 0 -1\n  outer loop\n   vertex 0 0 0\n   vertex 1 1 0\n   vertex 1 0 0\n  endloop\nendfacet\n\
            facet normal 0 -1 0\n  outer loop\n   vertex 0 0 0\n   vertex 0 0 1\n   vertex 1 1 0\n  endloop\nendfacet\n\
            facet normal -1 0 0\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 0 1\n endloop endfacet\n\
            facet normal 1 1 1\n  outer loop\n   vertex 1 1 0\n   vertex 0 0 1\n   vertex 1 0 0\n endloop endfacet\n\
            endsolid tetr\n";
        let path = write_tmp("hand-ascii.stl", ascii.as_bytes());
        let loaded = load_stl(&path).expect("hand-written tetra parses");
        let _ = std::fs::remove_file(&path);
        assert!(
            (loaded.volume() - 1.0 / 6.0).abs() < 1e-9,
            "tetra volume is 1/6: got {}",
            loaded.volume()
        );
    }

    #[test]
    fn import_sniffs_binary_despite_solid_header() {
        // Adversarial binary header starting with `solid`: only the
        // size-consistency sniff decides.
        let bytes = binary_fixture(
            &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            &[[0, 1, 2]],
            b"solid sneaky",
        );
        let path = write_tmp("sniff.stl", &bytes);
        assert!(is_binary_stl(&bytes), "sniff must choose binary");
        let loaded = parse_stl(&path, &bytes).expect("binary branch parses");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.1.len(), 1);
    }

    #[test]
    fn import_truncated_binary_errors_naming_offset() {
        // Header claims 2 triangles; only 1 present is fine arithmetic — but
        // a record with a NAN coordinate errors at a byte offset.
        let mut bytes: Vec<u8> = b"truncated".to_vec();
        bytes.resize(80, 0);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        for _ in 0..3 {
            bytes.extend_from_slice(&0f32.to_le_bytes());
        }
        // vertex a
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        // vertex b: second component NAN
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&f32::NAN.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        // vertex c
        bytes.extend_from_slice(&1f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        let path = write_tmp("bad-offset.stl", &bytes);
        let parsed = parse_stl(&path, &bytes);
        let _ = std::fs::remove_file(&path);
        let err = parsed.expect_err("NAN vertex reported");
        let msg = err.to_string();
        assert!(msg.contains("byte offset"), "must name an offset: {msg}");
        assert!(msg.contains("bad-offset.stl"), "must name the path: {msg}");
    }

    #[test]
    fn import_non_ascii_name_zone_never_panics() {
        // Regression (originally reproduced through the CLI): the tokenizer sliced UTF-8 at byte offsets with Latin-1 whitespace
        // semantics, so an accented name zone ("café") put a token boundary
        // mid-character and PANICKED. `is_ascii_whitespace` makes every
        // boundary an ASCII byte (a valid char boundary); non-ASCII bytes
        // ride inside tokens. The accented name is skipped as name-zone
        // noise; the tetra still parses.
        let ascii = "solid caf\u{e9}\n\
            facet normal 0 0 -1\n  outer loop\n   vertex 0 0 0\n   vertex 1 1 0\n   vertex 1 0 0\n  endloop\nendfacet\n\
            facet normal 0 -1 0\n  outer loop\n   vertex 0 0 0\n   vertex 0 0 1\n   vertex 1 1 0\n  endloop\nendfacet\n\
            facet normal -1 0 0\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 0 1\n  endloop\nendfacet\n\
            facet normal 1 1 1\n  outer loop\n   vertex 1 1 0\n   vertex 0 0 1\n   vertex 1 0 0\n  endloop\nendfacet\n\
            endsolid caf\u{e9}\n";
        let path = write_tmp("accented.stl", ascii.as_bytes());
        let loaded = load_stl(&path);
        let _ = std::fs::remove_file(&path);
        let solid = loaded.expect("accented name zone must parse, not panic");
        assert!((solid.volume() - 1.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn import_non_ascii_mid_structure_errors_not_panics() {
        // The adversarial sibling: a multibyte char glued to a KEYWORD
        // position ("fàcet") — absorbed into one token, so it fails the
        // keyword match with a LOUD error naming the offset. Never a panic.
        // The corrupted keyword sits AFTER a valid first facet (in-facet land is
        // strict; before any facet it would be name-zone noise \u2014 tolerated,
        // upstream-compatible).
        let ascii = "solid x\nfacet normal 0 0 -1\n  outer loop\n   vertex 0 0 0\n   vertex 1 1 0\n   vertex 1 0 0\n  endloop\nendfacet\nf\u{e0}cet normal 0 0 0\nendsolid x\n";
        let path = write_tmp("midstruct.stl", ascii.as_bytes());
        let loaded = load_stl(&path);
        let _ = std::fs::remove_file(&path);
        let err = match loaded {
            Ok(_) => panic!("a corrupted keyword must error"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("midstruct.stl"), "names the file: {msg}");
        assert!(msg.contains("byte offset"), "names the offset: {msg}");
        // Token CONTENT must never reach the error (the compile
        // result crosses into the AI/provider channel) — offset only.
        assert!(
            !msg.contains("f\u{e0}cet"),
            "errors must not echo file content: {msg}"
        );
    }

    #[test]
    fn import_nonmanifold_errors_loudly() {
        // One triangle is not a closed manifold; the wrapper names the file.
        let bytes = binary_fixture(
            &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            &[[0, 1, 2]],
            b"open",
        );
        let path = write_tmp("nonmanifold.stl", &bytes);
        let loaded = load_stl(&path);
        let _ = std::fs::remove_file(&path);
        let err = match loaded {
            Ok(_) => panic!("open sheet must fail manifold validation"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("nonmanifold.stl"), "must name the file: {msg}");
        assert!(msg.contains("non-manifold"), "must say non-manifold: {msg}");
    }
}
