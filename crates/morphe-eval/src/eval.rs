//! Expression evaluation with user-defined functions and statement
//! evaluation with user-defined module instantiation and `children()`
//! support.
//!
//! Semantics are ported from upstream openscad `src/core/Expression.cc`
//! (`Range`/`TernaryOp`/`MemberLookup`/`Let`/`FunctionCall`),
//! `Arguments.cc`, `Parameters.cc`, `LocalScope.cc`, `UserModule.cc`,
//! `Children.cc`, `control.cc`, `ScopeContext.cc`, `ModuleInstantiation.cc`
//! and `parser.y` (master, fetched 2026-07-02):
//!
//! - Operations that yield an undef *with a reason* surface that reason as
//!   a warning (`Expression::checkUndef`); indexing does not
//!   (`ArrayLookup::evaluate` has no check).
//! - `&&`/`||` short-circuit over `toBool()` and always produce a bool;
//!   the ternary evaluates only the taken branch (`TernaryOp::evaluateStep`).
//! - Ranges coerce begin/end (then step, default 1.0) to numbers; a
//!   non-number warns `Unable to convert ...` and yields plain undef
//!   (`Range::evaluate`).
//! - `let` binds sequentially in a fresh frame; duplicate names are ignored
//!   with a warning (first wins — unlike scope bodies, which are last-wins)
//!   and unnamed bindings warn (`Let::doSequentialAssignment`).
//! - Function calls: arguments evaluate in the *calling* context
//!   (`Arguments.cc`), parameters bind per `Parameters.cc`
//!   `parse_without_defaults` (positional args fill parameters in order,
//!   named args match parameter names — extras warn, with unexpected named
//!   arguments still bound), and defaults evaluate in the *defining*
//!   context only — earlier parameters are NOT visible to later defaults.
//!   Neither are same-call `$`-arguments: upstream evaluates defaults
//!   before the `Parameters` object's `ContextFrameHandle` puts the
//!   argument frame on the special-variable stack (`Parameters::parse`
//!   defaults loop precedes the `Parameters` constructor; verified against
//!   the OpenSCAD 2021.01 binary: `function f(a = $fn) = a; f($fn = 12)`
//!   yields the outer `$fn`, not 12). Defaults DO see `$`-variables bound
//!   in the caller's dynamic chain (`$fn = 12; f()` yields 12). Missing
//!   parameters bind plain undef (no outer fall-through). The body
//!   evaluates in a frame whose lexical parent is the definition site and
//!   whose dynamic parent is the caller, so `$`-variables propagate along
//!   the call chain (`apply_config_variables`).
//! - Definitions live in per-frame maps looked up lazily along the lexical
//!   chain (`LocalScope.cc` + `Context::lookup_function`/`lookup_module`),
//!   so calls textually before a definition resolve, a redefinition
//!   silently replaces, and nested definitions are visible only inside
//!   their defining scope.
//! - Statement evaluation ([`Evaluator::eval_program`] /
//!   [`Evaluator::eval_stmt`]) produces a [`CsgNode`] tree, the analogue
//!   of upstream's `AbstractNode` tree. Scope bodies evaluate like
//!   upstream `ScopeContext::init` + `LocalScope::instantiateModules`:
//!   definitions first, then merged assignments, then instantiations in
//!   order. Braced blocks are scope-transparent (`parser.y` adds their
//!   contents to the enclosing scope), and `*`-disabled instantiations
//!   never evaluate at all (`parser.y` deletes them at parse time).
//! - User modules instantiate per `UserModule::instantiate`: the body runs
//!   in a frame whose lexical parent is the definition site, parameters
//!   bind like function parameters, `$children` binds lexically to the
//!   instantiation's child count before parameters apply
//!   (`UserModuleContext`), and the body's nodes wrap in a
//!   [`CsgNode::Group`]. The instantiation's children evaluate lazily in
//!   the *caller's* context when `children()` selects them (`Children.cc`),
//!   while `$`-variables keep flowing dynamically through the module body.
//!   The `$parent_modules` seed (`UserModuleContext` binds it right after
//!   `$children`, from the `StaticModuleNameStack`) is deferred to the
//!   special-variables support alongside the other special-variable seeds
//!   (`$fn`/`$fa`/`$fs`/`$t`/...).
//! - `echo(...) expr` / `assert(...) expr` expressions (`Expression.cc
//!   Echo`/`Assert`) perform their step and yield the following expression
//!   (undef without one); in a function body that expression stays in tail
//!   position. A failed assert expression records an [`EvalError`] and
//!   yields undef.
//! - `echo`/`assert` are instantiation-time builtins (`control.cc`): echo
//!   formats its arguments via `Value::toEchoString` onto the console
//!   sink; a failed assert records an [`EvalError`] (upstream throws an
//!   `AssertionFailedException` that aborts the whole evaluation — here
//!   only the assert's subtree is dropped) and both wrap any children in a
//!   group. Builtin modules without a typed mapping stay unresolved
//!   [`CsgNode::Call`] nodes (see [`Evaluator::eval_instantiation`]).
//! - Control flow (`control.cc`): `if` coerces its
//!   condition via `toBool` and instantiates only the taken branch's scope
//!   — in a fresh scope over the calling context — as a group; a false
//!   condition without an else produces no node. `for` and
//!   `intersection_for` iterate their bindings per `Expression.cc
//!   doForEach`: ranges (guarded at [`MAX_FOR_RANGE_STEPS`]), vectors,
//!   strings (characters); undef iterates zero times, anything else once;
//!   multiple bindings nest cartesian with later bindings seeing earlier
//!   loop variables. Each iteration binds its loop variable in a fresh
//!   frame and instantiates the body in one more fresh scope, so loop
//!   variables never leak while `$`-variables keep flowing dynamically.
//!   `for` always yields a [`CsgNode::Group`] (even empty);
//!   `intersection_for` yields a [`CsgNode::Intersection`]. Modifier
//!   prefixes carry onto the produced node.
//! - Recursion: upstream guards function and module recursion with a
//!   native stack probe (`StackCheck`, throwing `RecursionException` —
//!   "Recursion detected calling function/module '%1$s'"). Here explicit
//!   counters do that job: user-function calls and module instantiations
//!   share the [`MAX_CALL_DEPTH`] cap, and module instantiations are further
//!   capped at [`MAX_MODULE_DEPTH`]. Exceeding a cap records an
//!   [`EvalError`] carrying the call's span and short-circuits (undef for
//!   functions, no node for modules). Function calls in tail position —
//!   what remains of a body after peeling ternaries and `let`s — do not
//!   recurse at all: like upstream `FunctionCall::evaluate`
//!   (`src/core/Expression.cc`), they loop, bounded by the
//!   [`MAX_TAIL_CALLS`] counter, so `n == 0 ? acc : f(n - 1, acc + 1)` runs
//!   a million deep. Two more guards, [`MAX_EXPR_DEPTH`] and
//!   [`MAX_INST_DEPTH`], bound raw expression and statement nesting the same
//!   way (upstream's `StackCheck` covers these too), so a pathological AST
//!   cannot overflow the native stack.
//! - Definition sites are pinned via [`ScopeStack::retain`] when a
//!   function/module is registered in a persistent scope, so a captured
//!   [`FrameId`] can never be silently aliased by later frame reuse.
//!   Module-body and loop/if-body definitions are NOT pinned: they are
//!   reachable only while the instantiation (or iteration) is in flight
//!   (those bodies export nothing), so
//!   [`Evaluator::instantiate_user_module`] and
//!   [`Evaluator::eval_transient_body`] drop them with the frame on exit
//!   and the arena stays flat across repeated instantiations/iterations.
//! - Function literals (`Expression.cc FunctionDefinition`) evaluate to
//!   [`Value::Function`] closing over the current frame, which is pinned
//!   ([`ScopeStack::retain`]) so the closure keeps working after the frame
//!   is popped — and sees bindings made in it later, as upstream's shared
//!   context does (a literal can call itself through the variable it is
//!   assigned to). A call `f(...)` resolves `f` frame by frame: definitions,
//!   then a variable holding a function, then `use`d libraries, then the
//!   builtins (`Context::lookup_function`, `ContextFrame::
//!   lookup_local_function`); any other callee expression must evaluate to
//!   a function. Calls through function values take part in tail-call
//!   elimination like named ones. Pinning means every evaluated literal
//!   keeps its frame alive until the evaluator is dropped.
//! - `include <file>` / `use <file>` (`src/core/lexer.l includefile`,
//!   `SourceFile.cc`, `FileContext.cc`, `parsersettings.cc
//!   find_valid_path`): includes are spliced into the AST before
//!   evaluation — textual insertion, as upstream's lexer does — while a
//!   `use`d file is evaluated once in its own isolated top-level frame and
//!   only its function/module definitions become visible to lookups that
//!   reach the using scope. Each loaded file is parsed at its own span base
//!   ([`crate::include`]), so errors inside it carry its location in
//!   [`EvalError::origin`]. Targets must lie under the source directory or
//!   a library directory (see [`Evaluator::set_library_paths`]). The same
//!   span map tells `import()`/`surface()` which file a call is written in,
//!   so their relative paths resolve next to that file, as upstream's
//!   `lookup_file` does with the instantiation's location.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use morphe_lang::ast::{Arg, BinaryOp, Expr, ExprKind, Modifiers, Param, Stmt, StmtKind, UnaryOp};
use morphe_lang::lexer::Span;

use crate::csg::{
    Affine, CsgNode, Fragments, LinearExtrudeParams, OffsetJoin, OffsetParams, Primitive,
    RotateExtrudeParams, TextParams, TransformKind,
};
pub use crate::include::ErrorOrigin;
use crate::include::{
    library_paths_from_env, read_capped_text, SourceMap, MAX_INCLUDE_BYTES, MAX_INCLUDE_DEPTH,
};
use crate::scope::{is_config_variable, merge_assignments, FrameId, ScopeStack};
use crate::value::{Range, Value};

/// Maximum depth of non-tail user-function calls (module instantiations
/// count towards it too).
///
/// Evaluation runs on a [`PIPELINE_STACK_BYTES`](crate::stack::PIPELINE_STACK_BYTES)
/// (256 MiB) thread, and the guards are sized together so that tripping all
/// of them at once — the worst case — stays under half of it in an
/// unoptimised debug build. Measured debug costs: ~5 KiB of native stack
/// per call (`h(g(n - 1))`, one expression level each), ~3.8 KiB per
/// builtin-call expression level, ~8.5 KiB per statement level. So at most
/// ~40 MiB for 8000 calls, ~85 MiB for [`MAX_EXPR_DEPTH`] levels mixing
/// calls and builtins, plus ~13 MiB of module recursion at
/// [`MAX_MODULE_DEPTH`]: ~91 MiB measured at worst, a margin of more than 2x
/// (pinned by `tests/depth_guards.rs`, which runs these worst cases on half
/// the pipeline stack).
pub const MAX_CALL_DEPTH: usize = 8_000;

/// Maximum user-module instantiation depth (the `$parent_modules` stack).
///
/// Module recursion nests two or three statement levels per call (the
/// instantiation plus e.g. an `if` and a transform), so this keeps it under
/// [`MAX_INST_DEPTH`] — recursion reports `recursion limit exceeded calling
/// module` rather than the generic nesting error — and keeps the CSG trees
/// it builds within the kernel's resolve-depth cap.
pub const MAX_MODULE_DEPTH: usize = 512;

/// Maximum number of user-function calls one call chains through tail
/// calls (the initial call included) before evaluation reports `Recursion
/// detected calling function` — upstream's `recursion_depth` counter in
/// `FunctionCall::evaluate` (`src/core/Expression.cc`).
pub const MAX_TAIL_CALLS: usize = 1_000_000;

/// Maximum expression nesting depth: a native-stack guard for pathological
/// ASTs (upstream relies on `StackCheck` stack probing instead). Ordinary
/// recursion nests two or three expression levels per call (`1 + f(n - 1)`
/// is two), so this is 2.5x [`MAX_CALL_DEPTH`]: such recursion still
/// reaches the call cap. See [`MAX_CALL_DEPTH`] for the stack budget.
pub const MAX_EXPR_DEPTH: usize = 20_000;

/// Maximum statement (module-instantiation) nesting depth: the statement
/// analogue of [`MAX_EXPR_DEPTH`]. `if`/`for` bodies recurse natively
/// through the evaluator without consuming call depth (upstream's
/// `StackCheck` probes the native stack here too), so a pathological AST
/// of nested control statements needs its own guard. Sized above the 1536
/// statement levels that legal [`MAX_MODULE_DEPTH`]-deep module recursion
/// through an `if` + `for` pair nests (three statements per call, so the
/// recursion guard keeps firing first), and at the kernel's resolve-depth
/// cap, `MAX_RESOLVE_DEPTH` (also 2048).
pub const MAX_INST_DEPTH: usize = 2048;

/// A hard evaluation error: evaluation records it and short-circuits the
/// offending expression to undef (warnings, by contrast, live in the
/// [`ScopeStack`] sink).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    pub message: String,
    /// Byte span in the MAIN source. For an error inside an included or
    /// used file this is the span of the main file's `include`/`use`
    /// statement that brought the file in, so a consumer that only knows
    /// the main source still points somewhere sensible; [`EvalError::origin`]
    /// carries the precise location.
    pub span: Span,
    pub kind: EvalErrorKind,
    /// The file and position the error actually occurred at, when that is
    /// an included or used file (`None` for the main source).
    pub origin: Option<ErrorOrigin>,
}

/// The category of an [`EvalError`], carrying any structured payload the CLI
/// and AI chat loop consume programmatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalErrorKind {
    /// Any ordinary hard error (recursion limit, failed assert, depth guard);
    /// the human-readable text lives in [`EvalError::message`].
    General,
    /// A construct morphe parses but does not yet support (see
    /// [`UNSUPPORTED_MODULES`]): `construct` is the bare name so callers can
    /// route around it, `hint` an actionable workaround.
    Unsupported { construct: String, hint: String },
}

impl EvalError {
    /// A [`EvalErrorKind::General`] error carrying a ready-made message.
    fn general(message: impl Into<String>, span: Span) -> EvalError {
        EvalError {
            message: message.into(),
            span,
            kind: EvalErrorKind::General,
            origin: None,
        }
    }

    /// 1-based `(line, col)` of the error: inside [`EvalError::origin`]'s
    /// file when set, otherwise in `main_source` (the text the evaluated
    /// AST was parsed from).
    #[must_use]
    pub fn line_col(&self, main_source: &str) -> (usize, usize) {
        match &self.origin {
            Some(origin) => (origin.line, origin.col),
            None => morphe_lang::lexer::line_col(main_source, self.span.start),
        }
    }

    /// The message, prefixed with `file.scad:line:col: ` when the error comes
    /// from an included/used file. Callers that mark errors in the MAIN
    /// buffer (an editor gutter, the AI tool result) keep `span` pointing at
    /// the `include`/`use` statement and show this message, so the real
    /// location is not lost. Only the file NAME is shown — never the full
    /// path — so absolute paths don't leak into the AI channel.
    #[must_use]
    pub fn located_message(&self) -> String {
        match &self.origin {
            Some(origin) => format!(
                "{}:{}:{}: {}",
                origin_name(origin),
                origin.line,
                origin.col,
                self.message
            ),
            None => self.message.clone(),
        }
    }

    /// A [`EvalErrorKind::Unsupported`] error for a not-yet-supported construct.
    /// The message is stable and greppable — `"unsupported: {construct}
    /// (not yet implemented in morphe)"` — while `construct`/`hint` stay structured for
    /// programmatic use by the CLI and AI loop.
    pub fn unsupported(
        construct: impl Into<String>,
        span: Span,
        hint: impl Into<String>,
    ) -> EvalError {
        let construct = construct.into();
        let hint = hint.into();
        EvalError {
            message: format!("unsupported: {construct} (not yet implemented in morphe)"),
            span,
            kind: EvalErrorKind::Unsupported { construct, hint },
            origin: None,
        }
    }
}

/// File name shown for an included/used file in diagnostics: the name only,
/// never the full path, so absolute paths don't leak into the AI channel.
fn origin_name(origin: &ErrorOrigin) -> String {
    origin
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<included file>".to_string())
}

/// A non-fatal evaluation warning together with where it was raised
/// (upstream passes the instantiation's or expression's `Location` to most
/// `LOG(message_group::Warning, ...)` calls and prints it as `in file ...,
/// line ...`). Here the location is the innermost expression or statement
/// being evaluated when the warning was raised. Obtained from
/// [`Evaluator::eval_warnings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalWarning {
    pub message: String,
    /// Byte span in the MAIN source, like [`EvalError::span`]: for a warning
    /// inside an included or used file, the span of the main file's
    /// `include`/`use` that brought it in. `None` when the warning was
    /// raised outside any expression or statement.
    pub span: Option<Span>,
    /// The file and position the warning actually occurred at, when that is
    /// an included or used file (`None` for the main source).
    pub origin: Option<ErrorOrigin>,
}

impl EvalWarning {
    /// 1-based `(line, col)` of the warning: inside [`EvalWarning::origin`]'s
    /// file when set, otherwise in `main_source`; `None` when unlocated.
    #[must_use]
    pub fn line_col(&self, main_source: &str) -> Option<(usize, usize)> {
        match &self.origin {
            Some(origin) => Some((origin.line, origin.col)),
            None => self
                .span
                .map(|span| morphe_lang::lexer::line_col(main_source, span.start)),
        }
    }

    /// The warning's location as `file:line:col`: the included/used file's
    /// NAME (never its full path) when [`EvalWarning::origin`] is set,
    /// otherwise `main_name` — or just `line:col` when `main_name` is `None`
    /// (a consumer that already knows it is showing the main buffer).
    /// `None` when the warning has no location.
    #[must_use]
    pub fn location(&self, main_source: &str, main_name: Option<&str>) -> Option<String> {
        let (line, col) = self.line_col(main_source)?;
        Some(match (&self.origin, main_name) {
            (Some(origin), _) => format!("{}:{line}:{col}", origin_name(origin)),
            (None, Some(name)) => format!("{name}:{line}:{col}"),
            (None, None) => format!("{line}:{col}"),
        })
    }

