//! `text()` — glyph outlines from the bundled Fira Sans.
//!
//! The font dependency, `ttf-parser`, parses the bundled Regular-weight TTF
//! (see `assets/fonts/` and the workspace Cargo.toml comment). `glyph_outlines` flattens each glyph's quadratic/cubic
//! Béziers into polylines at a `$fn`-derived segment count; `text_profile`
//! lays a whole run out (advances × `spacing`, halign/valign shifts) into one
//! even-odd [`Profile`] — the 2D primitive `linear_extrude` lifts, exactly as
//! upstream's text flow does.
//!
//! **License:** the bundled font is Fira Sans Regular — SIL Open Font
//! License 1.1 (`assets/fonts/OFL.txt` ships beside the TTF and is embedded
//! too; the OFL notice-preservation duty is honored under `include_bytes!`).
//! The acceptable set is OFL-1.1 (preferred) or Apache-2.0/MIT
//! dual-licensed, so the font can ship inside any build.
//! Fira Sans was chosen over IBM Plex Sans (repo layout churn moved its
//! release artifacts) and Inter (release distribution is zip-only, no stable
//! raw TTF URL); all three satisfy the license criteria.
//!
//! **Known divergences:** `direction` supports ltr only (rtl/ttb warn +
//! fall back at eval); no fontconfig enumeration — one bundled family, an
//! unknown `font=` warns + falls back (eval-level); `textmetrics()` is not
//! yet supported; NO kerning/shaping (advance = the glyph's h-metric only —
//! upstream ≥2019.05 applies HarfBuzz GPOS kerning, so multi-glyph run
//! widths can differ slightly against the OpenSCAD oracle); NO Freetype
//! hinting/grid-fit (upstream outlines are hinted at render size — small
//! per-glyph distortion, accepted); valign metrics use the font's hhea
//! ascender/descender while upstream uses grid-fitted max-glyph-bbox
//! (affects non-baseline valign only); halign centering uses
//! Σ(advance·spacing) as the run width (close to upstream's laid-out width —
//! accepted); `linear_extrude` of
//! whitespace-only/empty text hits morphe's established loud empty-profile
//! Degenerate error where upstream silently renders nothing.

use ttf_parser::{Face, OutlineBuilder};

use crate::profile::{FillRule, Profile};
use crate::tessellate::KernelError;

/// The bundled font: Fira Sans Regular (SIL OFL 1.1 — see `OFL.txt`).
pub(crate) const FONT_BYTES: &[u8] = include_bytes!("assets/fonts/FiraSans-Regular.ttf");

/// The bundled font's license text, embedded with it (OFL notice preservation).
pub(crate) const FONT_LICENSE_BYTES: &[u8] = include_bytes!("assets/fonts/OFL.txt");

/// Upstream-compatibility scale (oracle-measured, 2021.01): FreeType is set
/// to 1e5/64 pt at 100 dpi and outlines are divided by 1e5 — the
/// 100-dpi-vs-72-pt mismatch lands every glyph at ×(100/72) of the plain
/// size/upm scale. Upstream's own comment keeps a similar ratio "for
/// compatibility"; morphe does the same — compatibility is the goal.
/// (FreetypeRenderer.cc: scale=1e5, FT_Set_Char_Size(0, 1e5, 100, 100).)
const UPSTREAM_DPI_RATIO: f64 = 100.0 / 72.0;

/// The bundled family name — what corpus files pin `font=` to, and the
/// fallback for every unknown family.
pub const FONT_FAMILY: &str = "Fira Sans";

/// The bundled font's license text (SIL OFL 1.1) — the OFL notice must
/// accompany the font wherever it ships, so it is embedded
/// alongside the TTF and exposed for the app's about/legal surface.
#[must_use]
pub fn font_license() -> &'static str {
    std::str::from_utf8(FONT_LICENSE_BYTES).unwrap_or("license text unavailable")
}

