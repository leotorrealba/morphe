//! CSG node tree produced by statement evaluation.
//!
//! A minimal analogue of upstream openscad's `AbstractNode` tree
//! (`src/core/node.h`, master, fetched 2026-07-02): statement evaluation
//! walks the AST once and emits this tree; geometry never evaluates here.

use std::path::PathBuf;

use morphe_lang::ast::Modifiers;
use morphe_lang::lexer::Span;

use crate::value::Value;

/// One node of the CSG tree.
#[derive(Debug, Clone, PartialEq)]
pub enum CsgNode {
    /// Upstream `GroupNode`: the root scope, a user-module instantiation,
    /// or a `children()`/`echo`/`assert` wrapper.
    Group {
        children: Vec<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`); the root
        /// group carries the default (empty) set.
        modifiers: Modifiers,
    },
    /// An UNRESOLVED builtin module call: arguments are evaluated eagerly
    /// in the calling context (upstream `Arguments.cc`), but the module
    /// itself is not resolved to a typed node (builtins without a dedicated
    /// primitive/transform/boolean mapping).
    Call {
        name: String,
        /// Evaluated arguments in call order; `Some(name)` for named
        /// arguments (`$`-names keep their `$` prefix).
        args: Vec<(Option<String>, Value)>,
        children: Vec<CsgNode>,
        modifiers: Modifiers,
        span: Span,
    },
    /// Upstream `CsgOpNode(OpenSCADOperator::UNION)`: the
    /// additive combination of its children (`csgops.cc builtin_union`).
    /// Distinct from [`CsgNode::Group`], which is a *passive* scope container
    /// (the root, module bodies, transform/if/for/echo wrappers): `union()` is
    /// an EXPLICIT operator the author wrote, so it stays its own typed node —
    /// symmetric with [`Difference`](CsgNode::Difference)/[`Intersection`](
    /// CsgNode::Intersection)/[`Hull`](CsgNode::Hull). The kernel
    /// unions the children; a childless `union()` is empty geometry.
    Union {
        children: Vec<CsgNode>,
        modifiers: Modifiers,
    },
    /// Upstream `CsgOpNode(OpenSCADOperator::DIFFERENCE)`:
    /// the first child minus every subsequent child (`csgops.cc
    /// builtin_difference`). Children are kept in source order; the kernel
    /// treats `children[0]` as the minuend and subtracts
    /// `children[1..]` from it. Upstream collects children unconditionally, so
    /// no case is special-cased here: no children (or one) leaves nothing to
    /// subtract (the lone child, or empty, passes through), and an empty first
    /// child yields empty geometry.
    Difference {
        children: Vec<CsgNode>,
        modifiers: Modifiers,
    },
    /// Upstream `AbstractIntersectionNode` / `CsgOpNode(INTERSECTION)`:
    /// produced by `intersection_for` (`control.cc builtin_intersection_for`)
    /// AND by `intersection()` (`csgops.cc builtin_intersection`) — upstream
    /// maps both to the same `INTERSECTION` operator, so they share this one
    /// node kind rather than duplicating it. Its geometric meaning lives in
    /// the kernel; here it is a group whose children intersect.
    Intersection {
        children: Vec<CsgNode>,
        modifiers: Modifiers,
    },
    /// Upstream `CgalAdvNode(CgalAdvType::HULL)`: the convex
    /// hull of all children (`cgaladv.cc builtin_hull`). Whether the hull is
    /// 2D or 3D is decided by the kernel from the children's dimensionality
    /// (2D children → 2D hull, 3D → 3D); here the children are simply stored in
    /// order. `hull()` takes no parameters upstream.
    Hull {
        children: Vec<CsgNode>,
        modifiers: Modifiers,
    },
    /// A RESOLVED primitive leaf: one of the seven
    /// upstream `primitives.cc` `*Node`s, with its numeric parameters and
    /// fragment counts already resolved from the instantiation's
    /// `$fn`/`$fa`/`$fs`. Primitives are geometry leaves — like upstream,
    /// their child instantiations contribute no geometry, so the evaluator
    /// treats them as childless and does not evaluate any children under a
    /// primitive. The kernel consumes [`Primitive`] as pure
    /// geometry with no scope dependency.
    Primitive {
        primitive: Primitive,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED transform node: one of the seven
    /// upstream `TransformNode`/`ColorNode`/`CgalAdvNode(RESIZE)` transform
    /// modules, holding its resolved parameters (as a [`TransformKind`]) and
    /// the transformed subtree as a single child (upstream
    /// `children.instantiate(node)` attaches the instantiation's children).
    /// The child is always a [`CsgNode::Group`] wrapping the evaluated
    /// children, so a transform over several children groups them.
    Transform {
        kind: TransformKind,
        /// The transformed subtree — a group of the instantiation's children.
        child: Box<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `linear_extrude(...)` node: sweeps its 2D
    /// profile child along +Z into a 3D solid (upstream
    /// `src/core/LinearExtrudeNode.cc builtin_linear_extrude`). Its resolved
    /// parameters live in [`LinearExtrudeParams`]; the child is always a
    /// [`CsgNode::Group`] wrapping the instantiation's children (the 2D
    /// profile), matching the transform child-always-Group invariant.
    LinearExtrude {
        params: LinearExtrudeParams,
        /// The 2D profile — a group of the instantiation's children.
        child: Box<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `rotate_extrude(...)` node: revolves its 2D
    /// profile child about the Z axis into a 3D solid (upstream
    /// `src/core/RotateExtrudeNode.cc builtin_rotate_extrude`). Its resolved
    /// parameters live in [`RotateExtrudeParams`]; the child is always a
    /// [`CsgNode::Group`] wrapping the 2D profile.
    RotateExtrude {
        params: RotateExtrudeParams,
        /// The 2D profile — a group of the instantiation's children.
        child: Box<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `offset(...)` node: grows/shrinks its 2D
    /// profile child by `delta` — round joins in `r` mode (chamfer forced
    /// false), miter/bevel joins in `delta` mode (upstream `primitives.cc
    /// builtin_offset` / `OffsetNode`). The child is always a
    /// [`CsgNode::Group`] wrapping the instantiation's children (the
    /// transform child-always-Group invariant).
    Offset {
        /// The resolved `offset(...)` arguments.
        params: OffsetParams,
        /// The 2D profile — a group of the instantiation's children.
        child: Box<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `minkowski()` combinator: the Minkowski
    /// sum of its children — a children-carrying node exactly like
    /// [`CsgNode::Union`] (upstream `cgaladv.cc builtin_minkowski`).
    /// `convexity` is accepted-and-ignored (an upstream hint, not geometry).
    Minkowski {
        /// The operands, in source order.
        children: Vec<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `projection(cut=…)` node: projects its 3D
    /// child to the XY plane. `cut=true` is the z=0 cross-section region;
    /// `cut=false` (the default) is the shadow — the union of all
    /// cross-sections (upstream `ProjectionNode`). The child is always a
    /// [`CsgNode::Group`] wrapping the instantiation's children.
    Projection {
        /// The z=0 cross-section (true) vs the full shadow (false).
        cut: bool,
        /// The 3D solid(s) — a group of the instantiation's children.
        child: Box<CsgNode>,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `import(file=…)` node: an external STL loaded as a
    /// geometry leaf (upstream `import.cc builtin_import`/`ImportNode`). The
    /// resolved ABSOLUTE path is stored at eval time (resolved against the
    /// importing file's directory) so kernel resolution stays a pure file-open.
    /// `layer` (3MF/DXF-only) is warn-and-ignored for STL; `convexity` rides
    /// along as the upstream rendering hint. INERT for layer: the eval
    /// constructor warns on any `layer=` supplied for an `.stl` path.
    Import {
        /// The resolved absolute path to the STL file.
        path: PathBuf,
        /// Upstream's convexity hint: a finite number >= 1 as-is, anything
        /// else defaults to 1 (no upper clamp).
        convexity: u32,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `surface(file=…)` node: a `.dat`
    /// heightfield loaded as a solid (upstream `surface.cc`/`SurfaceNode`).
    /// The resolved ABSOLUTE path rides in the node (resolved against the
    /// source dir, identical to [`CsgNode::Import`]) so kernel resolution stays a pure
    /// file-open; `center`/`invert` mirror the upstream bools (only a Bool
    /// argument sets them), `convexity` is the accepted-and-stored rendering
    /// hint. Children contribute nothing (a geometry leaf, like import).
    Surface {
        /// The resolved absolute path to the `.dat` heightfield.
        path: PathBuf,
        /// Upstream `center`: shift the field by -(cols-1)/2, -(rows-1)/2.
        center: bool,
        /// Upstream `invert`: negate the heights (morphe applies it to `.dat`
        /// too — upstream is PNG-only; documented divergence).
        invert: bool,
        /// Upstream `convexity` hint, clamped to >= 1.
        convexity: u32,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A RESOLVED `text(...)` node: a 2D geometry leaf —
    /// the glyph outlines of [`TextParams`] as a Profile (upstream `TextNode`;
    /// primitives take no geometry children, so children contribute nothing).
    /// `linear_extrude` lifts the profile exactly as upstream's text flow.
    Text {
        /// The resolved parameters (text, size, alignment, spacing, fragments).
        params: TextParams,
        /// The instantiation's modifier prefixes (`!`/`#`/`%`).
        modifiers: Modifiers,
    },
    /// A placeholder for a statement the evaluator does not resolve:
    /// nothing (arguments included) is evaluated for it.
    Unresolved { kind: String, span: Span },
}

/// The resolved `text(...)` horizontal alignment (upstream `halign`:
/// "left" | "center" | "right" — default left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextHalign {
    /// The run starts at x = 0 (default).
    Left,
    /// The run is centered on x = 0.
    Center,
    /// The run ends at x = 0.
    Right,
}

/// The resolved `text(...)` vertical alignment (upstream `valign`:
/// "baseline" | "top" | "center" | "bottom" — default baseline; metrics from
/// the font's ascender/descender where upstream uses them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextValign {
    /// The glyph baseline sits on y = 0 (default).
    Baseline,
    /// The ascender line sits on y = 0.
    Top,
    /// The (ascender+descender)/2 midline sits on y = 0.
    Center,
    /// The descender line sits on y = 0.
    Bottom,
}

/// Resolved parameters of a [`CsgNode::Text`] leaf (upstream
/// `text.cc builtin_text` / `FontCache`). Only the supported set survives:
/// `font` is resolved at eval (an unknown family warns + falls back to the
/// bundled one, so the node carries no font name) and `direction` degrades to
/// ltr (rtl/ttb warn + fall back — not yet supported); `language`/`script`/`tp` are
/// warn-and-ignored (upstream Parameters). The `$fn`/`$fa`/`$fs` trio drives
/// the Bézier flattening segment count (kernel-side).
#[derive(Debug, Clone, PartialEq)]
pub struct TextParams {
    /// The string to render (upstream `text`, default empty — no ink).
    pub text: String,
    /// Glyph size in model units (upstream `size`, default 10; the em scale).
    pub size: f64,
    /// Horizontal alignment of the whole run.
    pub halign: TextHalign,
    /// Vertical alignment of the whole run.
    pub valign: TextValign,
    /// Inter-glyph spacing multiplier (upstream `spacing`, default 1.0).
    pub spacing: f64,
    /// The call-site `$fn`/`$fa`/`$fs` trio for curve flattening.
    pub fragments: Fragments,
}

/// A 4×4 affine transform matrix in **row-major** order (`m[row][col]`), the
/// analogue of upstream `Transform3d` (`geometry/linalg.h`, an Eigen affine
/// transform). Column 3 is the translation; the bottom row is `[0, 0, 0, 1]`
/// for every affine built here. `translate`/`rotate`/`scale`/
/// `mirror`/`multmatrix` all reduce to one of these, so the kernel
/// applies a single matrix per transform node.
pub type Affine = [[f64; 4]; 4];

/// The resolved payload of a [`CsgNode::Transform`]. Most transforms reduce
/// to a single 4×4 affine matrix ([`TransformKind::Affine`]); `resize` and
/// `color` are special — `resize` needs the child bounding box the kernel
/// computes, and `color` is non-geometric — so they keep distinct kinds.
///
/// Matrix construction is ported verbatim from upstream `src/core/`
/// (`TransformNode.cc`, `ColorNode.cc`, `CgalAdvNode.cc`) and
/// `src/utils/degree_trig.cc` (master, fetched 2026-07-03); see the builders
/// in [`crate::eval`] for the exact citations.
#[derive(Debug, Clone, PartialEq)]
pub enum TransformKind {
    /// `translate`/`rotate`/`scale`/`mirror`/`multmatrix` — a single 4×4
    /// affine matrix (upstream `TransformNode::matrix`, a `Transform3d`).
    Affine(Affine),
    /// `resize(newsize, auto)` — a DEFERRED-RESOLUTION transform. The actual
    /// per-axis scale factors depend on the child's bounding box, which only
    /// the kernel can compute, so the resolved parameters are
    /// stored verbatim (upstream `CgalAdvNode` `newsize`/`autosize`). A
    /// `newsize[i]` of `0` leaves axis `i` at its natural size (unless
    /// `auto[i]`); `auto[i]` scales axis `i` to preserve the aspect ratio of
    /// a resized axis.
    Resize { newsize: [f64; 3], auto: [bool; 3] },
    /// `color(c, alpha)` — a NON-GEOMETRIC transform: RGBA in `0.0..=1.0`
    /// (upstream `ColorNode::color`, a `Color4f`). The default sentinel
    /// `[-1, -1, -1, -1]` means "inherit" (no channel supplied). STL export
    /// ignores color, but the node is kept for fidelity with other exporters
    /// and preview.
    Color([f32; 4]),
}

/// The `$fn`/`$fa`/`$fs` fragment controls resolved at an extrude call site,
/// stored verbatim as upstream `LinearExtrudeNode`/`RotateExtrudeNode` keep
/// them (via `CurveDiscretizer`). The final slice/segment count depends on the
/// 2D profile's extent (max radius), which only the kernel can
/// measure — like [`TransformKind::Resize`]'s bounding-box dependency — so the
/// resolved trio is stored here rather than a single count. Each field is the
/// call-site named argument if present, else the dynamically-scoped special
/// variable, else the upstream `Builtins` seed (`$fn = 0`, `$fs = 2`,
/// `$fa = 12`). A positive `fn_` fixes the count independent of radius;
/// `fn_ = 0` defers to the `$fa`/`$fs` limits (upstream
/// `get_fragments_from_r`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fragments {
    /// `$fn` — a fixed facet count when positive, else `0`.
    pub fn_: f64,
    /// `$fs` — minimum arc-segment size.
    pub fs: f64,
    /// `$fa` — minimum arc-segment angle (degrees).
    pub fa: f64,
}

impl Fragments {
    /// This trio clamped the way upstream's `CurveDiscretizer` does
    /// (`$fn >= 0`, `$fs`/`$fa >= F_MINIMUM`) — a thin wrapper over the
    /// single shared [`clamp_fragment_vars`](crate::eval::clamp_fragment_vars).
    #[must_use]
    pub fn clamped(self) -> Fragments {
        let (fn_, fs, fa) = crate::eval::clamp_fragment_vars(self.fn_, self.fs, self.fa);
        Fragments { fn_, fs, fa }
    }
}

/// Resolved parameters of a [`CsgNode::Offset`] (upstream
/// `OffsetNode`, `primitives.cc builtin_offset`). Upstream precedence,
/// verbatim: a defined `r` WINS over `delta` and forces `chamfer = false`
/// (round joins); otherwise `delta` (+ `chamfer`) applies (miter at
/// chamfer=false, bevel at chamfer=true); when NEITHER `r` nor `delta` is
/// given, upstream warns and uses `delta = 100000`. `$fn`/`$fa`/`$fs`
/// resolve at the call site (the round-join arcs) via the standard
/// [`Fragments`] trio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OffsetParams {
    /// The offset distance — positive grows, negative shrinks. Carries
    /// `r`'s value in r-mode (the modes share one distance field, matching
    /// upstream's single `node->delta`).
    pub delta: f64,
    /// The join style, derived at eval from the argument PRECEDENCE (r-mode
    /// → round arcs; delta → miter; delta+chamfer → bevel). Upstream's
    /// stored `chamfer` bool cannot distinguish r-mode-round from
    /// delta-miter on its own, so the resolved join carries the mode
    /// explicitly and `chamfer` survives only for upstream-shape fidelity.
    pub join: OffsetJoin,
    /// Upstream's `chamfer` flag verbatim (`join == Bevel` ⇔ `chamfer`).
    pub chamfer: bool,
    /// The call-site `$fn`/`$fa`/`$fs` trio for the round-join arcs.
    pub fragments: Fragments,
}

/// The resolved `offset(...)` join style: which corner
/// treatment the kernel applies — the r-mode arcs, the delta-mode sharp
/// corner, or the delta+chamfer bevel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetJoin {
    /// `r`-mode: rounded corners (arc facets from `$fn`/`$fa`/`$fs`).
    Round,
    /// `delta`-mode: mitered (sharp) corners.
    Miter,
    /// `delta` + `chamfer = true`: beveled corners.
    Bevel,
}

/// Resolved parameters of a [`CsgNode::LinearExtrude`] (upstream
/// `LinearExtrudeNode`, master, fetched 2026-07-03). `linear_extrude` sweeps
/// its 2D profile along +Z; the vector-direction `v` form is not yet
/// modeled, so `height` is the scalar Z extrusion length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearExtrudeParams {
    /// Extrusion height along Z (upstream `node->height[2]`), default `100`,
    /// clamped so a non-positive height becomes `0` (upstream `if
    /// (node->height[2] <= 0) node->height[2] = 0`).
    pub height: f64,
    /// Whether the solid is centred on Z (upstream `center`, applied only when
    /// the argument is a bool); default `false`.
    pub center: bool,
    /// Twist in degrees applied top-to-bottom (upstream `twist`), default `0`.
    pub twist: f64,
    /// Explicit slice count when supplied and `>= 1` (upstream `has_slices` +
    /// `slices`); `None` lets the kernel derive the count from `twist` and
    /// [`Fragments`].
    pub slices: Option<u32>,
    /// Per-axis top scale `[x, y]` (upstream `scale_x`/`scale_y`), default
    /// `[1, 1]`; a scalar sets both. Negative components clamp to `0`.
    pub scale: [f64; 2],
    /// Rendering convexity hint (upstream `getPositiveInt`), default `1`.
    pub convexity: u32,
    /// Resolved `$fn`/`$fa`/`$fs` — see [`Fragments`].
    pub fragments: Fragments,
}

/// Resolved parameters of a [`CsgNode::RotateExtrude`] (upstream
/// `RotateExtrudeNode`, master, fetched 2026-07-03). `rotate_extrude` revolves
/// its 2D profile about the Z axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotateExtrudeParams {
    /// Sweep angle in degrees (upstream `angle`): default `360`; a supplied
    /// angle in `(-360, 360]` is used as-is, anything outside snaps to `360`.
    pub angle: f64,
    /// Start angle in degrees (upstream `start`): when `angle` is supplied the
    /// default is `0`, otherwise `180` (the legacy full-revolution start); an
    /// explicit `start` overrides either default.
    pub start: f64,
    /// Rendering convexity hint (upstream `max(2, (int)convexity)`), default
    /// `2`.
    pub convexity: u32,
    /// Resolved `$fn`/`$fa`/`$fs` — see [`Fragments`].
    pub fragments: Fragments,
}

/// A resolved primitive geometry leaf — the payload of
/// [`CsgNode::Primitive`]. Each variant maps to one upstream
/// `src/core/primitives.cc` node (master, fetched 2026-07-03), storing the
/// resolved numeric parameters and, where relevant, the segment count
/// already derived from the call's `$fn`/`$fa`/`$fs` via
/// [`crate::eval::resolve_fragments`] (upstream `get_fragments_from_r` /
/// `CurveDiscretizer::getCircularSegmentCount`). Argument parsing, defaults
/// and `d`-over-`r` precedence match upstream exactly (see
/// [`crate::eval`]'s primitive constructors).
///
/// 2D (`Square`/`Circle`/`Polygon`) and 3D (`Cube`/`Sphere`/`Cylinder`/
/// `Polyhedron`) leaves are kept distinct — the kernel treats them
/// differently — and can be told apart with [`Primitive::is_2d`].
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// 3D `cube(size, center)`. `size` is the resolved `[x, y, z]` (each
    /// upstream `CubeNode` component defaults to 1). `center` shifts the box
    /// to be origin-centred.
    Cube { size: [f64; 3], center: bool },
    /// 3D `sphere(r)`. `fragments` is the resolved circular segment count
    /// `get_fragments_from_r(r)`; upstream derives both the slice count and
    /// the ring count `(fragments + 1) / 2` from this single value.
    Sphere { r: f64, fragments: u32 },
    /// 3D `cylinder(h, r1, r2, center)` — a cone when `r1 != r2`.
    /// `fragments` is resolved from `max(r1, r2)` (upstream
    /// `getCircularSegmentCount(fmax(r1, r2))`).
    Cylinder {
        h: f64,
        r1: f64,
        r2: f64,
        center: bool,
        fragments: u32,
    },
    /// 2D `square(size, center)`. `size` is the resolved `[x, y]` (each
    /// defaults to 1).
    Square { size: [f64; 2], center: bool },
    /// 2D `circle(r)`. `fragments` is `get_fragments_from_r(r)`.
    Circle { r: f64, fragments: u32 },
    /// 2D `polygon(points, paths, convexity)`. `points` are `[x, y]`
    /// coordinates; `paths` is `None` when the argument is omitted or undef
    /// (the whole point list is one outline) and `Some(index-lists)` when
    /// supplied. `convexity` is clamped to a minimum of 1.
    Polygon {
        points: Vec<[f64; 2]>,
        paths: Option<Vec<Vec<usize>>>,
        convexity: u32,
    },
    /// 3D `polyhedron(points, faces, convexity)`. `points` are `[x, y, z]`
    /// coordinates; `faces` are index lists (each kept only when it has 3+
    /// in-bounds indices). Accepts the legacy `triangles` alias for `faces`
    /// (see the constructor). `convexity` is clamped to a minimum of 1.
    Polyhedron {
        points: Vec<[f64; 3]>,
        faces: Vec<Vec<usize>>,
        convexity: u32,
    },
}

impl Primitive {
    /// Whether this primitive produces 2D geometry (`square`/`circle`/
    /// `polygon`). The kernel treats 2D and 3D leaves
    /// differently, so it keys off this.
    pub fn is_2d(&self) -> bool {
        matches!(
            self,
            Primitive::Square { .. } | Primitive::Circle { .. } | Primitive::Polygon { .. }
        )
    }
}
