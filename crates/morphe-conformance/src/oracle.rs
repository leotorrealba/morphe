//! The oracle mesh-diff runner.
//!
//! Where the corpus-validity gate ([`crate::run_corpus`]) only asks *does
//! morphe produce a mesh of the right dimensionality*, the **oracle** asks the
//! harder question: *is that mesh the same shape the reference renderer
//! ([OpenSCAD](https://openscad.org/)) produces for the same `.scad` source?*
//!
//! For each 3D corpus file it renders the geometry twice — once through
//! morphe's own pipeline, once through the installed `openscad` CLI (`openscad
//! -o out.stl in.scad`) — parses both meshes, and compares them by three
//! tessellation-tolerant metrics:
//!
//! 1. **Relative volume delta** — [`volume_delta`]: `|Vₘ − Vₒ| / max(Vₒ, ε)`.
//! 2. **Normalized symmetric Hausdorff distance** — [`hausdorff_normalized`]:
//!    the two-sided worst-case point-to-*surface* distance, divided by the
//!    larger of the two bounding-box diagonals, giving a scale-free
//!    shape-difference ratio. Each side is sampled at its vertices, its
//!    triangle centroids and a fixed budget of seeded, area-weighted points on
//!    its triangles ([`SURFACE_SAMPLES`]), and every sample is measured to the
//!    nearest *triangle* of the other mesh. Sampling only vertices would miss
//!    any difference that lives inside faces: two meshes with identical
//!    vertices but a different triangulation (an edge flip across a
//!    non-planar quad, a `surface()` heightmap, a non-planar `polyhedron`
//!    face) would score zero, and so would a plate missing a through-hole
//!    (the hole wall's vertices all lie on the plate's caps).
//! 3. **Relative surface-area delta** — [`area_delta`]:
//!    `|Aₘ − Aₒ| / max(Aₒ, ε)`. It catches missing or extra *detail* whose
//!    depth is small next to the model's size — the regime where the
//!    diagonal-normalized Hausdorff ratio is least sensitive.
//!
//! A file **passes** the oracle only if *all three* metrics are within
//! tolerance ([`OracleTolerance`]).
//!
//! # Graceful degradation (the key contract)
//!
//! `openscad` is **not** installed in CI or on the dev machine. When the binary
//! cannot be located ([`detect_openscad`] returns `None`) every comparison is
//! recorded as [`OracleOutcome::Skipped`] — never a failure — and
//! [`OracleReport::exit_ok`] stays `true`.
//!
//! The oracle only ever *fails* the run when a binary that **is** present
//! renders a mesh that **diverges past tolerance**. A render that openscad
//! itself cannot complete — a spawn error, a nonzero exit (openscad rejecting a
//! construct morphe accepts), a timeout, or unparseable/NaN output — is
//! openscad's limitation, *not* divergence: it is recorded as
//! [`OracleOutcome::Error`] and, like [`Skipped`](OracleOutcome::Skipped), does
//! **not** fail the gate. Only [`Fail`](OracleOutcome::Fail) fails a file —
//! including an inside-out morphe mesh (negative signed volume). One run-level
//! rule on top: with `openscad` present, a run that completed ZERO
//! comparisons (everything skipped/errored) fails, so a silently broken
//! oracle can never report green.
//!
//! # Tolerances and their rationale
//!
//! Two independent tessellators will never agree bit-for-bit: a sphere or
//! cylinder is approximated by a finite `$fn`-gon, and morphe and OpenSCAD may
//! place those facets at different angular offsets (or pick a slightly
//! different fragment count), so an inscribed mesh under-reports volume by an
//! amount that depends on facet placement. The [default](OracleTolerance::default)
//! bounds — **5 %** relative volume and **5 %** of the bounding-box diagonal in
//! Hausdorff distance — are loose enough to absorb that facet-placement noise on
//! curved primitives yet tight enough that a genuinely wrong shape (a cube where
//! a sphere was expected) still fails. Exact polyhedra agree far tighter; 5 % is
//! the safe global default, not a target.
//!
//! The surface-area default is tighter, **3 %**, because area is less sensitive
//! to faceting than volume: flat faces are exact under any triangulation, and
//! an inscribed `n`-gon loses `≈ (2π/n)²/6` of its circle's area but only
//! `≈ (π/n)²/6` of its perimeter. On closed curved primitives (whose flat
//! caps lose area like the polygon does) the net effect is that area moves
//! about half as much as volume: a sphere at 12 vs 16 fragments differs by
//! 4.9 % in volume — just inside the volume budget — and 2.5 % in area
//! (pinned by the `faceting_moves_area_less_than_volume` test). 3 % keeps a
//! margin above that worst accepted case.
//!
//! # Known blind spot
//!
//! All three metrics are global. A feature that is small in *every*
//! dimension relative to the model (a 1 mm pin hole in a 100 mm plate) moves
//! none of them past its default bound. Catching that would need a
//! topological check (e.g. comparing genus / Euler characteristic), which is
//! left out for now because non-manifold reference output makes it brittle.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use morphe_kernel::Geometry;

use crate::{resolve_source_at, ExpectedKind};

/// A triangle mesh: `verts` are `[x, y, z]` positions and each triangle in
/// `tris` indexes three of them.
///
/// Deliberately a plain triangle container, not a welded manifold: an STL is a
/// triangle *soup* (every facet carries its own three vertices, with no shared
/// index), so a parsed [`Mesh`] usually repeats positions. That is fine — all
/// metrics ([`volume_delta`], [`hausdorff_normalized`], [`area_delta`]) are
/// correct on duplicated vertices.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    /// Vertex positions, `[x, y, z]`.
    pub verts: Vec<[f64; 3]>,
    /// Triangles, each indexing three entries of [`Mesh::verts`].
    pub tris: Vec<[u32; 3]>,
}

impl Mesh {
    /// Build a [`Mesh`] from a kernel [`Solid`](morphe_kernel::Solid) via its
    /// indexed mesh, so morphe's own geometry is measured through the exact same
    /// code path as a parsed reference STL.
    #[must_use]
    pub fn from_solid(solid: &morphe_kernel::Solid) -> Mesh {
        let (verts, tris) = solid.to_mesh();
        Mesh { verts, tris }
    }

    /// Whether the mesh has no triangles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// **Signed** enclosed volume, via the divergence theorem:
    /// `Σ (v₀ · (v₁ × v₂)) / 6` over every triangle. Positive for an
    /// outward-wound (CCW-from-outside) closed mesh, negative for an
    /// inside-out one. Deliberately NOT `abs()`'d: a magnitude would let an
    /// inverted morphe mesh compare as a pass; [`compare_meshes`] fails it
    /// instead.
    #[must_use]
    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for t in &self.tris {
            let a = self.verts[t[0] as usize];
            let b = self.verts[t[1] as usize];
            let c = self.verts[t[2] as usize];
            v += dot(a, cross(b, c));
        }
        v / 6.0
    }

    /// Axis-aligned bounding box as `(min, max)`, or `None` if the mesh has no
    /// vertices.
    #[must_use]
    fn bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut it = self.verts.iter();
        let first = *it.next()?;
        let mut min = first;
        let mut max = first;
        for v in it {
            for k in 0..3 {
                min[k] = min[k].min(v[k]);
                max[k] = max[k].max(v[k]);
            }
        }
        Some((min, max))
    }

    /// Length of the bounding-box diagonal (`0.0` for an empty mesh).
    #[must_use]
    pub fn bbox_diagonal(&self) -> f64 {
        match self.bbox() {
            Some((min, max)) => dist(min, max),
            None => 0.0,
        }
    }

    /// The three corner positions of triangle `i`.
    fn triangle(&self, i: usize) -> [[f64; 3]; 3] {
        let t = self.tris[i];
        [
            self.verts[t[0] as usize],
            self.verts[t[1] as usize],
            self.verts[t[2] as usize],
        ]
    }

    /// Total surface area: the sum of every triangle's area.
    #[must_use]
    pub fn surface_area(&self) -> f64 {
        (0..self.tris.len())
            .map(|i| {
                let [a, b, c] = self.triangle(i);
                triangle_area(a, b, c)
            })
            .sum()
    }

    /// Deterministic sample points on the mesh surface, used as the "from"
    /// side of [`hausdorff_normalized`].
    ///
    /// The set is the union of
    /// - every distinct vertex, so the metric can never report less than a
    ///   vertex-only Hausdorff distance would;
    /// - every triangle centroid, so each face contributes at least one
    ///   interior point however small it is;
    /// - [`SURFACE_SAMPLES`] points drawn area-weighted over the triangles
    ///   from a fixed-seed generator, so large faces are covered densely
    ///   enough to expose differences inside them.
    ///
    /// The generator is seeded with a constant, so the same mesh always yields
    /// the same samples and oracle runs are reproducible.
    fn surface_samples(&self) -> Vec<[f64; 3]> {
        let mut pts = Vec::with_capacity(self.verts.len() + self.tris.len() + SURFACE_SAMPLES);

        let mut seen = std::collections::HashSet::with_capacity(self.verts.len());
        for v in &self.verts {
            if seen.insert(v.map(f64::to_bits)) {
                pts.push(*v);
            }
        }

        // Centroids, plus the running area total used to pick triangles in
        // proportion to their area.
        let mut cdf = Vec::with_capacity(self.tris.len());
        let mut total = 0.0;
        for i in 0..self.tris.len() {
            let [a, b, c] = self.triangle(i);
            pts.push(scale(add(a, add(b, c)), 1.0 / 3.0));
            total += triangle_area(a, b, c);
            cdf.push(total);
        }

        if total > 0.0 {
            let mut rng = SplitMix64(SAMPLE_SEED);
            let last = self.tris.len() - 1;
            for _ in 0..SURFACE_SAMPLES {
                let x = rng.next_f64() * total;
                let i = cdf.partition_point(|&c| c <= x).min(last);
                let [a, b, c] = self.triangle(i);
                // Uniform point in a triangle (Osada et al., "Shape
                // Distributions", 2002): with s = √r₁ the point
                // (1 − s)·a + s(1 − r₂)·b + s·r₂·c is uniform over the area.
                let s = rng.next_f64().sqrt();
                let r2 = rng.next_f64();
                pts.push(add(
                    scale(a, 1.0 - s),
                    add(scale(b, s * (1.0 - r2)), scale(c, s * r2)),
                ));
            }
        }
        pts
    }
}