    /// The message prefixed with its [location](EvalWarning::location), e.g.
    /// `lib.scad:3:5: Ignoring unknown variable 'x'` — or the bare message
    /// when the warning has no location. The counterpart of
    /// [`EvalError::located_message`] for string-only consumers.
    #[must_use]
    pub fn located_message(&self, main_source: &str, main_name: Option<&str>) -> String {
        match self.location(main_source, main_name) {
            Some(location) => format!("{location}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

/// A callable user function: a registered `function name(params) = expr;`
/// definition, or a function literal's value. `def_frame` (the definition
/// site, or the literal's enclosing scope) becomes the body frame's lexical
/// parent.
use crate::value::FunctionValue as UserFunction;

/// What a call expression's callee resolved to (upstream
/// `FunctionCall::evaluate_function_expression`).
enum Callee<'a> {
    /// A user function: a definition or a function value.
    User(Arc<UserFunction>),
    /// A bare name with no user function or function value behind it:
    /// dispatch to the builtin library (which warns if it is unknown).
    Builtin(&'a str),
    /// A non-name callee that did not evaluate to a function (already
    /// warned).
    NotCallable,
}

/// Outcome of one [`Evaluator::simplify_function_body`] pass.
enum BodyStep {
    /// The call finished with this value.
    Done(Value),
    /// The body ended in a call to this user function, whose call frame is
    /// now current: continue with its body.
    TailCall(Arc<UserFunction>),
}

/// A registered `module name(params) ...` definition.
#[derive(Debug)]
struct UserModule {
    /// Frame of the definition site — the instantiation frame's lexical
    /// parent (upstream `InstantiableModule::defining_context`).
    def_frame: FrameId,
    params: Vec<Param>,
    body: Stmt,
}

/// The children of one user-module instantiation (upstream `Children`):
/// the instantiation's child statements plus the caller frame they
/// evaluate against when `children()` selects them.
#[derive(Debug, Clone)]
struct ChildrenContext {
    stmts: Arc<Vec<Stmt>>,
    caller_frame: FrameId,
}

/// How definitions made directly in a scope body live (see the module
/// docs on definition-site pinning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    /// Definitions stay reachable after the body finishes evaluating (the
    /// top level, and children scopes re-instantiable via `children()`),
    /// so their frame is pinned via [`ScopeStack::retain`].
    Persistent,
    /// A user-module body: definitions die with the instantiation
    /// ([`Evaluator::instantiate_user_module`] drops them with the frame),
    /// and a literal assignment overwriting a parameter warns.
    ModuleBody,
    /// An `if` branch or `for` iteration body: not a module body, but its
    /// definitions are equally unreachable once the body finishes
    /// ([`Evaluator::eval_transient_body`] drops them with the frame).
    Transient,
}

/// Modules registered by upstream `Builtins::initialize` (master, fetched
/// 2026-07-02: register_builtin_group/csgops/transform/color/primitives/
/// surface/render/import/projection/cgaladv/offset/linear_extrude/
/// rotate_extrude/roof/text), minus the control modules handled in
/// [`Evaluator::eval_instantiation`]. Calls to these stay unresolved
/// [`CsgNode::Call`] nodes unless a dedicated arm maps them to a typed node.
const BUILTIN_MODULES: &[&str] = &[
    "group",
    "union",
    "difference",
    "intersection",
    "translate",
    "rotate",
    "scale",
    "mirror",
    "multmatrix",
    "resize",
    "color",
    "cube",
    "sphere",
    "cylinder",
    "polyhedron",
    "square",
    "circle",
    "polygon",
    "surface",
    "render",
    "import",
    "projection",
    "minkowski",
    "hull",
    "fill",
    "offset",
    "linear_extrude",
    "rotate_extrude",
    "roof",
    "text",
];

/// Module constructs morphe parses but does not yet support: naming one of
/// these records an [`EvalError::unsupported`] carrying the call's span and
/// an actionable workaround hint, so the CLI and AI chat loop can point at
/// the exact line and route around it — as opposed to a genuinely-unknown
/// module (a typo), which stays the "Ignoring unknown module" warning.
/// `hull()`/`resize()`/`fill()`/`render()`/`roof()` and every
/// primitive/transform/boolean stay resolvable.
///
/// The set is currently EMPTY: every construct evaluates. A future deferral
/// (e.g. roof/fill/render) must be added here deliberately; set-equality
/// tests elsewhere keep this list in sync.
pub const UNSUPPORTED_MODULES: &[&str] = &[];

/// The bundled text() font family eval knows about (the kernel embeds the
/// TTF; eval only needs the NAME to warn on unknown `font=` values). The
/// kernel's `text::FONT_FAMILY` must equal this — pinned by a kernel test.
pub const TEXT_FONT_FAMILY: &str = "Fira Sans";

/// An actionable hint for a not-yet-supported module. Every hint says the
/// construct is "not yet implemented" so the AI loop can key off it.
fn unsupported_module_hint(name: &str) -> &'static str {
    // Unreachable while UNSUPPORTED_MODULES is empty; a safe generic fallback
    // keeps the invariant for any construct added there later.
    let _ = name;
    "this construct is not yet implemented in morphe; rewrite it with supported primitives"
}

/// Upstream `RangeType::MAX_RANGE_STEPS`: a `children()` range with this
/// many or more values is rejected.
const MAX_CHILDREN_RANGE_STEPS: u32 = 10_000;

/// Upstream `Expression.cc doForEach`: a `for` range with this many or
/// more values warns and iterates zero times (the literal `1000000` in the
/// RANGE branch's `steps >= 1000000` guard). The same guard bounds
/// `LcEach::evalRecur`'s range splice.
const MAX_FOR_RANGE_STEPS: u32 = 1_000_000;

/// Upstream `Expression.cc LcForC::evaluate`: the C-style comprehension
/// loop aborts once its iteration counter reaches this limit (the literal
/// `counter++ == 1000000` guard, which logs `For loop counter exceeded
/// limit` and throws `LoopCntException`). Upstream still emits the element
/// produced on the tripping iteration before aborting, so the guard fires
/// after the body runs.
const MAX_FORC_ITERATIONS: u64 = 1_000_000;

/// Tree-walking evaluator over a [`ScopeStack`]: expressions to [`Value`]s,
/// statements to [`CsgNode`]s.
#[derive(Debug)]
pub struct Evaluator {
    scope: ScopeStack,
    /// Per-frame definition tables (upstream `LocalScope` maps), looked up
    /// along the lexical chain. Frames holding definitions are
    /// [retained](ScopeStack::retain), so their ids are never reused and
    /// stale entries can never alias a later frame.
    functions: HashMap<FrameId, HashMap<String, Arc<UserFunction>>>,
    modules: HashMap<FrameId, HashMap<String, Arc<UserModule>>>,
    /// Children contexts of in-flight user-module instantiations, keyed by
    /// the instantiation's frame. `children()` resolves to the nearest one
    /// along the lexical chain, the analogue of upstream
    /// `Context::user_module_children()` walking the context parent chain
    /// to the nearest `UserModuleContext` — a plain innermost-call stack
    /// would be wrong: in `module outer() inner() children();` the
    /// `children()` statement is *inner's child*, evaluated while inner's
    /// instantiation is in flight, yet it must select outer's children.
    children_contexts: HashMap<FrameId, ChildrenContext>,
    call_depth: usize,
    expr_depth: usize,
    inst_depth: usize,
    /// Depth of the in-flight user-module instantiation stack (upstream
    /// `StaticModuleNameStack`), the value `UserModuleContext` binds to
    /// `$parent_modules`. Counts only user-module instantiations, not
    /// function calls or builtin modules, so it is distinct from
    /// `call_depth`.
    module_stack_depth: usize,
    errors: Vec<EvalError>,
    /// A sticky fatal-abort latch: once set, all further expression
    /// evaluation short-circuits to undef so no usable value can be
    /// produced. Distinct from the localized [`EvalError`] path (a failed
    /// `assert` drops only its subtree; see the module docs): it is the
    /// analogue of upstream's `LoopCntException`, thrown by the C-style
    /// comprehension counter cap, which unwinds past every enclosing vector
    /// so the whole expression yields nothing. An unbounded loop cannot
    /// yield a usable partial, so this one error is fatal rather than local.
    aborted: bool,
    console: Vec<String>,
    /// The program source, when attached via [`Evaluator::set_source`]:
    /// lets diagnostics quote expression text by slicing spans (upstream
    /// pretty-prints the AST via `operator<<` instead).
    source: Option<String>,
    /// The importing file's DIRECTORY, when attached via
    /// [`Evaluator::set_source_path`]: the base against which `import()`
    /// and `surface()` resolve relative file paths — upstream resolves
    /// siblings of the importing .scad. `None` = no context (a string-only
    /// pipeline, e.g. an unsaved buffer).
    source_dir: Option<PathBuf>,
    /// The main file itself, when known: seeds the `include` cycle check.
    source_file: Option<PathBuf>,
    /// Library directories `include`/`use` search after the including
    /// file's directory (default: `OPENSCADPATH`).
    library_paths: Vec<PathBuf>,
    /// Every included/used file's text and the span base it was parsed at.
    source_map: SourceMap,
    /// Canonical paths of the files whose `include` expansion is in
    /// progress (upstream `openfilenames`), innermost last.
    including: Vec<PathBuf>,
    /// Nesting depth of in-progress `use` loads.
    use_depth: usize,
    /// Top-level frame of each loaded `use` library (by load order), plus
    /// the canonical-path index into it. A library whose load failed maps
    /// to `None` so the failure is reported once.
    libraries: Vec<Option<FrameId>>,
    library_index: HashMap<PathBuf, usize>,
    /// Libraries `use`d by the scope evaluated in a frame (upstream
    /// `SourceFile::usedlibs`, consulted by `FileContext` after the file's
    /// own definitions), in declaration order.
    file_uses: HashMap<FrameId, Vec<usize>>,
}

impl Evaluator {
    pub fn new() -> Evaluator {
        Evaluator {
            scope: ScopeStack::new(),
            functions: HashMap::new(),
            modules: HashMap::new(),
            children_contexts: HashMap::new(),
            call_depth: 0,
            expr_depth: 0,
            inst_depth: 0,
            module_stack_depth: 0,
            errors: Vec::new(),
            aborted: false,
            console: Vec::new(),
            source: None,
            source_dir: None,
            source_file: None,
            library_paths: library_paths_from_env(),
            source_map: SourceMap::default(),
            including: Vec::new(),
            use_depth: 0,
            libraries: Vec::new(),
            library_index: HashMap::new(),
            file_uses: HashMap::new(),
        }
    }

    /// Attach the program source so diagnostics can quote expression text
    /// (e.g. the failed condition in an assert message). Spans in the
    /// evaluated AST must index into this string.
    pub fn set_source(&mut self, source: impl Into<String>) {
        self.source = Some(source.into());
    }

    /// Attach the importing file's path: the evaluator records the file's
    /// PARENT directory as the base for relative-path resolution by the
    /// file-loading constructs (`import()`, `surface()` — siblings of the
    /// importing .scad, matching upstream), which read it via
    /// [`Evaluator::source_dir`].
    pub fn set_source_path(&mut self, path: Option<&Path>) {
        self.source_dir = path.and_then(|p| p.parent()).map(Path::to_path_buf);
        self.source_file = path.map(Path::to_path_buf);
    }

    /// Replace the library directories `include <…>` / `use <…>` search
    /// after the including file's own directory (initialised from the
    /// `OPENSCADPATH` environment variable by [`Evaluator::new`]). Files
    /// under these directories may be loaded even though they lie outside
    /// the source directory.
    pub fn set_library_paths(&mut self, paths: Vec<PathBuf>) {
        self.library_paths = paths;
    }

    /// The library directories `include`/`use` search.
    #[must_use]
    pub fn library_paths(&self) -> &[PathBuf] {
        &self.library_paths
    }

    /// The importing file's directory, when attached via
    /// [`Evaluator::set_source_path`]; `None` when the pipeline ran
    /// string-only (no context). The import/surface path constructors
    /// resolve against this. The dir is stored VERBATIM (no canonicalization
    /// — by design: `..` segments and symlinks pass through as given;
    /// containment and canonicalization happen at the file-loading
    /// boundary; a bare filename yields `Some("")` = cwd-relative sibling
    /// resolution).
    #[must_use]
    pub fn source_dir(&self) -> Option<&Path> {
        self.source_dir.as_deref()
    }

    /// Resolve `name` in the current scope; unknown names warn and yield
    /// undef.
    pub fn lookup(&mut self, name: &str) -> Value {
        self.scope.lookup(name)
    }

    /// Warnings recorded so far, in order (messages only; see
    /// [`Evaluator::eval_warnings`] for their locations).
    pub fn warnings(&self) -> &[String] {
        self.scope.warnings()
    }

    /// Warnings recorded so far, in order, with where each was raised. A
    /// warning inside an included or used file carries that file's position
    /// in [`EvalWarning::origin`] and the entry `include`/`use` statement's
    /// span, exactly like [`EvalError`]s.
    #[must_use]
    pub fn eval_warnings(&self) -> Vec<EvalWarning> {
        self.scope
            .warnings()
            .iter()
            .zip(self.scope.warning_spans())
            .map(|(message, &span)| {
                let (origin, span) = match span.and_then(|s| self.source_map.origin(s)) {
                    Some((origin, entry)) => (Some(origin), Some(entry)),
                    None => (None, span),
                };
                EvalWarning {
                    message: message.clone(),
                    span,
                    origin,
                }
            })
            .collect()
    }

    /// Hard errors recorded so far, in order.
    pub fn errors(&self) -> &[EvalError] {
        &self.errors
    }

    /// Console output recorded so far (`echo` lines), in order.
    pub fn console(&self) -> &[String] {
        &self.console
    }

    /// Evaluate a program's top-level statements against the root scope,
    /// returning the root group node (upstream instantiates the file's
    /// `LocalScope` into a root `GroupNode`).
    ///
    /// Runs on a dedicated [`PIPELINE_STACK_BYTES`](crate::stack::PIPELINE_STACK_BYTES)
    /// thread ([`with_large_stack`](crate::stack::with_large_stack)), so the
    /// evaluator's depth guards — not the caller's native stack (e.g. a
    /// 2 MiB `spawn_blocking` worker) — bound pathological recursion.
    pub fn eval_program(&mut self, stmts: &[Stmt]) -> CsgNode {
        crate::stack::with_large_stack(|| {
            let children = self.eval_top_level(stmts);
            self.localize_errors();
            CsgNode::Group {
                children,
                modifiers: Modifiers::default(),
            }
        })
    }

    /// Splice `include`d files into `stmts` (see [`Evaluator::expand_includes`])
    /// and evaluate the result as a persistent scope body.
    fn eval_top_level(&mut self, stmts: &[Stmt]) -> Vec<CsgNode> {
        if !has_include(stmts) {
            return self.eval_scope_body(stmts, BodyKind::Persistent, None);
        }
        let main = self
            .source_file
            .as_ref()
            .and_then(|p| p.canonicalize().ok());
        self.including = main.into_iter().collect();
        let expanded = self.expand_includes(stmts);
        self.including.clear();
        self.eval_scope_body(&expanded, BodyKind::Persistent, None)
    }

    /// Evaluate one statement in the current scope, returning the nodes it
    /// produces (none for assignments and definitions, which bind in the
    /// current frame). Runs on the large pipeline stack like
    /// [`Evaluator::eval_program`].
    pub fn eval_stmt(&mut self, stmt: &Stmt) -> Vec<CsgNode> {
        crate::stack::with_large_stack(|| {
            let nodes = self.eval_top_level(std::slice::from_ref(stmt));
            self.localize_errors();
            nodes
        })
    }

    /// Evaluate one scope body in the *current* frame (upstream
    /// `ScopeContext::init` + `LocalScope::instantiateModules`):
    ///
    /// 1. Braced blocks flatten into the scope — `parser.y` adds a block's
    ///    statements (`statement: '{' inner_input '}'` and `child_statement:
    ///    '{' child_statements '}'`) to the enclosing scope at parse time.
    /// 2. Definitions register in the current frame regardless of position
    ///    (upstream builds `LocalScope` maps at parse time); the frame is
    ///    pinned so its id can never be aliased by later frame reuse.
    /// 3. Assignments bind with last-wins-in-place semantics (`parser.y
    ///    handle_assignment`); in a module body, a *literal* assignment
    ///    overwriting an existing local warns `Parameter ... is overwritten
    ///    with a literal` (`ScopeContext::init` — parameters share the
    ///    module context frame with the body).
    /// 4. Instantiations evaluate in order — all of them, or only the
    ///    positions in `indices` (upstream
    ///    `LocalScope::instantiateModules(context, target, indices)`, used
    ///    by `children(...)` selection; duplicates instantiate again).
    ///    `*`-disabled instantiations don't exist (`parser.y` deletes them
    ///    at parse time), so they are excluded from positions too.
    fn eval_scope_body(
        &mut self,
        stmts: &[Stmt],
        kind: BodyKind,
        indices: Option<&[usize]>,
    ) -> Vec<CsgNode> {
        let mut flat = Vec::new();
        flatten_scope(stmts, &mut flat);

        for stmt in &flat {
            match &stmt.kind {
                StmtKind::FunctionDef { name, params, body } => {
                    // LocalScope::addFunction insert-or-assigns silently.
                    let def_frame = self.scope.current();
                    // Pin the frame so the captured id is never aliased by
                    // frame reuse — except in a module or transient
                    // (loop/if) body, whose definitions die with the frame
                    // (see instantiate_user_module / eval_transient_body);
                    // pinning those would leak one frame per
                    // instantiation/iteration.
                    if kind == BodyKind::Persistent {
                        self.scope.retain(def_frame);
                    }
                    self.functions.entry(def_frame).or_default().insert(
                        name.clone(),
                        Arc::new(UserFunction {
                            name: name.clone(),
                            def_frame,
                            params: Arc::new(params.clone()),
                            body: Arc::new(body.clone()),
                        }),
                    );
                }
                StmtKind::ModuleDef { name, params, body } => {
                    let def_frame = self.scope.current();
                    if kind == BodyKind::Persistent {
                        self.scope.retain(def_frame);
                    }
                    self.modules.entry(def_frame).or_default().insert(
                        name.clone(),
                        Arc::new(UserModule {
                            def_frame,
                            params: params.clone(),
                            body: (**body).clone(),
                        }),
                    );
                }
                // Includes were spliced in before evaluation
                // (`expand_includes`), as upstream's lexer does.
                StmtKind::Include { .. } => {}
                StmtKind::Use { path } => self.register_use(path, stmt.span),
                _ => {}
            }
        }

        let assignments: Vec<(&str, (&Expr, Span))> = flat
            .iter()
            .filter_map(|stmt| match &stmt.kind {
                StmtKind::Assign { name, value } => Some((name.as_str(), (value, stmt.span))),
                _ => None,
            })
            .collect();
        // `merge_assignments` reports one warning per repeated assignment,
        // in source order; pair each with that assignment's span.
        let mut seen = HashSet::new();
        let repeat_spans: Vec<Span> = assignments
            .iter()
            .filter(|(name, _)| !seen.insert(*name))
            .map(|(_, (_, span))| *span)
            .collect();
        let (merged, warnings) = merge_assignments(assignments);
        for (warning, span) in warnings.into_iter().zip(repeat_spans) {
            self.scope.warn_at(span, warning);
        }
        for (name, (expr, span)) in merged {
            if kind == BodyKind::ModuleBody
                && is_literal(expr)
                && self.scope.lookup_local(name).is_some()
            {
                self.scope.warn_at(
                    span,
                    format!("Parameter '{name}' is overwritten with a literal"),
                );
            }
            let value = self.eval_nested(expr);
            self.scope.set(name, value);
        }

        let instantiations: Vec<&Stmt> = flat.into_iter().filter(|s| is_instantiation(s)).collect();
        let mut nodes = Vec::new();
        match indices {
            None => {
                for stmt in instantiations {
                    nodes.extend(self.eval_instantiation(stmt));
                }
            }
            Some(indices) => {
                for &index in indices {
                    if let Some(stmt) = instantiations.get(index) {
                        nodes.extend(self.eval_instantiation(stmt));
                    }
                }
            }
        }
        nodes
    }

    /// Evaluate one module instantiation (upstream
    /// `ModuleInstantiation::evaluate`): resolve the name — user-defined
    /// modules along the lexical chain first (`Context::lookup_module`),
    /// then builtins — and instantiate. Unknown names warn `Ignoring
    /// unknown module ...` and produce no node.
    ///
    /// Nesting deeper than [`MAX_INST_DEPTH`] records an [`EvalError`]
    /// carrying the offending statement's span and short-circuits to no
    /// node — `if`/`for` bodies recurse natively through here without
    /// consuming call depth, so this guard (not [`MAX_CALL_DEPTH`])
    /// protects the native stack from pathological statement nesting.
    fn eval_instantiation(&mut self, stmt: &Stmt) -> Option<CsgNode> {
        if self.inst_depth >= MAX_INST_DEPTH {
            self.errors
                .push(EvalError::general("statement nesting too deep", stmt.span));
            return None;
        }
        self.inst_depth += 1;
        let outer_span = self.scope.set_warn_span(Some(stmt.span));
        let node = self.eval_instantiation_kind(stmt);
        self.scope.set_warn_span(outer_span);
        self.inst_depth -= 1;
        node
    }

    fn eval_instantiation_kind(&mut self, stmt: &Stmt) -> Option<CsgNode> {
        match &stmt.kind {
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
                modifiers,
            } => self.builtin_if(cond, then_branch, else_branch.as_deref(), *modifiers),
            StmtKind::ModuleCall {
                name,
                args,
                modifiers,
                children,
            } => {
                if let Some(module) = self.lookup_module(name) {
                    return self.instantiate_user_module(
                        name, &module, args, children, *modifiers, stmt.span,
                    );
                }
                match name.as_str() {
                    "children" => self.builtin_children(args, children, *modifiers),
                    "echo" => self.builtin_echo(args, children, *modifiers),
                    "assert" => self.builtin_assert(args, children, *modifiers, stmt.span),
                    "for" => self.builtin_for(args, children, *modifiers, false),
                    "intersection_for" => self.builtin_for(args, children, *modifiers, true),
                    // Statement-`let` (upstream control.cc builtin_let): bind
                    // each named argument in a fresh child scope, then
                    // instantiate the body in a scope over that binding frame.
                    "let" => self.builtin_let(args, children, *modifiers),
                    // The seven geometry primitives map to
                    // typed CsgNode::Primitive leaves with fragment counts
                    // already resolved from the call's $fn/$fa/$fs. They are
                    // leaves: upstream primitive nodes take no geometric
                    // children, so any children are ignored (not evaluated).
                    // This arm must precede the BUILTIN_MODULES arm, which
                    // would otherwise leave them unresolved Call nodes.
                    "cube" | "sphere" | "cylinder" | "square" | "circle" | "polygon"
                    | "polyhedron" => {
                        let primitive = self.eval_primitive(name, args);
                        Some(CsgNode::Primitive {
                            primitive,
                            modifiers: *modifiers,
                        })
                    }
                    // The seven transform modules map to
                    // typed CsgNode::Transform nodes: their parameters resolve
                    // to a TransformKind (a 4x4 affine matrix for translate/
                    // rotate/scale/mirror/multmatrix; resize/color keep their
                    // own kinds), and the instantiation's children evaluate in
                    // a fresh scope over the calling context (upstream
                    // Children::scopeContext) and wrap in a group that becomes
                    // the transform's child. Arguments evaluate BEFORE the
                    // children (in the calling scope), matching upstream
                    // Parameters::parse preceding children.instantiate. This
                    // arm must precede the BUILTIN_MODULES arm, which would
                    // otherwise leave them unresolved Call nodes.
                    "translate" | "rotate" | "scale" | "mirror" | "multmatrix" | "resize"
                    | "color" => {
                        let kind = self.eval_transform(name, args);
                        self.scope.push_scope();
                        let child_nodes =
                            self.eval_scope_body(children, BodyKind::Persistent, None);
                        self.scope.pop();
                        Some(CsgNode::Transform {
                            kind,
                            child: Box::new(CsgNode::Group {
                                children: child_nodes,
                                modifiers: Modifiers::default(),
                            }),
                            modifiers: *modifiers,
                        })
                    }
                    // The boolean operators and hull map to
                    // typed CSG operation nodes holding their children in
                    // source order. Upstream csgops.cc / cgaladv.cc parse an
                    // EMPTY parameter set (no arguments) and attach the
                    // instantiation's children, which evaluate in a fresh scope
                    // over the calling context (upstream Children::scopeContext)
                    // — the same child collection the Call arm uses. Unlike a
                    // transform (one Group child), these operators store the
                    // children Vec directly, matching the existing Intersection
                    // node of intersection_for. intersection() reuses that node kind
                    // (upstream maps intersection() and intersection_for to the
                    // same OpenSCADOperator::INTERSECTION). This arm must
                    // precede the BUILTIN_MODULES arm.
                    "union" | "difference" | "intersection" | "hull" | "minkowski" => {
                        // Parameters::parse(args, {}): these operators take no
                        // parameters, so evaluating the (empty) set warns on any
                        // supplied argument, in the calling scope before children.
                        self.parse_builtin_args(args, &[]);
                        self.scope.push_scope();
                        let children = self.eval_scope_body(children, BodyKind::Persistent, None);
                        self.scope.pop();
                        let modifiers = *modifiers;
                        Some(match name.as_str() {
                            "union" => CsgNode::Union {
                                children,
                                modifiers,
                            },
                            "difference" => CsgNode::Difference {
                                children,
                                modifiers,
                            },
                            "intersection" => CsgNode::Intersection {
                                children,
                                modifiers,
                            },
                            "hull" => CsgNode::Hull {
                                children,
                                modifiers,
                            },
                            // minkowski is the fifth children combinator — same no-parameter parse
                            // (convexity warns like any unknown arg, matching
                            // upstream Parameters::parse(args, {})).
                            "minkowski" => CsgNode::Minkowski {
                                children,
                                modifiers,
                            },
                            _ => unreachable!("matched one of the five operator names"),
                        })
                    }
                    // linear_extrude / rotate_extrude map to
                    // typed extrude nodes: parameters (including $fn/$fa/$fs)
                    // resolve in the CALLING scope BEFORE the children, matching
                    // upstream Parameters::parse / CurveDiscretizer preceding
                    // children.instantiate, then the 2D profile children wrap in
                    // a group that becomes the extrude's child (child-always-
                    // Group, like a transform). This arm must precede the
                    // BUILTIN_MODULES arm.
                    "linear_extrude" | "rotate_extrude" => {
                        let is_linear = name == "linear_extrude";
                        let linear = is_linear.then(|| self.linear_extrude_params(args));
                        let rotate = (!is_linear).then(|| self.rotate_extrude_params(args));
                        self.scope.push_scope();
                        let child_nodes =
                            self.eval_scope_body(children, BodyKind::Persistent, None);
                        self.scope.pop();
                        let child = Box::new(CsgNode::Group {
                            children: child_nodes,
                            modifiers: Modifiers::default(),
                        });
                        Some(match (linear, rotate) {
                            (Some(params), _) => CsgNode::LinearExtrude {
                                params,
                                child,
                                modifiers: *modifiers,
                            },
                            (_, Some(params)) => CsgNode::RotateExtrude {
                                params,
                                child,
                                modifiers: *modifiers,
                            },
                            _ => unreachable!("exactly one of linear/rotate is Some"),
                        })
                    }
                    // projection(cut=…) resolves to the typed Projection node — params before
                    // children, child-always-Group (the extrude-arm pattern).
                    "projection" => {
                        let cut = self.projection_params(args);
                        self.scope.push_scope();
                        let child_nodes =
                            self.eval_scope_body(children, BodyKind::Persistent, None);
                        self.scope.pop();
                        let child = Box::new(CsgNode::Group {
                            children: child_nodes,
                            modifiers: Modifiers::default(),
                        });
                        Some(CsgNode::Projection {
                            cut,
                            child,
                            modifiers: *modifiers,
                        })
                    }
                    // offset(...) resolves to the typed Offset node — params (incl. $fn/$fa/$fs) before
                    // the children, child-always-Group, exactly like the
                    // extrudes above (upstream builtin_offset ordering).
                    "offset" => {
                        let params = self.offset_params(args);
                        self.scope.push_scope();
                        let child_nodes =
                            self.eval_scope_body(children, BodyKind::Persistent, None);
                        self.scope.pop();
                        let child = Box::new(CsgNode::Group {
                            children: child_nodes,
                            modifiers: Modifiers::default(),
                        });
                        Some(CsgNode::Offset {
                            params,
                            child,
                            modifiers: *modifiers,
                        })
                    }
                    // import(file=…) resolves to a typed Import leaf —
                    // relative paths resolve against the source-dir
                    // context at eval time; the kernel only opens
                    // the file. `layer=` on an .stl warns + ignores (upstream
                    // import.cc). Children contribute nothing, like any
                    // primitive/extrude-import leaf (collectChildren pattern).
                    "import" => {
                        let (path, convexity) = self.import_params(args, stmt.span);
                        // Children contribute nothing, matching upstream
                        // (import is a geometry leaf like primitives).
                        Some(CsgNode::Import {
                            path,
                            convexity,
                            modifiers: *modifiers,
                        })
                    }
                    // surface(file=…) resolves to the typed Surface leaf —
                    // path resolved at eval time (against the source dir),
                    // bools Bool-only (upstream), children
                    // contribute nothing.
                    "surface" => {
                        let (path, center, invert, convexity) =
                            self.surface_params(args, stmt.span);
                        Some(CsgNode::Surface {
                            path,
                            center,
                            invert,
                            convexity,
                            modifiers: *modifiers,
                        })
                    }
                    // text(...) resolves to the typed Text leaf — params before children (which are
                    // ignored, like every primitive), fragments from the
                    // call-site $-trio, unknown font/direction warnings here.
                    "text" => {
                        let params = self.text_params(args, stmt.span);
                        Some(CsgNode::Text {
                            params,
                            modifiers: *modifiers,
                        })
                    }
                    // A not-yet-supported construct: record a structured
                    // Unsupported error at the call's span (with an
                    // actionable hint) INSTEAD of a silent Call node, so the
                    // CLI and AI loop can route around it. This arm must
                    // precede the BUILTIN_MODULES arm — the unsupported names
                    // are a subset of upstream's registered builtins.
                    _ if UNSUPPORTED_MODULES.contains(&name.as_str()) => {
                        self.errors.push(EvalError::unsupported(
                            name.clone(),
                            stmt.span,
                            unsupported_module_hint(name),
                        ));
                        None
                    }
                    _ if BUILTIN_MODULES.contains(&name.as_str()) => {
                        let args = args
                            .iter()
                            .map(|arg| (arg.name.clone(), self.eval_nested(&arg.value)))
                            .collect();
                        // Children evaluate in a fresh scope over the
                        // calling context (upstream Children::scopeContext).
                        self.scope.push_scope();
                        let child_nodes =
                            self.eval_scope_body(children, BodyKind::Persistent, None);
                        self.scope.pop();
                        Some(CsgNode::Call {
                            name: name.clone(),
                            args,
                            children: child_nodes,
                            modifiers: *modifiers,
                            span: stmt.span,
                        })
                    }
                    _ => {
                        // Upstream Context::lookup_module.
                        self.scope.warn(format!("Ignoring unknown module '{name}'"));
                        None
                    }
                }
            }
            // Assignments, definitions and include/use are handled by the
            // enclosing scope body; blocks are flattened away.
            _ => None,
        }
    }

    /// Upstream `UserModule::instantiate`: bind parameters and `$children`
    /// in a frame chained to the definition site, evaluate the body as a
    /// full statement scope, and wrap its nodes in a group. The
    /// instantiation's children stay unevaluated until `children()`
    /// selects them in the caller's context.
    fn instantiate_user_module(
        &mut self,
        name: &str,
        module: &Arc<UserModule>,
        args: &[Arg],
        children: &[Stmt],
        modifiers: Modifiers,
        span: Span,
    ) -> Option<CsgNode> {
        if self.module_stack_depth >= MAX_MODULE_DEPTH || self.call_depth >= MAX_CALL_DEPTH {
            // Upstream StackCheck: "Recursion detected calling module ...".
            self.errors.push(EvalError::general(
                format!("recursion limit exceeded calling module '{name}'"),
                span,
            ));
            return None;
        }

        // Arguments evaluate in the calling context (upstream Arguments.cc).
        let args: Vec<(Option<&str>, Value)> = args
            .iter()
            .map(|arg| (arg.name.as_deref(), self.eval_nested(&arg.value)))
            .collect();
        let caller_frame = self.scope.current();
        let child_stmts = Arc::new(children.to_vec());
        let child_count = count_instantiations(&child_stmts);

        self.call_depth += 1;
        // Upstream pushes the module name onto StaticModuleNameStack at the
        // top of UserModule::instantiate, so the depth UserModuleContext
        // reads for $parent_modules includes this instantiation.
        self.module_stack_depth += 1;
        self.scope.push_call(module.def_frame);
        // Parameter defaults evaluate against the defining context (the
        // still-empty frame's lexical parent), so they never see $children
        // or $parent_modules.
        let bindings = self.bind_parameters(args, &module.params);
        // UserModuleContext binds $children then $parent_modules (both
        // before parameters apply, so a same-named parameter overrides
        // them). $children is a lexical binding (see is_config_variable);
        // $parent_modules is a config variable set from the module-name
        // stack depth (ScopeContext.cc UserModuleContext).
        self.scope
            .set("$children", Value::Number(child_count as f64));
        self.scope.set(
            "$parent_modules",
            Value::Number(self.module_stack_depth as f64),
        );
        for (name, value) in bindings {
            self.scope.set(&name, value);
        }
        let module_frame = self.scope.current();
        self.children_contexts.insert(
            module_frame,
            ChildrenContext {
                stmts: child_stmts,
                caller_frame,
            },
        );
        let nodes = self.eval_scope_body(
            std::slice::from_ref(&module.body),
            BodyKind::ModuleBody,
            None,
        );
        // Remove before popping: the frame id may be reclaimed and reused.
        // Body definitions go with the frame — a module body exports
        // nothing, so they are unreachable once the instantiation ends
        // (their frame is deliberately not retained; see eval_scope_body).
        self.children_contexts.remove(&module_frame);
        self.drop_body_definitions(module_frame);
        self.scope.pop();
        self.module_stack_depth -= 1;
        self.call_depth -= 1;
        Some(CsgNode::Group {
            children: nodes,
            modifiers,
        })
    }

    /// Evaluate an `if` branch or `for` iteration body in a fresh scope
    /// over the current context (upstream `Children::instantiate`'s
    /// ScopeContext). Loop/if bodies are not module bodies, but like them
    /// they export nothing, so their definitions get the same cleanup
    /// discipline as [`Evaluator::instantiate_user_module`]: the body
    /// frame is not retained and its definition tables drop with it on
    /// exit — repeated iterations keep the arena and tables flat, while
    /// body definitions stay callable within their own iteration.
    fn eval_transient_body(&mut self, stmts: &[Stmt]) -> Vec<CsgNode> {
        self.scope.push_scope();
        let frame = self.scope.current();
        let nodes = self.eval_scope_body(stmts, BodyKind::Transient, None);
        self.drop_body_definitions(frame);
        self.scope.pop();
        nodes
    }

    /// Drop a non-exporting body frame's definition tables. Must run
    /// before the frame pops: its id may be reclaimed and reused, and a
    /// stale entry would alias the reusing frame's definitions.
    fn drop_body_definitions(&mut self, frame: FrameId) {
        self.functions.remove(&frame);
        self.modules.remove(&frame);
        self.file_uses.remove(&frame);
    }

    /// Upstream `control.cc builtin_children`: instantiate the innermost
    /// user module's children — all of them, or the ones selected by a
    /// number, vector, or range `index` argument — in the caller's context.
    /// Out-of-range or ill-typed indices warn and select nothing; calling
    /// `children()` outside any user module silently produces no node.
    fn builtin_children(
        &mut self,
        args: &[Arg],
        child_ast: &[Stmt],
        modifiers: Modifiers,
    ) -> Option<CsgNode> {
        if count_instantiations(child_ast) > 0 {
            // BuiltinModule::noChildren (module.cc) warns iff
            // inst->scope->hasChildren(), and LocalScope::hasChildren is
            // !moduleInstantiations.empty() (LocalScope.h) — a child scope
            // holding only assignments (or nothing) never warns.
            self.scope
                .warn("module children() does not support child modules");
        }
        // Arguments parse before the module-context check (builtin_children
        // builds Arguments/Parameters first).
        let index = self.children_index_argument(args);
        let ctx = self.enclosing_children_context()?;
        let count = count_instantiations(&ctx.stmts);

        match index {
            // No index argument => all children. Master builtin_children
            // keys this off parameters.contains("index"), and an explicit
            // undef argument DOES bind (Parameters::contains finds the
            // undef entry), so children(undef) is a bad parameter type
            // below, not "all children".
            None => Some(self.instantiate_children(&ctx, None, modifiers)),
            Some(Value::Number(n)) => {
                let index = self.valid_child_index(n as i64, count)?;
                Some(self.instantiate_children(&ctx, Some(&[index]), modifiers))
            }
            Some(Value::Vector(ref values)) => {
                let mut indices = Vec::new();
                for value in values.iter() {
                    match value {
                        Value::Number(n) => {
                            if let Some(index) = self.valid_child_index(*n as i64, count) {
                                indices.push(index);
                            }
                        }
                        // validChildIndex(Value): toString, trailing period.
                        other => self.scope.warn(format!(
                            "Bad parameter type ({other}) for children, \
                             only accept: empty, number, vector, range."
                        )),
                    }
                }
                Some(self.instantiate_children(&ctx, Some(&indices), modifiers))
            }
            Some(Value::Range(ref range)) => {
                let steps = range.num_values();
                if steps >= MAX_CHILDREN_RANGE_STEPS {
                    self.scope.warn(format!(
                        "Bad range parameter for children: too many elements ({steps})"
                    ));
                    return None;
                }
                let mut indices = Vec::new();
                for d in range.iter() {
                    if let Some(index) = self.valid_child_index(d as i64, count) {
                        indices.push(index);
                    }
                }
                Some(self.instantiate_children(&ctx, Some(&indices), modifiers))
            }
            // The top-level bad-type branch prints via toEchoString and
            // without a trailing period.
            Some(other) => {
                self.scope.warn(format!(
                    "Bad parameter type ({}) for children, \
                     only accept: empty, number, vector, range",
                    other.to_echo_string()
                ));
                None
            }
        }
    }

    /// The children of the nearest enclosing user-module instantiation
    /// along the lexical chain (upstream `Context::user_module_children()`
    /// walks the context parent chain to the nearest `UserModuleContext`);
    /// `None` outside any user module.
    fn enclosing_children_context(&self) -> Option<ChildrenContext> {
        let mut frame = Some(self.scope.current());
        while let Some(id) = frame {
            if let Some(ctx) = self.children_contexts.get(&id) {
                return Some(ctx.clone());
            }
            frame = self.scope.lexical_parent(id);
        }
        None
    }

    /// The `index` argument of `children(...)`, if any (upstream
    /// `Parameters::parse` with no required and one optional parameter):
    /// the first positional argument or a named `index`.
    fn children_index_argument(&mut self, args: &[Arg]) -> Option<Value> {
        let mut index = None;
        let mut index_named = false;
        let mut warned_extra = false;
        for arg in args {
            let value = self.eval_nested(&arg.value);
            match arg.name.as_deref() {
                Some("index") => {
                    // Parameters.cc parse_without_defaults: "argument %1$s
                    // supplied more than once"; set_variable then
                    // insert-or-assigns, so the last value still wins.
                    if index_named {
                        self.scope.warn("argument 'index' supplied more than once");
                    }
                    index_named = true;
                    index = Some(value);
                }
                Some(name) if is_config_variable(name) => {}
                Some(name) => self
                    .scope
                    .warn(format!("variable '{name}' not specified as parameter")),
                None if index.is_none() => index = Some(value),
                None => {
                    if !warned_extra {
                        self.scope.warn("Too many unnamed arguments supplied");
                        warned_extra = true;
                    }
                }
            }
        }
        index
    }

    /// Upstream `control.cc validChildIndex`: out-of-range warns and
    /// selects nothing.
    fn valid_child_index(&mut self, n: i64, count: usize) -> Option<usize> {
        if n < 0 || n >= count as i64 {
            self.scope.warn(format!(
                "Children index ({n}) out of bounds ({count} children)"
            ));
            return None;
        }
        Some(n as usize)
    }

    /// Upstream `Children::instantiate`: evaluate the (selected) children
    /// in a fresh scope over the *caller's* context. The frame's dynamic
    /// parent is the module body, so `$`-variables set there stay visible
    /// (upstream special-variable lookup walks the live frame stack).
    fn instantiate_children(
        &mut self,
        ctx: &ChildrenContext,
        indices: Option<&[usize]>,
        modifiers: Modifiers,
    ) -> CsgNode {
        self.scope.push_call(ctx.caller_frame);
        let nodes = self.eval_scope_body(&ctx.stmts, BodyKind::Persistent, indices);
        self.scope.pop();
        CsgNode::Group {
            children: nodes,
            modifiers,
        }
    }

    /// Upstream `control.cc builtin_if`: the condition coerces via
    /// `toBool` (`arguments[0]->toBool()`), and the taken branch's scope
    /// instantiates in a fresh scope over the calling context
    /// (`Children(scope, context).instantiate(lazyUnionNode(inst))`),
    /// wrapped in a group. A false condition without an else produces no
    /// node ("should not count as valid CSGNode"); an empty taken branch
    /// still produces its (empty) group — builtin_if has no empty-children
    /// check, unlike echo/assert.
    fn builtin_if(
        &mut self,
        cond: &Expr,
        then_branch: &Stmt,
        else_branch: Option<&Stmt>,
        modifiers: Modifiers,
    ) -> Option<CsgNode> {
        let taken = if self.eval_nested(cond).to_bool() {
            then_branch
        } else {
            else_branch?
        };
        let children = self.eval_transient_body(std::slice::from_ref(taken));
        Some(CsgNode::Group {
            children,
            modifiers,
        })
    }

    /// Upstream `control.cc builtin_for` / `builtin_intersection_for`:
    /// iterate the bindings via `LcFor::forEach` and instantiate the body
    /// once per iteration into the node — a group for `for`, an
    /// [`CsgNode::Intersection`] for `intersection_for`. The node is
    /// produced even when the iteration is empty (no empty-children check,
    /// unlike echo/assert), and zero bindings iterate zero times
    /// (`if (!inst->arguments.empty())`).
    fn builtin_for(
        &mut self,
        args: &[Arg],
        body: &[Stmt],
        modifiers: Modifiers,
        intersection: bool,
    ) -> Option<CsgNode> {
        let mut children = Vec::new();
        if !args.is_empty() {
            self.for_each(args, body, &mut children);
        }
        Some(if intersection {
            CsgNode::Intersection {
                children,
                modifiers,
            }
        } else {
            CsgNode::Group {
                children,
                modifiers,
            }
        })
    }

    /// Upstream `control.cc builtin_let` (statement-form): bind each named
    /// argument sequentially in a fresh child scope — so a later argument
    /// sees prior bindings (`let(a=1, b=a+2)` binds `b=3`) — then
    /// instantiate the body in one more fresh scope over that binding frame
    /// (`Children::instantiate`'s ScopeContext, the same discipline `for`
    /// uses for its loop variable via [`Self::for_iteration`] +
    /// [`Self::eval_transient_body`]). `let` differs from `for` only in that
    /// it binds ALL args once rather than iterating. Unnamed arguments warn
    /// and are ignored (upstream requires named assignments).
    fn builtin_let(
        &mut self,
        args: &[Arg],
        body: &[Stmt],
        modifiers: Modifiers,
    ) -> Option<CsgNode> {
        self.scope.push_scope();
        for arg in args {
            match &arg.name {
                Some(name) => {
                    let value = self.eval_nested(&arg.value);
                    self.scope.set(name, value);
                }
                None => self
                    .scope
                    .warn("Ignoring unnamed argument in let() statement"),
            }
        }
        let children = self.eval_transient_body(body);
        self.scope.pop();
        Some(CsgNode::Group {
            children,
            modifiers,
        })
    }

    /// Upstream `Expression.cc doForEach` (via `LcFor::forEach`): the
    /// first binding's expression evaluates in the current context; each
    /// of its values binds in a fresh frame in which the remaining
    /// bindings recurse — multiple bindings therefore nest cartesian, with
    /// later bindings seeing earlier loop variables. Once every binding is
    /// bound, the body instantiates in one more fresh scope over the
    /// iteration context (`Children::instantiate`'s ScopeContext).
    ///
    /// Iterables: ranges (a range with [`MAX_FOR_RANGE_STEPS`] or more
    /// values warns `Bad range parameter in for statement` and iterates
    /// zero times), vectors (elements), and strings (one iteration per
    /// character, each a one-character string). Undef iterates zero times
    /// silently (`else if (type != UNDEFINED)`); any other value iterates
    /// once, bound as-is. Upstream's OBJECT branch has no analogue — our
    /// value model has no object type. An unnamed binding iterates without
    /// binding a variable (upstream binds the empty name, which no lookup
    /// can produce).
    fn for_each(&mut self, args: &[Arg], body: &[Stmt], out: &mut Vec<CsgNode>) {
        let Some((arg, rest)) = args.split_first() else {
            let nodes = self.eval_transient_body(body);
            out.extend(nodes);
            return;
        };
        let name = arg.name.as_deref();
        match self.eval_nested(&arg.value) {
            Value::Range(ref range) => {
                let steps = range.num_values();
                if steps >= MAX_FOR_RANGE_STEPS {
                    self.scope.warn(format!(
                        "Bad range parameter in for statement: too many elements ({steps})"
                    ));
                } else {
                    for v in range.iter() {
                        self.for_iteration(name, Value::Number(v), rest, body, out);
                    }
                }
            }
            Value::Vector(ref values) => {
                for value in values.iter() {
                    self.for_iteration(name, value.clone(), rest, body, out);
                }
            }
            Value::String(ref s) => {
                for ch in s.chars() {
                    self.for_iteration(name, Value::String(ch.to_string().into()), rest, body, out);
                }
            }
            Value::Undef(_) => {}
            value => self.for_iteration(name, value, rest, body, out),
        }
    }

    /// One `doForEach` iteration: a fresh frame (upstream `forContext`)
    /// binds the loop variable, then the remaining bindings recurse. The
    /// frame's dynamic parent is the enclosing context, so `$`-named loop
    /// variables (and `$`-bindings made in the body) keep flowing along
    /// the dynamic chain.
    fn for_iteration(
        &mut self,
        name: Option<&str>,
        value: Value,
        rest: &[Arg],
        body: &[Stmt],
        out: &mut Vec<CsgNode>,
    ) {
        self.scope.push_scope();
        if let Some(name) = name {
            self.scope.set(name, value);
        }
        self.for_each(rest, body, out);
        self.scope.pop();
    }

    /// Upstream `control.cc builtin_echo`: format the arguments (named
    /// ones as `name = value`, values via `Value::toEchoString`) onto the
    /// console sink, then wrap any children in a group.
    fn builtin_echo(
        &mut self,
        args: &[Arg],
        children: &[Stmt],
        modifiers: Modifiers,
    ) -> Option<CsgNode> {
        self.perform_echo(args);
        self.wrap_children(children, modifiers)
    }

    /// Print one `ECHO:` line for `args` — shared by the echo statement and
    /// the echo expression (upstream `Echo::evaluateStep` streams the same
    /// `Arguments` formatting).
    fn perform_echo(&mut self, args: &[Arg]) {
        let mut line = String::from("ECHO: ");
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                line.push_str(", ");
            }
            if let Some(name) = &arg.name {
                line.push_str(name);
                line.push_str(" = ");
            }
            let value = self.eval_nested(&arg.value);
            line.push_str(&value.to_echo_string());
        }
        self.console.push(line);
    }

    /// Upstream `Assert::performAssert` + `control.cc builtin_assert`: a
    /// false (or missing) condition records a hard error — upstream throws
    /// `AssertionFailedException`, aborting the whole evaluation; here only
    /// this subtree is dropped — otherwise any children wrap in a group.
    /// The optional message formats via `Value::toEchoString`.
    fn builtin_assert(
        &mut self,
        args: &[Arg],
        children: &[Stmt],
        modifiers: Modifiers,
        span: Span,
    ) -> Option<CsgNode> {
        if !self.perform_assert(args, span) {
            return None;
        }
        self.wrap_children(children, modifiers)
    }

    /// Upstream `Assert::performAssert`, shared by the assert statement and
    /// the assert expression: returns whether the assertion held, recording
    /// the failure as an [`EvalError`] at `span` otherwise.
    fn perform_assert(&mut self, args: &[Arg], span: Span) -> bool {
        // Parameters::parse with required {"condition"}, optional
        // {"message"}: positionals fill in that order.
        let mut condition = None;
        let mut message = None;
        let mut warned_extra = false;
        for arg in args {
            let value = self.eval_nested(&arg.value);
            match arg.name.as_deref() {
                Some("condition") => condition = Some(value),
                Some("message") => message = Some(value),
                Some(name) if is_config_variable(name) => {}
                Some(name) => self
                    .scope
                    .warn(format!("variable '{name}' not specified as parameter")),
                None if condition.is_none() => condition = Some(value),
                None if message.is_none() => message = Some(value),
                None => {
                    if !warned_extra {
                        self.scope.warn("Too many unnamed arguments supplied");
                        warned_extra = true;
                    }
                }
            }
        }
        if !condition.is_some_and(|c| c.to_bool()) {
            // Assert::performAssert logs "Assertion%1$s failed%2$s" where
            // %1$s quotes the condition expression — the first argument
            // named "condition" or unnamed. Upstream pretty-prints the
            // AST; here its source text is sliced by span (empty when no
            // source is attached, like upstream's null conditionExpression).
            let condition_text = args
                .iter()
                .find(|arg| matches!(arg.name.as_deref(), None | Some("condition")))
                .and_then(|arg| self.span_text(arg.value.span))
                .map(|text| format!(" '{text}'"))
                .unwrap_or_default();
            let message = match message {
                Some(m) => format!(": {}", m.to_echo_string()),
                None => String::new(),
            };
            self.errors.push(EvalError::general(
                format!("Assertion{condition_text} failed{message}"),
                span,
            ));
            return false;
        }
        true
    }

    /// Instantiate an `echo`/`assert` instantiation's children in a fresh
    /// scope over the calling context; without child geometry there is no
    /// node (upstream `builtin_echo`/`builtin_assert`).
    fn wrap_children(&mut self, children: &[Stmt], modifiers: Modifiers) -> Option<CsgNode> {
        self.scope.push_scope();
        let nodes = self.eval_scope_body(children, BodyKind::Persistent, None);
        self.scope.pop();
        if nodes.is_empty() {
            None
        } else {
            Some(CsgNode::Group {
                children: nodes,
                modifiers,
            })
        }
    }

    /// Resolve a function name along the lexical chain (upstream
    /// `Context::lookup_function` over per-scope `LocalScope` maps).
    ///
    /// Each frame offers, in order: its own function definitions
    /// (`ScopeContext::lookup_local_function`), then a variable bound in it
    /// that holds a function value (`ContextFrame::lookup_local_function` —
    /// a variable holding anything else does not shadow an outer function),
    /// then the function definitions of the libraries its scope `use`d
    /// (`FileContext::lookup_local_function` over `usedlibs`). The first
    /// frame with a match wins. A `$`-name is looked up as a dynamically
    /// scoped variable holding a function value only
    /// (`EvaluationSession::lookup_special_function`).
    fn lookup_function(&self, name: &str) -> Option<Arc<UserFunction>> {
        if is_config_variable(name) {
            return match self.scope.try_lookup(name) {
                Some(Value::Function(function)) => Some(Arc::clone(function)),
                _ => None,
            };
        }
        let mut frame = Some(self.scope.current());
        while let Some(id) = frame {
            if let Some(function) = self.functions.get(&id).and_then(|defs| defs.get(name)) {
                return Some(Arc::clone(function));
            }
            if let Some(Value::Function(function)) = self.scope.lookup_in(id, name) {
                return Some(Arc::clone(function));
            }
            if let Some(function) = self.used_definition(&self.functions, id, name) {
                return Some(function);
            }
            frame = self.scope.lexical_parent(id);
        }
        None
    }

    /// Resolve a call's callee (upstream
    /// `FunctionCall::evaluate_function_expression`): a bare name goes
    /// through [`Evaluator::lookup_function`] (falling back to the
    /// builtins); any other expression is evaluated and must yield a
    /// function value, else it warns `Can't call function on <type>`.
    fn resolve_callee<'a>(&mut self, callee: &'a Expr) -> Callee<'a> {
        match &callee.kind {
            ExprKind::Ident(name) | ExprKind::DollarIdent(name) => {
                match self.lookup_function(name) {
                    Some(function) => Callee::User(function),
                    None => Callee::Builtin(name),
                }
            }
            _ => match self.eval_nested(callee) {
                Value::Function(ref function) => Callee::User(Arc::clone(function)),
                value => {
                    self.scope
                        .warn(format!("Can't call function on {}", value.type_name()));
                    Callee::NotCallable
                }
            },
        }
    }

    /// A definition named `name` made directly in one of the libraries
    /// `use`d by `frame`'s scope (first library wins, in `use` order). Only
    /// the library's own definitions count, not those it `use`s itself
    /// (upstream checks `usedmod->scope.functions` / `.modules`).
    fn used_definition<T>(
        &self,
        table: &HashMap<FrameId, HashMap<String, Arc<T>>>,
        frame: FrameId,
        name: &str,
    ) -> Option<Arc<T>> {
        let libs = self.file_uses.get(&frame)?;
        libs.iter().find_map(|&lib| {
            let lib_frame = self.libraries[lib]?;
            table.get(&lib_frame)?.get(name).map(Arc::clone)
        })
    }

    /// Resolve a module name along the lexical chain (upstream
    /// `Context::lookup_module`; user definitions shadow builtins).
    fn lookup_module(&self, name: &str) -> Option<Arc<UserModule>> {
        let mut frame = Some(self.scope.current());
        while let Some(id) = frame {
            if let Some(module) = self.modules.get(&id).and_then(|defs| defs.get(name)) {
                return Some(Arc::clone(module));
            }
            if let Some(module) = self.used_definition(&self.modules, id, name) {
                return Some(module);
            }
            frame = self.scope.lexical_parent(id);
        }
        None
    }

    /// Evaluate one expression in the current scope.
    ///
    /// Runs on the large pipeline stack like [`Evaluator::eval_program`], so
    /// recursion up to the depth guards cannot overflow the caller's thread.
    /// Nesting deeper than [`MAX_EXPR_DEPTH`] records an [`EvalError`]
    /// carrying the offending expression's span and short-circuits to
    /// undef, protecting the native stack from pathological ASTs. Once the
    /// fatal [`Evaluator::aborted`] latch is set (the C-style comprehension
    /// counter cap), every expression short-circuits to undef so no usable
    /// value survives — the analogue of upstream's `LoopCntException`
    /// unwinding the stack.
    pub fn eval_expr(&mut self, expr: &Expr) -> Value {
        crate::stack::with_large_stack(|| {
            let value = self.eval_nested(expr);
            self.localize_errors();
            value
        })
    }

    /// [`Evaluator::eval_expr`] on the current thread: the evaluator's own
    /// recursive entry point, applying the depth guard and abort latch.
    fn eval_nested(&mut self, expr: &Expr) -> Value {
        if self.aborted {
            return Value::Undef(None);
        }
        if self.expr_depth >= MAX_EXPR_DEPTH {
            self.errors
                .push(EvalError::general("expression nesting too deep", expr.span));
            return Value::Undef(None);
        }
        self.expr_depth += 1;
        // Warnings raised while evaluating this expression (and not inside
        // a nested one) are attributed to it.
        let outer_span = self.scope.set_warn_span(Some(expr.span));
        let value = self.eval_expr_kind(expr);
        self.scope.set_warn_span(outer_span);
        self.expr_depth -= 1;
        value
    }

    fn eval_expr_kind(&mut self, expr: &Expr) -> Value {
        match &expr.kind {
            ExprKind::Number(n) => Value::Number(*n),
            ExprKind::Str(s) => Value::String(s.as_str().into()),
            ExprKind::Bool(b) => Value::Bool(*b),
            ExprKind::Undef => Value::Undef(None),
            // Plain names walk the lexical chain, `$`-names the dynamic
            // chain — both via ScopeStack::lookup (upstream Lookup).
            ExprKind::Ident(name) | ExprKind::DollarIdent(name) => self.scope.lookup(name),
            ExprKind::Vector(elements) => {
                // Upstream `Vector::evaluate`: each child evaluates and its
                // result is appended. A comprehension child yields an
                // `EmbeddedVectorType` whose elements splice into the
                // enclosing vector (flatten); a plain child contributes one
                // value. [`Evaluator::eval_comprehension`] models both — its
                // default arm is the plain-child, single-value case.
                let mut out = Vec::new();
                for element in elements {
                    self.eval_comprehension(element, &mut out);
                    // A fatal abort mid-vector discards the whole value
                    // (upstream's LoopCntException unwinds past this vector).
                    if self.aborted {
                        return Value::Undef(None);
                    }
                }
                Value::from(out)
            }
            ExprKind::Range { start, step, end } => self.eval_range(start, step.as_deref(), end),
            ExprKind::Unary { op, operand } => match op {
                UnaryOp::Not => self.eval_nested(operand).not(),
                // parser.y: unary '+' is the identity (`'+' unary { $$ = $2; }`).
                UnaryOp::Pos => self.eval_nested(operand),
                UnaryOp::Neg => {
                    let value = self.eval_nested(operand).neg();
                    self.check_undef(value)
                }
            },
            ExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs),
            ExprKind::Ternary {
                cond,
                then_branch,
                else_branch,
            } => {
                // Upstream TernaryOp::evaluateStep: only the taken branch
                // is evaluated.
                let branch = if self.eval_nested(cond).to_bool() {
                    then_branch
                } else {
                    else_branch
                };
                self.eval_nested(branch)
            }
            ExprKind::Call { callee, args } => self.eval_call(callee, args, expr.span),
            // Upstream ArrayLookup::evaluate applies no checkUndef.
            ExprKind::Index { target, index } => {
                let target = self.eval_nested(target);
                let index = self.eval_nested(index);
                target.index(&index)
            }
            ExprKind::Member { target, member } => {
                let value = self.eval_nested(target);
                // Upstream MemberLookup: vectors expose .x/.y/.z, ranges
                // .begin/.step/.end; anything else is plain undef.
                let index = match (&value, member.as_str()) {
                    (Value::Vector(_), "x") | (Value::Range(_), "begin") => 0.0,
                    (Value::Vector(_), "y") | (Value::Range(_), "step") => 1.0,
                    (Value::Vector(_), "z") | (Value::Range(_), "end") => 2.0,
                    _ => return Value::Undef(None),
                };
                value.index(&Value::Number(index))
            }
            ExprKind::Let { args, body } => {
                self.scope.push_scope();
                self.bind_let_args(args);
                let result = self.eval_nested(body);
                self.scope.pop();
                result
            }
            // Upstream FunctionDefinition::evaluate: a FunctionPtr over the
            // current context. The frame is pinned so the closure keeps
            // seeing it (including bindings made after the literal, such as
            // the variable a recursive literal is assigned to) after it is
            // popped.
            ExprKind::FunctionLiteral { params, body } => {
                let frame = self.scope.current();
                self.scope.retain(frame);
                Value::Function(Arc::new(UserFunction {
                    name: String::new(),
                    def_frame: frame,
                    params: Arc::clone(params),
                    body: Arc::clone(body),
                }))
            }
            // Upstream Echo::evaluate / Assert::evaluate: perform the step,
            // then evaluate the body (undef when there is none). A failed
            // assert yields undef without evaluating the body; upstream
            // throws AssertionFailedException, aborting the whole
            // evaluation, while here the error is recorded and evaluation
            // continues (as for the assert statement).
            ExprKind::Echo { args, body } => {
                self.perform_echo(args);
                match body {
                    Some(body) => self.eval_nested(body),
                    None => Value::Undef(None),
                }
            }
            ExprKind::Assert { args, body } => {
                if !self.perform_assert(args, expr.span) {
                    return Value::Undef(None);
                }
                match body {
                    Some(body) => self.eval_nested(body),
                    None => Value::Undef(None),
                }
            }
            ExprKind::LcFor { .. }
            | ExprKind::LcForC { .. }
            | ExprKind::LcEach { .. }
            | ExprKind::LcIf { .. }
            | ExprKind::LcLet { .. } => {
                // A comprehension is grammatically only a vector element, so
                // it normally reaches [`Evaluator::eval_comprehension`] via
                // the `Vector` arm. Should a bare one arrive here, evaluate
                // it as a comprehension and wrap the flattened elements —
                // upstream's stray `EmbeddedVectorType` likewise degrades to
                // a plain vector outside a vector context.
                let mut out = Vec::new();
                self.eval_comprehension(expr, &mut out);
                if self.aborted {
                    return Value::Undef(None);
                }
                Value::from(out)
            }
        }
    }

    /// Evaluate one comprehension element (upstream: the `Value` a vector
    /// child evaluates to), appending its flattened values to `out`. The
    /// five `Lc*` kinds flatten (a comprehension is upstream's
    /// `EmbeddedVectorType`, spliced by the enclosing `Vector::evaluate`);
    /// any other expression is a plain element contributing exactly one
    /// value. Recursion shares the [`MAX_EXPR_DEPTH`] guard so nested
    /// comprehensions cannot overflow the native stack.
    fn eval_comprehension(&mut self, expr: &Expr, out: &mut Vec<Value>) {
        if self.aborted {
            return;
        }
        if self.expr_depth >= MAX_EXPR_DEPTH {
            self.errors
                .push(EvalError::general("expression nesting too deep", expr.span));
            return;
        }
        self.expr_depth += 1;
        match &expr.kind {
            ExprKind::LcFor { args, body } => self.lc_for(args, body, out),
            ExprKind::LcForC {
                init,
                cond,
                update,
                body,
            } => self.lc_for_c(init, cond, update, body, out),
            ExprKind::LcEach { body } => self.lc_each(body, out),
            ExprKind::LcIf {
                cond,
                then_branch,
                else_branch,
            } => {
                // Upstream `LcIf::evaluate`: toBool picks a branch; a missing
                // else contributes no elements.
                let taken = if self.eval_nested(cond).to_bool() {
                    Some(then_branch.as_ref())
                } else {
                    else_branch.as_deref()
                };
                if let Some(taken) = taken {
                    self.eval_comprehension(taken, out);
                }
            }
            ExprKind::LcLet { args, body } => {
                // Upstream `LcLet::evaluate`: a `Let::sequentialAssignmentContext`
                // (the same first-wins sequential machinery as expression
                // `let`) then the body element.
                self.scope.push_scope();
                self.bind_let_args(args);
                self.eval_comprehension(body, out);
                self.scope.pop();
            }
            _ => out.push(self.eval_nested(expr)),
        }
        self.expr_depth -= 1;
    }

    /// Upstream `LcFor::evaluate`: iterate the arguments-grammar bindings via
    /// the shared `doForEach` dispatch ([`Evaluator::for_each_comprehension`])
    /// and evaluate the body as a comprehension element per iteration.
    fn lc_for(&mut self, args: &[Arg], body: &Expr, out: &mut Vec<Value>) {
        self.for_each_comprehension(args, body, out);
    }

    /// The comprehension analogue of [`Evaluator::for_each`] (upstream
    /// `doForEach`): the same range/vector/string/scalar/undef dispatch and
    /// cartesian multi-binding nesting, but each fully-bound iteration
    /// evaluates the body as a comprehension element rather than
    /// instantiating a statement body.
    ///
    /// Each binding level recurses natively (`for_each_comprehension` →
    /// [`Evaluator::for_iteration_comprehension`] → `for_each_comprehension`
    /// over the remaining bindings) *without* passing through `eval_nested` /
    /// [`Evaluator::eval_comprehension`], so a pathologically wide binding
    /// list `for(a=..,b=..,...,z=..)` — whose length the parser's nesting
    /// cap does not bound — needs its own share of the [`MAX_EXPR_DEPTH`]
    /// guard to stay off the native stack. A tripped guard records an
    /// [`EvalError`] and unwinds the recursion cleanly.
    fn for_each_comprehension(&mut self, args: &[Arg], body: &Expr, out: &mut Vec<Value>) {
        if self.aborted {
            return;
        }
        if self.expr_depth >= MAX_EXPR_DEPTH {
            self.errors
                .push(EvalError::general("expression nesting too deep", body.span));
            return;
        }
        self.expr_depth += 1;
        self.for_each_comprehension_bindings(args, body, out);
        self.expr_depth -= 1;
    }

    /// The binding dispatch of [`Evaluator::for_each_comprehension`], split
    /// out so the depth guard has a single decrement path.
    fn for_each_comprehension_bindings(&mut self, args: &[Arg], body: &Expr, out: &mut Vec<Value>) {
        let Some((arg, rest)) = args.split_first() else {
            self.eval_comprehension(body, out);
            return;
        };
        let name = arg.name.as_deref();
        match self.eval_nested(&arg.value) {
            Value::Range(ref range) => {
                let steps = range.num_values();
                if steps >= MAX_FOR_RANGE_STEPS {
                    self.scope.warn(format!(
                        "Bad range parameter in for statement: too many elements ({steps})"
                    ));
                } else {
                    for v in range.iter() {
                        self.for_iteration_comprehension(name, Value::Number(v), rest, body, out);
                    }
                }
            }
            Value::Vector(ref values) => {
                for value in values.iter() {
                    self.for_iteration_comprehension(name, value.clone(), rest, body, out);
                }
            }
            Value::String(ref s) => {
                for ch in s.chars() {
                    self.for_iteration_comprehension(
                        name,
                        Value::String(ch.to_string().into()),
                        rest,
                        body,
                        out,
                    );
                }
            }
            Value::Undef(_) => {}
            value => self.for_iteration_comprehension(name, value, rest, body, out),
        }
    }

    /// One comprehension `doForEach` iteration: a fresh frame binds the loop
    /// variable (so loop vars never leak and `$`-named ones flow
    /// dynamically), then the remaining bindings recurse.
    fn for_iteration_comprehension(
        &mut self,
        name: Option<&str>,
        value: Value,
        rest: &[Arg],
        body: &Expr,
        out: &mut Vec<Value>,
    ) {
        self.scope.push_scope();
        if let Some(name) = name {
            self.scope.set(name, value);
        }
        self.for_each_comprehension(rest, body, out);
        self.scope.pop();
    }

    /// Upstream `LcForC::evaluate`: bind `init` once in a fresh frame
    /// (`Let::sequentialAssignmentContext`), then loop while `cond` is truthy
    /// — appending the body element and re-running the `update` bindings each
    /// pass. Upstream reparents each iteration's context to the initial one;
    /// here the loop variables live in a single frame reassigned in place,
    /// which is equivalent because `update` reads current values then binds
    /// sequentially (the reparenting only bounds upstream's lookup-chain
    /// length and preserves captured-lambda references, neither of which
    /// applies to our value model). The [`MAX_FORC_ITERATIONS`] guard fires
    /// after the tripping iteration's element, matching upstream's
    /// post-emit `counter++ == 1000000` check — and, like upstream's
    /// `LoopCntException`, it is fatal: it sets the [`Evaluator::aborted`]
    /// latch so the enclosing vector (and every wrapping expression) yields
    /// undef rather than a bogus million-element partial.
    fn lc_for_c(
        &mut self,
        init: &[Arg],
        cond: &Expr,
        update: &[Arg],
        body: &Expr,
        out: &mut Vec<Value>,
    ) {
        self.scope.push_scope();
        self.bind_let_args(init);
        let mut counter: u64 = 0;
        while self.eval_nested(cond).to_bool() {
            self.eval_comprehension(body, out);
            if counter == MAX_FORC_ITERATIONS {
                self.errors.push(EvalError::general(
                    "For loop counter exceeded limit",
                    body.span,
                ));
                self.aborted = true;
                break;
            }
            counter += 1;
            self.bind_let_args(update);
        }
        self.scope.pop();
    }

    /// Upstream `LcEach::evaluate` + `evalRecur`: evaluate the body, then
    /// splice — ranges to their iterated values (guarded at
    /// [`MAX_FOR_RANGE_STEPS`]), vectors and strings one level (characters
    /// for strings), undef to nothing, any other value as a single element.
    /// When the body is itself a comprehension its flattened values are each
    /// spliced again, mirroring upstream's recursion into an
    /// `EmbeddedVectorType`.
    fn lc_each(&mut self, body: &Expr, out: &mut Vec<Value>) {
        if is_comprehension(&body.kind) {
            let mut inner = Vec::new();
            self.eval_comprehension(body, &mut inner);
            for value in inner {
                self.each_splice(value, out);
            }
        } else {
            let value = self.eval_nested(body);
            self.each_splice(value, out);
        }
    }

    /// Upstream `LcEach::evalRecur` applied to one already-evaluated value.
    fn each_splice(&mut self, value: Value, out: &mut Vec<Value>) {
        match value {
            Value::Range(ref range) => {
                let steps = range.num_values();
                if steps >= MAX_FOR_RANGE_STEPS {
                    self.scope.warn(format!(
                        "Bad range parameter in for statement: too many elements ({steps})"
                    ));
                } else {
                    for d in range.iter() {
                        out.push(Value::Number(d));
                    }
                }
            }
            Value::Vector(ref values) => out.extend(values.iter().cloned()),
            Value::String(ref s) => {
                for ch in s.chars() {
                    out.push(Value::String(ch.to_string().into()));
                }
            }
            Value::Undef(_) => {}
            other => out.push(other),
        }
    }

    /// Upstream `Expression::checkUndef`: an undef carrying a reason is
    /// "unchecked" and surfaces its reason as a warning.
    fn check_undef(&mut self, value: Value) -> Value {
        if let Value::Undef(Some(reason)) = &value {
            let reason = reason.to_string();
            self.scope.warn(reason);
        }
        value
    }

    /// Upstream `Range::evaluate`: begin and end evaluate first and must be
    /// numbers, then the step (default 1.0); any non-number warns and
    /// yields plain undef.
    fn eval_range(&mut self, start: &Expr, step: Option<&Expr>, end: &Expr) -> Value {
        let begin_value = self.eval_nested(start);
        let end_value = self.eval_nested(end);
        let (Value::Number(begin), Value::Number(end)) = (&begin_value, &end_value) else {
            self.scope.warn(format!(
                "Unable to convert [{begin_value}:...:{end_value}] to a range"
            ));
            return Value::Undef(None);
        };
        let step = match step {
            None => 1.0,
            Some(step) => match self.eval_nested(step) {
                Value::Number(s) => s,
                step_value => {
                    self.scope.warn(format!(
                        "Unable to convert [...:{step_value}:...] to a step value"
                    ));
                    return Value::Undef(None);
                }
            },
        };
        Value::Range(Range::new(*begin, step, *end))
    }

    fn eval_binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Value {
        // Upstream BinaryOp::evaluate short-circuits && / || over toBool().
        match op {
            BinaryOp::And => {
                return if !self.eval_nested(lhs).to_bool() {
                    Value::Bool(false)
                } else {
                    Value::Bool(self.eval_nested(rhs).to_bool())
                };
            }
            BinaryOp::Or => {
                return if self.eval_nested(lhs).to_bool() {
                    Value::Bool(true)
                } else {
                    Value::Bool(self.eval_nested(rhs).to_bool())
                };
            }
            _ => {}
        }
        let lhs = self.eval_nested(lhs);
        let rhs = self.eval_nested(rhs);
        let result = match op {
            BinaryOp::Lt => lhs.lt(&rhs),
            BinaryOp::Le => lhs.le(&rhs),
            BinaryOp::Gt => lhs.gt(&rhs),
            BinaryOp::Ge => lhs.ge(&rhs),
            BinaryOp::Eq => Value::Bool(lhs == rhs),
            BinaryOp::Ne => Value::Bool(lhs != rhs),
            BinaryOp::Add => lhs.add(&rhs),
            BinaryOp::Sub => lhs.sub(&rhs),
            BinaryOp::Mul => lhs.mul(&rhs),
            BinaryOp::Div => lhs.div(&rhs),
            BinaryOp::Mod => lhs.modulo(&rhs),
            BinaryOp::Pow => lhs.pow(&rhs),
            BinaryOp::And | BinaryOp::Or => unreachable!("handled above"),
        };
        self.check_undef(result)
    }

    /// Upstream `Let::doSequentialAssignment`: bindings evaluate in the
    /// target frame in order (earlier bindings visible to later ones);
    /// duplicates are ignored with a warning (first wins), unnamed
    /// bindings warn and bind nothing.
    fn bind_let_args(&mut self, args: &[Arg]) {
        let mut seen: HashSet<&str> = HashSet::new();
        for arg in args {
            let value = self.eval_nested(&arg.value);
            match arg.name.as_deref() {
                None => self
                    .scope
                    .warn(format!("Assignment without variable name {value}")),
                Some(name) if seen.contains(name) => self.scope.warn(format!(
                    "Ignoring duplicate variable assignment {name} = {value}"
                )),
                Some(name) => {
                    self.scope.set(name, value);
                    seen.insert(name);
                }
            }
        }
    }

    /// Upstream `FunctionCall::evaluate`: resolve the callee (a named
    /// definition, a function value, or — only after user lookup fails — a
    /// builtin, [`Self::eval_builtin`]), then call it.
    fn eval_call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Value {
        let function = match self.resolve_callee(callee) {
            Callee::User(function) => function,
            Callee::Builtin(name) => return self.eval_builtin(name, args),
            Callee::NotCallable => return Value::Undef(None),
        };
        if self.call_depth >= MAX_CALL_DEPTH {
            self.errors.push(EvalError::general(
                format!(
                    "recursion limit exceeded calling function '{}'",
                    callee_label(callee)
                ),
                span,
            ));
            return Value::Undef(None);
        }

        // Arguments evaluate in the calling context (upstream Arguments.cc).
        let args: Vec<(Option<&str>, Value)> = args
            .iter()
            .map(|arg| (arg.name.as_deref(), self.eval_nested(&arg.value)))
            .collect();

        self.call_depth += 1;
        let caller_frame = self.scope.current();
        // Lexical parent: definition site; dynamic parent: caller — so
        // `$`-variables flow like upstream apply_config_variables.
        self.scope.push_call(function.def_frame);
        let bindings = self.bind_parameters(args, &function.params);
        for (name, value) in bindings {
            self.scope.set(&name, value);
        }
        let result = self.eval_function_body(function, caller_frame);
        // The body may have pushed `let` frames (and tail calls replace the
        // call frame), so unwind to the caller rather than popping once.
        self.scope.pop_to(caller_frame);
        self.call_depth -= 1;
        result
    }

    /// Evaluate a user function's body with tail-call elimination (upstream
    /// `FunctionCall::evaluate` + `simplify_function_body`,
    /// `src/core/Expression.cc`).
    ///
    /// The body is simplified in place: a ternary evaluates its condition and
    /// continues with the taken branch, a `let` binds into a fresh frame and
    /// continues with its body. When what remains is a call to a user
    /// function — the same one or another, as upstream does not restrict
    /// this to self-recursion — the call's arguments evaluate in the current
    /// context, the frames of the finished call are replaced by the callee's
    /// fresh call frame, and the loop continues with the callee's body
    /// instead of recursing. Anything else evaluates normally and ends the
    /// call. `n == 0 ? acc : f(n - 1, acc + 1)` therefore runs in constant
    /// native stack and call depth.
    ///
    /// Like upstream, at most [`MAX_TAIL_CALLS`] calls (counting the initial
    /// one) chain this way; the next records a `Recursion detected calling
    /// function` [`EvalError`] and yields undef. On entry the current frame
    /// is the call frame pushed over `caller_frame`; on return the scope may
    /// hold extra frames above `caller_frame`, which the caller unwinds.
    fn eval_function_body(&mut self, function: Arc<UserFunction>, caller_frame: FrameId) -> Value {
        let mut function = function;
        let mut calls: usize = 1;
        loop {
            match self.simplify_function_body(&function, caller_frame, &mut calls) {
                BodyStep::Done(value) => return value,
                BodyStep::TailCall(next) => function = next,
            }
        }
    }

    /// One pass of [`Evaluator::eval_function_body`]: simplify `function`'s
    /// body until it yields a value or reaches a tail call, in which case the
    /// callee's call frame is already set up (parameters bound) when this
    /// returns.
    fn simplify_function_body(
        &mut self,
        function: &UserFunction,
        caller_frame: FrameId,
        calls: &mut usize,
    ) -> BodyStep {
        let mut expr: &Expr = &function.body;
        loop {
            if self.aborted {
                return BodyStep::Done(Value::Undef(None));
            }
            match &expr.kind {
                // Upstream TernaryOp::evaluateStep.
                ExprKind::Ternary {
                    cond,
                    then_branch,
                    else_branch,
                } => {
                    expr = if self.eval_nested(cond).to_bool() {
                        then_branch
                    } else {
                        else_branch
                    };
                }
                // Upstream Let::evaluateStep: sequential bindings in a fresh
                // frame, then continue with the body.
                ExprKind::Let { args, body } => {
                    self.scope.push_scope();
                    self.bind_let_args(args);
                    expr = body;
                }
                // Upstream Echo::evaluateStep / Assert::evaluateStep: the
                // body stays in tail position.
                ExprKind::Echo { args, body } => {
                    self.perform_echo(args);
                    match body {
                        Some(body) => expr = body,
                        None => return BodyStep::Done(Value::Undef(None)),
                    }
                }
                ExprKind::Assert { args, body } => {
                    if !self.perform_assert(args, expr.span) {
                        return BodyStep::Done(Value::Undef(None));
                    }
                    match body {
                        Some(body) => expr = body,
                        None => return BodyStep::Done(Value::Undef(None)),
                    }
                }
                ExprKind::Call { callee, args } => {
                    // Named definitions and function values (variables or
                    // any callee expression) both continue as tail calls,
                    // as upstream simplify_function_body does for its
                    // CallableUserFunction and FunctionType cases.
                    let next = match self.resolve_callee(callee) {
                        Callee::User(function) => function,
                        // Re-dispatch through eval_nested (the lookup is
                        // pure), so the builtin call counts as one
                        // expression level like any other.
                        Callee::Builtin(_) => return BodyStep::Done(self.eval_nested(expr)),
                        Callee::NotCallable => return BodyStep::Done(Value::Undef(None)),
                    };
                    if *calls >= MAX_TAIL_CALLS {
                        self.errors.push(EvalError::general(
                            format!(
                                "Recursion detected calling function '{}'",
                                callee_label(callee)
                            ),
                            expr.span,
                        ));
                        return BodyStep::Done(Value::Undef(None));
                    }
                    *calls += 1;
                    // Arguments evaluate in the current (calling) context.
                    let args: Vec<(Option<&str>, Value)> = args
                        .iter()
                        .map(|arg| (arg.name.as_deref(), self.eval_nested(&arg.value)))
                        .collect();
                    if self.aborted {
                        return BodyStep::Done(Value::Undef(None));
                    }
                    // Replace this call's frames with the callee's. `$`
                    // bindings made inside them (by `$`-arguments or `let`)
                    // carry over, as upstream copies the calling context's
                    // config variables into the new body context; everything
                    // the original caller sees stays visible through the
                    // dynamic parent.
                    let carried = self.scope.config_vars_above(caller_frame);
                    self.scope.pop_to(caller_frame);
                    self.scope.push_call(next.def_frame);
                    for (name, value) in carried {
                        self.scope.set(&name, value);
                    }
                    let bindings = self.bind_parameters(args, &next.params);
                    for (name, value) in bindings {
                        self.scope.set(&name, value);
                    }
                    return BodyStep::TailCall(next);
                }
                _ => return BodyStep::Done(self.eval_nested(expr)),
            }
        }
    }

    /// Dispatch a builtin function call (upstream
    /// `builtin_functions.cc`), reached only after user-function lookup
    /// fails. `is_undef` is special-cased because it must suppress the
    /// "unknown variable" warning for a bare-identifier argument (upstream
    /// `builtin_is_undef` inspects a `Lookup` argument via
    /// `try_lookup_variable`); every other builtin is a pure function of its
    /// evaluated arguments ([`crate::builtins`]). A name that is neither a
    /// user function nor a builtin warns `Ignoring unknown function ...`
    /// (upstream `Context::lookup_function` returning nothing).
    fn eval_builtin(&mut self, name: &str, args: &[Arg]) -> Value {
        if name == "is_undef" {
            return self.builtin_is_undef(args);
        }
        if crate::builtins::is_builtin(name) {
            let values: Vec<Value> = args
                .iter()
                .map(|arg| self.eval_nested(&arg.value))
                .collect();
            let mut warnings = Vec::new();
            let value = crate::builtins::call_builtin(name, &values, &mut warnings)
                .expect("is_builtin implies call_builtin returns Some");
            for warning in warnings {
                self.scope.warn(warning);
            }
            return value;
        }
        self.scope
            .warn(format!("Ignoring unknown function '{name}'"));
        Value::Undef(None)
    }

    /// Upstream `builtin_is_undef`: report whether the single argument is
    /// undef. A bare-identifier argument is resolved via
    /// `try_lookup_variable`, so an unknown variable reads as undef *without*
    /// the usual "unknown variable" warning.
    fn builtin_is_undef(&mut self, args: &[Arg]) -> Value {
        let [arg] = args else {
            self.scope
                .warn("is_undef() number of parameters does not match");
            return Value::Undef(None);
        };
        let is_undef = match &arg.value.kind {
            ExprKind::Ident(name) | ExprKind::DollarIdent(name) => {
                self.scope.try_lookup(name).is_none_or(Value::is_undef)
            }
            _ => self.eval_nested(&arg.value).is_undef(),
        };
        Value::Bool(is_undef)
    }

    /// Evaluate a primitive module call to a typed [`Primitive`] leaf.
    /// Arguments evaluate in the calling context
    /// (upstream `Arguments.cc`), bind to the primitive's fixed parameter
    /// list (upstream `Parameters::parse`), and each constructor applies the
    /// exact upstream `primitives.cc` defaults / precedence.
    fn eval_primitive(&mut self, name: &str, args: &[Arg]) -> Primitive {
        match name {
            // Parameters::parse(args, {"size", "center"}).
            "cube" => {
                let b = self.parse_builtin_args(args, &["size", "center"]);
                self.primitive_cube(&b)
            }
            // Parameters::parse(args, {"r"}, {"d"}).
            "sphere" => {
                let b = self.parse_builtin_args(args, &["r", "d"]);
                let r = self.lookup_radius(&b, "d", "r").unwrap_or(1.0);
                let fragments = self.resolve_fragments_for(&b, r);
                Primitive::Sphere { r, fragments }
            }
            // Parameters::parse(args, {"h","r1","r2","center"}, {"r","d","d1","d2"}).
            "cylinder" => {
                let b = self
                    .parse_builtin_args(args, &["h", "r1", "r2", "center", "r", "d", "d1", "d2"]);
                self.primitive_cylinder(&b)
            }
            // Parameters::parse(args, {"size", "center"}).
            "square" => {
                let b = self.parse_builtin_args(args, &["size", "center"]);
                self.primitive_square(&b)
            }
            // Parameters::parse(args, {"r"}, {"d"}).
            "circle" => {
                let b = self.parse_builtin_args(args, &["r", "d"]);
                let r = self.lookup_radius(&b, "d", "r").unwrap_or(1.0);
                let fragments = self.resolve_fragments_for(&b, r);
                Primitive::Circle { r, fragments }
            }
            // Parameters::parse(args, {"points", "paths", "convexity"}).
            "polygon" => {
                let b = self.parse_builtin_args(args, &["points", "paths", "convexity"]);
                self.primitive_polygon(&b)
            }
            // Parameters::parse(args, {"points", "faces", "convexity"}); the
            // extra `triangles` name accepts the legacy alias without a
            // "not specified as parameter" warning.
            "polyhedron" => {
                let b =
                    self.parse_builtin_args(args, &["points", "faces", "convexity", "triangles"]);
                self.primitive_polyhedron(&b)
            }
            _ => unreachable!("eval_primitive called with non-primitive '{name}'"),
        }
    }

    /// Resolve one transform module call to a
    /// [`TransformKind`], parsing its arguments in the calling context per
    /// the upstream `Parameters::parse` name lists (`TransformNode.cc`,
    /// `ColorNode.cc`, `CgalAdvNode.cc`).
    fn eval_transform(&mut self, name: &str, args: &[Arg]) -> TransformKind {
        match name {
            // Parameters::parse(args, {"v"}).
            "translate" => {
                let b = self.parse_builtin_args(args, &["v"]);
                TransformKind::Affine(self.transform_translate(&b))
            }
            "scale" => {
                let b = self.parse_builtin_args(args, &["v"]);
                TransformKind::Affine(self.transform_scale(&b))
            }
            // Parameters::parse(args, {"a", "v"}).
            "rotate" => {
                let b = self.parse_builtin_args(args, &["a", "v"]);
                TransformKind::Affine(self.transform_rotate(&b))
            }
            "mirror" => {
                let b = self.parse_builtin_args(args, &["v"]);
                TransformKind::Affine(self.transform_mirror(&b))
            }
            // Parameters::parse(args, {"m"}).
            "multmatrix" => {
                let b = self.parse_builtin_args(args, &["m"]);
                TransformKind::Affine(transform_multmatrix(&b))
            }
            // Parameters::parse(args, {"newsize", "auto", "convexity"}).
            "resize" => {
                let b = self.parse_builtin_args(args, &["newsize", "auto", "convexity"]);
                transform_resize(&b)
            }
            // Parameters::parse(args, {"c", "alpha"}).
            "color" => {
                let b = self.parse_builtin_args(args, &["c", "alpha"]);
                self.transform_color(&b)
            }
            _ => unreachable!("eval_transform called with non-transform '{name}'"),
        }
    }

    /// Resolve a `linear_extrude(...)` call to
    /// [`LinearExtrudeParams`], parsing its arguments in the calling context
    /// per upstream `builtin_linear_extrude` (`LinearExtrudeNode.cc`, master).
    /// The vector-direction `v` form is not yet modeled, so a
    /// supplied `v` is ignored and `height` is the scalar Z length.
    fn linear_extrude_params(&mut self, args: &[Arg]) -> LinearExtrudeParams {
        // Parameters::parse(args, {"height","v","scale","center","twist",
        // "slices","segments"}, {"convexity","h"}) — required + optional names,
        // both "specified parameters" (LinearExtrudeNode.cc:50, master). "v"/
        // "segments" are accepted (so such an argument does not warn) but not
        // modeled. "h" is upstream's genuine short alias for "height"
        // (LinearExtrudeNode.cc:64 reads parameters[{"height","h"}]) — a real
        // namesake of cylinder's separate "h", not a copy-paste of it.
        let b = self.parse_builtin_args(
            args,
            &[
                "height",
                "v",
                "scale",
                "center",
                "twist",
                "slices",
                "segments",
                "convexity",
                "h",
            ],
        );

        // height: default 100; the "h" alias is read when "height" is absent; a
        // supplied non-number warns and falls back to 100 (getFiniteDouble).
        let mut height = 100.0;
        if let Some(value) = b.get("height").or_else(|| b.get("h")) {
            if !value.is_undef() {
                match as_number(value) {
                    Some(n) if n.is_finite() => height = n,
                    _ => {
                        self.scope.warn("height when specified should be a number");
                    }
                }
            }
        }
        // Upstream: if (node->height[2] <= 0) node->height[2] = 0.
        if height <= 0.0 {
            height = 0.0;
        }

        // center applies only when the argument is a bool (upstream
        // type() == BOOL); default false.
        let center = b.get("center").and_then(as_bool).unwrap_or(false);

        // twist: default 0 (getFiniteDouble overrides only for a finite number).
        let twist = match b.get("twist").and_then(as_number) {
            Some(n) if n.is_finite() => n,
            _ => 0.0,
        };

        // slices: kept only when supplied as a number >= 1 (upstream
        // validate_integral(..., 1u)); otherwise None so the kernel derives it
        // from twist and the fragment controls.
        let slices = match b.get("slices").and_then(as_number) {
            Some(n) if n >= 1.0 => Some(n as u32),
            _ => None,
        };

        // scale: a scalar sets both axes, a vec2 sets each; default [1, 1];
        // negatives clamp to 0 (upstream).
        let mut scale = [1.0, 1.0];
        if let Some(value) = b.get("scale") {
            if let Some(n) = as_number(value) {
                if n.is_finite() {
                    scale = [n, n];
                } else {
                    self.warn_scale_unconvertible(value);
                }
            } else if let Some(v) = as_vec_n(value, 2) {
                scale = [v[0], v[1]];
            } else if !value.is_undef() {
                self.warn_scale_unconvertible(value);
            }
        }
        if scale[0] < 0.0 {
            scale[0] = 0.0;
        }
        if scale[1] < 0.0 {
            scale[1] = 0.0;
        }

        // convexity: default 1, overridden only by a positive number (upstream
        // getPositiveInt, which leaves the default otherwise).
        let convexity = match b.get("convexity").and_then(as_number) {
            Some(n) if n >= 1.0 => n as u32,
            _ => 1,
        };

        LinearExtrudeParams {
            height,
            center,
            twist,
            slices,
            scale,
            convexity,
            fragments: self.extrude_fragments(&b),
        }
    }

    /// Warn that a `linear_extrude` `scale` argument could not be converted
    /// (upstream `LinearExtrudeNode.cc` `scale=%1$s could not be converted`).
    fn warn_scale_unconvertible(&mut self, value: &Value) {
        self.scope.warn(format!(
            "linear_extrude(..., scale={}) could not be converted",
            value.to_echo_string()
        ));
    }

    /// Resolve a `rotate_extrude(...)` call to
    /// [`RotateExtrudeParams`], parsing its arguments in the calling context
    /// per upstream `builtin_rotate_extrude` (`RotateExtrudeNode.cc`, master).
    fn rotate_extrude_params(&mut self, args: &[Arg]) -> RotateExtrudeParams {
        // Parameters::parse(args, {"angle","start"}, {"convexity","a"}) —
        // required + optional names (RotateExtrudeNode.cc:51, master). "a" is
        // upstream's genuine short alias for "angle" (RotateExtrudeNode.cc:60
        // reads parameters[{"angle","a"}]) — distinct from the rotate()
        // transform's "a" parameter, not a copy-paste of it.
        let b = self.parse_builtin_args(args, &["angle", "start", "convexity", "a"]);

        // convexity: max(2, (int)toDouble); a non-number reads as 0 -> 2.
        let convexity = b
            .get("convexity")
            .and_then(as_number)
            .map(|n| n as i64)
            .unwrap_or(0)
            .max(2) as u32;

        // angle: read from "angle" or the "a" alias; a finite value is used
        // as-is, snapping out-of-range (<= -360 or > 360) to 360; when absent
        // the default is 360. start defaults to 0 when an angle is given, else
        // 180; an explicit start overrides either default (upstream verbatim).
        let angle_value = b
            .get("angle")
            .or_else(|| b.get("a"))
            .and_then(as_number)
            .filter(|a| a.is_finite());
        let (angle, mut start) = match angle_value {
            Some(a) => (if a <= -360.0 || a > 360.0 { 360.0 } else { a }, 0.0),
            None => (360.0, 180.0),
        };
        if let Some(s) = b.get("start").and_then(as_number).filter(|s| s.is_finite()) {
            start = s;
        }

        RotateExtrudeParams {
            angle,
            start,
            convexity,
            fragments: self.extrude_fragments(&b),
        }
    }

    /// Resolve the `$fn`/`$fa`/`$fs` trio for an extrude call site (upstream
    /// `CurveDiscretizer(parameters, ...)`): each is the call's named argument
    /// if present, else the dynamically-scoped special variable, else the
    /// `Builtins` seed. Stored on the node for the kernel to derive
    /// the final segment count from the 2D profile's extent.
    /// Resolve `offset(r=…, delta=…, chamfer=…, $fn/$fa/$fs=…)` arguments
    /// (upstream `primitives.cc builtin_offset`): a defined
    /// `r` WINS over `delta` and forces `chamfer = false`; else `delta` +
    /// `chamfer` apply; NEITHER given warns and uses upstream's default
    /// `delta = 100000`. The fragments trio resolves via the standard
    /// call-site path. Non-finite distances warn and fall back to the mode
    /// default (getFiniteDouble).
    fn offset_params(&mut self, args: &[Arg]) -> OffsetParams {
        let b = self.parse_builtin_args(args, &["r", "delta", "chamfer"]);

        let to_finite =
            |v: Option<&Value>| -> Option<f64> { v.and_then(as_number).filter(|n| n.is_finite()) };

        // A non-finite value in a SUPPLIED slot warns
        // even when the other slot takes over (upstream getFiniteDouble
        // warns on each failed read, not only when both fail).
        for name in ["r", "delta"] {
            if let Some(v) = b.get(name) {
                if matches!(v, Value::Number(n) if !n.is_finite()) {
                    self.scope.warn(format!(
                        "offset parameter {name} should be a finite number, ignoring"
                    ));
                }
            }
        }
        let (delta, join, chamfer) = if let Some(r) = to_finite(b.get("r")) {
            // r-mode: round arcs; chamfer is ignored (forced false).
            (r, OffsetJoin::Round, false)
        } else if let Some(d) = to_finite(b.get("delta")) {
            // delta-mode: miter (false) or bevel (true). Upstream
            // `parameters["chamfer"].toBool()`: Bool verbatim; a nonzero
            // Number is true (so `chamfer = 1` means bevel, matching
            // upstream's Value::toBool).
            // Upstream Value::toBool: Bool verbatim; nonzero Number true;
            // non-empty String/Vector true.
            let chamfer = match b.get("chamfer") {
                Some(Value::Bool(v)) => *v,
                Some(Value::Number(n)) => *n != 0.0,
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Vector(items)) => !items.is_empty(),
                _ => false,
            };
            (
                d,
                if chamfer {
                    OffsetJoin::Bevel
                } else {
                    OffsetJoin::Miter
                },
                chamfer,
            )
        } else {
            // Neither (or non-finite): upstream warns and uses 100000.
            self.scope
                .warn("offset parameter r or delta needed, using delta = 100000");
            if b.contains_key("r") || b.contains_key("delta") {
                self.scope
                    .warn("offset parameter should be a finite number, ignoring");
            }
            (100_000.0, OffsetJoin::Miter, false)
        };

        OffsetParams {
            delta,
            join,
            chamfer,
            fragments: Fragments {
                fn_: self.fragment_var(&b, "$fn", 0.0),
                fs: self.fragment_var(&b, "$fs", 2.0),
                fa: self.fragment_var(&b, "$fa", 12.0),
            },
        }
    }

    // --- include / use ---

    /// Move errors raised inside included/used files onto the main source:
    /// the precise location goes to [`EvalError::origin`] and `span`
    /// becomes the entry `include`/`use` statement's.
    fn localize_errors(&mut self) {
        for err in &mut self.errors {
            if err.origin.is_none() {
                if let Some((origin, entry)) = self.source_map.origin(err.span) {
                    err.origin = Some(origin);
                    err.span = entry;
                }
            }
        }
    }

    /// Source text covered by `span`, in whichever file it belongs to.
    fn span_text(&self, span: Span) -> Option<&str> {
        if self.source_map.locate(span).is_some() {
            return self.source_map.text(span);
        }
        self.source.as_deref()?.get(span.start..span.end)
    }

    /// Directory the relative `include`/`use` paths of the statement at
    /// `span` resolve against: the directory of the file containing it
    /// (upstream resolves against the including file, `lexer.l
    /// includefile` / `SourceFile::registerUse`), or `None` for an unsaved
    /// main buffer.
    fn include_base_dir(&self, span: Span) -> Option<PathBuf> {
        match self.source_map.locate(span) {
            Some(file) => file.path.parent().map(Path::to_path_buf),
            None => self.source_dir.as_ref().map(|dir| {
                if dir.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    dir.clone()
                }
            }),
        }
    }

    /// Resolve an `include`/`use` target to a canonical file path, or
    /// record an error and return `None`.
    ///
    /// Search order follows upstream `find_valid_path`
    /// (`src/core/parsersettings.cc`): an absolute path as is; a relative
    /// one against the including file's directory, then each
    /// [library path](Evaluator::set_library_paths).
    ///
    /// **Containment — a deliberate divergence from upstream**, matching
    /// the `import()`/`surface()` rule: the compile result reaches an AI
    /// agent, so the target must lie under the main file's directory or
    /// under a library directory. The check is lexical first (`..`
    /// resolved without touching the filesystem, so an escaping path is
    /// refused the same way whether or not it exists) and then repeated on
    /// the canonical path (so a symlink pointing out of the tree is
    /// refused). With no source directory (an unsaved buffer) only library
    /// directories qualify.
    fn resolve_include_path(&mut self, raw: &str, op: &str, span: Span) -> Option<PathBuf> {
        if raw.trim().is_empty() {
            self.errors.push(EvalError::general(
                format!("{op} <>: empty file name"),
                span,
            ));
            return None;
        }
        let raw_path = Path::new(raw);
        let base_dir = self.include_base_dir(span);
        let mut candidates = Vec::new();
        if raw_path.is_absolute() {
            candidates.push(raw_path.to_path_buf());
        } else {
            if let Some(dir) = &base_dir {
                candidates.push(dir.join(raw_path));
            }
            candidates.extend(self.library_paths.iter().map(|lib| lib.join(raw_path)));
        }

        // Containment roots, both as given (for the lexical check) and
        // canonical (for the post-symlink check).
        let mut given_roots: Vec<PathBuf> = Vec::new();
        if let Some(dir) = &self.source_dir {
            given_roots.push(if dir.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                dir.clone()
            });
        }
        given_roots.extend(self.library_paths.iter().cloned());
        let canonical_roots: Vec<PathBuf> = given_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();
        let lexical_roots: Vec<PathBuf> = given_roots
            .iter()
            .map(|root| normalize_lexically(root))
            .chain(canonical_roots.iter().cloned())
            .collect();

        let outside = |op: &str| {
            if self.source_dir.is_some() {
                format!(
                    "{op} <{raw}>: resolves outside the source file's directory and OPENSCADPATH — only files under them can be loaded"
                )
            } else {
                format!(
                    "{op} <{raw}>: the file is unsaved, so only files under OPENSCADPATH can be loaded — save the file to {op} files next to it"
                )
            }
        };
        for (i, candidate) in candidates.into_iter().enumerate() {
            let lexical = normalize_lexically(&candidate);
            if !lexical_roots.iter().any(|root| lexical.starts_with(root)) {
                // Only the explicit path or the including-directory
                // candidate is refused outright; a library candidate that
                // escapes (`<../x>` against a library dir) is just not a
                // match.
                if raw_path.is_absolute() || (i == 0 && base_dir.is_some()) {
                    let message = outside(op);
                    self.errors.push(EvalError::general(message, span));
                    return None;
                }
                continue;
            }
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if !canonical_roots
                .iter()
                .any(|root| canonical.starts_with(root))
            {
                let message = outside(op);
                self.errors.push(EvalError::general(message, span));
                return None;
            }
            return Some(canonical);
        }
        let message = if base_dir.is_none() && !raw_path.is_absolute() {
            format!(
                "{op} <{raw}>: {op} requires the file to be saved — relative paths resolve against the saved file's directory (not found under OPENSCADPATH either)"
            )
        } else if op == "use" {
            // Upstream: "Can't open library '%1$s'." (SourceFile.cc).
            format!("Can't open library '{raw}' (searched the including file's directory and OPENSCADPATH)")
        } else {
            // Upstream: "Can't open include file '%1$s'." (lexer.l).
            format!("Can't open include file '{raw}' (searched the including file's directory and OPENSCADPATH)")
        };
        self.errors.push(EvalError::general(message, span));
        None
    }

    /// Read and parse a resolved `include`/`use` target at its own span
    /// base. Read and parse failures are recorded (a parse error carries
    /// the loaded file's location) and yield `None`.
    fn read_source(&mut self, path: &Path, op: &str, raw: &str, span: Span) -> Option<Vec<Stmt>> {
        let text = match read_capped_text(path, MAX_INCLUDE_BYTES) {
            Ok(text) => text,
            Err(reason) => {
                self.errors
                    .push(EvalError::general(format!("{op} <{raw}>: {reason}"), span));
                return None;
            }
        };
        let entry_span = self.source_map.locate(span).map_or(span, |f| f.entry_span);
        let source: Arc<str> = text.into();
        let base = self
            .source_map
            .add(path.to_path_buf(), Arc::clone(&source), entry_span);
        match morphe_lang::parser::parse_program_at(&source, base) {
            Ok(stmts) => Some(stmts),
            Err(err) => {
                self.errors.push(EvalError::general(err.message, err.span));
                None
            }
        }
    }

    /// Splice every `include <file>` in `stmts` — at any nesting depth —
    /// into a block of the file's (recursively expanded) statements.
    ///
    /// Upstream handles `include` in the lexer (`lexer.l includefile`),
    /// switching its input to the named file, so the file's text is
    /// literally inserted: its definitions, assignments and top-level
    /// geometry all join the enclosing scope (blocks are scope-transparent
    /// here too). Doing it before evaluation keeps that model: the file is
    /// read once per `include` statement, regardless of how often the
    /// enclosing body runs. A file that is already being included further
    /// up the chain is a cycle and is reported as an error (upstream warns
    /// "circular include file" and skips it, `parsersettings.cc
    /// check_valid`); so is nesting deeper than [`MAX_INCLUDE_DEPTH`].
    fn expand_includes(&mut self, stmts: &[Stmt]) -> Vec<Stmt> {
        stmts.iter().map(|stmt| self.expand_stmt(stmt)).collect()
    }

    fn expand_stmt(&mut self, stmt: &Stmt) -> Stmt {
        if !has_include(std::slice::from_ref(stmt)) {
            return stmt.clone();
        }
        let kind = match &stmt.kind {
            StmtKind::Include { path } => StmtKind::Block(self.include_file(path, stmt.span)),
            StmtKind::Block(inner) => StmtKind::Block(self.expand_includes(inner)),
            StmtKind::ModuleCall {
                name,
                args,
                modifiers,
                children,
            } => StmtKind::ModuleCall {
                name: name.clone(),
                args: args.clone(),
                modifiers: *modifiers,
                children: self.expand_includes(children),
            },
            StmtKind::ModuleDef { name, params, body } => StmtKind::ModuleDef {
                name: name.clone(),
                params: params.clone(),
                body: Box::new(self.expand_stmt(body)),
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
                modifiers,
            } => StmtKind::If {
                cond: cond.clone(),
                then_branch: Box::new(self.expand_stmt(then_branch)),
                else_branch: else_branch
                    .as_deref()
                    .map(|branch| Box::new(self.expand_stmt(branch))),
                modifiers: *modifiers,
            },
            other => other.clone(),
        };
        Stmt {
            kind,
            span: stmt.span,
        }
    }

    /// The expanded statements of one `include <raw>` (empty on error).
    fn include_file(&mut self, raw: &str, span: Span) -> Vec<Stmt> {
        let Some(path) = self.resolve_include_path(raw, "include", span) else {
            return Vec::new();
        };
        if self.including.contains(&path) {
            let chain: Vec<String> = self
                .including
                .iter()
                .chain(std::iter::once(&path))
                .map(|p| file_label(p))
                .collect();
            self.errors.push(EvalError::general(
                format!("include <{raw}>: include cycle ({})", chain.join(" -> ")),
                span,
            ));
            return Vec::new();
        }
        if self.including.len() >= MAX_INCLUDE_DEPTH {
            self.errors.push(EvalError::general(
                format!("include <{raw}>: includes nested more than {MAX_INCLUDE_DEPTH} deep"),
                span,
            ));
            return Vec::new();
        }
        let Some(stmts) = self.read_source(&path, "include", raw, span) else {
            return Vec::new();
        };
        self.including.push(path);
        let expanded = self.expand_includes(&stmts);
        self.including.pop();
        expanded
    }

    /// `use <raw>` in the scope evaluated in the current frame: load the
    /// library (once per evaluation) and make its module and function
    /// definitions visible to lookups that reach this frame (upstream
    /// `SourceFile::registerUse` + `FileContext::lookup_local_*`).
    fn register_use(&mut self, raw: &str, span: Span) {
        let Some(path) = self.resolve_include_path(raw, "use", span) else {
            return;
        };
        let Some(lib) = self.load_library(path, raw, span) else {
            return;
        };
        let uses = self.file_uses.entry(self.scope.current()).or_default();
        if !uses.contains(&lib) {
            uses.push(lib);
        }
    }

    /// Load a `use`d library: parse it (splicing its own includes),
    /// evaluate its top level in an isolated, pinned frame, and return its
    /// index in [`Evaluator::libraries`].
    ///
    /// Upstream evaluates a used file in its own `FileContext` whose parent
    /// is the builtin context (`FileContext.cc`): its top-level assignments
    /// run (so its functions and modules see them), its top-level
    /// instantiations do not, and nothing of the importing file is visible
    /// to it. Here that frame has no lexical parent and gets its own `PI`.
    /// Upstream re-creates that context on every lookup; evaluating it once
    /// is equivalent except for top-level assignments that read
    /// `$`-variables or have side effects (`echo`), which happen once, at
    /// the `use`, instead of per call.
    ///
    /// The library is registered before its body runs, so mutually `use`ing
    /// files terminate (each sees the other's definitions as far as they
    /// were registered).
    fn load_library(&mut self, path: PathBuf, raw: &str, span: Span) -> Option<usize> {
        if let Some(&index) = self.library_index.get(&path) {
            return self.libraries[index].map(|_| index);
        }
        if self.use_depth >= MAX_INCLUDE_DEPTH {
            self.errors.push(EvalError::general(
                format!("use <{raw}>: libraries nested more than {MAX_INCLUDE_DEPTH} deep"),
                span,
            ));
            return None;
        }
        let index = self.libraries.len();
        self.libraries.push(None);
        self.library_index.insert(path.clone(), index);
        let stmts = self.read_source(&path, "use", raw, span)?;
        // A used file is parsed on its own: its include chain starts afresh.
        let outer_including = std::mem::replace(&mut self.including, vec![path]);
        let stmts = self.expand_includes(&stmts);
        self.including = outer_including;

        self.use_depth += 1;
        self.scope.push_isolated();
        let frame = self.scope.current();
        self.scope.retain(frame);
        self.scope.set("PI", Value::Number(std::f64::consts::PI));
        self.libraries[index] = Some(frame);
        // Definitions, nested `use`s and assignments — no instantiations.
        self.eval_scope_body(&stmts, BodyKind::Persistent, Some(&[]));
        self.scope.pop();
        self.use_depth -= 1;
        Some(index)
    }

    /// `..` components in a file-loading
    /// construct's path are rejected LOUDLY at eval. Rationale: the compile
    /// result reaches the AI channel (and thence the provider), so a
    /// prompt-injected buffer must not point the file loader at arbitrary
    /// out-of-tree content. Upstream OpenSCAD allows `..` (no sandbox) — a
    /// documented, deliberate divergence in morphe's harder trust model
    /// (prompt-injection hardening, extended to the file channel).
    fn reject_parent_dir(&mut self, raw: &Path, op: &str, span: Span) -> bool {
        if raw
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            self.errors.push(EvalError::general(
                format!(
                    "{op}(): '..' path components are not allowed — use a path relative to the source file (absolute paths must stay under its directory)"
                ),
                span,
            ));
            return true;
        }
        false
    }

    /// Resolve a file-loading construct's `file` argument (`import`,
    /// `surface`) to the path the kernel will open, or `None` (after
    /// recording a `General` error) when the path is not allowed. An empty
    /// `file` (its error already recorded) flows through as an empty path.
    ///
    /// **Containment — a deliberate divergence from upstream** (which
    /// loads any path, no sandbox): the compile result reaches the AI
    /// channel, so a prompt-injected buffer must not read out-of-tree files.
    /// On top of the `..` rejection ([`Self::reject_parent_dir`]), when the
    /// source directory is known:
    ///
    /// - a relative path joins the source dir; an absolute path is allowed
    ///   only when it already lies under the source dir;
    /// - when the target exists, both it and the source dir are
    ///   canonicalized (resolving symlinks) and the target must stay under
    ///   the canonical source dir — a symlink pointing out of the tree is
    ///   rejected — and the CANONICAL path is what the node carries, so the
    ///   kernel opens exactly what was checked;
    /// - a target that does not exist yet is checked lexically (it cannot
    ///   be read anyway; the kernel reports it missing).
    ///
    /// With no source dir (`None`: an unsaved buffer) the path is passed
    /// through unchanged, as before. In every case the kernel additionally
    /// requires a regular file (morphe-kernel's `read_capped`), so a device
    /// node or FIFO is refused even on the unknown-source-dir path.
    ///
    /// A call written inside an included or used file resolves against
    /// THAT file's directory instead (see
    /// [`Self::resolve_load_path_in_loaded_file`]).
    ///
    /// The check is path-based at eval time and the kernel re-opens by path
    /// later; a local attacker racing the filesystem between the two is out
    /// of scope (the threat is untrusted `.scad` text, not a hostile host).
    fn resolve_load_path(&mut self, file: String, op: &str, span: Span) -> Option<PathBuf> {
        if file.is_empty() {
            return Some(PathBuf::new());
        }
        let raw = PathBuf::from(file);
        if self.reject_parent_dir(&raw, op, span) {
            return None;
        }
        if let Some(loaded) = self.source_map.locate(span) {
            let file_dir = loaded.path.parent().map(Path::to_path_buf);
            return self.resolve_load_path_in_loaded_file(raw, file_dir, op, span);
        }
        let Some(dir) = self.source_dir.clone() else {
            return Some(raw);
        };
        let joined = if raw.is_absolute() {
            raw.clone()
        } else {
            dir.join(&raw)
        };
        // `Path::parent` of a bare file name is "" — canonicalize that as ".".
        let base = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir.as_path()
        };
        let canonical_dir = base.canonicalize().ok();
        let allowed = match joined.canonicalize() {
            Ok(canonical) => match &canonical_dir {
                Some(cdir) if canonical.starts_with(cdir) => Some(canonical),
                _ => None,
            },
            Err(_) => {
                let lexically_inside = !raw.is_absolute()
                    || canonical_dir.as_ref().is_some_and(|c| raw.starts_with(c))
                    || (dir.is_absolute() && raw.starts_with(&dir));
                lexically_inside.then_some(joined)
            }
        };
        if allowed.is_none() {
            self.errors.push(EvalError::general(
                format!(
                    "{op}(): `{}` resolves outside the source file's directory — only files under it can be loaded",
                    raw.display()
                ),
                span,
            ));
        }
        allowed
    }

    /// [`Self::resolve_load_path`] for an `import()`/`surface()` call written
    /// inside an included or used file whose (canonical) directory is
    /// `file_dir`.
    ///
    /// Upstream resolves the file relative to the file the statement is
    /// written in: `do_import` (`src/core/ImportNode.cc`) and
    /// `builtin_surface` (`src/core/SurfaceNode.cc`) call
    /// `lookup_file(file, inst->location().filePath().parent_path())`
    /// (`src/io/fileutils.cc`), where the location is the
    /// `ModuleInstantiation`'s own source position — so a module defined in
    /// a library loads data next to the library even when it is
    /// instantiated from the main file.
    ///
    /// Containment matches `include`/`use`: the target must lie under the
    /// main file's directory or a library directory, checked on the
    /// canonical path when the file exists (so a symlink cannot escape) and
    /// lexically when it does not. A library file may therefore load data
    /// stored next to it inside its library directory.
    fn resolve_load_path_in_loaded_file(
        &mut self,
        raw: PathBuf,
        file_dir: Option<PathBuf>,
        op: &str,
        span: Span,
    ) -> Option<PathBuf> {
        let mut given_roots: Vec<PathBuf> = Vec::new();
        if let Some(dir) = &self.source_dir {
            given_roots.push(if dir.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                dir.clone()
            });
        }
        given_roots.extend(self.library_paths.iter().cloned());
        let canonical_roots: Vec<PathBuf> = given_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();

        let joined = match &file_dir {
            Some(dir) if !raw.is_absolute() => dir.join(&raw),
            _ => raw.clone(),
        };
        let allowed = match joined.canonicalize() {
            Ok(canonical) => canonical_roots
                .iter()
                .any(|root| canonical.starts_with(root))
                .then_some(canonical),
            Err(_) => {
                let lexical = normalize_lexically(&joined);
                given_roots
                    .iter()
                    .map(|root| normalize_lexically(root))
                    .chain(canonical_roots.iter().cloned())
                    .any(|root| lexical.starts_with(&root))
                    .then_some(joined)
            }
        };
        if allowed.is_none() {
            self.errors.push(EvalError::general(
                format!(
                    "{op}(): `{}` resolves outside the source file's directory and OPENSCADPATH — only files under them can be loaded",
                    raw.display()
                ),
                span,
            ));
        }
        allowed
    }

    /// `import(file=…, layer=…, convexity=…)` argument
    /// resolution (upstream `import.cc builtin_import`/`ImportNode`):
    /// `file` is required (missing → a structured `General` error at the
    /// call span); relative paths resolve AT EVAL TIME against the
    /// directory of the file the call is written in — `source_dir` for the
    /// main file (the absolute PathBuf rides in the node so the
    /// kernel resolve seam is a pure file-open); absolute paths are
    /// untouched. `layer` is a 3MF/DXF-only parameter (upstream .dxf layer
    /// lookup) — on an .stl import it warns + ignores, matching upstream's
    /// ignored-params discipline for non-DXF formats. `convexity` defaults
    /// to `1` (upstream `getPositiveInt(node->convexity)` clamped to ≥1).
    fn import_params(&mut self, args: &[Arg], span: Span) -> (PathBuf, u32) {
        let bound = self.parse_builtin_args(args, &["file", "layer", "convexity"]);

        if bound.contains_key("layer") {
            self.scope.warn(
                "`import` ignores `layer` for STL (layer is a 3MF/DXF-only parameter)".to_string(),
            );
        }

        let file = match bound.get("file") {
            None | Some(Value::Undef(_)) => {
                self.errors.push(EvalError::general(
                    "import(file=…) needs the `file` argument (a non-empty string)".to_string(),
                    span,
                ));
                String::new()
            }
            Some(Value::String(s)) if !s.is_empty() => s.to_string(),
            _ => {
                self.errors.push(EvalError::general(
                    "import(file=…) needs the `file` argument (a non-empty string)".to_string(),
                    span,
                ));
                String::new()
            }
        };
        let Some(path) = self.resolve_load_path(file, "import", span) else {
            return (PathBuf::new(), 1);
        };

        let convexity = match bound.get("convexity") {
            Some(Value::Number(n)) if n.is_finite() && *n >= 1.0 => *n as u32,
            _ => 1,
        };
        (path, convexity)
    }

    /// `surface(file=…, center=…, invert=…, convexity=…)` argument
    /// resolution (upstream `surface.cc`/`SurfaceNode`): `file` is required
    /// (missing → a structured `General` error at the call span); relative
    /// paths resolve AT EVAL TIME against the directory of the file the call
    /// is written in (identical to `import_params`, so the kernel resolve is
    /// a pure file-open); absolute
    /// paths untouched. `center`/`invert` default false and are set ONLY by a
    /// Bool argument (upstream gates on `Value::Type::BOOL` — a numeric
    /// `center` is ignored, not `toBool`'d). `convexity` defaults 1.
    fn surface_params(&mut self, args: &[Arg], span: Span) -> (PathBuf, bool, bool, u32) {
        let bound = self.parse_builtin_args(args, &["file", "center", "invert", "convexity"]);

        let file = match bound.get("file") {
            None | Some(Value::Undef(_)) => {
                self.errors.push(EvalError::general(
                    "surface(file=…) needs the `file` argument (a non-empty string)".to_string(),
                    span,
                ));
                String::new()
            }
            Some(Value::String(s)) if !s.is_empty() => s.to_string(),
            _ => {
                self.errors.push(EvalError::general(
                    "surface(file=…) needs the `file` argument (a non-empty string)".to_string(),
                    span,
                ));
                String::new()
            }
        };
        let Some(path) = self.resolve_load_path(file, "surface", span) else {
            return (PathBuf::new(), false, false, 1);
        };

        let center = matches!(bound.get("center"), Some(Value::Bool(true)));
        let invert = matches!(bound.get("invert"), Some(Value::Bool(true)));
        let convexity = match bound.get("convexity") {
            Some(Value::Number(n)) if n.is_finite() && *n >= 1.0 => *n as u32,
            _ => 1,
        };
        (path, center, invert, convexity)
    }

    /// `text(text, size, font, halign, valign, spacing, direction,
    /// …)` argument resolution (upstream `text.cc builtin_text`): only the
    /// supported set survives — `language`/`script`/`tp` are warn-and-ignored
    /// (the generic unknown-parameter warning from `parse_builtin_args`).
    /// `font`: an unknown family (anything non-empty that isn't the bundled
    /// one) warns + falls back — the node carries no font name. `direction`:
    /// ltr only; rtl/ttb/btt warn + fall back (not yet supported). `halign`/`valign`
    /// validate against the upstream enums; invalid values warn + default.
    /// Fragments resolve from the call-site `$fn`/`$fa`/`$fs` trio (the
    /// Bézier flattening count).
    fn text_params(&mut self, args: &[Arg], span: Span) -> TextParams {
        use crate::csg::{TextHalign, TextValign};
        let bound = self.parse_builtin_args(
            args,
            &[
                "text",
                "size",
                "font",
                "halign",
                "valign",
                "spacing",
                "direction",
                "language",
                "script",
            ],
        );

        let text = match bound.get("text") {
            Some(Value::String(s)) => s.to_string(),
            _ => String::new(),
        };
        let size = match bound.get("size") {
            Some(Value::Number(n)) if n.is_finite() && *n > 0.0 => *n,
            Some(_) => {
                // A PROVIDED-but-invalid size is not silently substituted.
                self.scope
                    .warn("invalid text size (need a positive number), using 10".to_string());
                10.0
            }
            None => 10.0, // upstream default
        };
        if let Some(Value::String(font)) = bound.get("font") {
            if &**font != TEXT_FONT_FAMILY && !font.is_empty() {
                self.scope.warn(format!(
                    "unknown font '{font}', falling back to the bundled '{TEXT_FONT_FAMILY}'"
                ));
            }
        }
        let halign = match bound.get("halign") {
            None | Some(Value::Undef(_)) => TextHalign::Left,
            Some(Value::String(s)) => match &**s {
                "left" => TextHalign::Left,
                "center" => TextHalign::Center,
                "right" => TextHalign::Right,
                other => {
                    self.scope.warn(format!(
                        "unknown horizontal alignment '{other}', using 'left'"
                    ));
                    TextHalign::Left
                }
            },
            _ => TextHalign::Left,
        };
        let valign = match bound.get("valign") {
            None | Some(Value::Undef(_)) => TextValign::Baseline,
            Some(Value::String(s)) => match &**s {
                "baseline" => TextValign::Baseline,
                "top" => TextValign::Top,
                "center" => TextValign::Center,
                "bottom" => TextValign::Bottom,
                other => {
                    self.scope.warn(format!(
                        "unknown vertical alignment '{other}', using 'baseline'"
                    ));
                    TextValign::Baseline
                }
            },
            _ => TextValign::Baseline,
        };
        let spacing = match bound.get("spacing") {
            Some(Value::Number(n)) if n.is_finite() && *n > 0.0 => *n,
            Some(_) => {
                self.scope
                    .warn("invalid text spacing (need a positive number), using 1".to_string());
                1.0
            }
            None => 1.0,
        };
        if let Some(Value::String(dir)) = bound.get("direction") {
            if &**dir != "ltr" {
                self.scope.warn(format!(
                    "text direction '{dir}' is not supported (ltr only), using 'ltr'"
                ));
            }
        }
        let fragments = self.extrude_fragments(&bound);
        let _ = span;
        TextParams {
            text,
            size,
            halign,
            valign,
            spacing,
            fragments,
        }
    }

    /// Resolve `projection(cut=…)` arguments (upstream `ProjectionNode`):
    /// `cut` defaults false; upstream `toBool` semantics (Bool verbatim,
    /// nonzero Number true, non-empty String/Vector true).
    fn projection_params(&mut self, args: &[Arg]) -> bool {
        let b = self.parse_builtin_args(args, &["cut"]);
        match b.get("cut") {
            Some(Value::Bool(v)) => *v,
            Some(Value::Number(n)) => *n != 0.0,
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Vector(items)) => !items.is_empty(),
            _ => false,
        }
    }

    fn extrude_fragments(&self, bound: &HashMap<String, Value>) -> Fragments {
        Fragments {
            fn_: self.fragment_var(bound, "$fn", 0.0),
            fs: self.fragment_var(bound, "$fs", 2.0),
            fa: self.fragment_var(bound, "$fa", 12.0),
        }
    }

    /// Upstream `builtin_translate`: `v` is a vec3 (a vec2 is padded with
    /// `z = 0`); a non-vec/non-finite `v` warns and leaves the identity
    /// matrix (`getVec3(...,0.0)` fails, so `matrix.translate` never runs).
    fn transform_translate(&mut self, bound: &HashMap<String, Value>) -> Affine {
        let value = bound.get("v");
        match value.and_then(|v| as_vec3_pad(v, 0.0)) {
            Some(v) if v.iter().all(|c| c.is_finite()) => translate_matrix(v),
            _ => {
                self.scope.warn(format!(
                    "Unable to convert translate({}) parameter to a vec3 or vec2 of numbers",
                    echo_or_undef(value)
                ));
                identity_matrix()
            }
        }
    }

    /// Upstream `builtin_scale`: `v` is a vec3 (a vec2 is padded with
    /// `z = 1`) or a scalar (all three axes); default `[1, 1, 1]`. A
    /// non-vec/non-number `v` warns and keeps the identity. The upstream
    /// zero/non-finite `rangeCheck` warning is behind a debug flag (off by
    /// default), so a zero or negative scale is stored verbatim here.
    fn transform_scale(&mut self, bound: &HashMap<String, Value>) -> Affine {
        let value = bound.get("v");
        if let Some(v) = value.and_then(|v| as_vec3_pad(v, 1.0)) {
            scale_matrix(v)
        } else if let Some(n) = value.and_then(as_number) {
            scale_matrix([n, n, n])
        } else {
            self.scope.warn(format!(
                "Unable to convert scale({}) parameter to a number, a vec3 or vec2 of numbers or a number",
                echo_or_undef(value)
            ));
            scale_matrix([1.0, 1.0, 1.0])
        }
    }

    /// Upstream `builtin_rotate`: when `a` is a VECTOR, an Euler rotation
    /// `Rz(az) * Ry(ay) * Rx(ax)` (`v` is ignored, with a warning); missing
    /// components default to angle 0. Otherwise a scalar `a` rotates about
    /// the axis `v` (default `[0, 0, 1]`, i.e. the Z axis), padded via
    /// `getVec3(...,0.0)`. A non-finite/non-convertible `a` warns.
    fn transform_rotate(&mut self, bound: &HashMap<String, Value>) -> Affine {
        let val_v = bound.get("v");
        match bound.get("a") {
            Some(Value::Vector(vec_a)) => {
                let mut angles = [0.0, 0.0, 0.0];
                // Upstream reads index 0/1/2 for the x/y/z angles; a shorter
                // vector leaves the missing components at 0, a longer one
                // (or a non-finite component) sets ok=false (a warning) but
                // still uses the first three.
                let mut ok = vec_a.len() <= 3;
                for (i, angle) in angles.iter_mut().enumerate() {
                    if let Some(component) = vec_a.get(i) {
                        match as_number(component) {
                            Some(n) if n.is_finite() => *angle = n,
                            _ => ok = false,
                        }
                    }
                }
                if !ok {
                    self.scope.warn(format!(
                        "Problem converting rotate(a={}) parameter",
                        Value::Vector(vec_a.clone()).to_echo_string()
                    ));
                } else if val_v.is_some_and(|v| !v.is_undef()) {
                    self.scope
                        .warn("When parameter a is supplied as vector, v is ignored in rotate()");
                }
                rotate_euler_matrix(angles)
            }
            other => {
                let a = other
                    .and_then(as_number)
                    .filter(|a| a.is_finite())
                    .unwrap_or_else(|| {
                        if other.is_some_and(|v| !v.is_undef()) {
                            self.scope.warn(format!(
                                "Problem converting rotate(a={}) parameter",
                                echo_or_undef(other)
                            ));
                        }
                        0.0
                    });
                let v = val_v
                    .and_then(|v| as_vec3_pad(v, 0.0))
                    .unwrap_or([0.0, 0.0, 1.0]);
                angle_axis_matrix(a, v)
            }
        }
    }

    /// Upstream `builtin_mirror`: reflect across the plane through the origin
    /// with normal `v` (a vec2 padded with `z = 0`); default normal
    /// `[1, 0, 0]` (mirror across the YZ plane). A zero normal yields the
    /// identity (upstream skips the matrix when `x == y == z == 0`). A
    /// non-vec `v` warns and keeps the default normal.
    fn transform_mirror(&mut self, bound: &HashMap<String, Value>) -> Affine {
        let value = bound.get("v");
        match value.and_then(|v| as_vec3_pad(v, 0.0)) {
            Some(v) => mirror_matrix(v),
            None => {
                self.scope.warn(format!(
                    "Unable to convert mirror({}) parameter to a vec3 or vec2 of numbers",
                    echo_or_undef(value)
                ));
                mirror_matrix([1.0, 0.0, 0.0])
            }
        }
    }

    /// Upstream `builtin_color`: `c` a vec (first 4 components → RGBA, the
    /// rest default to 1.0) or a color string (named web color or `#hex`);
    /// an out-of-range channel warns. An optional `alpha` number overrides
    /// the alpha channel. Unsupplied channels keep the inherit sentinel
    /// `-1`. Named-color resolution covers the CSS web-color list and hex
    /// forms (the `xkcd:` palette is not embedded).
    fn transform_color(&mut self, bound: &HashMap<String, Value>) -> TransformKind {
        let mut color = [-1.0f32, -1.0, -1.0, -1.0];
        match bound.get("c") {
            Some(Value::Vector(vec)) => {
                for (i, channel) in color.iter_mut().enumerate() {
                    // Upstream: i < size ? (float)vec[i].toDouble() : 1.0
                    // (toDouble of a non-number is 0).
                    let c = match vec.get(i) {
                        Some(v) => as_number(v).unwrap_or(0.0) as f32,
                        None => 1.0,
                    };
                    if !(0.0..=1.0).contains(&c) {
                        self.scope.warn(format!(
                            "color() expects numbers between 0.0 and 1.0. Value of {c:.1} is out of range"
                        ));
                    }
                    *channel = c;
                }
            }
            Some(Value::String(name)) => match parse_color(name) {
                Some(rgba) => color = rgba,
                None => self.scope.warn(format!("Unable to parse color \"{name}\"")),
            },
            _ => {}
        }
        if let Some(Value::Number(alpha)) = bound.get("alpha") {
            color[3] = *alpha as f32;
            if !(0.0..=1.0).contains(&color[3]) {
                self.scope.warn(format!(
                    "color() expects alpha between 0.0 and 1.0. Value of {:.1} is out of range",
                    color[3]
                ));
            }
        }
        TransformKind::Color(color)
    }

    /// Bind evaluated builtin-module arguments to a fixed, ordered parameter
    /// name list (upstream `Parameters::parse` / `parse_without_defaults`):
    /// positional arguments fill `names` in declaration order (skipping
    /// names already given), named arguments match by name. A named argument
    /// for a name outside `names` warns `variable '%s' not specified as
    /// parameter` — except config (`$`) variables, which bind silently so a
    /// call-site `$fn`/`$fa`/`$fs` reaches fragment resolution. A duplicate
    /// name warns `argument '%s' supplied more than once` (last wins);
    /// surplus positional arguments warn `Too many unnamed arguments
    /// supplied` once. Returns the supplied name → value map; unsupplied
    /// parameters are absent, so each constructor applies the upstream
    /// node-constructor default.
    fn parse_builtin_args(&mut self, args: &[Arg], names: &[&str]) -> HashMap<String, Value> {
        let mut bound: HashMap<String, Value> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut position = 0;
        let mut warned_extra = false;
        for arg in args {
            let value = self.eval_nested(&arg.value);
            match arg.name.as_deref() {
                Some(name) => {
                    if seen.contains(name) {
                        self.scope
                            .warn(format!("argument '{name}' supplied more than once"));
                    } else if !is_config_variable(name) && !names.contains(&name) {
                        self.scope
                            .warn(format!("variable '{name}' not specified as parameter"));
                    }
                    seen.insert(name.to_string());
                    bound.insert(name.to_string(), value);
                }
                None => {
                    let mut placed = false;
                    while position < names.len() {
                        let candidate = names[position];
                        position += 1;
                        if !seen.contains(candidate) {
                            bound.insert(candidate.to_string(), value.clone());
                            seen.insert(candidate.to_string());
                            placed = true;
                            break;
                        }
                    }
                    if !placed && !warned_extra {
                        self.scope.warn("Too many unnamed arguments supplied");
                        warned_extra = true;
                    }
                }
            }
        }
        bound
    }

    /// Upstream `primitives.cc lookup_radius(parameters, inst, d_var,
    /// r_var)`: diameter wins over radius. When the diameter argument is a
    /// number it returns `d / 2` (warning `Ignoring radius variable '%s' as
    /// diameter '%s' is defined too.` if the radius is also a number);
    /// otherwise the radius if it is a number; otherwise `None` (upstream
    /// undefined). Only `NUMBER`-typed arguments count as defined, so an
    /// explicit undef behaves like an omitted argument.
    fn lookup_radius(
        &mut self,
        bound: &HashMap<String, Value>,
        d_var: &str,
        r_var: &str,
    ) -> Option<f64> {
        let d = bound.get(d_var).and_then(as_number);
        let r = bound.get(r_var).and_then(as_number);
        match (d, r) {
            (Some(d), Some(_)) => {
                self.scope.warn(format!(
                    "Ignoring radius variable '{r_var}' as diameter '{d_var}' is defined too."
                ));
                Some(d / 2.0)
            }
            (Some(d), None) => Some(d / 2.0),
            (None, Some(r)) => Some(r),
            (None, None) => None,
        }
    }

    /// Resolve a circular segment count for a primitive of radius `r`,
    /// reading `$fn`/`$fa`/`$fs` at the call site. A named `$fn`/`$fa`/`$fs`
    /// argument on the call overrides the dynamically-scoped value (upstream
    /// `Parameters::parse` binds it into the `CurveDiscretizer`'s context),
    /// then [`resolve_fragments`] applies (upstream `get_fragments_from_r`).
    fn resolve_fragments_for(&mut self, bound: &HashMap<String, Value>, r: f64) -> u32 {
        let fn_ = self.fragment_var(bound, "$fn", 0.0);
        let fs = self.fragment_var(bound, "$fs", 2.0);
        let fa = self.fragment_var(bound, "$fa", 12.0);
        resolve_fragments(r, fn_, fs, fa)
    }

    /// One `$fn`/`$fa`/`$fs` value for fragment resolution: the call's named
    /// argument if present, else the dynamically-scoped special variable,
    /// else the upstream `Builtins` seed `default`.
    fn fragment_var(&self, bound: &HashMap<String, Value>, name: &str, default: f64) -> f64 {
        if let Some(Value::Number(n)) = bound.get(name) {
            return *n;
        }
        match self.scope.try_lookup(name) {
            Some(Value::Number(n)) => *n,
            _ => default,
        }
    }

    /// Upstream `builtin_cube`: `size` is a scalar (all three dimensions) or
    /// a vec3; an undef/absent `size` keeps the default `[1, 1, 1]`, and any
    /// other value warns and keeps the default. `center` is applied only
    /// when the argument is a bool (upstream `type() == BOOL`).
    fn primitive_cube(&mut self, bound: &HashMap<String, Value>) -> Primitive {
        let mut size = [1.0, 1.0, 1.0];
        if let Some(value) = bound.get("size") {
            if !value.is_undef() {
                if let Some(n) = as_number(value) {
                    size = [n, n, n];
                } else if let Some(v) = as_vec_n(value, 3) {
                    size = [v[0], v[1], v[2]];
                } else {
                    self.scope.warn(format!(
                        "Unable to convert cube(size={}, ...) parameter to a number or a vec3 of numbers",
                        value.to_echo_string()
                    ));
                }
            }
        }
        let center = bound.get("center").and_then(as_bool).unwrap_or(false);
        Primitive::Cube { size, center }
    }

    /// Upstream `builtin_square`: like [`Self::primitive_cube`] but a scalar
    /// or vec2, default `[1, 1]`.
    fn primitive_square(&mut self, bound: &HashMap<String, Value>) -> Primitive {
        let mut size = [1.0, 1.0];
        if let Some(value) = bound.get("size") {
            if !value.is_undef() {
                if let Some(n) = as_number(value) {
                    size = [n, n];
                } else if let Some(v) = as_vec_n(value, 2) {
                    size = [v[0], v[1]];
                } else {
                    self.scope.warn(format!(
                        "Unable to convert square(size={}, ...) parameter to a number or a vec2 of numbers",
                        value.to_echo_string()
                    ));
                }
            }
        }
        let center = bound.get("center").and_then(as_bool).unwrap_or(false);
        Primitive::Square { size, center }
    }

    /// Upstream `builtin_cylinder`: `h` defaults to 1; `r`/`d` set both
    /// radii, then `r1`/`d1` and `r2`/`d2` override each end (defaults 1).
    /// A number `r`/`d` alongside any `r1`/`r2`/`d1`/`d2` warns `Cylinder
    /// parameters ambiguous`. The segment count derives from `max(r1, r2)`.
    fn primitive_cylinder(&mut self, bound: &HashMap<String, Value>) -> Primitive {
        let h = bound.get("h").and_then(as_number).unwrap_or(1.0);
        let r = self.lookup_radius(bound, "d", "r");
        let r1 = self.lookup_radius(bound, "d1", "r1");
        let r2 = self.lookup_radius(bound, "d2", "r2");
        if r.is_some() && (r1.is_some() || r2.is_some()) {
            self.scope.warn("Cylinder parameters ambiguous");
        }
        let mut cr1 = 1.0;
        let mut cr2 = 1.0;
        if let Some(r) = r {
            cr1 = r;
            cr2 = r;
        }
        if let Some(r1) = r1 {
            cr1 = r1;
        }
        if let Some(r2) = r2 {
            cr2 = r2;
        }
        let center = bound.get("center").and_then(as_bool).unwrap_or(false);
        let fragments = self.resolve_fragments_for(bound, cr1.max(cr2));
        Primitive::Cylinder {
            h,
            r1: cr1,
            r2: cr2,
            center,
            fragments,
        }
    }

    /// Upstream `builtin_polygon`: `points` is required (a non-vector warns
    /// and yields an empty polygon); each point is a vec2 (a bad one warns
    /// and becomes `[0, 0]`). `paths` is optional — a vector of index lists
    /// (out-of-bounds / non-number entries warn and are skipped), `None`
    /// when omitted or undef, and any other type warns and drops paths.
    fn primitive_polygon(&mut self, bound: &HashMap<String, Value>) -> Primitive {
        let Some(Value::Vector(items)) = bound.get("points") else {
            self.scope.warn(format!(
                "Unable to convert points = {} to a vector of coordinates",
                echo_or_undef(bound.get("points"))
            ));
            return Primitive::Polygon {
                points: Vec::new(),
                paths: None,
                convexity: 1,
            };
        };
        let mut points: Vec<[f64; 2]> = Vec::new();
        for (i, pv) in items.iter().enumerate() {
            match as_vec_n(pv, 2) {
                Some(v) if v[0].is_finite() && v[1].is_finite() => points.push([v[0], v[1]]),
                _ => {
                    self.scope.warn(format!(
                        "Unable to convert points[{i}] = {} to a vec2 of numbers",
                        pv.to_echo_string()
                    ));
                    points.push([0.0, 0.0]);
                }
            }
        }
        let paths = match bound.get("paths") {
            Some(Value::Vector(path_items)) => {
                let mut paths = Vec::new();
                for (pi, pathv) in path_items.iter().enumerate() {
                    let Value::Vector(idx_items) = pathv else {
                        self.scope.warn(format!(
                            "Unable to convert paths[{pi}] = {} to a vector of numbers",
                            pathv.to_echo_string()
                        ));
                        continue;
                    };
                    paths.push(self.parse_index_list(idx_items, points.len(), "paths", pi));
                }
                Some(paths)
            }
            Some(value) if !value.is_undef() => {
                self.scope.warn(format!(
                    "Unable to convert paths = {} to a vector of vector of point indices",
                    value.to_echo_string()
                ));
                return Primitive::Polygon {
                    points,
                    paths: None,
                    convexity: 1,
                };
            }
            _ => None,
        };
        Primitive::Polygon {
            points,
            paths,
            convexity: convexity_of(bound),
        }
    }

    /// Upstream `builtin_polyhedron`: `points` required (each a vec3, or a
    /// vec2 padded with `z = 0`; a bad one warns and becomes `[0, 0, 0]`).
    /// `faces` is a vector of index lists, each kept only with 3+ in-bounds
    /// indices. Accepts the legacy `triangles` alias (removed on OpenSCAD
    /// master, kept here for backward compatibility) with a deprecation warning; a
    /// missing/non-vector `faces` (and no `triangles`) warns and yields no
    /// faces.
    fn primitive_polyhedron(&mut self, bound: &HashMap<String, Value>) -> Primitive {
        let Some(Value::Vector(items)) = bound.get("points") else {
            self.scope.warn(format!(
                "Unable to convert points = {} to a vector of coordinates",
                echo_or_undef(bound.get("points"))
            ));
            return Primitive::Polyhedron {
                points: Vec::new(),
                faces: Vec::new(),
                convexity: 1,
            };
        };
        let mut points: Vec<[f64; 3]> = Vec::new();
        for (i, pv) in items.iter().enumerate() {
            match as_vec3_padded(pv) {
                Some(p) if p.iter().all(|c| c.is_finite()) => points.push(p),
                _ => {
                    self.scope.warn(format!(
                        "Unable to convert points[{i}] = {} to a vec3 of numbers",
                        pv.to_echo_string()
                    ));
                    points.push([0.0, 0.0, 0.0]);
                }
            }
        }

        // Legacy `triangles` fallback: only when `faces` is absent/undef.
        let faces_is_vector = matches!(bound.get("faces"), Some(Value::Vector(_)));
        let face_items: Option<&Vec<Value>> = if let Some(Value::Vector(v)) = bound.get("faces") {
            Some(v)
        } else if !faces_is_vector {
            match bound.get("triangles") {
                Some(Value::Vector(v)) => {
                    self.scope.warn(
                        "polyhedron(triangles=[]) will be removed in future releases. \
                         Use polyhedron(faces=[]) instead.",
                    );
                    Some(v)
                }
                _ => None,
            }
        } else {
            None
        };
        let Some(face_items) = face_items else {
            self.scope.warn(format!(
                "Unable to convert faces = {} to a vector of vector of point indices",
                echo_or_undef(bound.get("faces"))
            ));
            return Primitive::Polyhedron {
                points,
                faces: Vec::new(),
                convexity: 1,
            };
        };
        let mut faces = Vec::new();
        for (fi, facev) in face_items.iter().enumerate() {
            let Value::Vector(idx_items) = facev else {
                self.scope.warn(format!(
                    "Unable to convert faces[{fi}] = {} to a vector of numbers",
                    facev.to_echo_string()
                ));
                continue;
            };
            let face = self.parse_index_list(idx_items, points.len(), "faces", fi);
            // Upstream keeps a face only when it has 3+ vertices.
            if face.len() >= 3 {
                faces.push(face);
            }
        }
        Primitive::Polyhedron {
            points,
            faces,
            convexity: convexity_of(bound),
        }
    }

    /// Parse one vector of point indices (a polygon `path` or polyhedron
    /// `face`), upstream's inner loop: a non-number entry warns and is
    /// skipped; an out-of-bounds index warns and is skipped; the rest are
    /// collected in order. `kind`/`outer` name the enclosing collection for
    /// the warning text.
    fn parse_index_list(
        &mut self,
        idx_items: &[Value],
        point_count: usize,
        kind: &str,
        outer: usize,
    ) -> Vec<usize> {
        let mut out = Vec::new();
        for (ii, iv) in idx_items.iter().enumerate() {
            match as_number(iv) {
                Some(n) if n >= 0.0 && (n as usize) < point_count => out.push(n as usize),
                Some(n) => self.scope.warn(format!(
                    "Point index {} is out of bounds (from {kind}[{outer}][{ii}])",
                    n as i64
                )),
                None => self.scope.warn(format!(
                    "Unable to convert {kind}[{outer}][{ii}] = {} to a number",
                    iv.to_echo_string()
                )),
            }
        }
        out
    }

    /// Upstream `Parameters.cc parse_without_defaults` + the defaults loop
    /// of `Parameters::parse`.
    ///
    /// Runs with the (still empty) call frame current: defaults therefore
    /// evaluate against the *defining* context — earlier parameters are
    /// NOT visible to later defaults — while `$`-names still resolve
    /// dynamically through the caller. Returns the ordered bindings for
    /// the caller to install.
    fn bind_parameters(
        &mut self,
        args: Vec<(Option<&str>, Value)>,
        params: &[Param],
    ) -> Vec<(String, Value)> {
        let mut bound: Vec<(String, Value)> = Vec::with_capacity(params.len());
        let mut named_seen: HashSet<String> = HashSet::new();
        let mut position = 0;
        let mut warned_extra = false;

        for (arg_name, value) in args {
            let name = match arg_name {
                Some(name) => {
                    if named_seen.contains(name) {
                        self.scope
                            .warn(format!("argument '{name}' supplied more than once"));
                    } else if bound.iter().any(|(n, _)| n == name) {
                        self.scope
                            .warn(format!("argument '{name}' overrides positional argument"));
                    } else if !is_config_variable(name) && !params.iter().any(|p| p.name == name) {
                        // Upstream warns but still binds the variable.
                        self.scope
                            .warn(format!("variable '{name}' not specified as parameter"));
                    }
                    named_seen.insert(name.to_string());
                    name.to_string()
                }
                None => {
                    // Positional: next parameter not already given by name.
                    let mut positional = None;
                    while position < params.len() {
                        let candidate = &params[position].name;
                        position += 1;
                        if !named_seen.contains(candidate) {
                            positional = Some(candidate.clone());
                            break;
                        }
                    }
                    match positional {
                        Some(name) => name,
                        None => {
                            if !warned_extra {
                                self.scope.warn("Too many unnamed arguments supplied");
                                warned_extra = true;
                            }
                            continue;
                        }
                    }
                }
            };
            match bound.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = value,
                None => bound.push((name, value)),
            }
        }

        for param in params {
            if bound.iter().any(|(n, _)| n == &param.name) {
                continue;
            }
            let value = match &param.default {
                Some(default) => self.eval_nested(default),
                // Missing without default binds plain undef — no
                // fall-through to a same-named outer variable.
                None => Value::Undef(None),
            };
            bound.push((param.name.clone(), value));
        }
        bound
    }
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

