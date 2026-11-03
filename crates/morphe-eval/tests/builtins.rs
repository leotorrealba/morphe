//! The OpenSCAD builtin function library.
//!
//! Every rule here is transcribed from upstream openscad
//! `src/core/builtin_functions.cc` and `src/utils/degree_trig.cc` (master,
//! fetched 2026-07-03) and cross-checked against the installed OpenSCAD
//! 2021.01 binary via `echo(...)`. Documented divergences:
//!
//! - `min`/`max` of a single vector containing a non-number: master's
//!   `min_max_arguments` breaks on any non-number and returns undef; the
//!   2021.01 binary instead silently drops the bad element (`min([1,"x"])`
//!   -> `1`). We follow master (undef).
//! - `rands` uses a documented deterministic SplitMix64 stand-in, NOT
//!   boost/std `mt19937`: the seeded 4-argument form is reproducible but its
//!   numbers differ from upstream, and the unseeded 3-argument form (which
//!   upstream seeds non-deterministically from wall-clock time) here uses a
//!   fixed seed so it is deterministic.
//! - `version()`/`version_num()` return a fixed `[2021, 1, 0]` /
//!   `20210100`, matching the installed 2021.01 binary.

use morphe_eval::eval::Evaluator;
use morphe_eval::value::Value;
use morphe_lang::parser::parse_expr;

/// Evaluate a single expression in a fresh evaluator, returning the value
/// and the evaluator (for its warning sink).
fn eval_with(src: &str) -> (Value, Evaluator) {
    let mut evaluator = Evaluator::new();
    let expr = parse_expr(src).expect("expression should parse");
    let value = evaluator.eval_expr(&expr);
    (value, evaluator)
}

fn eval(src: &str) -> Value {
    eval_with(src).0
}

fn num(src: &str) -> f64 {
    match eval(src) {
        Value::Number(n) => n,
        other => panic!("expected number from `{src}`, got {other:?}"),
    }
}

fn vec_of_nums(src: &str) -> Vec<f64> {
    match &eval(src) {
        Value::Vector(v) => v
            .iter()
            .map(|e| match e {
                Value::Number(n) => *n,
                other => panic!("expected number element, got {other:?}"),
            })
            .collect(),
        other => panic!("expected vector from `{src}`, got {other:?}"),
    }
}

fn has_warning(evaluator: &Evaluator, needle: &str) -> bool {
    evaluator.warnings().iter().any(|w| w.contains(needle))
}

fn n(x: f64) -> Value {
    Value::Number(x)
}

fn s(x: &str) -> Value {
    Value::String(x.into())
}

const EPS: f64 = 1e-12;

// ---------------------------------------------------------- trig (degrees)

#[test]
fn sin_is_exact_at_special_angles() {
    assert_eq!(num("sin(0)"), 0.0);
    assert_eq!(num("sin(30)"), 0.5);
    assert_eq!(num("sin(90)"), 1.0);
    assert_eq!(num("sin(150)"), 0.5);
    assert_eq!(num("sin(180)"), 0.0);
    assert_eq!(num("sin(270)"), -1.0);
    assert_eq!(num("sin(-90)"), -1.0);
    // full-revolution reduction stays exact
    assert_eq!(num("sin(390)"), 0.5);
}

#[test]
fn cos_is_exact_at_special_angles() {
    assert_eq!(num("cos(0)"), 1.0);
    assert_eq!(num("cos(60)"), 0.5);
    assert_eq!(num("cos(90)"), 0.0);
    assert_eq!(num("cos(180)"), -1.0);
    assert_eq!(num("cos(120)"), -0.5);
}

#[test]
fn tan_is_exact_at_special_angles() {
    assert_eq!(num("tan(0)"), 0.0);
    assert_eq!(num("tan(45)"), 1.0);
    assert!(num("tan(90)").is_infinite() && num("tan(90)") > 0.0);
    assert!((num("tan(135)") - -1.0).abs() < EPS);
}

