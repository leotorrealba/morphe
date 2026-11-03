//! List-comprehension evaluation (`LcFor`, `LcForC`, `LcEach`,
//! `LcIf`, `LcLet`).
//!
//! Every rule here is transcribed from upstream openscad
//! src/core/Expression.cc (master, fetched 2026-07-03:
//! `Vector::evaluate`, `LcFor::forEach`/`doForEach`, `LcForC::evaluate`,
//! `LcEach::evaluate`/`evalRecur`, `LcIf::evaluate`, `LcLet::evaluate`):
//!
//! - A vector child that is a comprehension evaluates to an
//!   `EmbeddedVectorType` whose elements splice (flatten) into the enclosing
//!   vector; a plain child contributes exactly one value
//!   (`Vector::evaluate`).
//! - `LcFor` iterates its arguments-grammar bindings via the same
//!   `doForEach` dispatch as the `for` statement — ranges (guarded at
//!   1,000,000 steps), vectors, strings (characters); undef iterates zero
//!   times, any other value once — with multiple bindings nesting cartesian
//!   (later bindings see earlier loop variables). Each fully-bound iteration
//!   evaluates the body as a comprehension element.
//! - `LcForC` (C-style) binds `init` once, loops while `cond` is truthy,
//!   emits the body element then re-runs `update` each pass, and aborts once
//!   its iteration counter reaches 1,000,000 (`counter++ == 1000000` logs
//!   "For loop counter exceeded limit" and throws `LoopCntException`). The
//!   throw is FATAL: it unwinds past the enclosing vector, so the whole
//!   expression yields nothing usable — morphe mirrors this with a sticky
//!   abort latch that collapses the wrapping vector (and every enclosing
//!   expression) to undef rather than returning a bogus partial.
//! - `LcEach` splices: ranges to their values, vectors and strings one level
//!   (characters for strings), undef to nothing, any other value as one
//!   element; a comprehension body flattens then each result splices again
//!   (`evalRecur` recursing into an `EmbeddedVectorType`).
//! - `LcIf` picks a branch via `toBool`; a missing else contributes nothing.
//! - `LcLet` binds sequentially (first-wins duplicates, like expression
//!   `let`) then evaluates the body element.
//!
//! Expected values are cross-checked against the OpenSCAD 2021.01 binary via
//! `echo(...)`; deviations from that binary (where morphe follows master
//! instead) are cited at the test.

use morphe_eval::eval::{Evaluator, MAX_EXPR_DEPTH};
use morphe_eval::value::Value;
use morphe_lang::ast::{Arg, Expr, ExprKind};
use morphe_lang::lexer::Span;
use morphe_lang::parser::parse_expr;

/// Parse and evaluate one expression, returning its value.
fn eval(src: &str) -> Value {
    eval_with(src).0
}

/// Parse and evaluate one expression, returning the value and the evaluator
/// (for its warning/error sinks and post-eval scope probing).
fn eval_with(src: &str) -> (Value, Evaluator) {
    let mut evaluator = Evaluator::new();
    evaluator.set_source(src);
    let expr = parse_expr(src).expect("expression should parse");
    let value = evaluator.eval_expr(&expr);
    (value, evaluator)
}

/// The value rendered like an OpenSCAD echo argument (vectors bracket-
/// formatted with quoted strings), for direct comparison against oracle
/// `ECHO:` output.
fn echo(src: &str) -> String {
    eval(src).to_string()
}

fn has_warning(evaluator: &Evaluator, needle: &str) -> bool {
    evaluator.warnings().iter().any(|w| w.contains(needle))
}

fn has_error(evaluator: &Evaluator, needle: &str) -> bool {
    evaluator
        .errors()
        .iter()
        .any(|e| e.message.contains(needle))
}

// -------------------------------------------------------------- LcFor

/// `[for(i=[0:4]) i]` — the canonical range comprehension.
/// Oracle: `ECHO: [0, 1, 2, 3, 4]`.
#[test]
fn for_over_range() {
    assert_eq!(echo("[for(i=[0:4]) i]"), "[0, 1, 2, 3, 4]");
}