/// How diagnostics name a call's callee: the bare name, or upstream's
/// `(<expr>)` rendering for any other callee expression.
fn callee_label(callee: &Expr) -> String {
    match &callee.kind {
        ExprKind::Ident(name) | ExprKind::DollarIdent(name) => name.clone(),
        _ => format!("({})", crate::print::expr_to_string(callee)),
    }
}

/// Whether `stmts` contain an `include` statement at any nesting depth.
fn has_include(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match &stmt.kind {
        StmtKind::Include { .. } => true,
        StmtKind::Block(inner)
        | StmtKind::ModuleCall {
            children: inner, ..
        } => has_include(inner),
        StmtKind::ModuleDef { body, .. } => has_include(std::slice::from_ref(body)),
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            has_include(std::slice::from_ref(then_branch))
                || else_branch
                    .as_deref()
                    .is_some_and(|branch| has_include(std::slice::from_ref(branch)))
        }
        _ => false,
    })
}

/// Resolve `.` and `..` components without consulting the filesystem.
/// A relative path is first made absolute against the working directory,
/// so it compares like the canonical containment roots.
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A short name for a file in diagnostics: its file name, or the full path.
fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Flatten braced blocks into their enclosing scope (`parser.y` does this
/// at parse time: block statements are added to `scope_stack.top()`).
fn flatten_scope<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Block(inner) => flatten_scope(inner, out),
            _ => out.push(stmt),
        }
    }
}