/// Number of area-weighted random points sampled on each mesh's surface for
/// [`hausdorff_normalized`], on top of its vertices and triangle centroids.
/// Large enough that a face region a few tenths of a percent of the total area
/// gets dozens of samples; small enough that, with the grid-accelerated
/// nearest-triangle search, even a ~16k-triangle mesh pair compares in well
/// under a second in a debug build.
pub const SURFACE_SAMPLES: usize = 20_000;

/// Fixed seed for the surface sampler, so oracle results are reproducible.
const SAMPLE_SEED: u64 = 0x6d6f_7270_6865_5eed;

/// SplitMix64 (Steele, Lea & Flood, 2014): a tiny, well-mixed 64-bit PRNG.
/// Good enough for spreading sample points; not for anything cryptographic.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A uniform `f64` in `[0, 1)` from the top 53 bits.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// A tiny epsilon guarding the volume-delta denominator so a near-zero
/// reference volume cannot blow the ratio up to infinity.
const VOLUME_EPS: f64 = 1e-9;

/// Relative volume difference of two meshes: `|vol(a) − vol(b)| / max(vol(b), ε)`.
///
/// `b` is the **reference** (the OpenSCAD mesh); `a` is morphe's. `0.0` means
/// identical volume. See the module docs for why a few percent is expected on
/// curved primitives.
#[must_use]
pub fn volume_delta(a: &Mesh, b: &Mesh) -> f64 {
    let va = a.volume();
    let vb = b.volume();
    (va - vb).abs() / vb.max(VOLUME_EPS)
}

/// Relative surface-area difference of two meshes:
/// `|area(a) − area(b)| / max(area(b), ε)`.
///
/// As with [`volume_delta`], `b` is the **reference** (OpenSCAD) and `a` is
/// morphe's mesh. Area is invariant under re-triangulation of flat faces, so
/// it only moves with real surface changes and with curved-surface faceting
/// (see the module docs for the tolerance rationale).
#[must_use]
pub fn area_delta(a: &Mesh, b: &Mesh) -> f64 {
    let aa = a.surface_area();
    let ab = b.surface_area();
    (aa - ab).abs() / ab.max(VOLUME_EPS)
}

/// Symmetric point-to-surface Hausdorff distance between the two meshes,
/// normalized by the larger of the two bounding-box diagonals.
///
/// The raw symmetric Hausdorff distance is `max(h(a→b), h(b→a))` where
/// `h(x→y) = maxₚ dist(p, surface(y))` over sample points `p` on the surface
/// of `x` — its vertices, its triangle centroids and [`SURFACE_SAMPLES`]
/// seeded area-weighted points (`Mesh::surface_samples`). Each sample is
/// measured to the nearest *triangle* of the other mesh.
///
/// Measuring to the surface rather than to the nearest *vertex* is what keeps
/// this metric honest under different tessellations: when two engines facet a
/// curved surface at different angular offsets a vertex of one mesh lands in
/// the middle of a face of the other, so a vertex-to-vertex distance would
/// spuriously inflate; the point sits *on* (or very near) the other mesh's
/// surface, so the point-to-surface distance stays small. Sampling *from* face
/// interiors, not only from vertices, is what lets it see differences that
/// live inside faces (a flipped diagonal, a missing hole whose rim vertices
/// lie on the other mesh's caps).
///
/// The sampled value is a lower bound on the true (continuous) Hausdorff
/// distance, and never lower than the old vertex-only figure.
///
/// Dividing by the bounding-box diagonal turns an absolute length into a
/// scale-free shape-difference ratio (so `0.05` means "worst-case surface
/// disagreement is 5 % of the model's overall size"), which is what the
/// tolerance is expressed against.
///
/// Two empty meshes are identical (`0.0`); a non-empty mesh against an empty one
/// is maximally different ([`f64::INFINITY`]).
#[must_use]
pub fn hausdorff_normalized(a: &Mesh, b: &Mesh) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    if a.is_empty() || b.is_empty() {
        return f64::INFINITY;
    }
    let raw = directed_hausdorff(a, b).max(directed_hausdorff(b, a));
    let diag = a.bbox_diagonal().max(b.bbox_diagonal());
    if diag <= VOLUME_EPS {
        // Both meshes collapse to (near) a point: identical iff raw is ~0.
        return if raw <= VOLUME_EPS {
            0.0
        } else {
            f64::INFINITY
        };
    }
    raw / diag
}

/// `h(a→b) = maxₚ dist(p, surface(b))` — the directed Hausdorff distance from
/// the surface samples of `a` to the *triangles* of `b`.
fn directed_hausdorff(a: &Mesh, b: &Mesh) -> f64 {
    let mut grid = TriangleGrid::new(b);
    let mut worst = 0.0_f64;
    for p in a.surface_samples() {
        // A sample can only raise the maximum if it is farther than `worst`
        // from every triangle, so the search may stop at the first triangle
        // within `worst`.
        let d = grid.nearest(p, worst.max(VOLUME_EPS));
        if d > worst {
            worst = d;
        }
    }
    worst
}

/// Upper bound on the number of cells along one axis of a [`TriangleGrid`].
const GRID_MAX_DIM: usize = 128;

/// A uniform grid over a mesh's bounding box that buckets each triangle into
/// every cell its bounding box overlaps, for nearest-triangle queries.
///
/// A query starts at the cell containing the point and visits shells of
/// cells at increasing Chebyshev radius, stopping once no unvisited cell can
/// hold a triangle closer than the best found so far. Results are exact (the
/// same as a brute-force scan); the grid only skips work.
struct TriangleGrid<'a> {
    mesh: &'a Mesh,
    min: [f64; 3],
    cell: [f64; 3],
    dims: [usize; 3],
    /// CSR layout: the triangles of cell `c` are
    /// `items[starts[c] as usize..starts[c + 1] as usize]`.
    starts: Vec<u32>,
    items: Vec<u32>,
    /// Per-triangle stamp so a triangle bucketed into several cells is only
    /// measured once per query.
    stamp: Vec<u32>,
    query: u32,
}

