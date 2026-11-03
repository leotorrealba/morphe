//! `.dat` heightfield surfaces — `surface(file=…)` → a [`Solid`], ported from
//! upstream OpenSCAD `SurfaceNode::createGeometry` (2021.01 `src/surface.cc`,
//! the conformance oracle's vintage; the code path is byte-identical in
//! current master).
//!
//! **Note on upstream behavior:** a natural reading of `surface()` is "two
//! triangles per cell … z=0 closure". Upstream actually does NEITHER:
//!
//! * **Interior is FOUR triangles per cell**, each from one cell edge to a
//!   center vertex at `vx = (v1+v2+v3+v4)/4` (the average of the four corner
//!   heights). A side effect that simplifies conformance: the volume under a
//!   4-tri cell's top surface is EXACTLY the corner average (independent of
//!   any triangulation direction), which is what the analytic ramp test pins.
//! * **The bottom closes at `min(0, min_cell - 1)`, not z=0.** Both upstream
//!   versions agree: 2021.01 seeds `min_val = 0` and folds `min(v - 1)`;
//!   master seeds `min(v)` and subtracts 1 — `min(0, min_v - 1)` either way.
//!   So an all-positive heightfield with min ≥ 1 closes at z=0 (the common
//!   case), but any cell below 1 pulls the floor negative.
//!
//! **Intentional divergence (invert):** upstream applies `invert` ONLY to the
//! PNG pixel path; the `.dat` path ignores it. morphe pins
//! `surface_invert_flips_signs` for the `.dat` path (the sane, tested
//! behavior), so morphe negates heights on `.dat` too and the corpus
//! `surface_centered_invert.scad` is expected to diverge from a 2021.01
//! oracle on that file specifically.
//!
//! Layout: `grid[row][col]` = z, `row = y`, `col = x` (upstream
//! `data[x + y*width]`). `center` shifts by `-(columns-1)/2, -(lines-1)/2`
//! over 1×1 cells (upstream `ox`/`oy`). Errors are loud: ragged rows,
//! non-numeric cells, and NaN are rejected (upstream carves holes for NaN —
//! documented divergence, tracked out of scope).

use crate::solid::Solid;
use crate::tessellate::KernelError;

/// Parse a `.dat` heightfield: whitespace-separated numbers, one row per line
/// (`row = y`, `col = x`), `#` comment lines and blank lines skipped, CRLF
/// tolerant. Returns `grid[row][col]`. Ragged rows (different column counts),
/// non-numeric cells, and NaN cells are loud errors (module doc: upstream
/// carves holes for NaN; morphe rejects).
pub fn parse_dat(text: &str) -> Result<Vec<Vec<f64>>, KernelError> {
    let mut grid: Vec<Vec<f64>> = Vec::new();
    let mut columns = 0usize;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut row: Vec<f64> = Vec::new();
        for tok in line.split_whitespace() {
            let v: f64 = tok.parse().map_err(|_| {
                // No cell CONTENT in errors (same rule as the STL importer)
                // — the message reaches the AI channel; line+column suffice.
                KernelError::Resolve(format!(
                    "surface: non-numeric cell at line {} column {}",
                    lineno + 1,
                    row.len() + 1
                ))
            })?;
            if !v.is_finite() {
                return Err(KernelError::Resolve(format!(
                    "surface: NaN/inf cell at line {} column {} (upstream carves holes; morphe rejects — documented divergence)",
                    lineno + 1,
                    row.len() + 1
                )));
            }
            row.push(v);
        }
        if grid.is_empty() {
            columns = row.len();
        } else if row.len() != columns {
            return Err(KernelError::Resolve(format!(
                "surface: ragged row at line {} ({} cells, expected {columns})",
                lineno + 1,
                row.len()
            )));
        }
        grid.push(row);
    }
    if grid.is_empty() || columns == 0 {
        return Err(KernelError::Resolve(
            "surface: empty heightfield (no numeric rows)".to_string(),
        ));
    }
    Ok(grid)
}

/// Hard cap on `.dat` heightfield size (an uncapped read is the same OOM
/// class the STL cap closes).
pub const MAX_DAT_BYTES: u64 = 64 * 1024 * 1024;