#[test]
fn inverse_trig_returns_degrees() {
    assert_eq!(num("asin(0.5)"), 30.0);
    assert_eq!(num("asin(1)"), 90.0);
    assert_eq!(num("acos(0.5)"), 60.0);
    assert_eq!(num("acos(1)"), 0.0);
    assert_eq!(num("atan(1)"), 45.0);
    assert_eq!(num("atan(0)"), 0.0);
}

#[test]
fn atan2_covers_all_quadrants() {
    assert_eq!(num("atan2(0, 1)"), 0.0);
    assert_eq!(num("atan2(1, 1)"), 45.0);
    assert_eq!(num("atan2(1, 0)"), 90.0);
    assert_eq!(num("atan2(1, -1)"), 135.0);
    assert_eq!(num("atan2(0, -1)"), 180.0);
    assert_eq!(num("atan2(-1, -1)"), -135.0);
    assert_eq!(num("atan2(-1, 1)"), -45.0);
    assert_eq!(num("atan2(0, 0)"), 0.0);
}

// -------------------------------------------------------------- math scalar

#[test]
fn abs_and_sign() {
    assert_eq!(num("abs(-3)"), 3.0);
    assert_eq!(num("abs(3)"), 3.0);
    assert_eq!(num("abs(0)"), 0.0);
    assert_eq!(num("sign(-3)"), -1.0);
    assert_eq!(num("sign(0)"), 0.0);
    assert_eq!(num("sign(3)"), 1.0);
    assert!(num("abs(-1/0)").is_infinite());
}

#[test]
fn floor_ceil_round() {
    assert_eq!(num("floor(2.7)"), 2.0);
    assert_eq!(num("floor(-2.1)"), -3.0);
    assert_eq!(num("ceil(2.1)"), 3.0);
    assert_eq!(num("ceil(-2.9)"), -2.0);
}

#[test]
fn round_is_half_away_from_zero() {
    // Upstream builtin_round uses C round() = round-half-away-from-zero,
    // matching Rust f64::round.
    assert_eq!(num("round(2.5)"), 3.0);
    assert_eq!(num("round(-2.5)"), -3.0);
    assert_eq!(num("round(0.5)"), 1.0);
    assert_eq!(num("round(-0.5)"), -1.0);
    assert_eq!(num("round(2.4)"), 2.0);
    assert_eq!(num("round(-2.4)"), -2.0);
}

#[test]
fn logs_and_powers() {
    assert!((num("ln(2.718281828459045)") - 1.0).abs() < EPS);
    // log is base 10 with one arg, log(base, x) with two — both computed as
    // log(x)/log(base), so ~3 within FP error (the openscad binary displays
    // "3" only via format_number's 6-significant-digit rounding).
    assert!((num("log(1000)") - 3.0).abs() < EPS); // base 10
    assert!((num("log(2, 8)") - 3.0).abs() < EPS); // log_2(8)
    assert_eq!(num("pow(2, 10)"), 1024.0);
    assert_eq!(num("pow(9, 0.5)"), 3.0);
    assert_eq!(num("sqrt(16)"), 4.0);
    assert!((num("exp(1)") - std::f64::consts::E).abs() < EPS);
    assert_eq!(num("exp(0)"), 1.0);
    assert!(num("sqrt(-1)").is_nan());
    assert!(num("ln(-1)").is_nan());
}

// --------------------------------------------------------------- min / max

#[test]
fn min_max_variadic() {
    assert_eq!(num("min(1, 5, 3)"), 1.0);
    assert_eq!(num("max(1, 5, 3)"), 5.0);
    assert_eq!(num("min(-1, -5, 3)"), -5.0);
    assert_eq!(num("min(7)"), 7.0);
    assert_eq!(num("max(7)"), 7.0);
}

#[test]
fn min_max_single_vector() {
    assert_eq!(num("min([1, 5, 3])"), 1.0);
    assert_eq!(num("max([4, 2, 9])"), 9.0);
    assert_eq!(num("min([-3])"), -3.0);
}

#[test]
fn min_max_empty_is_undef_with_warning() {
    let (v, e) = eval_with("min()");
    assert!(v.is_undef());
    assert!(has_warning(&e, "min()"));
    assert!(eval("max()").is_undef());
}