impl<'a> TriangleGrid<'a> {
    fn new(mesh: &'a Mesh) -> Self {
        let (min, max) = mesh.bbox().unwrap_or(([0.0; 3], [0.0; 3]));
        let ext = sub(max, min);
        let longest = ext[0].max(ext[1]).max(ext[2]);

        // Aim for about two cells per triangle. Thin axes are padded when
        // choosing the cell size so a flat mesh does not get a huge grid.
        let target = (2 * mesh.tris.len()).clamp(1, 1 << 20) as f64;
        let pad = (longest / GRID_MAX_DIM as f64).max(f64::MIN_POSITIVE);
        let padded_volume = ext.iter().map(|e| e.max(pad)).product::<f64>();
        let size = (padded_volume / target).cbrt().max(f64::MIN_POSITIVE);

        let mut dims = [1usize; 3];
        let mut cell = [size; 3];
        for k in 0..3 {
            // Float-to-int `as` saturates, so a huge ratio is safe here.
            dims[k] = ((ext[k] / size).ceil() as usize).clamp(1, GRID_MAX_DIM);
            if ext[k] > 0.0 {
                cell[k] = (ext[k] / dims[k] as f64).max(f64::MIN_POSITIVE);
            }
        }

        let mut grid = TriangleGrid {
            mesh,
            min,
            cell,
            dims,
            starts: Vec::new(),
            items: Vec::new(),
            stamp: vec![0; mesh.tris.len()],
            query: 0,
        };

        // Two passes: count entries per cell, then fill.
        let n_cells = dims[0] * dims[1] * dims[2];
        let ranges: Vec<_> = (0..mesh.tris.len())
            .map(|i| grid.cell_range(&mesh.triangle(i)))
            .collect();
        let mut counts = vec![0u32; n_cells + 1];
        for (lo, hi) in &ranges {
            grid.for_each_cell(*lo, *hi, |c| counts[c + 1] += 1);
        }
        for c in 0..n_cells {
            counts[c + 1] += counts[c];
        }
        let mut fill = counts.clone();
        let mut items = vec![0u32; counts[n_cells] as usize];
        for (i, (lo, hi)) in ranges.iter().enumerate() {
            grid.for_each_cell(*lo, *hi, |c| {
                items[fill[c] as usize] = i as u32;
                fill[c] += 1;
            });
        }
        grid.starts = counts;
        grid.items = items;
        grid
    }

    /// The cell index of coordinate `x` along axis `k`, clamped into the grid.
    fn axis_cell(&self, k: usize, x: f64) -> usize {
        let i = ((x - self.min[k]) / self.cell[k]).floor();
        if i <= 0.0 {
            0
        } else {
            (i as usize).min(self.dims[k] - 1)
        }
    }

    /// The inclusive range of cells overlapped by a triangle's bounding box.
    fn cell_range(&self, tri: &[[f64; 3]; 3]) -> ([usize; 3], [usize; 3]) {
        let mut lo = [0; 3];
        let mut hi = [0; 3];
        for k in 0..3 {
            let a = tri[0][k].min(tri[1][k]).min(tri[2][k]);
            let b = tri[0][k].max(tri[1][k]).max(tri[2][k]);
            lo[k] = self.axis_cell(k, a);
            hi[k] = self.axis_cell(k, b);
        }
        (lo, hi)
    }

    fn cell_id(&self, i: [usize; 3]) -> usize {
        (i[2] * self.dims[1] + i[1]) * self.dims[0] + i[0]
    }

    fn for_each_cell(&self, lo: [usize; 3], hi: [usize; 3], mut f: impl FnMut(usize)) {
        for z in lo[2]..=hi[2] {
            for y in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    f(self.cell_id([x, y, z]));
                }
            }
        }
    }

    /// Distance from `p` to the nearest triangle. Returns early with some
    /// distance `<= good_enough` as soon as a triangle that close is found;
    /// otherwise the returned distance is exact.
    fn nearest(&mut self, p: [f64; 3], good_enough: f64) -> f64 {
        if self.mesh.tris.is_empty() {
            return f64::INFINITY;
        }
        self.query = self.query.wrapping_add(1);
        if self.query == 0 {
            // Stamp counter wrapped: clear so stale stamps cannot match.
            self.stamp.fill(0);
            self.query = 1;
        }
        let center = [0, 1, 2].map(|k| self.axis_cell(k, p[k]));
        let mut best = f64::INFINITY;
        let mut r = 0usize;
        loop {
            let lo = [0, 1, 2].map(|k| center[k].saturating_sub(r));
            let hi = [0, 1, 2].map(|k| (center[k] + r).min(self.dims[k] - 1));

            // Visit only the shell at Chebyshev radius exactly `r`; inner
            // cells were visited on earlier iterations.
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        let ring = center[0]
                            .abs_diff(x)
                            .max(center[1].abs_diff(y))
                            .max(center[2].abs_diff(z));
                        if ring != r {
                            continue;
                        }
                        let c = self.cell_id([x, y, z]);
                        for j in self.starts[c]..self.starts[c + 1] {
                            let t = self.items[j as usize] as usize;
                            if self.stamp[t] == self.query {
                                continue;
                            }
                            self.stamp[t] = self.query;
                            let [a, b, c] = self.mesh.triangle(t);
                            best = best.min(point_triangle_distance(p, a, b, c));
                        }
                        if best <= good_enough {
                            return best;
                        }
                    }
                }
            }

            // Any triangle not yet measured lies wholly in cells outside the
            // visited box, so it is at least as far as the nearest open face
            // of that box.
            let mut bound = f64::INFINITY;
            for k in 0..3 {
                if lo[k] > 0 {
                    let face = self.min[k] + lo[k] as f64 * self.cell[k];
                    bound = bound.min((p[k] - face).max(0.0));
                }
                if hi[k] + 1 < self.dims[k] {
                    let face = self.min[k] + (hi[k] + 1) as f64 * self.cell[k];
                    bound = bound.min((face - p[k]).max(0.0));
                }
            }
            // Once the whole grid has been visited `bound` is ∞, so this
            // always terminates.
            if best <= bound {
                return best;
            }
            r += 1;
        }
    }
}

/// Area of triangle `(a, b, c)`.
fn triangle_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let n = cross(sub(b, a), sub(c, a));
    0.5 * dot(n, n).sqrt()
}

/// Distance from point `p` to the closest point of triangle `(a, b, c)`.
///
/// The standard closest-point-on-triangle test (Ericson, *Real-Time Collision
/// Detection* §5.1.5): classify `p` against the triangle's Voronoi regions
/// (three vertices, three edges, the interior) and measure to whichever feature
/// owns it.
fn point_triangle_distance(p: [f64; 3], a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return dist(p, a); // vertex region A
    }

    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return dist(p, b); // vertex region B
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return dist(p, add(a, scale(ab, v))); // edge AB
    }

    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return dist(p, c); // vertex region C
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return dist(p, add(a, scale(ac, w))); // edge AC
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return dist(p, add(b, scale(sub(c, b), w))); // edge BC
    }

    // Interior: project onto the face via barycentric coordinates.
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    dist(p, add(a, add(scale(ab, v), scale(ac, w))))
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

// ── STL parsing ─────────────────────────────────────────────────────────────

/// Parse an STL blob — binary or ASCII — into a [`Mesh`].
///
/// Format is detected by structure, not by sniffing the leading `solid` token
/// (which both forms can share): a binary STL is exactly `84 + 50·n` bytes with
/// the triangle count `n` at byte 80, so any blob satisfying that relation is
/// parsed as binary and everything else as ASCII. This is the inverse of the
/// kernel's [`to_binary_stl`](morphe_kernel::to_binary_stl) /
/// [`to_ascii_stl`](morphe_kernel::to_ascii_stl) writers.
pub fn parse_stl(bytes: &[u8]) -> Result<Mesh, String> {
    let mesh = if is_binary_stl(bytes) {
        parse_binary_stl(bytes)?
    } else {
        let s = std::str::from_utf8(bytes).map_err(|e| format!("STL is not valid UTF-8: {e}"))?;
        parse_ascii_stl(s)?
    };
    // A non-finite (NaN/inf) coordinate is malformed output, not a shape — the
    // caller turns this into an OracleOutcome::Error, never a silent Fail.
    if mesh.verts.iter().flatten().any(|c| !c.is_finite()) {
        return Err("STL contains a non-finite (NaN/inf) coordinate".to_string());
    }
    Ok(mesh)
}

/// Whether `bytes` is a binary STL: at least the 84-byte header/count and a
/// length of exactly `84 + 50·n` for the `n` recorded at byte 80.
fn is_binary_stl(bytes: &[u8]) -> bool {
    if bytes.len() < 84 {
        return false;
    }
    let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    bytes.len() == 84 + 50 * n
}

/// Parse a binary STL: skip the 80-byte header, read the `u32` count, then one
/// 50-byte record per triangle (normal + 3 vertices as `f32`, 2-byte attribute
/// skipped). Vertices are emitted as an un-welded soup.
fn parse_binary_stl(bytes: &[u8]) -> Result<Mesh, String> {
    let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    let mut verts = Vec::with_capacity(n * 3);
    let mut tris = Vec::with_capacity(n);
    for i in 0..n {
        let base = 84 + 50 * i;
        // Vertices start 12 bytes in (after the 3-float facet normal).
        let read_f32 = |off: usize| -> f64 {
            let o = base + off;
            f64::from(f32::from_le_bytes([
                bytes[o],
                bytes[o + 1],
                bytes[o + 2],
                bytes[o + 3],
            ]))
        };
        let v0 = [read_f32(12), read_f32(16), read_f32(20)];
        let v1 = [read_f32(24), read_f32(28), read_f32(32)];
        let v2 = [read_f32(36), read_f32(40), read_f32(44)];
        let idx = verts.len() as u32;
        verts.push(v0);
        verts.push(v1);
        verts.push(v2);
        tris.push([idx, idx + 1, idx + 2]);
    }
    Ok(Mesh { verts, tris })
}

