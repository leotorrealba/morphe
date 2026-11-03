//! Expression and statement AST for the OpenSCAD language.
//!
//! Every node carries the byte [`Span`] of the source text it was parsed
//! from.

use std::sync::Arc;

use crate::lexer::Span;

/// An expression: a kind plus the source span it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// Prefix unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `+x`
    Pos,
    /// `!x`
    Not,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `^` (right-associative)
    Pow,
}

/// One argument in a function call: positional (`f(1)`) or named (`f(r = 1)`).
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    /// `Some(name)` for named arguments; `$`-names keep their `$` prefix
    /// (e.g. `"$fn"`).
    pub name: Option<String>,
    pub value: Expr,
    pub span: Span,
}

/// The different kinds of expression.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// Number literal (OpenSCAD numbers are all doubles).
    Number(f64),
    /// String literal.
    Str(String),
    /// `true` / `false`.
    Bool(bool),
    /// `undef`.
    Undef,
    /// Identifier, e.g. `x`.
    Ident(String),
    /// `$`-identifier, stored *with* the `$` prefix (e.g. `"$fn"`) so it
    /// can never collide with a plain identifier of the same name.
    DollarIdent(String),
    /// Vector literal `[a, b, c]` (may be empty).
    Vector(Vec<Expr>),
    /// Range `[start:end]` or `[start:step:end]`.
    Range {
        start: Box<Expr>,
        step: Option<Box<Expr>>,
        end: Box<Expr>,
    },
    /// Prefix unary operation.
    Unary { op: UnaryOp, operand: Box<Expr> },
    /// Binary operation.
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `cond ? then : else` (right-associative).
    Ternary {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    /// Function call `f(a, b = c)`.
    Call { callee: Box<Expr>, args: Vec<Arg> },
    /// Indexing `expr[i]`.
    Index { target: Box<Expr>, index: Box<Expr> },
    /// Member access `expr.x`.
    Member { target: Box<Expr>, member: String },
    /// Function literal `function(params) body` (upstream
    /// `FunctionDefinition`, `expr: TOK_FUNCTION '(' parameters ')' expr`):
    /// evaluates to a function value closing over the defining scope. The
    /// parameters and body are shared (`Arc`) so every value created from
    /// the literal points at the same AST, as upstream's `shared_ptr`s do.
    FunctionLiteral {
        params: Arc<Vec<Param>>,
        body: Arc<Expr>,
    },
    /// `echo(args) [body]` as an expression (upstream `Echo`, `expr:
    /// TOK_ECHO '(' arguments ')' expr_or_empty`): prints the arguments,
    /// then yields the body's value (undef without a body).
    Echo {
        args: Vec<Arg>,
        body: Option<Box<Expr>>,
    },
    /// `assert(args) [body]` as an expression (upstream `Assert`): checks
    /// the condition, then yields the body's value (undef without a body).
    Assert {
        args: Vec<Arg>,
        body: Option<Box<Expr>>,
    },
    /// `let(a = 1, b = 2) body`. Bindings use the call-`arguments` grammar
    /// (upstream `expr: TOK_LET '(' arguments ')' expr`): named or
    /// positional, at most one trailing comma; the validity of positional
    /// entries is decided at eval.
    Let { args: Vec<Arg>, body: Box<Expr> },
    /// List-comprehension `for (bindings) element`, only valid as a vector
    /// element. Bindings use the call-`arguments` grammar (named or
    /// positional, trailing comma), matching upstream `LcFor`.
    LcFor { args: Vec<Arg>, body: Box<Expr> },
    /// C-style list-comprehension `for (init; condition; update) element`
    /// (upstream `LcForC`). `init` and `update` use the `arguments` grammar.
    LcForC {
        init: Vec<Arg>,
        cond: Box<Expr>,
        update: Vec<Arg>,
        body: Box<Expr>,
    },
    /// List-comprehension `each element` — splices the element's value into
    /// the enclosing vector (upstream `LcEach`).
    LcEach { body: Box<Expr> },
    /// List-comprehension `if (cond) element [else element]` (upstream
    /// `LcIf`); a missing else contributes no elements.
    LcIf {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Option<Box<Expr>>,
    },
    /// List-comprehension `let (bindings) element`, whose body must itself
    /// be a comprehension element (upstream `LcLet`: a `let` with a plain
    /// expression body parses as [`ExprKind::Let`] instead).
    LcLet { args: Vec<Arg>, body: Box<Expr> },
}

/// A statement: a kind plus the source span it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// Modifier prefixes on a module instantiation, e.g. `#cube(1);`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// `!` — show only this subtree.
    pub root: bool,
    /// `#` — highlight (debug).
    pub debug: bool,
    /// `%` — background (transparent, excluded from render).
    pub background: bool,
    /// `*` — disable.
    pub disable: bool,
}

/// One parameter in a `module`/`function` definition: `p` or `p = default`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Parameter name; `$`-names keep their `$` prefix (e.g. `"$fn"`).
    pub name: String,
    pub default: Option<Expr>,
    pub span: Span,
}

/// The different kinds of statement.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `name = expr;` (also `$name = expr;`, name kept with its `$`).
    Assign { name: String, value: Expr },
    /// Module instantiation `name(args)` with optional modifier prefixes and
    /// children: none (`name(args);`), a block (`name(args) { ... }`), or a
    /// single child (`translate(v) cube(1);` — one-element `children`).
    ///
    /// Mirrors upstream `single_module_instantiation: module_id '(' arguments
    /// ')'` where `module_id` also covers `for`, `let`, `each`, `echo`, and
    /// `assert` (and `intersection_for` is a plain identifier), so those
    /// parse as instantiations named `"for"`, `"let"`, ... and are resolved
    /// at evaluation time.
    ModuleCall {
        name: String,
        args: Vec<Arg>,
        modifiers: Modifiers,
        children: Vec<Stmt>,
    },
    /// `module name(params) { ... }` or `module name(params) stmt;`.
    ModuleDef {
        name: String,
        params: Vec<Param>,
        body: Box<Stmt>,
    },
    /// `function name(params) = expr;`.
    FunctionDef {
        name: String,
        params: Vec<Param>,
        body: Expr,
    },
    /// `if (cond) stmt [else stmt]`. Upstream an if/else is itself a
    /// `module_instantiation` (`IfElseModuleInstantiation`), so it accepts
    /// modifier prefixes too (`!if (x) cube();`).
    If {
        cond: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
        modifiers: Modifiers,
    },
    /// `{ ... }`.
    Block(Vec<Stmt>),
    /// `include <path>`. The evaluator splices the named file's statements
    /// in its place before evaluating (upstream handles `include` in the
    /// lexer, as textual insertion).
    Include { path: String },
    /// `use <path>` (top level only): imports the named file's module and
    /// function definitions.
    Use { path: String },
}