#[test]
fn min_of_vector_with_non_number_is_undef() {
    // Divergence: master breaks on non-number -> undef (2021.01 -> 1).
    let (v, e) = eval_with("min([1, \"x\"])");
    assert!(v.is_undef());
    assert!(has_warning(&e, "min()"));
}

// ----------------------------------------------------------- norm / cross

#[test]
fn norm_is_euclidean_length() {
    assert_eq!(num("norm([3, 4])"), 5.0);
    assert_eq!(num("norm([1, 2, 2])"), 3.0);
    assert_eq!(num("norm([])"), 0.0);
    let (v, e) = eval_with("norm([1, \"x\"])");
    assert!(v.is_undef());
    assert!(has_warning(&e, "Incorrect arguments to norm()"));
}

#[test]
fn cross_3d_and_2d() {
    assert_eq!(eval("cross([1, 0, 0], [0, 1, 0])"), eval("[0, 0, 1]"));
    assert_eq!(eval("cross([2, 3, 4], [5, 6, 7])"), eval("[-3, 6, -3]"));
    // 2D cross is the scalar z-component.
    assert_eq!(num("cross([1, 2], [3, 4])"), -2.0);
}

#[test]
fn cross_bad_sizes_and_values_are_undef() {
    let (v, e) = eval_with("cross([1, 2, 3], [1, 2])");
    assert!(v.is_undef());
    assert!(has_warning(
        &e,
        "Invalid vector size of parameter for cross()"
    ));
    assert!(eval("cross([1, 2, 3], [0/0, 1, 2])").is_undef());
}

// ------------------------------------------------------------------- len

#[test]
fn len_of_string_counts_utf8_chars() {
    assert_eq!(num("len(\"hello\")"), 5.0);
    assert_eq!(num("len(\"héllo\")"), 5.0); // é is one glyph, two bytes
    assert_eq!(num("len(\"Лаλ\")"), 3.0);
    assert_eq!(num("len(\"\")"), 0.0);
}

#[test]
fn len_of_vector_is_length() {
    assert_eq!(num("len([1, 2, 3])"), 3.0);
    assert_eq!(num("len([])"), 0.0);
    assert_eq!(num("len([[1, 2], [3, 4]])"), 2.0);
}

#[test]
fn len_of_number_is_undef_with_warning() {
    let (v, e) = eval_with("len(5)");
    assert!(v.is_undef());
    assert!(has_warning(&e, "len()"));
}

// ---------------------------------------------------------------- concat

#[test]
fn concat_flattens_one_level() {
    assert_eq!(eval("concat([1, 2], [3, 4], 5)"), eval("[1, 2, 3, 4, 5]"));
    assert_eq!(eval("concat([[1, 2]], [3])"), eval("[[1, 2], 3]"));
    assert_eq!(eval("concat(1, [2, 3], \"x\")"), eval("[1, 2, 3, \"x\"]"));
    assert_eq!(eval("concat(\"ab\", \"cd\")"), eval("[\"ab\", \"cd\"]"));
}

#[test]
fn concat_empty_is_empty_vector() {
    assert_eq!(eval("concat()"), Value::from(vec![]));
}

// ------------------------------------------------------------------- str

#[test]
fn str_concatenates_string_forms() {
    assert_eq!(eval("str(1, \"x\", true)"), s("1xtrue"));
    assert_eq!(eval("str()"), s(""));
    assert_eq!(eval("str(1.5)"), s("1.5"));
    assert_eq!(eval("str([1, 2])"), s("[1, 2]"));
    assert_eq!(eval("str(undef)"), s("undef"));
    // matches format_number: 6 significant digits
    assert_eq!(eval("str(1/3)"), s("0.333333"));
}

// --------------------------------------------------------------- chr / ord