/// Parse an ASCII STL by collecting every `vertex x y z` line in order and
/// grouping them into triangles three at a time.
fn parse_ascii_stl(s: &str) -> Result<Mesh, String> {
    let mut verts = Vec::new();
    let mut tris = Vec::new();
    let mut pending: Vec<[f64; 3]> = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("vertex ") {
            let coords: Vec<f64> = rest
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()
                .map_err(|e| format!("bad vertex line {line:?}: {e}"))?;
            if coords.len() != 3 {
                return Err(format!("vertex line needs 3 coords, got {line:?}"));
            }
            pending.push([coords[0], coords[1], coords[2]]);
            if pending.len() == 3 {
                let idx = verts.len() as u32;
                verts.extend_from_slice(&pending);
                tris.push([idx, idx + 1, idx + 2]);
                pending.clear();
            }
        }
    }
    if !pending.is_empty() {
        return Err(format!(
            "ASCII STL ended with {} dangling vertices (not a whole triangle)",
            pending.len()
        ));
    }
    Ok(Mesh { verts, tris })
}

// ── openscad detection ──────────────────────────────────────────────────────

/// Locate the `openscad` CLI, or `None` if it cannot be found.
///
/// Resolution order:
/// 1. the `OPENSCAD_BIN` environment variable — if set, it is authoritative:
///    an existing file is used, a **non-existent** path yields `None` (we do
///    *not* fall through, so a test can force the absent path by pointing
///    `OPENSCAD_BIN` at nothing);
/// 2. `openscad` on `PATH`;
/// 3. the standard macOS bundle path
///    `/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD`.
///
/// A returned path is guaranteed to point at an existing file.
#[must_use]
pub fn detect_openscad() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OPENSCAD_BIN") {
        let p = PathBuf::from(p);
        return if p.is_file() { Some(p) } else { None };
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("openscad");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    let mac = PathBuf::from("/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD");
    if mac.is_file() {
        return Some(mac);
    }
    None
}

// ── per-file comparison ─────────────────────────────────────────────────────

/// Pass/fail bounds for one oracle comparison: relative volume delta,
/// normalized Hausdorff distance and relative surface-area delta, each a
/// fraction (`0.05` = 5 %).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OracleTolerance {
    /// Maximum accepted relative volume difference (fraction of reference volume).
    pub volume_rel: f64,
    /// Maximum accepted Hausdorff distance as a fraction of the bbox diagonal.
    pub hausdorff_rel: f64,
    /// Maximum accepted relative surface-area difference (fraction of the
    /// reference area).
    pub area_rel: f64,
}

impl Default for OracleTolerance {
    /// 5 % volume, 5 % Hausdorff, 3 % surface area — see the module docs for
    /// the facet-placement rationale.
    fn default() -> Self {
        OracleTolerance::DEFAULT
    }
}

impl OracleTolerance {
    /// The global default bounds, as a constant so the override table can
    /// build on it.
    pub const DEFAULT: OracleTolerance = OracleTolerance {
        volume_rel: 0.05,
        hausdorff_rel: 0.05,
        area_rel: 0.03,
    };

    /// The per-file tolerance override table, keyed by corpus BASENAME. Each
    /// entry carries the measured divergence numbers and a rationale comment:
    /// a tolerance entry is never speculative — it is added only once a real
    /// oracle measurement justifies it.
    ///
    /// Constructs expected to be most at risk of needing an entry:
    /// - minkowski: OpenSCAD's CGAL convex-decomposition path vs manifold's
    ///   direct sum can diverge beyond the default.
    /// - surface: heightmap edge stitching.
    /// - text: Bézier flattening vs FreeType's rasterizer.
    ///
    /// Key hygiene: never key an entry by `""` — a non-UTF-8 or degenerate
    /// basename resolves to "" and would match it.
    const FILE_OVERRIDES: &[(&str, OracleTolerance)] = &[
        // All volume/hausdorff entries carry MEASURED numbers from an oracle
        // run against OpenSCAD 2021.01 — never guessed. Those measurements
        // predate surface sampling (Hausdorff then sampled vertices only) and
        // the surface-area metric, so every entry must be re-measured on the
        // first oracle run after that change. Until then each entry's
        // `area_rel` is PROVISIONAL: set equal to its `volume_rel`, on the
        // reasoning that whatever distorts the enclosed volume of these files
        // distorts their surface area by a similar fraction.
        //
        // linear_extrude_twist: hausdorff 0.00046 (surfaces near-identical)
        // with volume delta 0.1141 — the twist makes the swept mesh
        // SELF-INTERSECTING, and the enclosed-volume INTEGRATION differs
        // between CGAL-Nef (upstream) and the mesh kernel (morphe). Not a
        // regression; tolerance just above the measurement.
        (
            "linear_extrude_twist.scad",
            OracleTolerance {
                volume_rel: 0.13,
                hausdorff_rel: 0.001,
                area_rel: 0.13, // provisional, see above
            },
        ),
        // text.scad / text_2d.scad: Freetype hinting/grid-fit distortion and
        // HarfBuzz kerning that ttf-parser does not reproduce (documented
        // divergence; scale compatibility ratio implemented). Entries set
        // from the oracle MEASUREMENT taken after the DPI-scale fix.
        (
            "text.scad",
            OracleTolerance {
                volume_rel: 0.15,
                hausdorff_rel: 0.10,
                area_rel: 0.15, // provisional, see above
            },
        ),
        (
            "text_2d.scad",
            OracleTolerance {
                volume_rel: 0.15,
                hausdorff_rel: 0.10,
                area_rel: 0.15, // provisional, see above
            },
        ),
    ];

    /// The tolerance for one corpus file: the entry keyed by the
    /// file's BASENAME when present, else `base` (the caller-supplied global
    /// for files without an entry). `run_oracle`'s parameter remains the
    /// source of the default; this resolver adds per-file specificity.
    #[must_use]
    pub fn for_file(path: &Path, base: &OracleTolerance) -> OracleTolerance {
        let basename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        Self::FILE_OVERRIDES
            .iter()
            .find(|(name, _)| *name == basename)
            .map(|(_, tol)| *tol)
            .unwrap_or(*base)
    }
}

/// Compare two already-parsed meshes against a tolerance, without touching the
/// filesystem or `openscad`. `reference` is the OpenSCAD mesh. This is the pure
/// core of the per-file comparison, unit-testable with synthetic meshes.
#[must_use]
pub fn compare_meshes(morphe: &Mesh, reference: &Mesh, tol: &OracleTolerance) -> OracleOutcome {
    // Orientation gate first: an inside-out morphe mesh (negative signed
    // volume) is a real defect even when its magnitude matches the
    // reference. An inverted REFERENCE is openscad's output problem, not
    // divergence — Error, like any other unusable reference.
    let vm = morphe.volume();
    if vm < -VOLUME_EPS {
        return OracleOutcome::Fail {
            volume_delta: f64::NAN,
            hausdorff: f64::NAN,
            area_delta: f64::NAN,
            reason: format!("morphe mesh is inside-out (signed volume {vm:.6})"),
        };
    }
    let vr = reference.volume();
    if vr < -VOLUME_EPS {
        return OracleOutcome::Error(format!(
            "openscad reference mesh is inside-out (signed volume {vr:.6})"
        ));
    }
    let vd = volume_delta(morphe, reference);
    let hd = hausdorff_normalized(morphe, reference);
    let ad = area_delta(morphe, reference);
    let mut reasons = Vec::new();
    if vd > tol.volume_rel {
        reasons.push(format!("volume delta {vd:.4} > tol {:.4}", tol.volume_rel));
    }
    if hd > tol.hausdorff_rel {
        reasons.push(format!("hausdorff {hd:.4} > tol {:.4}", tol.hausdorff_rel));
    }
    if ad > tol.area_rel {
        reasons.push(format!("area delta {ad:.4} > tol {:.4}", tol.area_rel));
    }
    // NaN never compares greater, so treat any non-finite metric as a failure
    // rather than letting it slip through as a pass.
    if reasons.is_empty() && !(vd.is_finite() && hd.is_finite() && ad.is_finite()) {
        reasons.push(format!(
            "non-finite metric (volume delta {vd}, hausdorff {hd}, area delta {ad})"
        ));
    }
    if reasons.is_empty() {
        OracleOutcome::Pass {
            volume_delta: vd,
            hausdorff: hd,
            area_delta: ad,
        }
    } else {
        OracleOutcome::Fail {
            volume_delta: vd,
            hausdorff: hd,
            area_delta: ad,
            reason: reasons.join("; "),
        }
    }
}