/// Nested `for` flattens into a single vector (each inner comprehension is
/// an embedded vector spliced by the outer).
/// Oracle: `ECHO: [0, 1, 10, 11]`.
#[test]
fn nested_for_flattens() {
    assert_eq!(echo("[for(i=[0:1]) for(j=[0:1]) i*10+j]"), "[0, 1, 10, 11]");
}

/// Three levels of `for` flatten just as flat.
/// Oracle: `ECHO: [0, 1, 2, 3, 4, 5, 6, 7]`.
#[test]
fn triple_nested_for_flattens() {
    assert_eq!(
        echo("[for(i=[0:1]) for(j=[0:1]) for(k=[0:1]) i*4+j*2+k]"),
        "[0, 1, 2, 3, 4, 5, 6, 7]"
    );
}

/// Cartesian multi-binding: later bindings see earlier loop variables and
/// the last binding varies fastest.
/// Oracle: `ECHO: [[1, "x"], [1, "y"], [2, "x"], [2, "y"]]`.
#[test]
fn cartesian_multi_binding_order() {
    assert_eq!(
        echo("[for(a=[1,2],b=[\"x\",\"y\"]) [a,b]]"),
        "[[1, \"x\"], [1, \"y\"], [2, \"x\"], [2, \"y\"]]"
    );
}

/// A string binding iterates one single-character string per character.
/// Oracle: `ECHO: ["h", "i"]`.
#[test]
fn for_over_string() {
    assert_eq!(echo("[for(c=\"hi\") c]"), "[\"h\", \"i\"]");
}

/// A scalar (non-iterable, non-undef) binding iterates exactly once.
/// Oracle: `ECHO: [7]`.
#[test]
fn for_over_scalar_iterates_once() {
    assert_eq!(echo("[for(x=7) x]"), "[7]");
}

/// An undef binding iterates zero times, silently.
/// Oracle (`[for(i=undef) i]`): `ECHO: []`.
#[test]
fn for_over_undef_iterates_zero() {
    let (value, evaluator) = eval_with("[for(i=undef) i]");
    assert_eq!(value.to_string(), "[]");
    assert!(evaluator.warnings().is_empty());
}

/// An empty-vector binding iterates zero times.
/// Oracle: `ECHO: []`.
#[test]
fn for_over_empty_vector() {
    assert_eq!(echo("[for(i=[]) i]"), "[]");
}

/// A range with >= 1,000,000 steps warns and iterates zero times.
#[test]
fn for_over_huge_range_warns_and_skips() {
    let (value, evaluator) = eval_with("[for(i=[0:1000000]) i]");
    assert_eq!(value.to_string(), "[]");
    assert!(has_warning(
        &evaluator,
        "Bad range parameter in for statement: too many elements"
    ));
}

/// DEVIATION: `[for(i=[5:0]) i]` (begin > end, default +1 step). The
/// OpenSCAD 2021.01 binary DEPRECATED-reverses the range and echoes
/// `[0, 1, 2, 3, 4, 5]`; master (which morphe ports) treats a positive-step
/// range with begin > end as empty (`RangeType::numValues` returns 0), so
/// morphe yields `[]`.
#[test]
fn for_over_reversed_range_is_empty_on_master() {
    assert_eq!(echo("[for(i=[5:0]) i]"), "[]");
}

// -------------------------------------------------------------- LcIf

/// `if` without else filters out the falsy iterations.
/// Oracle: `ECHO: [2, 3]`.
#[test]
fn for_if_filters() {
    assert_eq!(echo("[for(i=[0:3]) if(i>1) i]"), "[2, 3]");
}

/// `if`/`else` selects a branch element per iteration.
/// Oracle: `ECHO: [0, -1, 2, -3]`.
#[test]
fn for_if_else_selects_branch() {
    assert_eq!(
        echo("[for(i=[0:3]) if(i%2==0) i else -i]"),
        "[0, -1, 2, -3]"
    );
}