/// Whether a statement is a module instantiation for scope evaluation and
/// `$children`/`children(...)` indexing (upstream
/// `LocalScope::moduleInstantiations` — if/else counts, and `*`-disabled
/// instantiations were deleted at parse time upstream, so they never count).
fn is_instantiation(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::ModuleCall { modifiers, .. } | StmtKind::If { modifiers, .. } => {
            !modifiers.disable
        }
        _ => false,
    }
}

/// Number of module instantiations in a children scope (upstream
/// `Children::size`, i.e. `moduleInstantiations.size()`).
fn count_instantiations(stmts: &[Stmt]) -> usize {
    let mut flat = Vec::new();
    flatten_scope(stmts, &mut flat);
    flat.into_iter().filter(|s| is_instantiation(s)).count()
}

/// Whether an expression is one of the five list-comprehension kinds
/// (upstream's `ListComprehension` subclasses), which flatten into their
/// enclosing vector rather than contributing a single value.
fn is_comprehension(kind: &ExprKind) -> bool {
    matches!(
        kind,
        ExprKind::LcFor { .. }
            | ExprKind::LcForC { .. }
            | ExprKind::LcEach { .. }
            | ExprKind::LcIf { .. }
            | ExprKind::LcLet { .. }
    )
}

/// Number of facets a circle/arc of radius `r` is rendered with, given the
/// special variables `$fn`/`$fa`/`$fs` (upstream `Calc::get_fragments_from_r`,
/// `src/utils/calc.cc` at tag openscad-2021.01, lines 39-46; preserved on
/// master as `CurveDiscretizer::getCircularSegmentCount` for the full-circle
/// case):
///
/// ```text
/// if (r < GRID_FINE || isinf(fn) || isnan(fn)) return 3;
/// if (fn > 0.0) return static_cast<int>(fn >= 3 ? fn : 3);
/// return static_cast<int>(ceil(fmax(fmin(360 / fa, r*2*PI / fs), 5)));
/// ```
///
/// A positive `$fn` forces a fixed count, clamped up to 3 and truncated
/// toward zero. A zero `$fn` derives the count from the angular (`$fa`) and
/// arc-size (`$fs`) limits — whichever is tighter — with a floor of 5. A
/// degenerate radius below `GRID_FINE` yields the minimum 3. The primitive
/// constructors consume this for cylinder/sphere/circle segment counts.
///
/// The parameter order matches upstream `get_fragments_from_r(double r,
/// double fn, double fs, double fa)` exactly — `$fs` (arc size) precedes
/// `$fa` (angle) — so a caller porting upstream positional arguments maps
/// straight across without transposition.
///
/// `$fn`/`$fs`/`$fa` are first clamped by [`clamp_fragment_vars`] (upstream
/// `CurveDiscretizer` floors `$fs`/`$fa` at [`F_MINIMUM`]), so `$fa=0` or
/// `$fs=0` can no longer divide by zero into a `u32::MAX` count. The result
/// is NOT capped here — the kernel tessellators enforce their own hard
/// fragment caps (a loud error, never a silent re-shape).
pub fn resolve_fragments(r: f64, fn_: f64, fs: f64, fa: f64) -> u32 {
    // GRID_FINE (upstream src/geometry/Grid.h): 2^-20.
    const GRID_FINE: f64 = 0.000_000_953_674_316_406_25;
    let (fn_, fs, fa) = clamp_fragment_vars(fn_, fs, fa);
    if r < GRID_FINE || fn_.is_infinite() || fn_.is_nan() {
        return 3;
    }
    if fn_ > 0.0 {
        // static_cast<int>(fn >= 3 ? fn : 3): truncates toward zero.
        return if fn_ >= 3.0 { fn_ as u32 } else { 3 };
    }
    (360.0 / fa)
        .min(r * 2.0 * std::f64::consts::PI / fs)
        .max(5.0)
        .ceil() as u32
}