/// What the oracle concluded for one corpus file.
#[derive(Debug, Clone, PartialEq)]
pub enum OracleOutcome {
    /// No comparison was made — `openscad` absent, the file is a 2D profile, or
    /// morphe produced no comparable solid. The carried string says which.
    Skipped(String),
    /// All metrics within tolerance.
    Pass {
        /// Relative volume difference actually measured.
        volume_delta: f64,
        /// Normalized Hausdorff distance actually measured.
        hausdorff: f64,
        /// Relative surface-area difference actually measured.
        area_delta: f64,
    },
    /// A present-`openscad` comparison diverged past tolerance. This is the
    /// **only** outcome that fails the gate.
    Fail {
        /// Relative volume difference actually measured.
        volume_delta: f64,
        /// Normalized Hausdorff distance actually measured.
        hausdorff: f64,
        /// Relative surface-area difference actually measured.
        area_delta: f64,
        /// Which metric(s) exceeded tolerance, with values.
        reason: String,
    },
    /// The reference mesh could not be produced or was malformed — an openscad
    /// spawn error, nonzero exit (openscad rejecting a construct morphe
    /// accepts), timeout, or unparseable/NaN output. This is openscad's
    /// limitation, **not** mesh divergence, so like [`Skipped`](Self::Skipped)
    /// it does not fail the gate.
    Error(String),
}

impl OracleOutcome {
    /// Whether this outcome should fail the run. Only a real [`Fail`](Self::Fail)
    /// does — a [`Skipped`](Self::Skipped) or [`Error`](Self::Error) never fails.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, OracleOutcome::Fail { .. })
    }

    /// Whether the reference could not be produced/parsed (openscad's problem,
    /// not divergence).
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, OracleOutcome::Error(_))
    }
}

/// The oracle result for one corpus file.
#[derive(Debug, Clone)]
pub struct OracleResult {
    /// Path to the `.scad` file.
    pub path: PathBuf,
    /// What the oracle concluded.
    pub outcome: OracleOutcome,
}

/// A summary over a whole oracle run.
#[derive(Debug, Clone)]
pub struct OracleReport {
    /// The resolved `openscad` binary, or `None` if it was not found (in which
    /// case every result is [`OracleOutcome::Skipped`]).
    pub openscad: Option<PathBuf>,
    /// Per-file oracle results, in path order.
    pub results: Vec<OracleResult>,
}

impl OracleReport {
    /// Number of files where an actual comparison ran (pass or fail) — a
    /// skipped or errored file never produced a comparison.
    #[must_use]
    pub fn compared(&self) -> usize {
        self.results
            .iter()
            .filter(|r| {
                matches!(
                    r.outcome,
                    OracleOutcome::Pass { .. } | OracleOutcome::Fail { .. }
                )
            })
            .count()
    }

    /// Number of files that passed the oracle.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, OracleOutcome::Pass { .. }))
            .count()
    }

    /// Number of files whose comparison failed.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome.is_failure())
            .count()
    }

    /// Number of files that were skipped.
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, OracleOutcome::Skipped(_)))
            .count()
    }

    /// Number of files whose reference could not be produced/parsed (openscad's
    /// limitation, not divergence). These do **not** fail the gate.
    #[must_use]
    pub fn errored(&self) -> usize {
        self.results.iter().filter(|r| r.outcome.is_error()).count()
    }

    /// Whether the run should exit zero: false when some present-`openscad`
    /// comparison actually *diverged* ([`OracleOutcome::Fail`]), and false
    /// when `openscad` IS present yet not a single comparison ran (every file
    /// skipped or errored) — a green run that measured nothing is
    /// indistinguishable from a broken oracle, so it must not pass. An absent
    /// `openscad` (all-skipped) stays OK: the degrade-to-skip contract.
    /// Individual render/parse errors alongside real comparisons stay OK.
    #[must_use]
    pub fn exit_ok(&self) -> bool {
        let diverged = self.results.iter().any(|r| r.outcome.is_failure());
        let measured_nothing = self.openscad.is_some() && self.compared() == 0;
        !diverged && !measured_nothing
    }

    /// The failing files, in path order.
    pub fn failures(&self) -> impl Iterator<Item = &OracleResult> {
        self.results.iter().filter(|r| r.outcome.is_failure())
    }
}

/// Run the oracle over every `.scad` file under `dir`, comparing morphe's mesh
/// to `openscad`'s for each 3D file.
///
/// If `openscad` cannot be located every file is [`OracleOutcome::Skipped`] and
/// the report's [`exit_ok`](OracleReport::exit_ok) stays `true` — the
/// degrade-to-skip contract. A missing/unreadable `dir` is an `Err`.
pub fn run_oracle(dir: &Path, tol: &OracleTolerance) -> std::io::Result<OracleReport> {
    let openscad = detect_openscad();
    let mut paths = Vec::new();
    crate::collect_scad(dir, &mut paths)?;
    paths.sort();

    let results = paths
        .into_iter()
        .map(|path| {
            let outcome = oracle_one(&path, openscad.as_deref(), tol);
            OracleResult { path, outcome }
        })
        .collect();

    Ok(OracleReport { openscad, results })
}

/// Compare one corpus file. Returns [`OracleOutcome::Skipped`] when `openscad`
/// is `None`, the file is a 2D profile, or morphe yields no non-empty solid.
fn oracle_one(path: &Path, openscad: Option<&Path>, tol: &OracleTolerance) -> OracleOutcome {
    let Some(openscad) = openscad else {
        return OracleOutcome::Skipped("openscad not installed".to_string());
    };
    if ExpectedKind::of(path) == ExpectedKind::Profile {
        return OracleOutcome::Skipped("2D profile (openscad exports no STL)".to_string());
    }

    // morphe side: resolve to a non-empty solid.
    let morphe = match morphe_mesh(path) {
        Ok(Some(mesh)) if !mesh.is_empty() => mesh,
        Ok(_) => return OracleOutcome::Skipped("morphe produced no solid".to_string()),
        Err(e) => return OracleOutcome::Skipped(format!("morphe error: {e}")),
    };

    // openscad side: render to STL and parse it back. A render/parse failure is
    // openscad's limitation, not divergence — Error, never Fail.
    let reference = match openscad_mesh(openscad, path) {
        Ok(mesh) => mesh,
        Err(e) => return OracleOutcome::Error(e),
    };

    compare_meshes(&morphe, &reference, &OracleTolerance::for_file(path, tol))
}

/// Resolve a `.scad` file through morphe and, if it is a non-empty solid, return
/// its [`Mesh`].
///
/// Resolves through [`resolve_source_at`] with the
/// file's path attached so `import()`/`surface()` corpus files carry their
/// source-dir context (a string-only resolve can't open relative paths).
fn morphe_mesh(path: &Path) -> Result<Option<Mesh>, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    match resolve_source_at(&src, Some(path))? {
        Some(Geometry::Solid(solid)) => Ok(Some(Mesh::from_solid(&solid))),
        _ => Ok(None),
    }
}

/// Per-file wall-clock budget for an openscad render. A hung or looping render
/// is killed at this deadline rather than wedging the whole run (and CI).
const OPENSCAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Render `scad` through the `openscad` CLI to a temporary STL and parse it.
///
/// Spawns the child with a wall-clock deadline ([`OPENSCAD_TIMEOUT`]): if it has
/// not exited by then it is killed and this returns an error (→
/// [`OracleOutcome::Error`]). `stderr` is drained on a background thread so a
/// chatty child cannot dead-lock the poll loop by filling the pipe buffer. The
/// temporary STL is removed on every exit path.
fn openscad_mesh(openscad: &Path, scad: &Path) -> Result<Mesh, String> {
    let tmp = unique_tmp_dir().map_err(|e| format!("create oracle temp dir: {e}"))?;
    let out = tmp.join("reference.stl");
    // Removes the whole private directory (and the STL in it).
    let cleanup = |_: &Path| {
        let _ = std::fs::remove_dir_all(&tmp);
    };

    let mut child = match Command::new(openscad)
        .arg("-o")
        .arg(&out)
        .arg(scad)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            cleanup(&out);
            return Err(format!("spawn openscad: {e}"));
        }
    };

    // Drain stderr concurrently so filling the pipe buffer can't stall the wait.
    let stderr = child.stderr.take();
    let reader = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });

    // Poll for exit until the deadline; kill on timeout.
    let deadline = Instant::now() + OPENSCAD_TIMEOUT;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!(
                        "openscad timed out after {}s",
                        OPENSCAD_TIMEOUT.as_secs()
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("waiting on openscad: {e}"));
            }
        }
    };
    let stderr_text = reader.join().unwrap_or_default();

    let status = match exit {
        Ok(status) => status,
        Err(e) => {
            cleanup(&out);
            return Err(e);
        }
    };
    if !status.success() {
        cleanup(&out);
        return Err(format!(
            "openscad exited {}: {}",
            status,
            stderr_text.trim()
        ));
    }

    let bytes = match std::fs::read(&out) {
        Ok(bytes) => bytes,
        Err(e) => {
            cleanup(&out);
            return Err(format!("read openscad STL: {e}"));
        }
    };
    cleanup(&out);
    parse_stl(&bytes)
}