/// A bare `if` whose condition is false contributes no element.
/// Oracle (`[if(false) 1]`): `ECHO: []`.
#[test]
fn if_false_without_else_is_empty() {
    assert_eq!(echo("[if(false) 1]"), "[]");
}

// -------------------------------------------------------------- LcEach

/// `each` splices a vector's elements one level.
/// Oracle: `ECHO: [1, 2, 3, 4]`.
#[test]
fn each_splices_vector() {
    assert_eq!(echo("[each [1,2,3], 4]"), "[1, 2, 3, 4]");
}

/// `each` splices a range to its iterated values.
/// Oracle: `ECHO: [1, 3, 5]`.
#[test]
fn each_splices_range() {
    assert_eq!(echo("[each [1:2:5]]"), "[1, 3, 5]");
}

/// `each` over a scalar contributes the single value.
/// Oracle: `ECHO: [5]`.
#[test]
fn each_scalar_is_single_element() {
    assert_eq!(echo("[each 5]"), "[5]");
}

/// `each` over a string splices it into single-character strings — the same
/// character split as the STRING branch of `doForEach` (upstream `evalRecur`
/// STRING branch).
/// Oracle: `ECHO: ["a", "b", "c", "d"]`.
#[test]
fn each_splices_string_into_characters() {
    assert_eq!(
        echo("[each \"abc\", \"d\"]"),
        "[\"a\", \"b\", \"c\", \"d\"]"
    );
}

/// `each undef` contributes nothing.
/// Oracle (`[each undef, 9]`): `ECHO: [9]`.
#[test]
fn each_undef_contributes_nothing() {
    assert_eq!(echo("[each undef, 9]"), "[9]");
}

/// `each []` contributes nothing.
/// Oracle: `ECHO: []`.
#[test]
fn each_empty_vector_is_empty() {
    assert_eq!(echo("[each []]"), "[]");
}

/// A range `each`-ed with >= 1,000,000 steps warns and splices nothing.
#[test]
fn each_huge_range_warns_and_skips() {
    let (value, evaluator) = eval_with("[each [0:1000000]]");
    assert_eq!(value.to_string(), "[]");
    assert!(has_warning(
        &evaluator,
        "Bad range parameter in for statement: too many elements"
    ));
}

/// `each` over a BARE comprehension recurses (`evalRecur` into the embedded
/// vector): each produced element is spliced again, so `[i,i]` vectors
/// flatten fully.
/// Oracle (`[each for(i=[0:1]) [i,i]]`): `ECHO: [0, 0, 1, 1]`.
#[test]
fn each_bare_comprehension_recurses() {
    assert_eq!(echo("[each for(i=[0:1]) [i,i]]"), "[0, 0, 1, 1]");
}

/// `each` over a `[...]`-WRAPPED comprehension splices only one level: the
/// wrapping vector evaluates to a plain vector of `[i,i]` elements, which
/// `each` splices without recursing into each element.
/// Oracle (`[each [for(i=[0:1]) [i,i]]]`): `ECHO: [[0, 0], [1, 1]]`.
#[test]
fn each_wrapped_comprehension_splices_one_level() {
    assert_eq!(echo("[each [for(i=[0:1]) [i,i]]]"), "[[0, 0], [1, 1]]");
}

// -------------------------------------------------------------- LcLet

/// `let` in a comprehension binds then evaluates the body element.
/// Oracle: `ECHO: [1, 11, 21]`.
#[test]
fn let_in_comprehension() {
    assert_eq!(echo("[for(i=[0:2]) let(j=i*10) j+1]"), "[1, 11, 21]");
}