#[test]
fn chr_from_code_points() {
    assert_eq!(eval("chr(65)"), s("A"));
    assert_eq!(eval("chr([72, 73])"), s("HI"));
    assert_eq!(eval("chr(955)"), s("λ"));
    assert_eq!(eval("chr(127843)"), s("🍣"));
    assert_eq!(eval("chr(0)"), s("")); // c == 0 produces nothing
    assert_eq!(eval("chr(-1)"), s("")); // v <= 0 produces nothing
    assert_eq!(eval("chr([65:67])"), s("ABC")); // range form
}

#[test]
fn ord_of_first_char() {
    assert_eq!(num("ord(\"A\")"), 65.0);
    assert_eq!(num("ord(\"Л\")"), 1051.0);
    assert_eq!(num("ord(\"abc\")"), 97.0); // first char only
    assert!(eval("ord(\"\")").is_undef()); // empty -> undef
}

#[test]
fn chr_ord_roundtrip_including_unicode() {
    assert_eq!(num("ord(chr(65))"), 65.0);
    assert_eq!(num("ord(chr(955))"), 955.0);
    assert_eq!(num("ord(chr(127843))"), 127843.0);
    assert_eq!(eval("chr(ord(\"λ\"))"), s("λ"));
}

// --------------------------------------------------------------- lookup

#[test]
fn lookup_interpolates_between_keys() {
    // Linear interpolation: at key 3 between (0,0) and (10,100) -> 30.
    assert_eq!(num("lookup(3, [[0, 0], [10, 100]])"), 30.0);
    assert_eq!(num("lookup(5, [[0, 0], [10, 100]])"), 50.0);
}

#[test]
fn lookup_clamps_out_of_range() {
    assert_eq!(num("lookup(-5, [[0, 0], [10, 100]])"), 0.0); // below low key
    assert_eq!(num("lookup(50, [[0, 0], [10, 100]])"), 100.0); // above high key
}

#[test]
fn lookup_exact_key_hits_table_value() {
    assert_eq!(num("lookup(10, [[0, 0], [10, 100], [20, 200]])"), 100.0);
    assert_eq!(num("lookup(0, [[0, 0], [10, 100]])"), 0.0);
}

#[test]
fn lookup_bad_arguments_are_undef() {
    let (v, e) = eval_with("lookup(\"x\", [[0, 0]])");
    assert!(v.is_undef());
    assert!(has_warning(&e, "lookup"));
    assert!(eval("lookup(1, 2)").is_undef());
}

// --------------------------------------------------------------- search

#[test]
fn search_string_in_string() {
    // default num_returns_per_match = 1 -> flat index list, one per char.
    assert_eq!(eval("search(\"a\", \"abcdabcd\")"), eval("[0]"));
    // num_returns_per_match = 0 -> all matches, nested per char.
    assert_eq!(eval("search(\"a\", \"abcdabcd\", 0)"), eval("[[0, 4]]"));
    // no match with num_returns = 1 contributes nothing.
    assert_eq!(eval("search(\"e\", \"abcdabcd\", 1)"), eval("[]"));
    // multi-char find, all matches.
    assert_eq!(
        eval("search(\"aЛ\", \"aЛЛa\", 0)"),
        eval("[[0, 3], [1, 2]]")
    );
}

#[test]
fn search_string_in_vector_table() {
    assert_eq!(
        eval("search(\"abc\", [[\"a\", 1], [\"b\", 2], [\"c\", 3], [\"d\", 4], [\"a\", 5]], 0)"),
        eval("[[0, 4], [1], [2]]")
    );
    // default num_returns = 1: first match per char, flat.
    assert_eq!(
        eval("search(\"abc\", [[\"a\", 1], [\"b\", 2], [\"c\", 3]])"),
        eval("[0, 1, 2]")
    );
}

#[test]
fn search_number_in_vector_with_index_column() {
    // Search column 1 for the number 3.
    assert_eq!(
        eval("search(3, [[\"a\", 1], [\"b\", 2], [\"c\", 3]], 0, 1)"),
        eval("[2]")
    );
    // default column 0.
    assert_eq!(eval("search(5, [1, 5, 3, 5])"), eval("[1]"));
    assert_eq!(eval("search(5, [1, 5, 3, 5], 0)"), eval("[1, 3]"));
}