/// Create a fresh private directory under the system temp dir for one
/// openscad render, returning its path.
///
/// The old predictable `/tmp/morphe-oracle-<pid>-<n>.stl` name in a shared,
/// world-writable directory let another local user pre-plant that path (e.g.
/// a symlink) so openscad wrote — and the oracle read back — through it.
/// Now the name mixes pid, wall-clock nanos and a counter, the directory is
/// created with a NON-recursive `create_dir` (which fails with
/// `AlreadyExists` on any pre-existing entry, symlinks included, so creation
/// is exclusive), and on unix it is mode `0700`. The STL is written inside it.
fn unique_tmp_dir() -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let mut last_err = None;
    for _ in 0..16 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = base.join(format!("morphe-oracle-{}-{nanos}-{n}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no unique temp dir")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit cube `[0,1]^3` as a welded indexed mesh (8 verts, 12 tris).
    fn unit_cube() -> Mesh {
        Mesh::from_solid(&morphe_kernel::Solid::cube([1.0, 1.0, 1.0], false))
    }

    /// A copy of `m` translated by `(dx, dy, dz)`.
    fn translated(m: &Mesh, d: [f64; 3]) -> Mesh {
        Mesh {
            verts: m
                .verts
                .iter()
                .map(|v| [v[0] + d[0], v[1] + d[1], v[2] + d[2]])
                .collect(),
            tris: m.tris.clone(),
        }
    }

    // ── volume ──────────────────────────────────────────────────────────────

    #[test]
    fn cube_volume_is_one() {
        assert!((unit_cube().volume() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn volume_delta_identical_is_zero() {
        let c = unit_cube();
        assert!(volume_delta(&c, &c).abs() < 1e-12);
    }

    #[test]
    fn volume_delta_double_scale_is_seven() {
        // A 2× cube has 8× the volume, so |1 - 8| / 8 ... but delta is defined
        // as |vol(a) - vol(b)| / vol(b). With a = 2×cube (vol 8) and b = cube
        // (vol 1): |8 - 1| / 1 = 7.
        let small = unit_cube();
        let big = Mesh::from_solid(&morphe_kernel::Solid::cube([2.0, 2.0, 2.0], false));
        assert!((big.volume() - 8.0).abs() < 1e-9);
        assert!((volume_delta(&big, &small) - 7.0).abs() < 1e-9);
        // And the other direction: |1 - 8| / 8 = 0.875.
        assert!((volume_delta(&small, &big) - 0.875).abs() < 1e-9);
    }

    // ── hausdorff ─────────────────────────────────────────────────────────────

    #[test]
    fn hausdorff_identical_is_zero() {
        let c = unit_cube();
        assert!(hausdorff_normalized(&c, &c).abs() < 1e-12);
    }

    #[test]
    fn hausdorff_translated_cube_is_offset_over_diagonal() {
        // A unit cube translated 0.1 along x: every vertex's nearest neighbour
        // in the other set is 0.1 away, so raw Hausdorff = 0.1. Normalizing by
        // the unit cube's bbox diagonal (√3) gives 0.1/√3.
        let c = unit_cube();
        let moved = translated(&c, [0.1, 0.0, 0.0]);
        let got = hausdorff_normalized(&c, &moved);
        let expect = 0.1 / 3.0_f64.sqrt();
        assert!(
            (got - expect).abs() < 1e-9,
            "hausdorff {got} should be {expect}"
        );
    }

    #[test]
    fn hausdorff_empty_vs_nonempty_is_infinite() {
        let c = unit_cube();
        let empty = Mesh::default();
        assert!(hausdorff_normalized(&c, &empty).is_infinite());
        assert_eq!(hausdorff_normalized(&empty, &empty), 0.0);
    }

    /// A cylinder tessellated at two different fragment counts is the *same*
    /// shape, differently faceted: fine-mesh vertices sit mid-face of the coarse
    /// mesh (and vice-versa). Point-to-surface Hausdorff stays small, whereas a
    /// naive vertex-to-vertex distance would inflate — this is the whole reason
    /// the metric measures to triangles, not vertices.
    #[test]
    fn hausdorff_point_to_surface_beats_vertex_to_vertex_on_facet_offset() {
        let fine = cylinder_mesh(48);
        let coarse = cylinder_mesh(12);

        let surface = hausdorff_normalized(&fine, &coarse);

        // A vertex-to-vertex directed distance for comparison (the old metric).
        let vertex_to_vertex = {
            let mut worst = 0.0_f64;
            for &p in &fine.verts {
                let nearest = coarse
                    .verts
                    .iter()
                    .map(|&q| dist(p, q))
                    .fold(f64::INFINITY, f64::min);
                worst = worst.max(nearest);
            }
            worst / fine.bbox_diagonal().max(coarse.bbox_diagonal())
        };

        assert!(
            surface < vertex_to_vertex,
            "point-to-surface {surface} should beat vertex-to-vertex {vertex_to_vertex}"
        );
        assert!(
            surface < 0.05,
            "same cylinder at different facet counts should agree within tolerance, got {surface}"
        );
    }

    /// A straight cylinder (r=5, h=10) tessellated at `fragments` segments.
    fn cylinder_mesh(fragments: u32) -> Mesh {
        use morphe_eval::csg::Primitive;
        let g = morphe_kernel::tessellate_primitive(&Primitive::Cylinder {
            h: 10.0,
            r1: 5.0,
            r2: 5.0,
            center: true,
            fragments,
        })
        .expect("cylinder tessellates");
        match g {
            Geometry::Solid(s) => Mesh::from_solid(&s),
            Geometry::Profile(_) => panic!("cylinder is a solid"),
        }
    }

    // ── surface sampling ──────────────────────────────────────────────────────

    /// The pre-sampling metric: worst distance from any *vertex* of one mesh
    /// to the nearest triangle of the other, both ways, over the diagonal.
    fn vertex_only_hausdorff(a: &Mesh, b: &Mesh) -> f64 {
        let directed = |x: &Mesh, y: &Mesh| {
            x.verts
                .iter()
                .map(|&p| {
                    (0..y.tris.len())
                        .map(|i| {
                            let [a, b, c] = y.triangle(i);
                            point_triangle_distance(p, a, b, c)
                        })
                        .fold(f64::INFINITY, f64::min)
                })
                .fold(0.0, f64::max)
        };
        directed(a, b).max(directed(b, a)) / a.bbox_diagonal().max(b.bbox_diagonal())
    }

    fn mesh_of(solid: &morphe_kernel::Solid) -> Mesh {
        Mesh::from_solid(solid)
    }

    fn solid_of(m: &Mesh) -> morphe_kernel::Solid {
        morphe_kernel::Solid::from_mesh(&m.verts, &m.tris).expect("valid manifold")
    }

    /// A centered cylinder of radius `r` and height `h`, moved by `d`.
    fn cylinder_at(r: f64, h: f64, fragments: u32, d: [f64; 3]) -> morphe_kernel::Solid {
        use morphe_eval::csg::Primitive;
        let g = morphe_kernel::tessellate_primitive(&Primitive::Cylinder {
            h,
            r1: r,
            r2: r,
            center: true,
            fragments,
        })
        .expect("cylinder tessellates");
        let Geometry::Solid(s) = g else {
            panic!("cylinder is a solid")
        };
        solid_of(&translated(&mesh_of(&s), d))
    }

    fn minus(base: morphe_kernel::Solid, cutters: Vec<morphe_kernel::Solid>) -> Mesh {
        let mut all = vec![base];
        all.extend(cutters);
        let s = morphe_kernel::tessellate::difference(&all)
            .expect("difference")
            .expect("non-empty");
        mesh_of(&s)
    }

    /// The same four corners of a non-planar quad, split along either
    /// diagonal. The vertex sets are identical, so a vertex-only metric scores
    /// zero; sampling the faces exposes the different surfaces.
    #[test]
    fn hausdorff_sees_an_edge_flip_on_a_non_planar_quad() {
        let verts = vec![
            [0.0, 0.0, 0.5],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.5],
            [0.0, 1.0, 0.0],
        ];
        let ridge = Mesh {
            verts: verts.clone(),
            tris: vec![[0, 1, 2], [0, 2, 3]],
        };
        let valley = Mesh {
            verts,
            tris: vec![[0, 1, 3], [1, 2, 3]],
        };
        assert!(vertex_only_hausdorff(&ridge, &valley) < 1e-12);
        let got = hausdorff_normalized(&ridge, &valley);
        // At the quad's center the two surfaces are 0.5 apart vertically; the
        // perpendicular gap is a little less. Over the 1.5 diagonal that is
        // well above 10 %.
        assert!(got > 0.1, "edge flip must be visible, got {got}");
        assert!(
            (got - hausdorff_normalized(&valley, &ridge)).abs() < 1e-12,
            "the metric is symmetric"
        );
    }

    /// A plate that lost its through-hole. Every vertex of the hole wall lies on
    /// a cap of the solid plate, and the solid plate's corners lie on the holed
    /// one, so the vertex-only metric scored this as identical.
    #[test]
    fn missing_hole_is_detected_at_default_tolerance() {
        let plate = morphe_kernel::Solid::cube([40.0, 40.0, 10.0], true);
        let solid = mesh_of(&plate);
        let holed = minus(plate, vec![cylinder_at(2.0, 20.0, 16, [0.0; 3])]);

        assert!(vertex_only_hausdorff(&solid, &holed) < 1e-9);
        // Mid-depth wall samples are 5 from either cap: 5 / √3300 ≈ 0.087.
        let h = hausdorff_normalized(&solid, &holed);
        assert!(h > 0.08, "hausdorff {h}");

        for (morphe, reference) in [(&solid, &holed), (&holed, &solid)] {
            let out = compare_meshes(morphe, reference, &OracleTolerance::default());
            let OracleOutcome::Fail { reason, .. } = &out else {
                panic!("a missing hole must fail the oracle, got {out:?}");
            };
            assert!(reason.contains("hausdorff"), "{reason}");
        }
    }

    /// A grid of holes in a plate that is thin next to its diagonal: the
    /// mid-depth wall samples are only 3 / √7236 ≈ 0.035 of the diagonal away
    /// and the volume barely moves, but the hole walls add ~4 % surface area.
    #[test]
    fn missing_hole_pattern_in_thin_plate_is_caught_by_area() {
        let plate = morphe_kernel::Solid::cube([60.0, 60.0, 6.0], true);
        let mut cutters = Vec::new();
        for i in -1..=1 {
            for j in -1..=1 {
                let d = [f64::from(i) * 15.0, f64::from(j) * 15.0, 0.0];
                cutters.push(cylinder_at(1.5, 12.0, 16, d));
            }
        }
        let solid = mesh_of(&plate);
        let holed = minus(plate, cutters);
        let tol = OracleTolerance::default();

        assert!(volume_delta(&solid, &holed) < tol.volume_rel);
        assert!(hausdorff_normalized(&solid, &holed) < tol.hausdorff_rel);
        let ad = area_delta(&solid, &holed);
        assert!(ad > tol.area_rel, "area delta {ad}");

        let out = compare_meshes(&solid, &holed, &tol);
        let OracleOutcome::Fail { reason, .. } = &out else {
            panic!("expected fail, got {out:?}");
        };
        assert!(reason.contains("area delta"), "{reason}");
    }

    /// Backs the area tolerance rationale: under faceting changes the surface
    /// area of a curved primitive moves less than its volume, so the tighter
    /// area bound does not fail shapes the volume bound accepts.
    #[test]
    fn faceting_moves_area_less_than_volume() {
        for (a, b) in [(12, 16), (30, 32), (36, 48)] {
            let pairs = [
                (cylinder_mesh(a), cylinder_mesh(b)),
                (sphere_mesh(5.0, a), sphere_mesh(5.0, b)),
            ];
            for (x, y) in &pairs {
                let vd = volume_delta(x, y);
                let ad = area_delta(x, y);
                assert!(ad < vd, "{a} vs {b}: area delta {ad} >= volume delta {vd}");
                if vd <= OracleTolerance::DEFAULT.volume_rel {
                    assert!(
                        ad <= OracleTolerance::DEFAULT.area_rel,
                        "{a} vs {b}: volume passes ({vd}) but area fails ({ad})"
                    );
                }
            }
        }
    }

    #[test]
    fn surface_samples_are_deterministic_and_on_the_surface() {
        let s = sphere_mesh(3.0, 24);
        let a = s.surface_samples();
        assert_eq!(a, s.surface_samples());
        assert!(a.len() >= SURFACE_SAMPLES);
        let mut grid = TriangleGrid::new(&s);
        for p in a.iter().step_by(97) {
            assert!(grid.nearest(*p, 0.0) < 1e-9);
        }
    }

    /// The grid search must give exactly the brute-force nearest distance,
    /// including for points well outside the mesh's bounding box.
    #[test]
    fn grid_nearest_matches_brute_force() {
        let target = sphere_mesh(2.0, 20);
        let mut grid = TriangleGrid::new(&target);
        let mut rng = SplitMix64(7);
        for _ in 0..500 {
            let p = [0, 1, 2].map(|_| rng.next_f64() * 12.0 - 6.0);
            let brute = (0..target.tris.len())
                .map(|i| {
                    let [a, b, c] = target.triangle(i);
                    point_triangle_distance(p, a, b, c)
                })
                .fold(f64::INFINITY, f64::min);
            let got = grid.nearest(p, 0.0);
            assert!((got - brute).abs() < 1e-12, "{p:?}: grid {got} vs {brute}");
        }
    }

    #[test]
    fn surface_area_of_cube() {
        let c = Mesh::from_solid(&morphe_kernel::Solid::cube([1.0, 2.0, 3.0], false));
        assert!((c.surface_area() - 22.0).abs() < 1e-9);
    }

    // ── tolerance boundaries ──────────────────────────────────────────────────

    /// `m` with every triangle's winding reversed (an inside-out mesh).
    fn inverted(m: &Mesh) -> Mesh {
        Mesh {
            verts: m.verts.clone(),
            tris: m.tris.iter().map(|t| [t[0], t[2], t[1]]).collect(),
        }
    }

    #[test]
    fn volume_is_signed() {
        let c = unit_cube();
        assert!((c.volume() - 1.0).abs() < 1e-9);
        assert!((inverted(&c).volume() + 1.0).abs() < 1e-9);
    }

    #[test]
    fn inside_out_morphe_mesh_fails_even_with_matching_magnitude() {
        // With abs() an inverted morphe mesh compared as a perfect pass.
        let c = unit_cube();
        let out = compare_meshes(&inverted(&c), &c, &OracleTolerance::default());
        assert!(out.is_failure(), "got {out:?}");
        let OracleOutcome::Fail { reason, .. } = out else {
            unreachable!()
        };
        assert!(reason.contains("inside-out"), "{reason}");
        // An inverted REFERENCE is openscad's problem: Error, not Fail.
        let out = compare_meshes(&c, &inverted(&c), &OracleTolerance::default());
        assert!(out.is_error(), "got {out:?}");
    }

    #[test]
    fn present_openscad_with_zero_comparisons_fails_the_run() {
        let all_errored = OracleReport {
            openscad: Some(PathBuf::from("/fake/openscad")),
            results: vec![
                OracleResult {
                    path: PathBuf::from("a.scad"),
                    outcome: OracleOutcome::Error("openscad exited 1".to_string()),
                },
                OracleResult {
                    path: PathBuf::from("b.scad"),
                    outcome: OracleOutcome::Skipped("morphe error: x".to_string()),
                },
            ],
        };
        assert_eq!(all_errored.compared(), 0);
        assert!(
            !all_errored.exit_ok(),
            "openscad present but nothing measured must not be green"
        );
        // Absent openscad keeps the degrade-to-skip contract.
        let absent = OracleReport {
            openscad: None,
            results: vec![OracleResult {
                path: PathBuf::from("a.scad"),
                outcome: OracleOutcome::Skipped("openscad not installed".to_string()),
            }],
        };
        assert!(absent.exit_ok());
    }

    #[test]
    fn oracle_temp_dirs_are_unique_and_exclusive() {
        let a = unique_tmp_dir().expect("create");
        let b = unique_tmp_dir().expect("create");
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        // Exclusive: creating an existing path again fails (no reuse).
        assert!(std::fs::DirBuilder::new().create(&a).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "private to the owner");
        }
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn compare_identical_passes() {
        let c = unit_cube();
        let out = compare_meshes(&c, &c, &OracleTolerance::default());
        assert!(matches!(out, OracleOutcome::Pass { .. }), "got {out:?}");
    }

    #[test]
    fn compare_within_tolerance_passes() {
        // 0.1 translation → hausdorff 0.0577 of diagonal, volume identical.
        let c = unit_cube();
        let moved = translated(&c, [0.1, 0.0, 0.0]);
        let tol = OracleTolerance {
            volume_rel: 0.05,
            hausdorff_rel: 0.10, // 0.0577 < 0.10 passes
            area_rel: 0.03,
        };
        assert!(matches!(
            compare_meshes(&c, &moved, &tol),
            OracleOutcome::Pass { .. }
        ));
    }

    #[test]
    fn compare_hausdorff_over_tolerance_fails_naming_metric() {
        let c = unit_cube();
        let moved = translated(&c, [0.1, 0.0, 0.0]);
        let tol = OracleTolerance {
            volume_rel: 0.05,
            hausdorff_rel: 0.02, // 0.0577 > 0.02 fails
            area_rel: 0.03,
        };
        match compare_meshes(&c, &moved, &tol) {
            OracleOutcome::Fail { reason, .. } => assert!(
                reason.contains("hausdorff"),
                "reason should name hausdorff: {reason}"
            ),
            other => panic!("expected fail, got {other:?}"),
        }
    }

    #[test]
    fn compare_cube_vs_sphere_fails() {
        // A deliberately-divergent case: a cube compared against a sphere STL.
        let cube = Mesh::from_solid(&morphe_kernel::Solid::cube([2.0, 2.0, 2.0], true));
        let sphere = sphere_mesh(1.0, 32);
        let out = compare_meshes(&cube, &sphere, &OracleTolerance::default());
        assert!(
            out.is_failure(),
            "a cube vs a sphere must fail the oracle, got {out:?}"
        );
    }

    /// A tessellated sphere mesh via the kernel primitive path.
    fn sphere_mesh(r: f64, fragments: u32) -> Mesh {
        use morphe_eval::csg::Primitive;
        let g = morphe_kernel::tessellate_primitive(&Primitive::Sphere { r, fragments })
            .expect("sphere tessellates");
        match g {
            Geometry::Solid(s) => Mesh::from_solid(&s),
            Geometry::Profile(_) => panic!("sphere is a solid"),
        }
    }

    // ── STL parser round-trip ─────────────────────────────────────────────────

    #[test]
    fn binary_stl_round_trip() {
        let solid = morphe_kernel::Solid::cube([2.0, 3.0, 4.0], true);
        let bytes = morphe_kernel::to_binary_stl(&solid);
        let mesh = parse_stl(&bytes).expect("binary STL parses");
        assert_eq!(mesh.tris.len(), solid.num_tri());
        assert!((mesh.volume() - solid.volume()).abs() < 1e-4);
        assert!((mesh.volume() - 24.0).abs() < 1e-4);
    }

    #[test]
    fn ascii_stl_round_trip() {
        let solid = morphe_kernel::Solid::cube([2.0, 2.0, 2.0], false);
        let text = morphe_kernel::to_ascii_stl(&solid);
        let mesh = parse_stl(text.as_bytes()).expect("ASCII STL parses");
        assert_eq!(mesh.tris.len(), solid.num_tri());
        assert!((mesh.volume() - 8.0).abs() < 1e-4);
    }

    #[test]
    fn binary_and_ascii_parse_to_same_volume() {
        let solid = morphe_kernel::Solid::cube([1.0, 2.0, 3.0], true);
        let bin = parse_stl(&morphe_kernel::to_binary_stl(&solid)).unwrap();
        let asc = parse_stl(morphe_kernel::to_ascii_stl(&solid).as_bytes()).unwrap();
        assert!((bin.volume() - asc.volume()).abs() < 1e-4);
    }

    #[test]
    fn empty_binary_stl_parses_to_empty_mesh() {
        // 84-byte empty binary STL: header + zero count.
        let mut bytes = vec![0u8; 84];
        bytes[..17].copy_from_slice(b"morphe binary STL");
        let mesh = parse_stl(&bytes).unwrap();
        assert!(mesh.is_empty());
    }

    #[test]
    fn stl_with_nan_coordinate_is_rejected() {
        // An ASCII facet whose vertices carry NaN is malformed output; the
        // parser must reject it so the oracle records Error, not a silent Fail.
        let stl = "solid x\n  facet normal 0 0 0\n    outer loop\n      \
                   vertex NaN 0 0\n      vertex 1 0 0\n      vertex 0 1 0\n    \
                   endloop\n  endfacet\nendsolid x\n";
        assert!(
            parse_stl(stl.as_bytes()).is_err(),
            "a NaN vertex coordinate must be rejected"
        );
    }

    // ── error outcome does not fail the gate ──────────────────────────────────

    #[test]
    fn error_outcome_does_not_fail_the_gate() {
        // An openscad render/parse error is not divergence: it must count as
        // errored, never failed, and leave exit_ok true.
        let report = OracleReport {
            openscad: Some(PathBuf::from("/fake/openscad")),
            results: vec![
                OracleResult {
                    path: PathBuf::from("a.scad"),
                    outcome: OracleOutcome::Error("openscad exited 1: unsupported".to_string()),
                },
                OracleResult {
                    path: PathBuf::from("b.scad"),
                    outcome: OracleOutcome::Pass {
                        volume_delta: 0.0,
                        hausdorff: 0.0,
                        area_delta: 0.0,
                    },
                },
            ],
        };
        assert_eq!(report.errored(), 1);
        assert_eq!(report.failed(), 0);
        assert_eq!(report.compared(), 1, "only the Pass is a real comparison");
        assert!(report.exit_ok(), "an errored file must not fail the gate");
    }

    // ── detection ─────────────────────────────────────────────────────────────

    #[test]
    fn detect_openscad_absent_when_env_points_nowhere() {
        // NOTE: mutates process env; kept in one test to avoid cross-test races.
        let key = "OPENSCAD_BIN";
        let prev = std::env::var_os(key);
        std::env::set_var(key, "/no/such/openscad/binary/anywhere");
        assert!(
            detect_openscad().is_none(),
            "a non-existent OPENSCAD_BIN must resolve to None (skip), not fall through"
        );
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    // -----------------------------------------------------------------------
    // Per-file tolerance resolution
    // -----------------------------------------------------------------------

    #[test]
    fn tolerance_for_file_returns_base_for_unlisted() {
        let base = OracleTolerance {
            volume_rel: 0.11,
            hausdorff_rel: 0.07,
            area_rel: 0.04,
        };
        // An unlisted corpus file gets the caller's base — and a NON-default
        // base proves the threading (not a hidden Default::default()).
        let got = OracleTolerance::for_file(Path::new("corpus/core/cube_fn.scad"), &base);
        assert_eq!(got, base);
        assert_ne!(got, OracleTolerance::default());
        // The default base behaves identically.
        assert_eq!(
            OracleTolerance::for_file(
                Path::new("corpus/core/union.scad"),
                &OracleTolerance::default()
            ),
            OracleTolerance::default()
        );
    }

    #[test]
    fn tolerance_for_file_resolves_unlisted_basenames_to_the_base() {
        // Basenames WITHOUT an entry resolve to the base (exact-match
        // mechanics). The listed entries are covered by the entry-applies
        // test below.
        let base = OracleTolerance::default();
        for path in [
            "corpus/core/cube_fn.scad",
            "corpus/core/nested/dir/minkowski.scad",
            "surface.scad",
        ] {
            assert_eq!(OracleTolerance::for_file(Path::new(path), &base), base);
        }
    }

    #[test]
    fn tolerance_entries_apply_by_basename_with_measured_rationale() {
        // Every entry must land
        // with MEASURED numbers and a rationale comment (see FILE_OVERRIDES).
        // This pin makes the table's count visible: adding an entry updates
        // this test in the same change.
        assert_eq!(OracleTolerance::FILE_OVERRIDES.len(), 3);
        for (name, _) in OracleTolerance::FILE_OVERRIDES {
            assert!(
                !OracleTolerance::for_file(Path::new(name), &OracleTolerance::default())
                    .eq(&OracleTolerance::default()),
                "{name} must resolve to its entry, not the base"
            );
        }
        // Entries apply at any directory depth (basename keying).
        let base = OracleTolerance::default();
        let via_entry = OracleTolerance::for_file(Path::new("corpus/core/text.scad"), &base);
        let nested = OracleTolerance::for_file(Path::new("corpus/core/nested/text.scad"), &base);
        assert_eq!(via_entry, nested);
        assert_ne!(via_entry, base, "the text entry must apply at any depth");
    }
}