/// `let` duplicate bindings are first-wins with a warning (shared with
/// expression `let`).
/// Oracle: `ECHO: [0, 1]` plus a duplicate-assignment warning.
#[test]
fn let_duplicate_binding_first_wins() {
    let (value, evaluator) = eval_with("[for(i=[0:1]) let(x=i,x=99) x]");
    assert_eq!(value.to_string(), "[0, 1]");
    assert!(has_warning(
        &evaluator,
        "Ignoring duplicate variable assignment x = 99"
    ));
}

/// `let` can bind a `$`-variable visible (dynamically) to the body.
/// Oracle (`[let($x=5) for(i=[0:1]) $x+i]`): `ECHO: [5, 6]`.
#[test]
fn let_binds_dollar_variable_for_body() {
    assert_eq!(echo("[let($x=5) for(i=[0:1]) $x+i]"), "[5, 6]");
}

// -------------------------------------------------------------- LcForC

/// C-style `for(init; cond; update)`.
/// Oracle: `ECHO: [0, 1, 2]`.
#[test]
fn c_style_for_basic() {
    assert_eq!(echo("[for(a=0;a<3;a=a+1) a]"), "[0, 1, 2]");
}

/// C-style `for` with multiple init and update bindings.
/// Oracle: `ECHO: [[0, 10], [1, 9], [2, 8]]`.
#[test]
fn c_style_for_multi_binding() {
    assert_eq!(
        echo("[for(a=0,b=10;a<3;a=a+1,b=b-1) [a,b]]"),
        "[[0, 10], [1, 9], [2, 8]]"
    );
}

/// A never-false C-style `for` aborts FATALLY at the 1,000,000 iteration
/// cap: it records the error and, unlike a localized assert failure,
/// discards the whole expression's value (upstream's `LoopCntException`
/// unwinds past the enclosing vector). The bogus million-element partial is
/// never returned — the expression yields undef.
#[test]
fn c_style_for_hits_iteration_cap() {
    let (value, evaluator) = eval_with("[for(a=0;true;a=a+1) a]");
    assert!(value.is_undef());
    assert!(has_error(&evaluator, "For loop counter exceeded limit"));
}

/// The cap is fatal to the ENCLOSING vector too: a comprehension that trips
/// the counter must not leave the sibling elements around it — the whole
/// vector collapses to undef, matching upstream's stack-unwinding abort
/// (`[1, for(a=0;true;a=a) a, 2]` must NOT yield `[1, ..., 2]`).
#[test]
fn c_style_for_cap_discards_enclosing_vector() {
    let (value, evaluator) = eval_with("[1, for(a=0;true;a=a) a, 2]");
    assert!(value.is_undef());
    assert!(has_error(&evaluator, "For loop counter exceeded limit"));
}

// ---------------------------------------------------------- scoping

/// A `$`-named loop variable is bound and visible in the body.
/// Oracle (`[for($i=[0:2]) $i]`): `ECHO: [0, 1, 2]`.
#[test]
fn dollar_loop_variable_visible_in_body() {
    assert_eq!(echo("[for($i=[0:2]) $i]"), "[0, 1, 2]");
}

/// Loop variables never leak past the comprehension: after evaluating one
/// that binds `i`, `i` is unknown in the enclosing scope.
#[test]
fn loop_variable_does_not_leak() {
    let (_value, mut evaluator) = eval_with("[for(i=[0:2]) i]");
    let probe = parse_expr("i").expect("parse");
    assert!(evaluator.eval_expr(&probe).is_undef());
    assert!(has_warning(&evaluator, "Ignoring unknown variable 'i'"));
}

// ---------------------------------------------------------- mixed / depth

/// Comprehension and plain elements freely mix in one vector, each splicing
/// or contributing as its kind dictates.
/// Oracle (`[0, for(i=[1:2]) i, each [8,9], 10]`): `ECHO: [0, 1, 2, 8, 9, 10]`.
#[test]
fn mixed_plain_and_comprehension_elements() {
    assert_eq!(
        echo("[0, for(i=[1:2]) i, each [8,9], 10]"),
        "[0, 1, 2, 8, 9, 10]"
    );
}