/// Parse the bundled font (fresh `Face` per call — `ttf_parser` borrows).
pub fn face() -> Result<Face<'static>, KernelError> {
    Face::parse(FONT_BYTES, 0)
        .map_err(|e| KernelError::Resolve(format!("bundled font `{FONT_FAMILY}` unusable: {e}")))
}

/// One glyph's outlines as closed polylines in FONT UNITS along the path, then
/// scaled/positioned by the caller. Collects via [`OutlineBuilder`], keeping
/// one open contour at a time; `move_to` starts a new one.
struct OutlineCollector {
    /// Flattening segments per Bézier curve (Fragments-derived).
    segments: usize,
    contours: Vec<Vec<[f64; 2]>>,
    current: Vec<[f64; 2]>,
    pen: [f64; 2],
}

impl OutlineCollector {
    fn finish_contour(&mut self) {
        if self.current.len() >= 3 {
            self.contours.push(std::mem::take(&mut self.current));
        } else {
            self.current.clear();
        }
    }
}

impl OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_contour();
        self.pen = [f64::from(x), f64::from(y)];
        self.current.push(self.pen);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.pen = [f64::from(x), f64::from(y)];
        self.current.push(self.pen);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0 = self.pen;
        let p1 = [f64::from(x1), f64::from(y1)];
        let p2 = [f64::from(x), f64::from(y)];
        for k in 1..=self.segments {
            let t = k as f64 / self.segments as f64;
            // de Casteljau, degree 2.
            let a = (1.0 - t) * p0[0] + t * p1[0];
            let b = (1.0 - t) * p0[1] + t * p1[1];
            let c = (1.0 - t) * p1[0] + t * p2[0];
            let d = (1.0 - t) * p1[1] + t * p2[1];
            self.current
                .push([(1.0 - t) * a + t * c, (1.0 - t) * b + t * d]);
        }
        self.pen = p2;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = self.pen;
        let p1 = [f64::from(x1), f64::from(y1)];
        let p2 = [f64::from(x2), f64::from(y2)];
        let p3 = [f64::from(x), f64::from(y)];
        for k in 1..=self.segments {
            let t = k as f64 / self.segments as f64;
            let mt = 1.0 - t;
            // de Casteljau, degree 3.
            let a = mt * p0[0] + t * p1[0];
            let b = mt * p0[1] + t * p1[1];
            let c = mt * p1[0] + t * p2[0];
            let d = mt * p1[1] + t * p2[1];
            let e = mt * p2[0] + t * p3[0];
            let f = mt * p2[1] + t * p3[1];
            let ab = mt * a + t * c;
            let bc = mt * b + t * d;
            let cd = mt * c + t * e;
            let dd = mt * d + t * f;
            self.current.push([mt * ab + t * cd, mt * bc + t * dd]);
        }
        self.pen = p3;
    }

    fn close(&mut self) {
        self.finish_contour();
    }
}

/// Flatten glyph `id`'s outline into closed contours at `size` (glyph-local:
/// origin at the baseline pen start, y-up), with `segments` points per Bézier
/// curve. `None` when the glyph has no outline (spaces — callers advance
/// only). Callers resolve chars to ids first (missing chars → `.notdef`,
/// glyph 0, upstream behavior).
pub fn glyph_outlines(
    face: &Face<'_>,
    id: ttf_parser::GlyphId,
    size: f64,
    segments: usize,
) -> Option<Vec<Vec<[f64; 2]>>> {
    let mut collector = OutlineCollector {
        segments: segments.max(1),
        contours: Vec::new(),
        current: Vec::new(),
        pen: [0.0; 2],
    };
    face.outline_glyph(id, &mut collector)?;
    collector.finish_contour();
    let scale = size / f64::from(face.units_per_em()) * UPSTREAM_DPI_RATIO;
    Some(
        collector
            .contours
            .into_iter()
            .map(|c| {
                c.into_iter()
                    .map(|p| [p[0] * scale, p[1] * scale])
                    .collect()
            })
            .collect(),
    )
}