/// OpenSCAD `F_MINIMUM` (`src/core/CurveDiscretizer.cc`): the floor the
/// curve discretizer clamps `$fs`/`$fa` to before any fragment arithmetic.
pub const F_MINIMUM: f64 = 0.01;

/// The ONE place `$fn`/`$fs`/`$fa` are clamped before fragment arithmetic
/// (upstream `CurveDiscretizer` constructor: `fn = max(fn, 0)`,
/// `fs = max(fs, F_MINIMUM)`, `fa = max(fa, F_MINIMUM)`). Returned in the
/// `(fn, fs, fa)` order of [`resolve_fragments`]'s trailing parameters.
///
/// The comparisons mirror C++ `std::max(a, b)` (`a < b ? b : a`), so a NaN
/// passes through unchanged exactly as upstream (Rust's `f64::max` would
/// silently replace it); [`resolve_fragments`] then maps a NaN `$fn` to 3.
/// Every fragment consumer — [`resolve_fragments`],
/// [`Fragments::clamped`](crate::csg::Fragments::clamped) and through it the
/// kernel's extrude slice estimators — routes through this function.
#[must_use]
pub fn clamp_fragment_vars(fn_: f64, fs: f64, fa: f64) -> (f64, f64, f64) {
    let floor = |v: f64, min: f64| if v < min { min } else { v };
    (floor(fn_, 0.0), floor(fs, F_MINIMUM), floor(fa, F_MINIMUM))
}