/// Hard cap on the number of heightfield cells (`rows × columns` data
/// points) [`surface_solid`] will mesh — a morphe hardening with no upstream
/// analogue.
///
/// The byte cap alone does not bound the OUTPUT: a 64 MB `.dat` of `0 `
/// tokens is ~5800×5800 cells → ~134M triangles (~11 GB of mesh). Each cell
/// emits four interior triangles (twelve unshared vertices), so `4_000_000`
/// cells (e.g. 2000×2000) caps the mesh at ~16M triangles (~1.5 GB) — far
/// beyond any practical heightmap, checked before any mesh buffer exists.
pub const MAX_SURFACE_CELLS: usize = 4_000_000;

/// Build the heightfield solid from `grid[row][col]` (upstream
/// `SurfaceNode::createGeometry`). `center` recenters the field on the
/// origin; `invert` negates the heights (morphe applies it to `.dat` too —
/// see the module-doc divergence note). An empty grid, or one with more
/// than [`MAX_SURFACE_CELLS`] cells, is a loud [`KernelError::Resolve`].
pub fn surface_solid(grid: &[Vec<f64>], center: bool, invert: bool) -> Result<Solid, KernelError> {
    let lines = grid.len();
    let columns = grid.first().map_or(0, Vec::len);
    if lines == 0 || columns == 0 {
        return Err(KernelError::Resolve(
            "surface: empty heightfield (no numeric rows)".to_string(),
        ));
    }
    if grid.iter().any(|row| row.len() != columns) {
        return Err(KernelError::Resolve(
            "surface: ragged heightfield (rows differ in length)".to_string(),
        ));
    }
    let cells = lines.saturating_mul(columns);
    if cells > MAX_SURFACE_CELLS {
        return Err(KernelError::Resolve(format!(
            "surface: heightfield is {lines}×{columns} = {cells} cells (cap {MAX_SURFACE_CELLS})"
        )));
    }

    // Resolve heights (invert negates) and the bottom floor in one pass.
    // Bottom = min(0, min_cell - 1) — upstream 2021.01 folds min(v-1) into a
    // 0-seeded accumulator; master subtracts 1 from a min-seeded one. Same.
    let mut min_val = 0.0_f64;
    let mut h = vec![vec![0.0_f64; columns]; lines];
    for (i, row) in grid.iter().enumerate() {
        for (j, &v) in row.iter().enumerate() {
            let z = if invert { -v } else { v };
            h[i][j] = z;
            min_val = min_val.min(z - 1.0);
        }
    }

    let ox = if center {
        -(columns as f64 - 1.0) / 2.0
    } else {
        0.0
    };
    let oy = if center {
        -(lines as f64 - 1.0) / 2.0
    } else {
        0.0
    };

    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut push = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
        let base = verts.len() as u32;
        verts.push(a);
        verts.push(b);
        verts.push(c);
        tris.push([base, base + 1, base + 2]);
    };

    // ── Interior: 4 triangles per cell to the center vertex (upstream). ────
    // v1=(x,y) v2=(x+1,y) v3=(x,y+1) v4=(x+1,y+1); center at the corner avg.
    for i in 1..lines {
        for j in 1..columns {
            let (v1, v2, v3, v4) = (h[i - 1][j - 1], h[i - 1][j], h[i][j - 1], h[i][j]);
            let vx = (v1 + v2 + v3 + v4) / 4.0;
            let c = [ox + j as f64 - 0.5, oy + i as f64 - 0.5, vx];
            let p1 = [ox + j as f64 - 1.0, oy + i as f64 - 1.0, v1];
            let p2 = [ox + j as f64, oy + i as f64 - 1.0, v2];
            let p3 = [ox + j as f64 - 1.0, oy + i as f64, v3];
            let p4 = [ox + j as f64, oy + i as f64, v4];
            // CCW-from-+z (outward): bottom edge, right edge, top edge, left.
            push(p1, p2, c);
            push(p2, p4, c);
            push(p4, p3, c);
            push(p3, p1, c);
        }
    }

    // ── Border walls: quads down to the floor, two triangles each. ─────────
    // Left (x=0) and right (x=columns-1) walls, walking +y.
    for i in 1..lines {
        let z0 = h[i - 1][0];
        let z1 = h[i][0];
        let y0 = oy + (i - 1) as f64;
        let y1 = oy + i as f64;
        // left wall faces -x (outward): floor0 -> top0 -> top1 -> floor1
        let f0 = [ox + 0.0, y0, min_val];
        let f1 = [ox + 0.0, y1, min_val];
        let t0 = [ox + 0.0, y0, z0];
        let t1 = [ox + 0.0, y1, z1];
        push(f0, t0, t1);
        push(f0, t1, f1);

        let xr = ox + (columns - 1) as f64;
        let z0 = h[i - 1][columns - 1];
        let z1 = h[i][columns - 1];
        // right wall faces +x (outward): floor1 -> top1 -> top0 -> floor0
        let f0 = [xr, y0, min_val];
        let f1 = [xr, y1, min_val];
        let t0 = [xr, y0, z0];
        let t1 = [xr, y1, z1];
        push(f1, t1, t0);
        push(f1, t0, f0);
    }
    // Front (y=0) and back (y=lines-1) walls, walking +x.
    for j in 1..columns {
        let x0 = ox + (j - 1) as f64;
        let x1 = ox + j as f64;
        let z0 = h[0][j - 1];
        let z1 = h[0][j];
        // front wall faces -y (outward): floor1 -> top1 -> top0 -> floor0
        let f0 = [x0, oy + 0.0, min_val];
        let f1 = [x1, oy + 0.0, min_val];
        let t0 = [x0, oy + 0.0, z0];
        let t1 = [x1, oy + 0.0, z1];
        push(f1, t1, t0);
        push(f1, t0, f0);

        let yb = oy + (lines - 1) as f64;
        let z0 = h[lines - 1][j - 1];
        let z1 = h[lines - 1][j];
        // back wall faces +y (outward): floor0 -> top0 -> top1 -> floor1
        let f0 = [x0, yb, min_val];
        let f1 = [x1, yb, min_val];
        let t0 = [x0, yb, z0];
        let t1 = [x1, yb, z1];
        push(f0, t0, t1);
        push(f0, t1, f1);
    }

    // ── Bottom: one n-gon at the floor, triangulated as a fan facing -z. ────
    // Perimeter order (upstream): +y up the left, +x across the back, -y down
    // the right, -x across the front — CCW seen from BELOW (outward -z).
    if columns > 1 && lines > 1 {
        let mut ring: Vec<[f64; 3]> = Vec::new();
        for i in 0..lines - 1 {
            ring.push([ox + 0.0, oy + i as f64, min_val]);
        }
        for j in 0..columns - 1 {
            ring.push([ox + j as f64, oy + (lines - 1) as f64, min_val]);
        }
        for i in (1..lines).rev() {
            ring.push([ox + (columns - 1) as f64, oy + i as f64, min_val]);
        }
        for j in (1..columns).rev() {
            ring.push([ox + j as f64, oy + 0.0, min_val]);
        }
        // Fan from the ring CENTROID — a fan from ring[0] makes degenerate
        // collinear triangles along the two edges through ring[0] (zero-area
        // faces are rejected as non-manifold). The centroid is strictly inside
        // (the perimeter ring is convex), so every fan triangle has area.
        let n = ring.len();
        let mut cx = 0.0_f64;
        let mut cy = 0.0_f64;
        for p in &ring {
            cx += p[0];
            cy += p[1];
        }
        cx /= n as f64;
        cy /= n as f64;
        let center = [cx, cy, min_val];
        for k in 0..n {
            let b = ring[k];
            let c = ring[(k + 1) % n];
            push(center, b, c);
        }
    }

    let (verts, tris) = crate::import::dedupe(verts, tris);
    Solid::from_mesh(&verts, &tris).map_err(|e| {
        KernelError::Resolve(format!(
            "surface: heightfield mesh rejected as non-manifold: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_flat_grid_of_h_is_box_of_volume_h_times_cells() {
        // Uniform h → exact prism. 3x3 grid of 2.0 spans (3-1)x(3-1)=4
        // cells (an n×m grid → (n-1)×(m-1) interpolated cells), 2×2
        // footprint, height 2 → volume 8. Bottom closes at min(0, 2-1)=0, so
        // NO negative slab — the box sits on z=0.
        let grid = vec![vec![2.0; 3]; 3];
        let s = surface_solid(&grid, false, false).expect("flat grid resolves");
        assert!(
            (s.volume() - 8.0).abs() < 1e-9,
            "flat 3x3 of 2.0 → volume 8 (4 cells × 1×1 × 2), got {}",
            s.volume()
        );
        // bbox min z is 0 (bottom rule: min>=1 closes at 0, never negative).
        let (_, _, stats) = {
            let (v, t) = s.to_mesh();
            let mut minz = f64::INFINITY;
            for p in &v {
                minz = minz.min(p[2]);
            }
            let _ = t;
            (0, 0, minz)
        };
        assert_eq!(stats, 0.0, "flat grid bottom sits at z=0, got {stats}");
    }

    #[test]
    fn surface_linear_ramp_volume_exact() {
        // Heights [[0,0,0],[0,1,2],[0,2,4]] — an exactly computable volume
        // pinning interior triangulation + closure. Under the TRUE upstream
        // rule (4-tri center vertex + bottom=min(0,min-1)):
        //   top (z from 0 to surface): per-cell volume = corner average
        //     cells: (0+0+0+1)/4=0.25, (0+0+1+2)/4=0.75,
        //            (0+1+0+2)/4=0.75, (1+2+2+4)/4=2.25 → sum = 4.0
        //   bottom: min cell = 0 → floor at min(0, 0-1) = -1 → slab z∈[-1,0]
        //     over the full 2×2 footprint = 4.0
        //   total = 4.0 + 4.0 = 8.0.
        let grid = vec![
            vec![0.0, 0.0, 0.0],
            vec![0.0, 1.0, 2.0],
            vec![0.0, 2.0, 4.0],
        ];
        let s = surface_solid(&grid, false, false).expect("ramp resolves");
        assert!(
            (s.volume() - 8.0).abs() < 1e-9,
            "linear ramp → exactly 8.0 (top 4.0 avg-corners + slab 4.0), got {}",
            s.volume()
        );
    }

    #[test]
    fn surface_negative_heights_extend_below_zero() {
        // A grid with a negative value pulls the floor negative (bottom =
        // min(0, min-1)) AND the heightfield dips: bbox min z < 0.
        let grid = vec![
            vec![1.0, 1.0],
            vec![1.0, -2.0], // min cell -2 → floor at min(0, -3) = -3
        ];
        let s = surface_solid(&grid, false, false).expect("negative grid resolves");
        let (v, _) = s.to_mesh();
        let minz = v.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
        assert!(
            minz < 0.0,
            "negative height extends below zero, minz={minz}"
        );
        assert_eq!(minz, -3.0, "floor at min(0, -2 - 1) = -3, got {minz}");
    }

    #[test]
    fn surface_center_recenters_origin() {
        // 3x3 → 2×2 footprint normally [0,2]×[0,2]; centered → [-1,1]×[-1,1].
        let grid = vec![vec![1.0; 3]; 3];
        let s = surface_solid(&grid, true, false).expect("centered resolves");
        let (v, _) = s.to_mesh();
        let (mut minx, mut miny) = (f64::INFINITY, f64::INFINITY);
        let (mut maxx, mut maxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in &v {
            minx = minx.min(p[0]);
            maxx = maxx.max(p[0]);
            miny = miny.min(p[1]);
            maxy = maxy.max(p[1]);
        }
        assert_eq!(minx, -1.0, "centered min x = -(columns-1)/2 = -1");
        assert_eq!(maxx, 1.0);
        assert_eq!(miny, -1.0, "centered min y = -(lines-1)/2 = -1");
        assert_eq!(maxy, 1.0);
    }

    #[test]
    fn surface_invert_flips_signs() {
        // invert negates heights (morphe applies it to .dat; upstream is
        // PNG-only — documented divergence). A grid of 2.0 inverted is -2.0;
        // the top surface's max z is 0 (it sits below z=0 after negation, but
        // the closure floor is at min(0,-2-1)=-3). Just assert the sign flip:
        // every heightfield (non-floor) vertex is at -2.0.
        let grid = vec![vec![2.0; 2]; 2];
        let pos = surface_solid(&grid, false, false).expect("pos");
        let neg = surface_solid(&grid, false, true).expect("neg");
        // Volumes: pos = 1×1×2 = 2 (floor 0); neg spans z=-3..-2 → 1×1×1 = 1.
        assert!(
            (pos.volume() - 2.0).abs() < 1e-9,
            "pos vol 2, got {}",
            pos.volume()
        );
        assert!(
            (neg.volume() - 1.0).abs() < 1e-9,
            "neg vol 1, got {}",
            neg.volume()
        );
        let (nv, _) = neg.to_mesh();
        let maxz = nv.iter().map(|p| p[2]).fold(f64::NEG_INFINITY, f64::max);
        assert_eq!(maxz, -2.0, "inverted surface top at -2.0, got {maxz}");
    }

    #[test]
    fn surface_ragged_row_errors() {
        let err = parse_dat("1 2 3\n4 5\n").expect_err("ragged rows rejected");
        assert!(err.to_string().contains("ragged"), "{err}");
    }

    #[test]
    fn surface_nan_cell_errors() {
        // Upstream carves holes for NaN; morphe rejects (documented divergence).
        let err = parse_dat("1 2\n3 nan\n").expect_err("NaN rejected");
        assert!(
            err.to_string().contains("NaN") || err.to_string().contains("2, 1"),
            "{err}"
        );
        // Cell content must NOT be echoed into the error.
        assert!(!err.to_string().contains("nan cell `nan`"), "{err}");
        let err = parse_dat("1 2\n3 inf\n").expect_err("inf rejected");
        assert!(err.to_string().contains("inf"), "{err}");
    }

    #[test]
    fn read_capped_rejects_oversize_and_allows_within() {
        // THE cap-enforcement pin \u2014 a declared cap with no test is
        // worthless.
        let dir = std::env::temp_dir().join(format!("morphe-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("h.dat");
        std::fs::write(&path, b"0123456789").expect("write 10 bytes");

        // Over the (test-sized) cap: loud error naming path + cap.
        let err = crate::read_capped(&path, "surface", 5).expect_err("oversize rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("11") || msg.contains("10"),
            "names the size: {msg}"
        );
        assert!(msg.contains("cap 5"), "names the cap: {msg}");
        assert!(msg.contains("h.dat"), "names the path: {msg}");
        assert!(!msg.contains("0123"), "never echoes content: {msg}");

        // Exactly at and within the cap: Ok, returns the bytes.
        assert_eq!(
            crate::read_capped(&path, "surface", 10).expect("at cap"),
            b"0123456789"
        );
        assert_eq!(
            crate::read_capped(&path, "surface", 100).expect("within cap"),
            b"0123456789"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn read_capped_rejects_devices_and_directories() {
        // `/dev/zero` reports len 0 and passed the old stat check, then read
        // until OOM; a directory (and a FIFO) must likewise be refused
        // BEFORE any read.
        for p in ["/dev/zero", "/"] {
            let err = crate::read_capped(std::path::Path::new(p), "surface", MAX_DAT_BYTES)
                .expect_err("non-regular rejected");
            assert!(err.to_string().contains("not a regular file"), "{err}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_capped_rejects_fifo_without_hanging() {
        let dir = std::env::temp_dir().join(format!(
            "morphe-fifo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let fifo = dir.join("pipe.dat");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            let err =
                crate::read_capped(&fifo, "surface", MAX_DAT_BYTES).expect_err("FIFO rejected");
            assert!(err.to_string().contains("not a regular file"), "{err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn surface_cell_cap_rejects_before_meshing() {
        // 64 MB of zeros parsed to ~5800x5800 cells -> ~134M triangles; the
        // cell cap must refuse before any mesh buffer is built.
        let side = 2001; // 2001^2 > MAX_SURFACE_CELLS
        let grid = vec![vec![0.0_f64; side]; side];
        let Err(err) = surface_solid(&grid, false, false) else {
            panic!("over cell cap must be rejected")
        };
        let msg = err.to_string();
        assert!(msg.contains("cap 4000000"), "{msg}");
        // Empty / ragged direct input is an error, not an index panic.
        assert!(surface_solid(&[], false, false).is_err());
        assert!(surface_solid(&[vec![1.0, 2.0], vec![3.0]], false, false).is_err());
    }

    #[test]
    fn stl_and_dat_caps_are_enforced_constants() {
        // The .dat cap is enforced via the TESTED helper
        // (read_capped_rejects_oversize_and_allows_within); the STL
        // cap goes through the same `read_capped` in load_stl.
        // The ordering below documents the relationship between the two.
        let dat = MAX_DAT_BYTES;
        let stl = crate::import::MAX_STL_BYTES;
        assert!(dat > 0 && stl > dat, "dat {dat} must sit under stl {stl}");
    }

    #[test]
    fn parse_dat_handles_crlf_and_comments() {
        let grid = parse_dat("# heightmap\r\n1 2\r\n3 4\r\n").expect("crlf+comment parse");
        assert_eq!(grid, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }
}