/// Lay a whole text run out as one even-odd [`Profile`]:
/// per-glyph outlines at `params.size`, advance = xAdvance·size/unitsPerEm·
/// `params.spacing`, the run shifted per `halign`/`valign` (upstream text-node
/// semantics; valign metrics from the font's ascender/descender). Glyphs with
/// no outline (whitespace, missing) advance only. Even-odd fill so counters
/// (an `o`'s hole) survive.
pub fn text_profile(
    text: &str,
    params: &morphe_eval::csg::TextParams,
) -> Result<Profile, KernelError> {
    use morphe_eval::csg::{TextHalign, TextValign};
    let face = face()?;
    let upem = f64::from(face.units_per_em());
    let scale = params.size / upem * UPSTREAM_DPI_RATIO;
    // Flattening: upstream uses a FRACTION of the fragment count for font
    // curves ("curved segments of most fonts are relatively short" —
    // text.cc): max(floor(frags/8) + 1, 2), where frags comes from the same
    // get_fragments_from_r(size, fn, fs, fa). (Measured against OpenSCAD:
    // "hi" = 276 facets vs morphe 2760 at the full count.)
    let fragments_count = morphe_eval::eval::resolve_fragments(
        params.size,
        params.fragments.fn_,
        params.fragments.fs,
        params.fragments.fa,
    );
    crate::tessellate::check_fragment_cap("text", fragments_count)?;
    let segments = (fragments_count / 8 + 1).max(2) as usize;

    let ascender = f64::from(face.ascender()) * scale;
    let descender = f64::from(face.descender()) * scale;

    let mut contours: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut pen_x = 0.0_f64;
    for ch in text.chars() {
        // Missing glyphs (no cmap entry) fall back to the font's .notdef
        // (glyph 0) — upstream Freetype/HarfBuzz behavior: the char CONSUMES
        // .notdef's advance and renders .notdef's outline (usually a box).
        // An earlier skip-without-advance made a mixed-script run silently
        // reflow; the documented "missing advance only" behavior is now
        // what actually happens — via .notdef.
        let id = face.glyph_index(ch).unwrap_or(ttf_parser::GlyphId(0));
        let advance = f64::from(face.glyph_hor_advance(id).unwrap_or(0)) * scale * params.spacing;
        if let Some(outlines) = glyph_outlines(&face, id, params.size, segments) {
            for mut c in outlines {
                for p in &mut c {
                    p[0] += pen_x;
                }
                contours.push(c);
            }
        }
        pen_x += advance;
    }

    // Horizontal shift of the whole run.
    let run_width = pen_x;
    let dx = match params.halign {
        TextHalign::Left => 0.0,
        TextHalign::Center => -run_width / 2.0,
        TextHalign::Right => -run_width,
    };
    // Vertical shift from the font's line metrics.
    let dy = match params.valign {
        TextValign::Baseline => 0.0,
        TextValign::Top => -ascender,
        TextValign::Center => -(ascender + descender) / 2.0,
        TextValign::Bottom => -descender,
    };
    for c in contours.iter_mut() {
        for p in c.iter_mut() {
            p[0] += dx;
            p[1] += dy;
        }
    }

    Ok(Profile::from_contours(&contours, FillRule::EvenOdd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use morphe_eval::csg::{Fragments, TextHalign, TextParams, TextValign};

    fn params(text: &str) -> TextParams {
        TextParams {
            text: text.to_string(),
            size: 10.0,
            halign: TextHalign::Left,
            valign: TextValign::Baseline,
            spacing: 1.0,
            fragments: Fragments {
                fn_: 0.0,
                fs: 2.0,
                fa: 12.0,
            },
        }
    }

    fn vertex_count(contours: &[Vec<[f64; 2]>]) -> usize {
        contours.iter().map(|c| c.len()).sum()
    }

    #[test]
    fn font_license_decision() {
        // The acceptable set is ENUMERATED — OFL-1.1
        // (preferred), or Apache-2.0/MIT dual-licensed; the OFL notice ships with the TTF even under
        // include_bytes!. Assert the embedded license really is the OFL.
        let license = std::str::from_utf8(FONT_LICENSE_BYTES).expect("utf-8 license");
        assert!(
            license.contains("SIL OPEN FONT LICENSE Version 1.1"),
            "the bundled font's license must be the OFL 1.1 text"
        );
        assert!(
            license.contains("Mozilla Foundation"),
            "the Fira copyright notice must be preserved"
        );
        // And the face parses with the documented family.
        let face = face().expect("bundled font parses");
        let families: Vec<String> = face
            .names()
            .into_iter()
            .filter(|name| name.name_id == ttf_parser::name_id::FAMILY)
            .filter_map(|name| name.to_string())
            .collect();
        assert!(
            families.iter().any(|f| f == FONT_FAMILY),
            "family must be `{FONT_FAMILY}`, got {families:?}"
        );
    }

    #[test]
    fn glyph_outline_h_is_single_contour() {
        // `h` is one contour, nonempty, positive area, bbox ≈ the pinned
        // font's metrics (golden constants from Fira Sans Regular at size 10).
        let face = face().expect("font");
        let contours = glyph_outlines(&face, face.glyph_index('h').unwrap(), 10.0, 16)
            .expect("h has an outline");
        assert_eq!(contours.len(), 1, "h is a single contour (no counters)");
        let pts = &contours[0];
        assert!(pts.len() >= 8, "flattened h has a real vertex count");
        let minx = pts.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let maxx = pts.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let miny = pts.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let maxy = pts.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        // Golden bbox from the pinned font (Fira Sans Regular, size 10 —
        // measured once from this exact TTF and pinned; a font swap breaks
        // this test loudly, which is the point). Includes the upstream
        // 100/72 dpi compatibility ratio (see text_profile).
        assert!(
            (minx - 0.950 * UPSTREAM_DPI_RATIO).abs() < 0.02
                && (maxx - 4.960 * UPSTREAM_DPI_RATIO).abs() < 0.02,
            "h bbox x = [{minx:.3}, {maxx:.3}], expected ≈ [1.320, 6.889]"
        );
        assert!(
            (miny - 0.0).abs() < 0.01 && (maxy - 7.480 * UPSTREAM_DPI_RATIO).abs() < 0.03,
            "h bbox y = [{miny:.3}, {maxy:.3}], expected ≈ [0, 10.389]"
        );
    }

    #[test]
    fn glyph_outline_o_is_two_contours() {
        // `o` has an outer contour and an inner counter — two contours,
        // combined under EvenOdd by text_profile (the hole survives: area of
        // the o profile is well below the outer bbox area).
        let face = face().expect("font");
        let contours = glyph_outlines(&face, face.glyph_index('o').unwrap(), 10.0, 16)
            .expect("o has an outline");
        assert_eq!(contours.len(), 2, "o = outer + counter");
    }

    #[test]
    fn glyph_segments_follow_fn() {
        // $fn=8 vs $fn=32 change the flattening vertex count.
        let face = face().expect("font");
        let coarse =
            glyph_outlines(&face, face.glyph_index('o').unwrap(), 10.0, 8).expect("outline");
        let fine =
            glyph_outlines(&face, face.glyph_index('o').unwrap(), 10.0, 32).expect("outline");
        assert!(
            vertex_count(&fine) > vertex_count(&coarse),
            "fn=32 ({}) must yield more vertices than fn=8 ({})",
            vertex_count(&fine),
            vertex_count(&coarse)
        );
    }

    #[test]
    fn text_hi_two_glyph_profiles_nonempty_area() {
        // "hi" lays out two glyph profiles with positive profile area.
        let profile = text_profile("hi", &params("hi")).expect("hi profiles");
        let area = profile.area();
        assert!(area > 0.0, "hi has positive area, got {area}");
        assert!(profile.num_contour() >= 2, "h + i contours present");
    }

    #[test]
    fn text_halign_right_shifts_bbox_negative_x() {
        let left = text_profile("hi", &params("hi")).expect("left");
        let mut p = params("hi");
        p.halign = TextHalign::Right;
        let right = text_profile("hi", &p).expect("right");
        let left_minx = bbox(&left).0;
        let right_minx = bbox(&right).0;
        assert!(
            right_minx < left_minx - 1.0,
            "right-aligned run shifts negative x: left min {left_minx:.2}, right min {right_minx:.2}"
        );
    }

    #[test]
    fn text_valign_center_moves_off_baseline() {
        let base = text_profile("hi", &params("hi")).expect("baseline");
        let mut p = params("hi");
        p.valign = TextValign::Center;
        let centered = text_profile("hi", &p).expect("centered");
        let (_, base_maxy) = bbox(&base);
        let (_, c_maxy) = bbox(&centered);
        assert!(
            c_maxy < base_maxy - 1.0,
            "centered valign moves the run off the baseline: baseline top {base_maxy:.2}, centered top {c_maxy:.2}"
        );
    }

    fn bbox(profile: &Profile) -> (f64, f64) {
        let mut minx = f64::INFINITY;
        let mut maxy = f64::NEG_INFINITY;
        for contour in profile.to_polygons() {
            for p in contour {
                minx = minx.min(p[0]);
                maxy = maxy.max(p[1]);
            }
        }
        (minx, maxy)
    }

    #[test]
    fn text_spacing_squeezes_run_width() {
        // Spacing 0.5 halves the inter-glyph advance delta. The run width
        // shrinks by the h-glyph advance/2 — not exactly half (the final
        // glyph's ink is unchanged) — so pin that the narrowing is one
        // inter-glyph delta's worth (>= 20% of the original width here).
        let p1 = params("hi");
        let mut p2 = params("hi");
        p2.spacing = 0.5;
        let w1 = run_width(&text_profile("hi", &p1).expect("w1"));
        let w2 = run_width(&text_profile("hi", &p2).expect("w2"));
        assert!(w2 < w1, "spacing 0.5 narrows the run: {w1:.2} → {w2:.2}");
        assert!(
            (w1 - w2) > (w1 * 0.2),
            "the narrowing is one inter-glyph delta's worth: {w1:.2} vs {w2:.2}"
        );
    }

    fn run_width(profile: &Profile) -> f64 {
        let (minx, _) = bbox(profile);
        let mut maxx = f64::NEG_INFINITY;
        for contour in profile.to_polygons() {
            for p in contour {
                maxx = maxx.max(p[0]);
            }
        }
        maxx - minx
    }

    #[test]
    fn text_space_advances_without_contours() {
        // A lone space produces a Profile with no contours (advance only).
        let profile = text_profile(" ", &params(" ")).expect("space profiles");
        assert!(profile.is_empty(), "a space carries no ink");
    }

    #[test]
    fn font_family_constants_are_pinned_across_crates() {
        // Eval's
        // TEXT_FONT_FAMILY (what warns on unknown font=) must equal the
        // kernel's bundled family (what corpus files pin). Drift = compile
        // failure HERE, not a silent corpus mismatch.
        assert_eq!(
            morphe_eval::eval::TEXT_FONT_FAMILY,
            FONT_FAMILY,
            "eval's known-font name and the kernel's bundled family must match"
        );
    }

    #[test]
    fn missing_glyphs_render_notdef_and_consume_advance() {
        // A char with no cmap entry resolves to the
        // font's .notdef — it CONSUMES .notdef's advance and renders its
        // outline (the box), matching upstream Freetype/HarfBuzz. Probe that
        // motivated the fix: text("a中b") was byte-identical to text("ab")
        // (skip-without-advance, silent reflow).
        let ab = text_profile("ab", &params("ab")).expect("ab profiles");
        let mixed = text_profile("a\u{4e2d}b", &params("a\u{4e2d}b")).expect("a中b profiles");
        assert!(
            mixed.area() > ab.area(),
            "the .notdef box adds ink: {} vs {}",
            mixed.area(),
            ab.area()
        );
        // And the run is WIDER (中 consumed an advance + the box's own ink).
        let ab_width = run_width(&ab);
        let mixed_width = run_width(&mixed);
        assert!(
            mixed_width > ab_width,
            "missing glyph advances (.notdef): {mixed_width:.2} vs {ab_width:.2}"
        );
    }
}