/// A [`Value`] as a plain number, else `None` (upstream `Value::getDouble`
/// / `type() == NUMBER`).
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

/// A [`Value`] as a bool, else `None` (upstream `type() == BOOL`).
fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// A [`Value`] as an exactly-`n`-element vector of numbers, else `None`
/// (upstream `Value::getVec2`/`getVec3`, which require the exact arity).
fn as_vec_n(value: &Value, n: usize) -> Option<Vec<f64>> {
    let Value::Vector(items) = value else {
        return None;
    };
    if items.len() != n {
        return None;
    }
    items.iter().map(as_number).collect()
}

/// A [`Value`] as a `[x, y, z]`, accepting a vec2 padded with `z = 0`
/// (upstream `getVec3(x, y, z, 0.0)`, used by `polyhedron` points), else
/// `None`.
fn as_vec3_padded(value: &Value) -> Option<[f64; 3]> {
    let Value::Vector(items) = value else {
        return None;
    };
    match items.len() {
        2 => Some([as_number(&items[0])?, as_number(&items[1])?, 0.0]),
        3 => Some([
            as_number(&items[0])?,
            as_number(&items[1])?,
            as_number(&items[2])?,
        ]),
        _ => None,
    }
}

/// A [`Value`] as a `[x, y, z]`, accepting a vec2 padded with `z = pad`
/// (upstream `VectorType::getVec3(x, y, z, pad)`, which accepts exactly a
/// vec2 or vec3 — not a scalar, not other arities), else `None`. `translate`/
/// `rotate(v=…)`/`mirror` pad with `0`, `scale` with `1`.
fn as_vec3_pad(value: &Value, pad: f64) -> Option<[f64; 3]> {
    let Value::Vector(items) = value else {
        return None;
    };
    match items.len() {
        2 => Some([as_number(&items[0])?, as_number(&items[1])?, pad]),
        3 => Some([
            as_number(&items[0])?,
            as_number(&items[1])?,
            as_number(&items[2])?,
        ]),
        _ => None,
    }
}

/// Degrees-to-radians factor (upstream `M_DEG2RAD`).
const DEG2RAD: f64 = std::f64::consts::PI / 180.0;