#[test]
fn search_bad_arity_is_undef() {
    let (v, e) = eval_with("search(\"a\")");
    assert!(v.is_undef());
    assert!(has_warning(&e, "search()"));
}

// ----------------------------------------------------------- predicates

#[test]
fn is_undef_predicate() {
    assert_eq!(eval("is_undef(undef)"), Value::Bool(true));
    assert_eq!(eval("is_undef(5)"), Value::Bool(false));
    assert_eq!(eval("is_undef(\"x\")"), Value::Bool(false));
}

#[test]
fn is_undef_of_unknown_variable_does_not_warn() {
    // Upstream is_undef uses try_lookup_variable and suppresses the
    // "unknown variable" warning for a bare identifier argument.
    let (v, e) = eval_with("is_undef(nonexistent)");
    assert_eq!(v, Value::Bool(true));
    assert!(!has_warning(&e, "Ignoring unknown variable"));
}

#[test]
fn is_num_predicate() {
    assert_eq!(eval("is_num(5)"), Value::Bool(true));
    assert_eq!(eval("is_num(-1.5)"), Value::Bool(true));
    assert_eq!(eval("is_num(\"x\")"), Value::Bool(false));
    assert_eq!(eval("is_num(undef)"), Value::Bool(false));
    // NaN is not considered a number by is_num.
    assert_eq!(eval("is_num(0/0)"), Value::Bool(false));
}

#[test]
fn is_bool_string_list_predicates() {
    assert_eq!(eval("is_bool(true)"), Value::Bool(true));
    assert_eq!(eval("is_bool(1)"), Value::Bool(false));
    assert_eq!(eval("is_string(\"a\")"), Value::Bool(true));
    assert_eq!(eval("is_string(1)"), Value::Bool(false));
    assert_eq!(eval("is_list([1, 2])"), Value::Bool(true));
    assert_eq!(eval("is_list([])"), Value::Bool(true));
    assert_eq!(eval("is_list(\"a\")"), Value::Bool(false));
}

#[test]
fn is_function_is_true_only_for_function_values() {
    assert_eq!(eval("is_function(5)"), Value::Bool(false));
    assert_eq!(eval("is_function(undef)"), Value::Bool(false));
    assert_eq!(eval("is_function(function(x) x)"), Value::Bool(true));
}

// ------------------------------------------------------------- version

#[test]
fn version_and_version_num() {
    assert_eq!(eval("version()"), eval("[2021, 1, 0]"));
    assert_eq!(num("version_num()"), 20210100.0);
    assert_eq!(num("version_num([2019, 5, 1])"), 20190501.0);
}

// --------------------------------------------------------------- rands

#[test]
fn rands_is_deterministic_and_in_range() {
    let a = vec_of_nums("rands(0, 1, 5, 42)");
    let b = vec_of_nums("rands(0, 1, 5, 42)");
    assert_eq!(a.len(), 5);
    assert_eq!(a, b); // same seed -> same sequence
    for x in &a {
        assert!(*x >= 0.0 && *x < 1.0, "out of range: {x}");
    }
    // different seed -> (almost surely) different sequence
    let c = vec_of_nums("rands(0, 1, 5, 99)");
    assert_ne!(a, c);
    // range respected for a non-unit interval
    for x in vec_of_nums("rands(10, 20, 8, 7)") {
        assert!((10.0..20.0).contains(&x));
    }
}

#[test]
fn rands_min_equals_max_is_constant() {
    assert_eq!(vec_of_nums("rands(3, 3, 4, 1)"), vec![3.0, 3.0, 3.0, 3.0]);
}

#[test]
fn rands_huge_count_warns_and_is_undef_not_abort() {
    // 1e11 used to abort on allocation failure; 1e300 saturated the
    // `as usize` cast and panicked in Vec::with_capacity.
    for src in [
        "rands(0, 1, 1e11)",
        "rands(0, 1, 1e300)",
        "rands(0, 1, -1e300)",
    ] {
        let (v, e) = eval_with(src);
        assert!(v.is_undef(), "`{src}` must be undef, got {v:?}");
        assert!(has_warning(&e, "exceeds the limit"), "`{src}` must warn");
    }
}