/// A right-leaning chain of `depth` single-iteration `for` comprehensions
/// around `1`, built programmatically: the parser caps its own nesting at
/// 256, but the evaluator must survive any AST via the shared
/// [`MAX_EXPR_DEPTH`] guard.
fn nested_for_comprehension(depth: usize) -> Expr {
    let span = Span { start: 0, end: 1 };
    let mut inner = Expr {
        kind: ExprKind::Number(1.0),
        span,
    };
    for _ in 0..depth {
        inner = Expr {
            kind: ExprKind::LcFor {
                args: vec![Arg {
                    name: Some("i".to_string()),
                    value: Expr {
                        kind: ExprKind::Range {
                            start: Box::new(Expr {
                                kind: ExprKind::Number(0.0),
                                span,
                            }),
                            step: None,
                            end: Box::new(Expr {
                                kind: ExprKind::Number(0.0),
                                span,
                            }),
                        },
                        span,
                    },
                    span,
                }],
                body: Box::new(inner),
            },
            span,
        };
    }
    // Wrap in the enclosing vector, the only place a comprehension is valid.
    Expr {
        kind: ExprKind::Vector(vec![inner]),
        span,
    }
}

#[test]
fn deep_comprehension_nesting_errors() {
    // A dedicated big stack keeps the native cost of the guard's wrapper
    // frames off the harness thread: the test must observe the guard, never
    // a real overflow.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(deep_comprehension_nesting_body)
        .expect("spawn")
        .join()
        .expect("join");
}

fn deep_comprehension_nesting_body() {
    let mut evaluator = Evaluator::new();
    let expr = nested_for_comprehension(MAX_EXPR_DEPTH + 100);
    evaluator.eval_expr(&expr);
    assert!(has_error(&evaluator, "expression nesting too deep"));

    // The depth counter unwinds: the evaluator stays usable.
    let ok = parse_expr("[for(i=[0:2]) i]").expect("parse");
    assert_eq!(evaluator.eval_expr(&ok).to_string(), "[0, 1, 2]");
}

/// A single `for` with `bindings` single-value bindings
/// (`[for(x0=[0:0], x1=[0:0], ...) 1]`), built programmatically. Each
/// binding is one cartesian level, so the arguments-list length — which the
/// parser's nesting cap does NOT bound — drives the per-binding recursion
/// depth. A wide enough list must trip the [`MAX_EXPR_DEPTH`] guard, not
/// overflow the native stack.
fn wide_for_comprehension(bindings: usize) -> Expr {
    let span = Span { start: 0, end: 1 };
    let unit_range = || Expr {
        kind: ExprKind::Range {
            start: Box::new(Expr {
                kind: ExprKind::Number(0.0),
                span,
            }),
            step: None,
            end: Box::new(Expr {
                kind: ExprKind::Number(0.0),
                span,
            }),
        },
        span,
    };
    let args = (0..bindings)
        .map(|k| Arg {
            name: Some(format!("x{k}")),
            value: unit_range(),
            span,
        })
        .collect();
    let lc = Expr {
        kind: ExprKind::LcFor {
            args,
            body: Box::new(Expr {
                kind: ExprKind::Number(1.0),
                span,
            }),
        },
        span,
    };
    Expr {
        kind: ExprKind::Vector(vec![lc]),
        span,
    }
}

#[test]
fn wide_binding_list_errors_without_overflow() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(wide_binding_list_body)
        .expect("spawn")
        .join()
        .expect("join");
}

fn wide_binding_list_body() {
    let mut evaluator = Evaluator::new();
    let expr = wide_for_comprehension(MAX_EXPR_DEPTH + 100);
    evaluator.eval_expr(&expr);
    assert!(has_error(&evaluator, "expression nesting too deep"));

    // The depth counter unwinds: the evaluator stays usable.
    let ok = parse_expr("[for(i=[0:2]) i]").expect("parse");
    assert_eq!(evaluator.eval_expr(&ok).to_string(), "[0, 1, 2]");
}