/// The 4×4 identity affine matrix.
fn identity_matrix() -> Affine {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Upstream `Transform3d::translate(v)` from identity: the translation goes
/// in column 3.
fn translate_matrix(v: [f64; 3]) -> Affine {
    let mut m = identity_matrix();
    m[0][3] = v[0];
    m[1][3] = v[1];
    m[2][3] = v[2];
    m
}

/// Upstream `Transform3d::scale(v)` from identity: the per-axis factors go on
/// the diagonal.
fn scale_matrix(v: [f64; 3]) -> Affine {
    let mut m = identity_matrix();
    m[0][0] = v[0];
    m[1][1] = v[1];
    m[2][2] = v[2];
    m
}

/// Embed a 3×3 rotation `r` (row-major) into a 4×4 affine matrix.
fn affine_from_rotation(r: [[f64; 3]; 3]) -> Affine {
    let mut m = identity_matrix();
    for row in 0..3 {
        for col in 0..3 {
            m[row][col] = r[row][col];
        }
    }
    m
}

/// Upstream `builtin_rotate` Euler branch (`TransformNode.cc`, master):
/// the composed rotation `Rz(az) * Ry(ay) * Rx(ax)` with
/// `sx = sin(ax)`, `cx = cos(ax)`, etc., built verbatim as
/// ```text
/// M << cy*cz, cz*sx*sy - cx*sz, cx*cz*sy + sx*sz,
///      cy*sz, cx*cz + sx*sy*sz, -cz*sx + cx*sy*sz,
///      -sy,   cy*sx,            cx*cy;
/// ```
fn rotate_euler_matrix(a: [f64; 3]) -> Affine {
    let (sx, cx) = (sin_degrees(a[0]), cos_degrees(a[0]));
    let (sy, cy) = (sin_degrees(a[1]), cos_degrees(a[1]));
    let (sz, cz) = (sin_degrees(a[2]), cos_degrees(a[2]));
    affine_from_rotation([
        [cy * cz, cz * sx * sy - cx * sz, cx * cz * sy + sx * sz],
        [cy * sz, cx * cz + sx * sy * sz, -cz * sx + cx * sy * sz],
        [-sy, cy * sx, cx * cy],
    ])
}

/// Upstream `angle_axis_degrees(a, v)` (`degree_trig.cc`, master): a rotation
/// of `a` degrees about the axis `v`, via the Rodrigues form
/// (Wikipedia "Rotation matrix from axis and angle"). With `s = sin(a)`,
/// `c = cos(a)`, `m = |v|²`, `Cv = v·(1 - c)/m` and `us = v̂·s`:
/// ```text
/// M << Cv0*v0 + c,   Cv1*v0 - us2, Cv2*v0 + us1,
///      Cv0*v1 + us2, Cv1*v1 + c,   Cv2*v1 - us0,
///      Cv0*v2 - us1, Cv1*v2 + us0, Cv2*v2 + c;
/// ```
/// A zero-length axis (`m == 0`) yields the identity.
fn angle_axis_matrix(a: f64, v: [f64; 3]) -> Affine {
    let s = sin_degrees(a);
    let c = cos_degrees(a);
    let m = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if m <= 0.0 {
        return identity_matrix();
    }
    let cv = [
        v[0] * (1.0 - c) / m,
        v[1] * (1.0 - c) / m,
        v[2] * (1.0 - c) / m,
    ];
    let norm = m.sqrt();
    let us = [v[0] / norm * s, v[1] / norm * s, v[2] / norm * s];
    affine_from_rotation([
        [cv[0] * v[0] + c, cv[1] * v[0] - us[2], cv[2] * v[0] + us[1]],
        [cv[0] * v[1] + us[2], cv[1] * v[1] + c, cv[2] * v[1] - us[0]],
        [cv[0] * v[2] - us[1], cv[1] * v[2] + us[0], cv[2] * v[2] + c],
    ])
}

/// Upstream `builtin_mirror` (`TransformNode.cc`, master): the Householder
/// reflection across the plane through the origin with normal `(x, y, z)`.
/// With `a = x² + y² + z²` (avoiding a `sqrt` to reduce float error):
/// ```text
/// m << 1 - 2x²/a, -2yx/a,   -2zx/a,   0,
///      -2xy/a,    1 - 2y²/a, -2zy/a,   0,
///      -2xz/a,    -2yz/a,    1 - 2z²/a, 0,
///      0,         0,         0,         1;
/// ```
/// A zero normal (`x == y == z == 0`) yields the identity.
fn mirror_matrix(n: [f64; 3]) -> Affine {
    let [x, y, z] = n;
    if x == 0.0 && y == 0.0 && z == 0.0 {
        return identity_matrix();
    }
    let a = x * x + y * y + z * z;
    [
        [
            1.0 - 2.0 * x * x / a,
            -2.0 * y * x / a,
            -2.0 * z * x / a,
            0.0,
        ],
        [
            -2.0 * x * y / a,
            1.0 - 2.0 * y * y / a,
            -2.0 * z * y / a,
            0.0,
        ],
        [
            -2.0 * x * z / a,
            -2.0 * y * z / a,
            1.0 - 2.0 * z * z / a,
            0.0,
        ],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Upstream `builtin_multmatrix` (`TransformNode.cc`, master): a 4×4 identity
/// filled from a vector-of-vectors `m`, taking only the first
/// `min(rows, 4)` rows and `min(cols, 4)` columns (missing rows/cols keep
/// the identity, and a non-number cell is left untouched — `getDouble` only
/// writes on success). If `m[3][3] != 1`, the whole matrix is divided by it.
/// A non-vector `m` yields the identity.
fn transform_multmatrix(bound: &HashMap<String, Value>) -> Affine {
    let mut m = identity_matrix();
    if let Some(Value::Vector(rows)) = bound.get("m") {
        for (r, rowv) in rows.iter().take(4).enumerate() {
            if let Value::Vector(cols) = rowv {
                for (c, cellv) in cols.iter().take(4).enumerate() {
                    if let Some(x) = as_number(cellv) {
                        m[r][c] = x;
                    }
                }
            }
        }
        let w = m[3][3];
        if w != 1.0 {
            for row in &mut m {
                for cell in row {
                    *cell /= w;
                }
            }
        }
    }
    m
}

/// Upstream `builtin_resize` (`CgalAdvNode.cc`, master): `newsize` is a
/// vector whose first three components (missing ones default to `0`) are the
/// target sizes (`toDouble`, so a non-number cell reads as `0`); a
/// non-vector `newsize` leaves `[0, 0, 0]`. `auto` is a vec of bools (first
/// three, missing default `false`) or a single bool applied to all three
/// axes; anything else leaves `[false, false, false]`. `convexity` is
/// parsed by upstream but not needed for the transform, so it is dropped.
fn transform_resize(bound: &HashMap<String, Value>) -> TransformKind {
    let mut newsize = [0.0; 3];
    if let Some(Value::Vector(vs)) = bound.get("newsize") {
        for (i, slot) in newsize.iter_mut().enumerate() {
            if let Some(v) = vs.get(i) {
                *slot = as_number(v).unwrap_or(0.0);
            }
        }
    }
    let mut auto = [false; 3];
    match bound.get("auto") {
        Some(Value::Vector(va)) => {
            for (i, slot) in auto.iter_mut().enumerate() {
                if let Some(v) = va.get(i) {
                    *slot = v.to_bool();
                }
            }
        }
        Some(Value::Bool(b)) => auto = [*b, *b, *b],
        _ => {}
    }
    TransformKind::Resize { newsize, auto }
}

/// The CSS Color Module Level 4 named colors plus OpenSCAD's `transparent`
/// (upstream `src/core/WebColors.h`, fetched 2026-07-03), as raw `[r, g, b,
/// a]` bytes in `0..=255`. Normalized to `0.0..=1.0` on lookup, matching
/// upstream's `Color4f(int r, int g, int b, int a)` (which divides by 255).
const WEB_COLORS: &[(&str, [u8; 4])] = &[
    ("aliceblue", [240, 248, 255, 255]),
    ("antiquewhite", [250, 235, 215, 255]),
    ("aqua", [0, 255, 255, 255]),
    ("aquamarine", [127, 255, 212, 255]),
    ("azure", [240, 255, 255, 255]),
    ("beige", [245, 245, 220, 255]),
    ("bisque", [255, 228, 196, 255]),
    ("black", [0, 0, 0, 255]),
    ("blanchedalmond", [255, 235, 205, 255]),
    ("blue", [0, 0, 255, 255]),
    ("blueviolet", [138, 43, 226, 255]),
    ("brown", [165, 42, 42, 255]),
    ("burlywood", [222, 184, 135, 255]),
    ("cadetblue", [95, 158, 160, 255]),
    ("chartreuse", [127, 255, 0, 255]),
    ("chocolate", [210, 105, 30, 255]),
    ("coral", [255, 127, 80, 255]),
    ("cornflowerblue", [100, 149, 237, 255]),
    ("cornsilk", [255, 248, 220, 255]),
    ("crimson", [220, 20, 60, 255]),
    ("cyan", [0, 255, 255, 255]),
    ("darkblue", [0, 0, 139, 255]),
    ("darkcyan", [0, 139, 139, 255]),
    ("darkgoldenrod", [184, 134, 11, 255]),
    ("darkgray", [169, 169, 169, 255]),
    ("darkgreen", [0, 100, 0, 255]),
    ("darkgrey", [169, 169, 169, 255]),
    ("darkkhaki", [189, 183, 107, 255]),
    ("darkmagenta", [139, 0, 139, 255]),
    ("darkolivegreen", [85, 107, 47, 255]),
    ("darkorange", [255, 140, 0, 255]),
    ("darkorchid", [153, 50, 204, 255]),
    ("darkred", [139, 0, 0, 255]),
    ("darksalmon", [233, 150, 122, 255]),
    ("darkseagreen", [143, 188, 143, 255]),
    ("darkslateblue", [72, 61, 139, 255]),
    ("darkslategray", [47, 79, 79, 255]),
    ("darkslategrey", [47, 79, 79, 255]),
    ("darkturquoise", [0, 206, 209, 255]),
    ("darkviolet", [148, 0, 211, 255]),
    ("deeppink", [255, 20, 147, 255]),
    ("deepskyblue", [0, 191, 255, 255]),
    ("dimgray", [105, 105, 105, 255]),
    ("dimgrey", [105, 105, 105, 255]),
    ("dodgerblue", [30, 144, 255, 255]),
    ("firebrick", [178, 34, 34, 255]),
    ("floralwhite", [255, 250, 240, 255]),
    ("forestgreen", [34, 139, 34, 255]),
    ("fuchsia", [255, 0, 255, 255]),
    ("gainsboro", [220, 220, 220, 255]),
    ("ghostwhite", [248, 248, 255, 255]),
    ("gold", [255, 215, 0, 255]),
    ("goldenrod", [218, 165, 32, 255]),
    ("gray", [128, 128, 128, 255]),
    ("green", [0, 128, 0, 255]),
    ("greenyellow", [173, 255, 47, 255]),
    ("grey", [128, 128, 128, 255]),
    ("honeydew", [240, 255, 240, 255]),
    ("hotpink", [255, 105, 180, 255]),
    ("indianred", [205, 92, 92, 255]),
    ("indigo", [75, 0, 130, 255]),
    ("ivory", [255, 255, 240, 255]),
    ("khaki", [240, 230, 140, 255]),
    ("lavender", [230, 230, 250, 255]),
    ("lavenderblush", [255, 240, 245, 255]),
    ("lawngreen", [124, 252, 0, 255]),
    ("lemonchiffon", [255, 250, 205, 255]),
    ("lightblue", [173, 216, 230, 255]),
    ("lightcoral", [240, 128, 128, 255]),
    ("lightcyan", [224, 255, 255, 255]),
    ("lightgoldenrodyellow", [250, 250, 210, 255]),
    ("lightgray", [211, 211, 211, 255]),
    ("lightgreen", [144, 238, 144, 255]),
    ("lightgrey", [211, 211, 211, 255]),
    ("lightpink", [255, 182, 193, 255]),
    ("lightsalmon", [255, 160, 122, 255]),
    ("lightseagreen", [32, 178, 170, 255]),
    ("lightskyblue", [135, 206, 250, 255]),
    ("lightslategray", [119, 136, 153, 255]),
    ("lightslategrey", [119, 136, 153, 255]),
    ("lightsteelblue", [176, 196, 222, 255]),
    ("lightyellow", [255, 255, 224, 255]),
    ("lime", [0, 255, 0, 255]),
    ("limegreen", [50, 205, 50, 255]),
    ("linen", [250, 240, 230, 255]),
    ("magenta", [255, 0, 255, 255]),
    ("maroon", [128, 0, 0, 255]),
    ("mediumaquamarine", [102, 205, 170, 255]),
    ("mediumblue", [0, 0, 205, 255]),
    ("mediumorchid", [186, 85, 211, 255]),
    ("mediumpurple", [147, 112, 219, 255]),
    ("mediumseagreen", [60, 179, 113, 255]),
    ("mediumslateblue", [123, 104, 238, 255]),
    ("mediumspringgreen", [0, 250, 154, 255]),
    ("mediumturquoise", [72, 209, 204, 255]),
    ("mediumvioletred", [199, 21, 133, 255]),
    ("midnightblue", [25, 25, 112, 255]),
    ("mintcream", [245, 255, 250, 255]),
    ("mistyrose", [255, 228, 225, 255]),
    ("moccasin", [255, 228, 181, 255]),
    ("navajowhite", [255, 222, 173, 255]),
    ("navy", [0, 0, 128, 255]),
    ("oldlace", [253, 245, 230, 255]),
    ("olive", [128, 128, 0, 255]),
    ("olivedrab", [107, 142, 35, 255]),
    ("orange", [255, 165, 0, 255]),
    ("orangered", [255, 69, 0, 255]),
    ("orchid", [218, 112, 214, 255]),
    ("palegoldenrod", [238, 232, 170, 255]),
    ("palegreen", [152, 251, 152, 255]),
    ("paleturquoise", [175, 238, 238, 255]),
    ("palevioletred", [219, 112, 147, 255]),
    ("papayawhip", [255, 239, 213, 255]),
    ("peachpuff", [255, 218, 185, 255]),
    ("peru", [205, 133, 63, 255]),
    ("pink", [255, 192, 203, 255]),
    ("plum", [221, 160, 221, 255]),
    ("powderblue", [176, 224, 230, 255]),
    ("purple", [128, 0, 128, 255]),
    ("rebeccapurple", [102, 51, 153, 255]),
    ("red", [255, 0, 0, 255]),
    ("rosybrown", [188, 143, 143, 255]),
    ("royalblue", [65, 105, 225, 255]),
    ("saddlebrown", [139, 69, 19, 255]),
    ("salmon", [250, 128, 114, 255]),
    ("sandybrown", [244, 164, 96, 255]),
    ("seagreen", [46, 139, 87, 255]),
    ("seashell", [255, 245, 238, 255]),
    ("sienna", [160, 82, 45, 255]),
    ("silver", [192, 192, 192, 255]),
    ("skyblue", [135, 206, 235, 255]),
    ("slateblue", [106, 90, 205, 255]),
    ("slategray", [112, 128, 144, 255]),
    ("slategrey", [112, 128, 144, 255]),
    ("snow", [255, 250, 250, 255]),
    ("springgreen", [0, 255, 127, 255]),
    ("steelblue", [70, 130, 180, 255]),
    ("tan", [210, 180, 140, 255]),
    ("teal", [0, 128, 128, 255]),
    ("thistle", [216, 191, 216, 255]),
    ("tomato", [255, 99, 71, 255]),
    ("turquoise", [64, 224, 208, 255]),
    ("violet", [238, 130, 238, 255]),
    ("wheat", [245, 222, 179, 255]),
    ("white", [255, 255, 255, 255]),
    ("whitesmoke", [245, 245, 245, 255]),
    ("yellow", [255, 255, 0, 255]),
    ("yellowgreen", [154, 205, 50, 255]),
    ("transparent", [0, 0, 0, 0]),
];

/// Resolve a `color(...)` string to RGBA in `0.0..=1.0` (upstream
/// `OpenSCAD::parse_color`, `ColorUtil.cc`): a named web color (matched
/// case-insensitively) wins, otherwise a `#hex` form. The `xkcd:` palette is
/// not embedded. `None` when the string is neither.
fn parse_color(name: &str) -> Option<[f32; 4]> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, bytes)) = WEB_COLORS.iter().find(|(n, _)| *n == lower) {
        return Some([
            bytes[0] as f32 / 255.0,
            bytes[1] as f32 / 255.0,
            bytes[2] as f32 / 255.0,
            bytes[3] as f32 / 255.0,
        ]);
    }
    parse_hex_color(name)
}