#[test]
fn rands_count_at_limit_is_allowed() {
    let (v, _) = eval_with("rands(0, 1, 1000000, 3)");
    match &v {
        Value::Vector(items) => assert_eq!(items.len(), 1_000_000),
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn rands_nan_count_warns_and_is_empty() {
    let (v, e) = eval_with("rands(0, 1, 0/0)");
    assert_eq!(v, Value::from(vec![]));
    assert!(has_warning(&e, "NaN"));
}

#[test]
fn rands_negative_count_uses_magnitude() {
    assert_eq!(vec_of_nums("rands(0, 1, -3, 5)").len(), 3);
}

#[test]
fn rands_bad_arity_is_undef() {
    let (v, e) = eval_with("rands(0, 1)");
    assert!(v.is_undef());
    assert!(has_warning(&e, "rands()"));
}

// ----------------------------------------------------- arity / type errors

#[test]
fn scalar_math_type_mismatch_is_undef_with_warning() {
    let (v, e) = eval_with("abs(\"x\")");
    assert!(v.is_undef());
    assert!(has_warning(&e, "abs()"));
    assert!(eval("sin(\"x\")").is_undef());
    assert!(eval("sqrt([1])").is_undef());
}

#[test]
fn scalar_math_arity_mismatch_is_undef_with_warning() {
    let (v, e) = eval_with("abs(1, 2)");
    assert!(v.is_undef());
    assert!(has_warning(&e, "abs()"));
    assert!(eval("pow(2)").is_undef());
    assert!(eval("atan2(1)").is_undef());
    assert!(eval("sin()").is_undef());
}

#[test]
fn unknown_function_still_warns_and_is_undef() {
    // Names that are neither user functions nor builtins keep the old path.
    let (v, e) = eval_with("not_a_builtin(1)");
    assert!(v.is_undef());
    assert!(has_warning(&e, "Ignoring unknown function 'not_a_builtin'"));
}

#[test]
fn builtin_does_not_shadow_user_function() {
    // A user-defined function named like a builtin wins (upstream
    // Context::lookup_function precedes the builtin table).
    let mut evaluator = Evaluator::new();
    let program = morphe_lang::parser::parse_program("function abs(x) = 42;").expect("parse");
    evaluator.eval_program(&program);
    let call = parse_expr("abs(-3)").expect("parse");
    assert_eq!(evaluator.eval_expr(&call), n(42.0));
}

// -------------------------------------------- oracle-exactness corrections

#[test]
fn min_max_propagate_nan_like_std_element_search() {
    // std::max_element / std::min_element keep the accumulator unless the
    // next element compares strictly greater/less; NaN comparisons are all
    // false, so a NaN accumulator survives but a later NaN does not.
    assert!(num("max([0/0, 5])").is_nan());
    assert_eq!(num("max([5, 0/0])"), 5.0);
    assert!(num("min([0/0, 5])").is_nan());
    assert_eq!(num("min([5, 0/0])"), 5.0);
    // variadic form behaves identically
    assert!(num("max(0/0, 5, 3)").is_nan());
    assert_eq!(num("min(5, 0/0, 3)"), 3.0);
}

#[test]
fn norm_non_vector_single_arg_is_silent() {
    // 2021.01: norm(5) is undef with NO warning; only a wrong arg *count*
    // warns.
    let (v, e) = eval_with("norm(5)");
    assert!(v.is_undef());
    assert!(!has_warning(&e, "norm()"));
    let (v2, e2) = eval_with("norm(1, 2)");
    assert!(v2.is_undef());
    assert!(has_warning(
        &e2,
        "norm() number of parameters does not match"
    ));
    assert!(eval("norm()").is_undef());
}

#[test]
fn cross_type_and_count_messages() {
    let (v, e) = eval_with("cross(5, 6)");
    assert!(v.is_undef());
    assert!(has_warning(&e, "Invalid type of parameters for cross()"));
    let (v2, e2) = eval_with("cross([1, 2, 3])");
    assert!(v2.is_undef());
    assert!(has_warning(&e2, "Invalid number of parameters for cross()"));
    assert!(eval("cross([1, 0, 0], [0, 1, 0], [0, 0, 1])").is_undef());
}

#[test]
fn ord_error_messages() {
    let (v, e) = eval_with("ord(5)");
    assert!(v.is_undef());
    assert!(has_warning(&e, "ord() argument 5 is not of type string"));
    let (v2, e2) = eval_with("ord(\"a\", \"b\")");
    assert!(v2.is_undef());
    assert!(has_warning(
        &e2,
        "ord() called with 2 arguments, only 1 argument expected"
    ));
}

#[test]
fn chr_wraps_huge_code_points_mod_2_32() {
    // Upstream C `(gunichar)v` wraps modulo 2^32 (not saturate):
    // 0x100000041 -> 0x41 == 'A'.
    assert_eq!(eval("chr(4294967361)"), s("A")); // 0x1_0000_0041
    assert_eq!(eval("chr(4294967296 + 955)"), s("λ")); // 0x1_0000_03BB
                                                       // beyond i64 range still yields nothing, matching the binary
    assert_eq!(eval("chr(1e30)"), s(""));
    // an exact multiple of 2^32 wraps to 0 and is skipped
    assert_eq!(eval("chr(4294967296)"), s(""));
}

#[test]
fn lookup_requires_exactly_two_arguments() {
    // 2021.01 rejects extra arguments (check_arguments requires {num,vec}).
    let (v, e) = eval_with("lookup(3, [[0, 0], [10, 100]], 99, 99)");
    assert!(v.is_undef());
    assert!(has_warning(
        &e,
        "lookup() number of parameters does not match"
    ));
}

#[test]
fn lookup_non_finite_key_is_undef_with_named_message() {
    let (v, e) = eval_with("lookup(1/0, [[0, 0], [10, 100]])");
    assert!(v.is_undef());
    assert!(has_warning(
        &e,
        "lookup(inf, ...) first argument is not a number"
    ));
    let (v2, e2) = eval_with("lookup(0/0, [[0, 0], [10, 100]])");
    assert!(v2.is_undef());
    assert!(has_warning(
        &e2,
        "lookup(nan, ...) first argument is not a number"
    ));
}

#[test]
fn search_non_matchable_find_is_undef() {
    // A bool/undef/range find falls through to undef (not an empty vector).
    assert!(eval("search(true, [1, true, 2])").is_undef());
    assert!(eval("search(undef, [1, 2])").is_undef());
}

#[test]
fn search_too_short_table_entry_aborts_to_empty() {
    let (v, e) = eval_with("search(\"a\", [[\"a\"], [\"b\"]], 0, 1)");
    assert_eq!(v, Value::from(vec![]));
    assert!(has_warning(&e, "Invalid entry in search vector at index 0"));
}

#[test]
fn version_num_accepts_two_element_vector() {
    // getVec3 defaults a missing day to 0.
    assert_eq!(num("version_num([2019, 5])"), 20190500.0);
    assert_eq!(num("version_num([2019, 5, 1])"), 20190501.0);
    // other shapes are undef
    assert!(eval("version_num([2019])").is_undef());
    assert!(eval("version_num([2019, 5, 1, 9])").is_undef());
    assert!(eval("version_num(5)").is_undef());
}

#[test]
fn rands_negative_count_uses_absolute_value() {
    // Upstream `std::abs(numresults)`: a negative count yields |count|
    // results (verified against the 2021.01 binary).
    assert_eq!(vec_of_nums("rands(0, 1, -5, 42)").len(), 5);
}

#[test]
fn atan2_snaps_to_whole_degrees() {
    // Both master and 2021.01 `atan2_degrees` snap within 3e-14 of a whole
    // degree, so exact quadrant angles come back exact (not just via
    // display rounding).
    assert_eq!(num("atan2(1, 1)"), 45.0);
    assert_eq!(num("atan2(1, -1)"), 135.0);
    assert_eq!(num("atan2(-1, -1)"), -135.0);
}