/// Parse a CSS hex color (upstream `parse_hex_color`, `ColorUtil.cc`):
/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`. Short syntax uses one hex digit
/// per channel (max 15), long syntax two (max 255); a missing alpha defaults
/// to `1.0`. `None` when malformed.
fn parse_hex_color(hex: &str) -> Option<[f32; 4]> {
    let short = hex.len() == 4 || hex.len() == 5;
    let long = hex.len() == 7 || hex.len() == 9;
    if !short && !long {
        return None;
    }
    let bytes = hex.as_bytes();
    if bytes[0] != b'#' || !bytes[1..].iter().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let stride = if short { 1 } else { 2 };
    let channel_max = if short { 15.0 } else { 255.0 };
    let mut rgba = [0.0f32, 0.0, 0.0, 1.0];
    let channels = (hex.len() - 1) / stride;
    for (i, channel) in rgba.iter_mut().enumerate().take(channels) {
        let start = 1 + i * stride;
        let chunk = &hex[start..start + stride];
        let value = u32::from_str_radix(chunk, 16).ok()?;
        *channel = value as f32 / channel_max;
    }
    Some(rgba)
}

/// Degrees `sin`, accurate at multiples of 30/45/60/90 (upstream
/// `sin_degrees`, `degree_trig.cc`): the exact-value cases keep 90° rotations
/// free of float error, so composed matrices land on clean `0`/`±1` entries.
fn sin_degrees(mut x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    if !(0.0..360.0).contains(&x) {
        x -= 360.0 * (x / 360.0).floor();
    }
    let oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
    }
    let r = if x < 45.0 {
        if x == 30.0 {
            0.5
        } else {
            (x * DEG2RAD).sin()
        }
    } else if x == 45.0 {
        std::f64::consts::FRAC_1_SQRT_2
    } else if x == 60.0 {
        3.0_f64.sqrt() / 2.0
    } else {
        ((90.0 - x) * DEG2RAD).cos()
    };
    if oppose {
        -r
    } else {
        r
    }
}

/// Degrees `cos`, accurate at multiples of 30/45/60/90 (upstream
/// `cos_degrees`, `degree_trig.cc`).
fn cos_degrees(mut x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    if !(0.0..360.0).contains(&x) {
        x -= 360.0 * (x / 360.0).floor();
    }
    let mut oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
        oppose = !oppose;
    }
    let r = if x > 45.0 {
        if x == 60.0 {
            0.5
        } else {
            ((90.0 - x) * DEG2RAD).sin()
        }
    } else if x == 45.0 {
        std::f64::consts::FRAC_1_SQRT_2
    } else if x == 30.0 {
        3.0_f64.sqrt() / 2.0
    } else {
        (x * DEG2RAD).cos()
    };
    if oppose {
        -r
    } else {
        r
    }
}

/// Upstream `(int)parameters["convexity"].toDouble()` clamped to a minimum
/// of 1: a number truncates toward zero, anything absent/undef/non-number
/// reads as 0, and the result is floored at 1.
fn convexity_of(bound: &HashMap<String, Value>) -> u32 {
    let c = bound
        .get("convexity")
        .and_then(as_number)
        .map(|n| n as i64)
        .unwrap_or(0);
    c.max(1) as u32
}

/// Render an optional argument for a warning, printing `undef` when the
/// argument is absent (upstream reads `parameters[name]`, which is undef
/// when unsupplied).
fn echo_or_undef(value: Option<&Value>) -> String {
    match value {
        Some(value) => value.to_echo_string(),
        None => "undef".to_string(),
    }
}

/// Upstream `Expression::isLiteral`: the `Literal` node covers exactly
/// number, string, boolean, and undef.
fn is_literal(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Number(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Undef
    )
}

#[cfg(test)]
mod tests {
    use morphe_lang::parser::{parse_expr, parse_program};

    use super::*;
    use crate::csg::{TextHalign, TextValign};

    /// A function registered in a nested frame must keep addressing its
    /// definition site after that frame is popped, even when new frames
    /// are pushed afterwards — [`ScopeStack::retain`] keeps the FrameId
    /// (which both the per-frame definition table and the captured
    /// `def_frame` use) from being silently aliased by slot reuse.
    #[test]
    fn function_def_frame_survives_frame_reuse() {
        let mut evaluator = Evaluator::new();
        evaluator.scope.push_scope(); // frame 1: the definition site
        evaluator.scope.set("captured", Value::Number(7.0));
        let program = parse_program("function f() = captured;").expect("parse");
        evaluator.eval_program(&program);
        evaluator.scope.pop();

        // Without retain this push would reuse the definition frame's id,
        // aliasing both f's registration and its captured def_frame.
        evaluator.scope.push_scope();
        evaluator.scope.set("captured", Value::Number(9.0));

        // Definitions are lexically scoped, so f is not visible here...
        let call = parse_expr("f()").expect("parse");
        let value = evaluator.eval_expr(&call);
        assert_eq!(value, Value::Undef(None));
        assert!(evaluator
            .warnings()
            .iter()
            .any(|w| w.contains("Ignoring unknown function 'f'")));

        // ...but re-entering the retained definition scope resolves it to
        // the original frame, not the impostor binding.
        evaluator.scope.push_call(1);
        assert_eq!(evaluator.eval_expr(&call), Value::Number(7.0));
        assert!(evaluator.errors().is_empty());
    }

    /// Module-body definitions must not pin their instantiation frame:
    /// they are unreachable once the instantiation ends, so the frame is
    /// reclaimed and the per-frame definition tables are emptied — the
    /// arena stays flat across repeated instantiations of a
    /// definition-containing module.
    #[test]
    fn module_body_definitions_do_not_grow_arena() {
        let mut evaluator = Evaluator::new();
        let program = parse_program(
            "module m() { function h() = 1; module inner() cube(h()); inner(); } m();",
        )
        .expect("parse");
        evaluator.eval_program(&program);
        let frames = evaluator.scope.frame_count();
        let function_tables = evaluator.functions.len();
        let module_tables = evaluator.modules.len();

        let call = parse_program("m();").expect("parse");
        for _ in 0..100 {
            evaluator.eval_stmt(&call[0]);
        }
        assert_eq!(evaluator.scope.frame_count(), frames);
        assert_eq!(evaluator.functions.len(), function_tables);
        assert_eq!(evaluator.modules.len(), module_tables);
        assert!(evaluator.errors().is_empty());
        assert!(evaluator.warnings().is_empty());
    }

    /// Tail calls replace the finished call's frames — its call frame and
    /// any `let` frames — instead of stacking new ones, so a long chain
    /// leaves the arena exactly as it found it.
    #[test]
    fn tail_calls_do_not_grow_arena() {
        let mut evaluator = Evaluator::new();
        let program = parse_program(
            "function f(n, acc = 0) = let(m = n - 1) let($k = n) \
             n == 0 ? acc : f(m, acc + 1);",
        )
        .expect("parse");
        evaluator.eval_program(&program);
        let frames = evaluator.scope.frame_count();
        let call = parse_expr("f(10000)").expect("parse");
        assert_eq!(evaluator.eval_expr(&call), Value::Number(10000.0));
        assert_eq!(evaluator.scope.frame_count(), frames);
        assert_eq!(evaluator.scope.current(), ScopeStack::ROOT);
        assert!(evaluator.errors().is_empty());
    }

    /// Loop/if body definitions must not pin their body frame either
    /// (eval_transient_body): the body exports nothing, so each
    /// iteration's frame is reclaimed and its definition tables emptied —
    /// the arena and tables stay flat across 100 iterations, while the
    /// body's definitions stay callable within their own iteration.
    ///
    /// The grammar (like upstream `child_statements`) cannot place
    /// definitions in a loop body, but the evaluator's public API accepts
    /// arbitrary ASTs, so the equivalent of `for (i = [1 : 100]) {
    /// function h() = i; module inner() echo(h()); inner(); }` is built
    /// programmatically.
    #[test]
    fn loop_body_definitions_do_not_grow_arena() {
        let span = Span { start: 0, end: 1 };
        let number = |n: f64| Expr {
            kind: ExprKind::Number(n),
            span,
        };
        let body =
            parse_program("function h() = i; module inner() echo(h()); inner();").expect("parse");
        let for_stmt = Stmt {
            kind: StmtKind::ModuleCall {
                name: "for".to_string(),
                args: vec![Arg {
                    name: Some("i".to_string()),
                    value: Expr {
                        kind: ExprKind::Range {
                            start: Box::new(number(1.0)),
                            step: None,
                            end: Box::new(number(100.0)),
                        },
                        span,
                    },
                    span,
                }],
                modifiers: Modifiers::default(),
                children: body,
            },
            span,
        };

        let mut evaluator = Evaluator::new();
        let frames = evaluator.scope.frame_count();
        let function_tables = evaluator.functions.len();
        let module_tables = evaluator.modules.len();
        evaluator.eval_program(std::slice::from_ref(&for_stmt));

        // The body's function is callable within its own iteration...
        assert_eq!(evaluator.console().len(), 100);
        assert_eq!(evaluator.console()[0], "ECHO: 1");
        assert_eq!(evaluator.console()[99], "ECHO: 100");
        // ...while the arena and definition tables stay flat.
        assert_eq!(evaluator.scope.frame_count(), frames);
        assert_eq!(evaluator.functions.len(), function_tables);
        assert_eq!(evaluator.modules.len(), module_tables);
        assert!(evaluator.errors().is_empty());
        assert!(evaluator.warnings().is_empty());
    }

    // ---- Special variables ($fn/$fa/$fs/$t/$vp*/$preview/$parent_modules) ----

    /// Read a special variable against a fresh root scope (its seeded value).
    fn root_special(name: &str) -> Value {
        let mut evaluator = Evaluator::new();
        let expr = parse_expr(name).expect("parse");
        let value = evaluator.eval_expr(&expr);
        assert!(evaluator.warnings().is_empty(), "unexpected warnings");
        value
    }

    /// Evaluate a program and return its recorded `echo` console lines — the
    /// idiom for observing a config variable's value at a nesting point.
    fn console_of(src: &str) -> Vec<String> {
        let program = parse_program(src).expect("parse");
        let mut evaluator = Evaluator::new();
        evaluator.eval_program(&program);
        assert!(evaluator.errors().is_empty(), "unexpected errors");
        evaluator.console().to_vec()
    }

    #[test]
    fn special_var_fn_default_is_zero() {
        assert_eq!(root_special("$fn"), Value::Number(0.0));
    }

    #[test]
    fn special_var_fa_default_is_twelve() {
        assert_eq!(root_special("$fa"), Value::Number(12.0));
    }

    #[test]
    fn special_var_fs_default_is_two() {
        assert_eq!(root_special("$fs"), Value::Number(2.0));
    }

    #[test]
    fn special_var_t_default_is_zero() {
        assert_eq!(root_special("$t"), Value::Number(0.0));
    }

    #[test]
    fn special_var_preview_default_is_undef() {
        assert_eq!(root_special("$preview"), Value::Undef(None));
    }

    #[test]
    fn special_var_vpt_default_is_zero_vector() {
        assert_eq!(
            root_special("$vpt"),
            Value::from(vec![
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(0.0),
            ])
        );
    }

    #[test]
    fn special_var_vpr_default_is_zero_vector() {
        assert_eq!(
            root_special("$vpr"),
            Value::from(vec![
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(0.0),
            ])
        );
    }

    #[test]
    fn special_var_vpd_default_is_five_hundred() {
        assert_eq!(root_special("$vpd"), Value::Number(500.0));
    }

    #[test]
    fn special_var_vpf_default_is_22_5() {
        assert_eq!(root_special("$vpf"), Value::Number(22.5));
    }

    #[test]
    fn special_var_parent_modules_default_is_zero() {
        assert_eq!(root_special("$parent_modules"), Value::Number(0.0));
    }

    /// A `$fn` set at the call site is visible inside a called module body
    /// (dynamic scoping along the call chain).
    #[test]
    fn fn_propagates_into_module_body() {
        assert_eq!(
            console_of("$fn = 12; module m() echo($fn); m();"),
            vec!["ECHO: 12".to_string()]
        );
    }

    /// Without any override, a module body reads the root seed.
    #[test]
    fn default_fn_visible_in_module_body() {
        assert_eq!(
            console_of("module m() echo($fn); m();"),
            vec!["ECHO: 0".to_string()]
        );
    }

    /// A `$fn` set at the top reaches deep into nested module bodies.
    #[test]
    fn fn_propagates_into_deeply_nested_modules() {
        let src = "\
            $fn = 8;\
            module a() b();\
            module b() c();\
            module c() echo($fn);\
            a();";
        assert_eq!(console_of(src), vec!["ECHO: 8".to_string()]);
    }

    /// A module can override `$fn` for its whole subtree, and the override
    /// is restored (scoped out) once the module returns.
    #[test]
    fn module_local_fn_override_scoped_to_subtree() {
        let src = "\
            module inner() echo($fn);\
            module outer() { $fn = 6; inner(); }\
            $fn = 12;\
            outer();\
            echo($fn);";
        assert_eq!(
            console_of(src),
            vec!["ECHO: 6".to_string(), "ECHO: 12".to_string()]
        );
    }

    /// An override in one module does not leak into a sibling instantiation.
    #[test]
    fn fn_override_does_not_leak_to_sibling() {
        let src = "\
            module show() echo($fn);\
            module bumped() { $fn = 99; show(); }\
            $fn = 3;\
            bumped();\
            show();";
        assert_eq!(
            console_of(src),
            vec!["ECHO: 99".to_string(), "ECHO: 3".to_string()]
        );
    }

    /// `$t` seeds to 0 and propagates dynamically like the fragment vars.
    #[test]
    fn t_default_propagates_into_module_body() {
        assert_eq!(
            console_of("module m() echo($t); m();"),
            vec!["ECHO: 0".to_string()]
        );
        assert_eq!(
            console_of("$t = 0.25; module m() echo($t); m();"),
            vec!["ECHO: 0.25".to_string()]
        );
    }

    /// `$parent_modules` counts the enclosing user-module instantiations
    /// (0 at top level, then 1, 2, ... going deeper).
    #[test]
    fn parent_modules_increments_in_nested_user_modules() {
        let src = "\
            module inner() echo($parent_modules);\
            module outer() { echo($parent_modules); inner(); }\
            outer();\
            echo($parent_modules);";
        assert_eq!(
            console_of(src),
            vec![
                "ECHO: 1".to_string(),
                "ECHO: 2".to_string(),
                "ECHO: 0".to_string(),
            ]
        );
    }

    // ---- resolve_fragments (upstream Calc::get_fragments_from_r) ----

    // Argument order matches upstream get_fragments_from_r(r, fn, fs, fa):
    // $fs (arc size) before $fa (angle).

    #[test]
    fn resolve_fragments_fn_path_takes_fn() {
        // $fn > 0 forces exactly that many facets (>= 3), ignoring $fa/$fs.
        assert_eq!(resolve_fragments(50.0, 24.0, 2.0, 12.0), 24);
    }

    #[test]
    fn resolve_fragments_fn_below_three_clamps_to_three() {
        assert_eq!(resolve_fragments(50.0, 2.0, 2.0, 12.0), 3);
    }

    #[test]
    fn resolve_fragments_fn_truncates_toward_zero() {
        // static_cast<int>(fn) truncates: 5.9 -> 5.
        assert_eq!(resolve_fragments(50.0, 5.9, 2.0, 12.0), 5);
    }

    #[test]
    fn resolve_fragments_fa_dominated() {
        // $fn = 0: r=10, $fa=12 -> 360/12 = 30; r*2*pi/$fs = 31.4 -> min = 30.
        assert_eq!(resolve_fragments(10.0, 0.0, 2.0, 12.0), 30);
    }

    #[test]
    fn resolve_fragments_fs_dominated() {
        // r=2, $fa=1 -> 360; r*2*pi/0.5 = 25.13 -> min = 25.13 -> ceil = 26.
        assert_eq!(resolve_fragments(2.0, 0.0, 0.5, 1.0), 26);
    }

    #[test]
    fn resolve_fragments_minimum_five_clamp() {
        // Small radius: min(360/12, 1*2*pi/2 = 3.14) = 3.14, floored to 5.
        assert_eq!(resolve_fragments(1.0, 0.0, 2.0, 12.0), 5);
    }

    #[test]
    fn resolve_fragments_radius_zero_is_three() {
        assert_eq!(resolve_fragments(0.0, 0.0, 2.0, 12.0), 3);
    }

    #[test]
    fn resolve_fragments_non_finite_fn_is_three() {
        assert_eq!(resolve_fragments(50.0, f64::INFINITY, 2.0, 12.0), 3);
        assert_eq!(resolve_fragments(50.0, f64::NAN, 2.0, 12.0), 3);
    }

    #[test]
    fn resolve_fragments_default_large_cylinder() {
        // The canonical default-quality large cylinder ($fn=0, r=10) renders
        // with 30 facets in upstream OpenSCAD.
        assert_eq!(resolve_fragments(10.0, 0.0, 2.0, 12.0), 30);
    }

    #[test]
    fn resolve_fragments_zero_fa_fs_clamp_to_f_minimum() {
        // `$fa=0; $fs=0; sphere(1);` divided by zero into u32::MAX (then a
        // `capacity overflow` panic in the kernel). Upstream floors both at
        // F_MINIMUM = 0.01: min(360/0.01, 2π·1/0.01) = 628.3… → 629.
        assert_eq!(resolve_fragments(1.0, 0.0, 0.0, 0.0), 629);
        // Negative values clamp the same way.
        assert_eq!(resolve_fragments(1.0, -5.0, -1.0, -1.0), 629);
        assert_eq!(
            resolve_fragments(1.0, 0.0, F_MINIMUM, F_MINIMUM),
            resolve_fragments(1.0, 0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn clamp_fragment_vars_floors_and_passes_nan_like_std_max() {
        assert_eq!(clamp_fragment_vars(-1.0, 0.0, 0.0), (0.0, 0.01, 0.01));
        assert_eq!(clamp_fragment_vars(8.0, 2.0, 12.0), (8.0, 2.0, 12.0));
        let (fn_, _, _) = clamp_fragment_vars(f64::NAN, 2.0, 12.0);
        assert!(fn_.is_nan(), "NaN passes through like C++ std::max");
        assert_eq!(resolve_fragments(1.0, f64::NAN, 2.0, 12.0), 3);
    }

    // ---- unsupported constructs ----

    /// Evaluate `src` and return its recorded hard errors.
    fn errors_of(src: &str) -> Vec<EvalError> {
        let program = parse_program(src).expect("parse");
        let mut evaluator = Evaluator::new();
        evaluator.set_source(src);
        evaluator.eval_program(&program);
        evaluator.errors().to_vec()
    }

    #[test]
    fn minkowski_now_evaluates() {
        // minkowski evaluates (no Unsupported error) and the node carries
        // its children in source order (walk the tree, not just errors()).
        let src = "minkowski() { cube(10); sphere(2); }";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let root = ev.eval_program(&stmts);
        assert!(
            ev.errors().is_empty(),
            "minkowski must not record Unsupported: {:?}",
            ev.errors()
        );
        let mut kinds: Option<Vec<&str>> = None;
        fn walk_m(node: &CsgNode, kinds: &mut Option<Vec<&str>>) {
            match node {
                CsgNode::Minkowski { children, .. } => {
                    let mut ks = Vec::new();
                    for kid in children {
                        match kid {
                            CsgNode::Primitive {
                                primitive: Primitive::Cube { .. },
                                ..
                            } => ks.push("cube"),
                            CsgNode::Primitive {
                                primitive: Primitive::Sphere { .. },
                                ..
                            } => ks.push("sphere"),
                            other => panic!("unexpected child {other:?}"),
                        }
                    }
                    *kinds = Some(ks);
                }
                CsgNode::Group { children, .. } => children.iter().for_each(|c| walk_m(c, kinds)),
                _ => {}
            }
        }
        walk_m(&root, &mut kinds);
        assert_eq!(
            kinds,
            Some(vec!["cube", "sphere"]),
            "children in source order on the typed node"
        );
    }

    /// text evaluates to a typed Text leaf — no Unsupported error.
    #[test]
    fn text_now_evaluates_to_text_leaf() {
        let mut ev = Evaluator::new();
        let stmts = parse_program("text(\"hi\");").expect("parses");
        let _ = ev.eval_program(&stmts);
        assert!(
            ev.errors().is_empty(),
            "text must not record Unsupported: {:?}",
            ev.errors()
        );
    }

    #[test]
    fn text_args_defaults() {
        // Defaults — size 10, halign left, valign baseline, spacing 1,
        // direction ltr (no warnings for a bare call).
        let mut ev = Evaluator::new();
        let stmts = parse_program("text(\"hi\");").expect("parses");
        let root = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty(), "no errors: {:?}", ev.errors());
        let CsgNode::Group { children: tops, .. } = root else {
            panic!("root is a Group")
        };
        let Some(CsgNode::Text { params, .. }) = tops.first() else {
            panic!("expected a Text leaf, got {:?}", tops.first())
        };
        assert_eq!(params.text, "hi");
        assert!((params.size - 10.0).abs() < 1e-12);
        assert_eq!(params.halign, TextHalign::Left);
        assert_eq!(params.valign, TextValign::Baseline);
        assert!((params.spacing - 1.0).abs() < 1e-12);
    }

    #[test]
    fn text_ignores_children_like_primitives() {
        // Children contribute nothing — the Text node is a leaf, and the
        // child cube() never appears in the tree.
        let mut ev = Evaluator::new();
        let stmts = parse_program("text(\"hi\") cube(1);").expect("parses");
        let root = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty(), "no errors: {:?}", ev.errors());
        let CsgNode::Group { children: tops, .. } = root else {
            panic!("root is a Group")
        };
        assert_eq!(tops.len(), 1, "only the Text leaf (children ignored)");
        assert!(matches!(tops[0], CsgNode::Text { .. }));
    }

    #[test]
    fn text_unknown_font_warns_and_falls_back() {
        // An unknown family warns (eval-level) and still resolves to the
        // typed leaf (the kernel always uses the bundled family).
        let mut ev = Evaluator::new();
        let stmts = parse_program("text(\"hi\", font = \"Comic Sans\");").expect("parses");
        let _ = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty(), "fallback is not an error");
        assert!(
            ev.warnings().iter().any(|w| w.contains("Comic Sans")),
            "unknown font warns: {:?}",
            ev.warnings()
        );
        // The bundled family itself never warns.
        let mut ev = Evaluator::new();
        let stmts = parse_program("text(\"hi\", font = \"Fira Sans\");").expect("parses");
        let _ = ev.eval_program(&stmts);
        assert!(
            !ev.warnings().iter().any(|w| w.contains("font")),
            "the bundled family does not warn: {:?}",
            ev.warnings()
        );
    }

    #[test]
    fn text_direction_rtl_warns_and_falls_back() {
        let mut ev = Evaluator::new();
        let stmts = parse_program("text(\"hi\", direction = \"rtl\");").expect("parses");
        let _ = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty());
        assert!(
            ev.warnings().iter().any(|w| w.contains("rtl")),
            "rtl warns + falls back to ltr: {:?}",
            ev.warnings()
        );
    }

    #[test]
    fn offset_now_evaluates() {
        // offset evaluates to a typed node with NO Unsupported error.
        let src = "offset(r=2) square(1);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let _ = ev.eval_program(&stmts);
        assert!(
            ev.errors().is_empty(),
            "offset must not record Unsupported: {:?}",
            ev.errors()
        );
    }

    /// import evaluates to the typed Import node — no `Unsupported` error.
    #[test]
    fn import_now_evaluates_to_import_leaf() {
        let mut ev = Evaluator::new();
        let stmts = parse_program("import(\"part.stl\");").expect("parses");
        let _ = ev.eval_program(&stmts);
        assert!(
            ev.errors().is_empty(),
            "import must not record Unsupported: {:?}",
            ev.errors()
        );
    }

    #[test]
    fn projection_now_evaluates() {
        // projection evaluates to a typed node with NO Unsupported error.
        let src = "projection(cut = true) translate([0, 0, -1]) cube(10);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let _ = ev.eval_program(&stmts);
        assert!(
            ev.errors().is_empty(),
            "projection must not record Unsupported: {:?}",
            ev.errors()
        );
    }

    /// hull() is supported — never an Unsupported error. It resolves to a
    /// typed Hull node.
    #[test]
    fn hull_is_supported_not_unsupported() {
        let src = "hull() { cube(1); sphere(1); }";
        let program = parse_program(src).expect("parse");
        let mut evaluator = Evaluator::new();
        let node = evaluator.eval_program(&program);
        assert!(evaluator.errors().is_empty(), "hull must not error");
        let CsgNode::Group { children, .. } = node else {
            panic!("root is a group");
        };
        assert!(
            matches!(&children[0], CsgNode::Hull { children, .. } if children.len() == 2),
            "hull resolves to a typed Hull node: {:?}",
            children[0]
        );
    }

    /// resize() is supported (a transform) — no unsupported error.
    #[test]
    fn resize_is_supported() {
        let errors = errors_of("resize([1,2,3]) cube(1);");
        assert!(errors.is_empty(), "resize must not error: {errors:?}");
    }

    /// A genuinely-unknown module (a typo) stays the unknown-module WARNING,
    /// never an Unsupported error — this is the unsupported/unknown distinction.
    #[test]
    fn unknown_module_warns_not_unsupported() {
        let src = "frobnicate() cube(1);";
        let program = parse_program(src).expect("parse");
        let mut evaluator = Evaluator::new();
        evaluator.eval_program(&program);
        assert!(
            evaluator.errors().is_empty(),
            "a typo is not a hard error: {:?}",
            evaluator.errors()
        );
        assert!(evaluator
            .warnings()
            .iter()
            .any(|w| w.contains("Ignoring unknown module 'frobnicate'")));
    }

    // NOTE: span-through-eval tests for the Unsupported arm (for-body /
    // module-body span slicing) are not present because the arm is
    // unreachable while UNSUPPORTED_MODULES is empty. The machinery stays
    // for future deferrals (e.g. roof/fill/render); when one is added,
    // re-add span tests alongside it.

    /// Formerly-unsupported constructs record no errors.
    #[test]
    fn multiple_unsupported_each_recorded() {
        // UNSUPPORTED_MODULES is empty — every construct evaluates, so the
        // formerly-unsupported trio records NO errors at all.
        let errors = errors_of("surface(file=\"h\") cube(1); text(\"x\"); import(\"a.stl\");");
        assert_eq!(
            errors.len(),
            0,
            "the unsupported set is empty: no Unsupported errors remain"
        );
    }

    /// A formerly-unsupported construct in a multi-iteration `for` loop.
    #[test]
    fn unsupported_recorded_per_iteration() {
        // UNSUPPORTED_MODULES is empty — the loop body evaluates cleanly
        // across every iteration.
        let errors = errors_of("for (i = [0:2]) text(\"hi\") cube(i);");
        assert_eq!(
            errors.len(),
            0,
            "no Unsupported errors while the unsupported set is empty"
        );
    }

    /// The Unsupported message is stable and greppable for tooling.
    #[test]
    fn unsupported_message_is_greppable() {
        // With no unsupported constructs, the stable message format is
        // pinned directly on the error constructor (future deferrals must
        // keep this exact format).
        let span = Span { start: 3, end: 9 };
        let err = EvalError::unsupported(
            "future_thing".to_string(),
            span,
            "future_thing() is not yet implemented in morphe",
        );
        assert_eq!(
            err.message,
            "unsupported: future_thing (not yet implemented in morphe)"
        );
        let EvalErrorKind::Unsupported { construct, hint } = &err.kind else {
            panic!("kind must be Unsupported");
        };
        assert_eq!(construct, "future_thing");
        assert_eq!(hint, "future_thing() is not yet implemented in morphe");
        assert_eq!(err.span, span);
    }

    /// Back-compat: an ordinary hard error (a failed assert) keeps kind
    /// General, so existing callers see no behavioral change.
    #[test]
    fn general_error_keeps_general_kind() {
        let errors = errors_of("assert(false);");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, EvalErrorKind::General);
    }

    // -----------------------------------------------------------------------
    // import argument resolution
    // -----------------------------------------------------------------------

    /// Drive an `import(...)` call through eval, pull the resolved Import
    /// node out, and return it with any collected errors. The dispatch arm
    /// chains the context through the import arm before the UNSUPPORTED
    /// arm, matching the offset/projection ordering.
    fn run_import(ev: &mut Evaluator, src: &str) -> (Option<CsgNode>, Vec<EvalError>) {
        let stmts = parse_program(src).expect("parses");
        let root = ev.eval_program(&stmts);
        let CsgNode::Group { children: tops, .. } = root else {
            panic!("program root is a Group")
        };
        let node = tops.iter().find_map(|n| match n {
            CsgNode::Import { .. } => Some(n.clone()),
            _ => None,
        });
        (node, ev.errors().to_vec())
    }

    #[test]
    fn import_relative_resolves_against_source_dir() {
        // A RELATIVE file resolves against the source dir at eval time —
        // consumed end-to-end (also checked via
        // evaluate_file_at in morphe-conformance).
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        let (node, errors) = run_import(&mut ev, "import(\"model.stl\");");
        assert!(errors.is_empty(), "no errors: {errors:?}");
        let Some(CsgNode::Import { path, .. }) = node else {
            panic!("expected an Import node, got {node:?}")
        };
        assert_eq!(path, PathBuf::from("/proj/parts/model.stl"));
        // And an absolute source_dir joins too (never canonicalized).
    }

    #[test]
    fn import_absolute_path_under_source_dir_untouched() {
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        let (node, errors) = run_import(&mut ev, "import(\"/proj/parts/sub/model.stl\");");
        assert!(errors.is_empty(), "{errors:?}");
        let Some(CsgNode::Import { path, .. }) = node else {
            panic!("expected an Import node")
        };
        assert_eq!(path, PathBuf::from("/proj/parts/sub/model.stl"));
    }

    #[test]
    fn import_absolute_path_outside_source_dir_rejected() {
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        let (_, errors) = run_import(&mut ev, "import(\"/abs/model.stl\");");
        assert!(errs_have_general(&errors), "{errors:?}");
        assert!(errors[0]
            .message
            .contains("outside the source file's directory"));
        // An EXISTING out-of-tree file is rejected the same way.
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        let (_, errors) = run_surface(&mut ev, "surface(\"/\");");
        assert!(errs_have_general(&errors), "{errors:?}");
    }

    #[test]
    fn import_absolute_path_without_source_dir_untouched() {
        // Unknown source dir (unsaved buffer): previous behavior is kept;
        // the kernel still requires a regular file.
        let mut ev = Evaluator::new();
        let (node, errors) = run_import(&mut ev, "import(\"/abs/model.stl\");");
        assert!(errors.is_empty());
        let Some(CsgNode::Import { path, .. }) = node else {
            panic!("expected an Import node")
        };
        assert_eq!(path, PathBuf::from("/abs/model.stl"));
    }

    #[cfg(unix)]
    #[test]
    fn import_symlink_escaping_source_dir_rejected() {
        let root = std::env::temp_dir().join(format!(
            "morphe-contain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let src = root.join("proj");
        std::fs::create_dir_all(&src).unwrap();
        let outside = root.join("secret.stl");
        std::fs::write(&outside, b"solid x\nendsolid x\n").unwrap();
        std::fs::write(src.join("inside.stl"), b"solid x\nendsolid x\n").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("link.stl")).unwrap();
        let scad = src.join("main.scad");

        // A symlink inside the tree pointing out of it: rejected.
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(&scad));
        let (_, errors) = run_import(&mut ev, "import(\"link.stl\");");
        assert!(errs_have_general(&errors), "{errors:?}");

        // The absolute path of the outside file: rejected.
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(&scad));
        let src_text = format!("import(\"{}\");", outside.display());
        let (_, errors) = run_import(&mut ev, &src_text);
        assert!(errs_have_general(&errors), "{errors:?}");

        // A regular in-tree file: accepted, carrying the canonical path.
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(&scad));
        let (node, errors) = run_import(&mut ev, "import(\"inside.stl\");");
        assert!(errors.is_empty(), "{errors:?}");
        let Some(CsgNode::Import { path, .. }) = node else {
            panic!("expected an Import node")
        };
        assert_eq!(path, src.join("inside.stl").canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_dotdot_paths_rejected() {
        // The compile result reaches the AI channel, so a
        // prompt-injected buffer must not point the file loader at
        // out-of-tree content. '..' components are a loud General error.
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/bracket.scad")));
        let (_, errors) = run_import(&mut ev, "import(\"../secrets/x.stl\");");
        assert!(errs_have_general(&errors), "one General error: {errors:?}");
        assert!(
            errors[0].message.contains("'..'"),
            "the error names the '..' policy: {:?}",
            errors[0].message
        );
    }

    #[test]
    fn surface_dotdot_paths_rejected() {
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/terrain.scad")));
        let (_, errors) = run_surface(&mut ev, "surface(\"../h.dat\");");
        assert!(errs_have_general(&errors), "one General error: {errors:?}");
    }

    #[test]
    fn import_missing_arg_errors() {
        // Missing `file` is a structured General error naming the span,
        // while downstream callers must not see an Import node.
        let mut ev = Evaluator::new();
        let (node, errors) = run_import(&mut ev, "import();");
        assert!(errs_have_general(&errors), "one General error: {errors:?}");
        // The node still flows (empty path) — the error drives the failure.
        assert!(node.is_some());
    }

    fn errs_have_general(errors: &[EvalError]) -> bool {
        !errors.is_empty()
            && errors
                .iter()
                .all(|e| matches!(e.kind, EvalErrorKind::General))
    }

    // -----------------------------------------------------------------------
    // surface argument resolution
    // -----------------------------------------------------------------------

    /// Drive a `surface(...)` call through eval and pull the resolved node.
    fn run_surface(ev: &mut Evaluator, src: &str) -> (Option<CsgNode>, Vec<EvalError>) {
        let stmts = parse_program(src).expect("parses");
        let root = ev.eval_program(&stmts);
        let CsgNode::Group { children: tops, .. } = root else {
            panic!("program root is a Group")
        };
        let node = tops.iter().find_map(|n| match n {
            CsgNode::Surface { .. } => Some(n.clone()),
            _ => None,
        });
        (node, ev.errors().to_vec())
    }

    #[test]
    fn surface_args_defaults_and_resolution() {
        // Defaults (center=false, invert=false, convexity=1) and the
        // relative path resolves against the source dir.
        let mut ev = Evaluator::new();
        ev.set_source_path(Some(Path::new("/proj/parts/terrain.scad")));
        let (node, errors) = run_surface(&mut ev, "surface(\"heightmap.dat\");");
        assert!(errors.is_empty(), "no errors: {errors:?}");
        let Some(CsgNode::Surface {
            path,
            center,
            invert,
            convexity,
            ..
        }) = node
        else {
            panic!("expected a Surface node, got {node:?}")
        };
        assert_eq!(path, PathBuf::from("/proj/parts/heightmap.dat"));
        assert!(!center);
        assert!(!invert);
        assert_eq!(convexity, 1);

        // An absolute path under the source dir is untouched; named bools
        // (Bool-only, upstream) apply.
        let (node, errors) = run_surface(
            &mut ev,
            "surface(file = \"/proj/parts/abs/h.dat\", center = true, invert = true, convexity = 4);",
        );
        assert!(errors.is_empty());
        let Some(CsgNode::Surface {
            path,
            center,
            invert,
            convexity,
            ..
        }) = node
        else {
            panic!("expected a Surface node")
        };
        assert_eq!(path, PathBuf::from("/proj/parts/abs/h.dat"));
        assert!(center);
        assert!(invert);
        assert_eq!(convexity, 4);

        // A NUMERIC center is ignored (upstream gates on BOOL), not toBool'd.
        let (node, _) = run_surface(&mut ev, "surface(\"h.dat\", center = 1);");
        let Some(CsgNode::Surface { center, .. }) = node else {
            panic!("expected a Surface node")
        };
        assert!(!center, "numeric center is ignored (upstream BOOL gate)");
    }

    #[test]
    fn surface_missing_arg_errors() {
        let mut ev = Evaluator::new();
        let (_, errors) = run_surface(&mut ev, "surface();");
        assert!(errs_have_general(&errors), "one General error: {errors:?}");
    }

    #[test]
    fn surface_now_evaluates_to_surface_leaf() {
        // surface evaluates to a typed node with NO Unsupported error.
        let mut ev = Evaluator::new();
        let stmts = parse_program("surface(file = \"h.dat\");").expect("parses");
        let _ = ev.eval_program(&stmts);
        assert!(
            ev.errors().is_empty(),
            "surface must not record Unsupported: {:?}",
            ev.errors()
        );
    }

    #[test]
    fn import_layer_on_stl_warns_and_ignores() {
        let mut ev = Evaluator::new();
        let (node, errors) = run_import(&mut ev, "import(\"a.stl\", layer=\"x\");");
        assert!(
            errors.is_empty(),
            "layer on .stl must not error: {errors:?}"
        );
        let Some(CsgNode::Import { .. }) = node else {
            panic!("expected Import node")
        };
        assert!(
            ev.warnings().iter().any(|w| w.contains("layer")),
            "layer on .stl warns + ignores: {:?}",
            ev.warnings()
        );
    }

    // -----------------------------------------------------------------------
    // offset argument resolution
    // -----------------------------------------------------------------------

    /// Helper: build named-arg lists for offset_params tests (the AST Arg
    /// shape the parser produces for `offset(r = 2, ...)`).
    fn offset_args(pairs: &[(&str, f64)]) -> Vec<Arg> {
        let span = Span { start: 0, end: 1 };
        pairs
            .iter()
            .map(|(name, n)| Arg {
                name: Some((*name).to_string()),
                value: Expr {
                    kind: ExprKind::Number(*n),
                    span,
                },
                span,
            })
            .collect()
    }

    #[test]
    fn offset_args_r_delta_chamfer_precedence() {
        // A defined r WINS over delta and forces chamfer=false.
        let mut ev = Evaluator::new();
        let p = ev.offset_params(&offset_args(&[
            ("r", 2.0),
            ("delta", 5.0),
            ("chamfer", 1.0),
        ]));
        assert_eq!(p.delta, 2.0);
        assert!(!p.chamfer, "chamfer is ignored under r mode");
        assert_eq!(p.join, OffsetJoin::Round, "r-mode resolves round joins");

        // delta + chamfer applies (miter at chamfer=false).
        let p = ev.offset_params(&offset_args(&[("delta", 5.0)]));
        assert_eq!((p.delta, p.chamfer), (5.0, false));
        assert_eq!(p.join, OffsetJoin::Miter, "delta without chamfer is miter");

        // delta + chamfer=true → bevel.
        let p = ev.offset_params(&offset_args(&[("delta", 5.0), ("chamfer", 1.0)]));
        assert_eq!((p.delta, p.chamfer), (5.0, true));
        assert_eq!(p.join, OffsetJoin::Bevel, "delta + chamfer is bevel");

        // NEITHER given → upstream default 100000 (and a warning).
        let p = ev.offset_params(&[]);
        assert_eq!(p.delta, 100_000.0);
        assert!(!p.chamfer);
        assert!(
            ev.scope
                .warnings()
                .iter()
                .any(|w| w.contains("r or delta needed")),
            "the default-mode warning is recorded"
        );
    }

    #[test]
    fn offset_fragments_from_named_args() {
        // $fn/$fa/$fs as named args land in the params.
        let mut ev = Evaluator::new();
        let p = ev.offset_params(&offset_args(&[
            ("r", 1.0),
            ("$fn", 32.0),
            ("$fs", 0.5),
            ("$fa", 3.0),
        ]));
        assert_eq!(
            (p.fragments.fn_, p.fragments.fs, p.fragments.fa),
            (32.0, 0.5, 3.0)
        );

        // Without named args: the upstream seeds.
        let p = ev.offset_params(&offset_args(&[("r", 1.0)]));
        assert_eq!(
            (p.fragments.fn_, p.fragments.fs, p.fragments.fa),
            (0.0, 2.0, 12.0)
        );
    }

    #[test]
    fn offset_node_child_is_group_params_roundtrip() {
        // The typed node carries params verbatim; the child slot types the
        // transform Group invariant. (Hand-built node, independent of the
        // eval dispatch.)
        let params = OffsetParams {
            delta: 2.5,
            join: OffsetJoin::Bevel,
            chamfer: true,
            fragments: Fragments {
                fn_: 16.0,
                fs: 2.0,
                fa: 12.0,
            },
        };
        let node = CsgNode::Offset {
            params,
            child: Box::new(CsgNode::Group {
                children: vec![CsgNode::Primitive {
                    primitive: Primitive::Square {
                        size: [10.0, 10.0],
                        center: false,
                    },
                    modifiers: Modifiers::default(),
                }],
                modifiers: Modifiers::default(),
            }),
            modifiers: Modifiers::default(),
        };
        match &node {
            CsgNode::Offset {
                params: p, child, ..
            } => {
                assert_eq!(*p, params, "params round-trip verbatim");
                assert!(
                    matches!(child.as_ref(), CsgNode::Group { .. }),
                    "child-always-Group"
                );
            }
            other => panic!("expected Offset, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // the offset dispatch arm
    // -----------------------------------------------------------------------

    #[test]
    fn offset_evaluates_to_typed_node_end_to_end() {
        // Through the REAL eval path (parse → eval_program), not a
        // hand-built node.
        let src = "linear_extrude(height = 5) offset(r = 2) square(10);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let root = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty(), "no Unsupported: {:?}", ev.errors());

        // The tree: LinearExtrude { child: Offset { child: square } }.
        let mut found_offset = false;
        fn walk(node: &CsgNode, found: &mut bool) {
            match node {
                CsgNode::Offset { params, child, .. } => {
                    *found = true;
                    assert_eq!(params.delta, 2.0);
                    assert_eq!(params.join, OffsetJoin::Round);
                    assert!(matches!(child.as_ref(), CsgNode::Group { .. }));
                }
                CsgNode::Group { children, .. } | CsgNode::Union { children, .. } => {
                    children.iter().for_each(|c| walk(c, found))
                }
                CsgNode::LinearExtrude { child, .. } | CsgNode::RotateExtrude { child, .. } => {
                    walk(child, found)
                }
                _ => {}
            }
        }
        walk(&root, &mut found_offset);
        assert!(found_offset, "the tree carries a typed Offset node");
    }

    #[test]
    fn offset_chamfer_mode_evaluates_with_bevel_join() {
        // delta+chamfer through the real path resolves Bevel — asserted
        // ON THE TREE, not just via the unit resolver.
        let src = "offset(delta = 1, chamfer = true) square(10);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let root = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty());
        let mut join = None;
        fn walk_join(node: &CsgNode, join: &mut Option<OffsetJoin>) {
            match node {
                CsgNode::Offset { params, .. } => *join = Some(params.join),
                CsgNode::Group { children, .. } => children.iter().for_each(|c| walk_join(c, join)),
                _ => {}
            }
        }
        walk_join(&root, &mut join);
        assert_eq!(join, Some(OffsetJoin::Bevel), "Bevel rode the eval path");
    }

    #[test]
    fn offset_non_finite_r_warns_while_delta_takes_over() {
        // A non-finite value in a SUPPLIED slot warns even when the other
        // slot wins.
        let mut ev = Evaluator::new();
        let p = ev.offset_params(&offset_args(&[("r", f64::INFINITY), ("delta", 5.0)]));
        assert_eq!((p.delta, p.join), (5.0, OffsetJoin::Miter));
        assert!(
            ev.scope
                .warnings()
                .iter()
                .any(|w| w.contains("offset parameter r should be a finite")),
            "the non-finite r warned: {:?}",
            ev.scope.warnings()
        );
        // And the reverse: non-finite delta with finite r.
        let mut ev = Evaluator::new();
        let p = ev.offset_params(&offset_args(&[("r", 2.0), ("delta", f64::NAN)]));
        assert_eq!(p.delta, 2.0, "finite r still wins");
        assert!(ev
            .scope
            .warnings()
            .iter()
            .any(|w| w.contains("offset parameter delta should be a finite")));
    }

    #[test]
    fn offset_dynamic_scope_fn_flows_into_the_node() {
        // The dynamic-scope $fn path (no named arg) —
        // `$fn = 12;` in the enclosing scope reaches the offset node.
        let src = "$fn = 12; offset(r = 1) square(10);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let root = ev.eval_program(&stmts);
        let mut fn_val = None;
        fn walk_dyn(node: &CsgNode, fn_val: &mut Option<f64>) {
            match node {
                CsgNode::Offset { params, .. } => *fn_val = Some(params.fragments.fn_),
                CsgNode::Group { children, .. } => {
                    children.iter().for_each(|c| walk_dyn(c, fn_val))
                }
                _ => {}
            }
        }
        walk_dyn(&root, &mut fn_val);
        assert_eq!(fn_val, Some(12.0), "dynamic-scope $fn reached the node");
    }

    #[test]
    fn offset_fn_named_arg_flows_into_the_node() {
        // $fn at the call site lands in the node fragments (the
        // params-before-children ordering).
        let src = "offset(r = 1, $fn = 24) square(10);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let root = ev.eval_program(&stmts);
        let mut fn_val = None;
        fn walk_fn(node: &CsgNode, fn_val: &mut Option<f64>) {
            match node {
                CsgNode::Offset { params, .. } => *fn_val = Some(params.fragments.fn_),
                CsgNode::Group { children, .. } => children.iter().for_each(|c| walk_fn(c, fn_val)),
                _ => {}
            }
        }
        walk_fn(&root, &mut fn_val);
        assert_eq!(fn_val, Some(24.0), "$fn flowed through the dispatch");
    }

    // -----------------------------------------------------------------------
    // projection eval tests
    // -----------------------------------------------------------------------

    #[test]
    fn projection_cut_default_false_and_named_true() {
        // cut defaults false; named-arg resolution (upstream
        // toBool semantics).
        let mut ev = Evaluator::new();
        assert!(!ev.projection_params(&[]), "default cut=false");
        let span = Span { start: 0, end: 1 };
        let num = |n: f64| Expr {
            kind: ExprKind::Number(n),
            span,
        };
        let arg = |name: &str, value: Expr| Arg {
            name: Some(name.to_string()),
            value,
            span,
        };
        let t = Expr {
            kind: ExprKind::Bool(true),
            span,
        };
        let f = Expr {
            kind: ExprKind::Bool(false),
            span,
        };
        assert!(ev.projection_params(&[arg("cut", t)]));
        assert!(!ev.projection_params(&[arg("cut", f)]));
        assert!(ev.projection_params(&[arg("cut", num(1.0))]));
        assert!(!ev.projection_params(&[arg("cut", num(0.0))]));
    }

    #[test]
    fn projection_evaluates_to_typed_node_end_to_end() {
        // Through the REAL eval path: the tree carries Projection { cut:
        // true, child: Group } over the translate.
        let src = "projection(cut = true) translate([0, 0, -1]) cube(10);";
        let stmts = parse_program(src).expect("parses");
        let mut ev = Evaluator::new();
        ev.set_source(src);
        let root = ev.eval_program(&stmts);
        assert!(ev.errors().is_empty());
        let mut found = None;
        fn walk_p(node: &CsgNode, found: &mut Option<bool>) {
            match node {
                CsgNode::Projection { cut, child, .. } => {
                    *found = Some(*cut);
                    assert!(matches!(child.as_ref(), CsgNode::Group { .. }));
                }
                CsgNode::Group { children, .. } => children.iter().for_each(|c| walk_p(c, found)),
                _ => {}
            }
        }
        walk_p(&root, &mut found);
        assert_eq!(found, Some(true), "typed Projection node with cut=true");
    }
}
